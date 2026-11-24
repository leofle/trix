//! Context gauge and token counts: how full each agent's context window is,
//! and how many tokens it has used.
//!
//! Claude and Codex both log every turn's token usage in their conversation
//! file. A background thread follows those files, reading only what was
//! appended since the last look, and reports each agent's latest figure and
//! its running total (priced in `cost.rs`).
//!
//! - Codex logs the window too (`model_context_window`), so its gauge is exact.
//! - Claude doesn't. trix assumes 200K, and learns 1M for a model once any of
//!   its conversations goes past 200K without being compacted (remembered in
//!   prefs.json). `TRIX_CLAUDE_CONTEXT=<tokens>` sets it outright.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent::Kind;
use crate::app::AppEvent;

const STANDARD: u64 = 200_000;
const LONG: u64 = 1_000_000;
/// How much of a log to read at a time.
const CHUNK: u64 = 4 << 20;
/// How soon to look again for a log that wasn't there.
const RELOOK: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Context {
    /// Tokens in the context as of the last turn.
    pub used: u64,
    pub window: u64,
    /// The window comes from the agent itself (Codex, or an override), not a guess.
    pub exact: bool,
}

impl Context {
    pub fn used_frac(&self) -> f32 {
        if self.window == 0 { 0.0 } else { (self.used as f32 / self.window as f32).min(1.0) }
    }
    pub fn used_percent(&self) -> u8 {
        (self.used_frac() * 100.0).round() as u8
    }
    pub fn left(&self) -> u64 {
        self.window.saturating_sub(self.used)
    }
}

/// "690k", "1.2M", "800".
pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => format!("{}k", (n as f64 / 1_000.0).round() as u64),
        _ => {
            let m = n as f64 / 1_000_000.0;
            if m >= 10.0 || (m - m.round()).abs() < 0.05 { format!("{}M", m.round() as u64) } else { format!("{m:.1}M") }
        }
    }
}

/// What the watcher needs to find an agent's conversation log.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub id: u32,
    pub kind: Kind,
    /// From the agent's hooks (Claude's `transcript_path`), when it sent one.
    pub transcript: Option<PathBuf>,
    /// The conversation id, to look the log up otherwise.
    pub session: Option<String>,
    /// When the agent started, in Unix seconds: its log counts towards the
    /// month from then (`ledger.rs`).
    pub since: u64,
}

pub type Targets = Arc<Mutex<Vec<Target>>>;

/// One kind of model call, as it's priced: which model, in fast mode or
/// not, and how long its prompt was (`cost::tier`: some models charge more
/// past a size).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct Bucket {
    pub model: String,
    pub fast: bool,
    pub prompt: u32,
}

/// Tokens used, by how they're charged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tokens {
    /// Input that wasn't read from the prompt cache or written to it.
    pub input: u64,
    /// Written to the cache: for 5 minutes (Claude), or at all (Codex).
    pub cache_write_5m: u64,
    /// Written to the cache for an hour (Claude).
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.cache_write() + self.cache_read + self.output
    }
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }
    pub(crate) fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
        self.cache_read += o.cache_read;
        self.output += o.output;
    }
    fn sub(&mut self, o: &Tokens) {
        self.input = self.input.saturating_sub(o.input);
        self.cache_write_5m = self.cache_write_5m.saturating_sub(o.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.saturating_sub(o.cache_write_1h);
        self.cache_read = self.cache_read.saturating_sub(o.cache_read);
        self.output = self.output.saturating_sub(o.output);
    }
    fn covers(&self, o: &Tokens) -> bool {
        self.input >= o.input && self.cache_write_5m >= o.cache_write_5m && self.cache_write_1h >= o.cache_write_1h && self.cache_read >= o.cache_read && self.output >= o.output
    }
}

/// An agent's tokens so far, per kind of call.
pub type Spend = BTreeMap<Bucket, Tokens>;

/// A `Spend` as a list, which JSON can hold: its keys aren't strings.
pub fn to_list(s: &Spend) -> Vec<(Bucket, Tokens)> {
    s.iter().map(|(b, t)| (b.clone(), *t)).collect()
}

/// Every kind of call's tokens added up.
pub fn total(s: &Spend) -> Tokens {
    let mut t = Tokens::default();
    for v in s.values() {
        t.add(v);
    }
    t
}

pub fn merge(into: &mut Spend, from: &Spend) {
    for (b, t) in from {
        into.entry(b.clone()).or_default().add(t);
    }
}

// ───────────────────────────── reading logs ─────────────────────────────

/// The latest usage in a Claude log line: (tokens in context, model).
pub fn claude_line(v: &Value) -> Option<(u64, String)> {
    if v.get("type")?.as_str()? != "assistant" || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let msg = v.get("message")?;
    let model = msg.get("model")?.as_str()?;
    if model.starts_with('<') {
        return None; // "<synthetic>": a placeholder, not a model call
    }
    let u = msg.get("usage")?;
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let used = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens") + n("output_tokens");
    (used > 0).then(|| (used, model.to_string()))
}

/// A Claude log line's model call, to count: its message id (Claude writes a
/// line per content block, each with the message's usage), how it's priced
/// and its tokens. Subagents' turns count too: they're spent all the same.
pub fn claude_spend(v: &Value) -> Option<(Option<String>, Bucket, Tokens)> {
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let msg = v.get("message")?;
    let model = msg.get("model")?.as_str()?;
    if model.starts_with('<') {
        return None;
    }
    let u = msg.get("usage")?;
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let written = n(u, "cache_creation_input_tokens");
    let (mut w5, w1) = match u.get("cache_creation") {
        Some(c) => (n(c, "ephemeral_5m_input_tokens"), n(c, "ephemeral_1h_input_tokens")),
        None => (written, 0),
    };
    w5 += written.saturating_sub(w5 + w1);
    let t = Tokens { input: n(u, "input_tokens"), cache_write_5m: w5, cache_write_1h: w1, cache_read: n(u, "cache_read_input_tokens"), output: n(u, "output_tokens") };
    if t.total() == 0 {
        return None;
    }
    let bucket = Bucket {
        model: model.to_string(),
        fast: u.get("speed").and_then(Value::as_str) == Some("fast"),
        prompt: crate::cost::tier(t.input + t.cache_write() + t.cache_read),
    };
    let id = msg.get("id").and_then(Value::as_str).map(str::to_string);
    Some((id, bucket, t))
}

/// A Codex log line's running total for the conversation. Codex counts
/// cached input as part of input; here it's apart.
pub fn codex_total(v: &Value) -> Option<Tokens> {
    let p = v.get("payload")?;
    if p.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let u = p.get("info")?.get("total_token_usage")?;
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (read, written) = (n("cached_input_tokens"), n("cache_write_input_tokens"));
    Some(Tokens { input: n("input_tokens").saturating_sub(read + written), cache_write_5m: written, cache_write_1h: 0, cache_read: read, output: n("output_tokens") })
}

/// How long a Codex count's last prompt was.
pub fn codex_prompt(v: &Value) -> u64 {
    v.pointer("/payload/info/last_token_usage/input_tokens").and_then(Value::as_u64).unwrap_or(0)
}

/// The model a Codex turn runs on, from its `turn_context` line.
pub fn codex_model(v: &Value) -> Option<String> {
    if v.get("type")?.as_str()? != "turn_context" {
        return None;
    }
    Some(v.get("payload")?.get("model")?.as_str()?.to_string())
}

/// The latest usage in a Codex log line: (tokens in context, window).
pub fn codex_line(v: &Value) -> Option<(u64, u64)> {
    let p = v.get("payload")?;
    if p.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let info = p.get("info")?;
    let used = info.get("last_token_usage")?.get("total_tokens")?.as_u64()?;
    let window = info.get("model_context_window")?.as_u64()?;
    Some((used, window))
}

/// Claude's window for `model`: an override, the `[1m]` variant, what this
/// model was seen to hold before, or the standard 200K. Past 200K means 1M:
/// there's nothing in between.
pub fn claude_window(model: &str, used: u64, overridden: Option<u64>, learned: &BTreeMap<String, u64>) -> (u64, bool) {
    if let Some(w) = overridden {
        return (w, true);
    }
    if model.contains("[1m]") {
        return (LONG, true);
    }
    let known = learned.get(model).copied().unwrap_or(STANDARD);
    (if used > known { LONG.max(known) } else { known }, false)
}

/// A log being followed.
#[derive(Default)]
struct Log {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    claude: Option<(u64, String)>,
    codex: Option<(u64, u64)>,
    /// Every turn's tokens so far.
    spend: Spend,
    /// Claude: each message's tokens as counted, so a message logged again
    /// (a line per content block) replaces itself rather than adding up.
    messages: HashMap<String, (Bucket, Tokens)>,
    /// Codex: the running total as of the last count, and the model in use.
    codex_total: Tokens,
    codex_model: Option<String>,
    /// Each turn's tokens read since the ledger last took them, dated.
    fresh: Vec<crate::ledger::Entry>,
    /// Where the line being read ends.
    line_end: u64,
}

impl Log {
    fn new(path: PathBuf) -> Log {
        Log { path, ..Log::default() }
    }

    /// Read what was appended since last time, a piece at a time. False
    /// when the file is gone.
    fn advance(&mut self) -> bool {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return false };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Log::new(std::mem::take(&mut self.path)); // rewritten
        }
        while self.offset < len {
            if f.seek(SeekFrom::Start(self.offset)).is_err() {
                return true;
            }
            let mut buf = Vec::new();
            if (&mut f).take((len - self.offset).min(CHUNK)).read_to_end(&mut buf).is_err() || buf.is_empty() {
                return true;
            }
            self.offset += buf.len() as u64;
            let mut data = std::mem::take(&mut self.partial);
            let mut end = self.offset - buf.len() as u64 - data.len() as u64;
            data.extend_from_slice(&buf);
            let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
            let rest = lines.pop().unwrap_or_default().to_vec();
            for line in lines {
                end += line.len() as u64 + 1;
                self.line_end = end;
                self.line(line);
            }
            self.partial = rest;
        }
        true
    }

    fn line(&mut self, line: &[u8]) {
        // Most of a log is messages and tool output: only usage is parsed.
        let has = |w: &[u8]| line.windows(w.len()).any(|x| x == w);
        if !has(b"usage") && !has(b"turn_context") {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
        if let Some(c) = claude_line(&v) {
            self.claude = Some(c);
        } else if let Some(c) = codex_line(&v) {
            self.codex = Some(c);
        }
        let at = v.get("timestamp").and_then(Value::as_str).and_then(crate::ledger::parse_time).unwrap_or_else(crate::prices::now);
        if let Some((id, bucket, t)) = claude_spend(&v) {
            // What this line adds: a message logged again adds only what grew.
            let mut new = t;
            if let Some(id) = id
                && let Some((b, old)) = self.messages.insert(id, (bucket.clone(), t))
            {
                self.spend.entry(b).or_default().sub(&old);
                new.sub(&old);
            }
            self.spend.entry(bucket.clone()).or_default().add(&t);
            self.dated(at, bucket, new);
        } else if let Some(m) = codex_model(&v) {
            self.codex_model = Some(m);
        } else if let Some(total) = codex_total(&v) {
            // A count lower than the last one started over.
            let mut new = total;
            if total.covers(&self.codex_total) {
                new.sub(&self.codex_total);
            }
            self.codex_total = total;
            if new.total() > 0 {
                let model = self.codex_model.clone().unwrap_or_else(|| "unknown".into());
                let prompt = crate::cost::tier(codex_prompt(&v));
                let bucket = Bucket { model, fast: false, prompt };
                self.spend.entry(bucket.clone()).or_default().add(&new);
                self.dated(at, bucket, new);
            }
        }
    }
}

impl Log {
    fn dated(&mut self, at: u64, bucket: Bucket, tokens: Tokens) {
        if tokens.total() > 0 {
            self.fresh.push(crate::ledger::Entry { end: self.line_end, at, bucket, tokens });
        }
    }

    /// What it read since last time, for the ledger.
    fn take(&mut self, since: u64) -> crate::ledger::Read {
        crate::ledger::Read { path: self.path.clone(), done: self.offset - self.partial.len() as u64, since, entries: std::mem::take(&mut self.fresh) }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".claude"))
}

fn codex_dir() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"))
}

/// `<claude>/projects/<any project>/<session>.jsonl`.
fn find_claude(root: &Path, session: &str) -> Option<PathBuf> {
    let name = format!("{session}.jsonl");
    std::fs::read_dir(root.join("projects")).ok()?.flatten().map(|e| e.path().join(&name)).find(|p| p.is_file())
}

/// `<codex>/sessions/YYYY/MM/DD/rollout-YYYY-MM-DDTHH-MM-SS-<thread>.jsonl`,
/// newest days first.
fn find_codex(root: &Path, thread: &str) -> Option<PathBuf> {
    const PREFIX: usize = "rollout-2026-11-11T15-19-07-".len();
    let is_it = |n: &str| n.strip_suffix(".jsonl").is_some_and(|stem| stem.starts_with("rollout-") && stem.get(PREFIX..) == Some(thread));
    let sorted = |p: &Path| {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        v.sort();
        v.reverse();
        v
    };
    for y in sorted(&root.join("sessions")) {
        for m in sorted(&y) {
            for d in sorted(&m) {
                if let Some(f) = sorted(&d).into_iter().find(|f| f.file_name().and_then(|n| n.to_str()).is_some_and(is_it)) {
                    return Some(f);
                }
            }
        }
    }
    None
}

/// An agent's conversation log, where trix would follow it.
pub fn log_of(kind: Kind, transcript: Option<PathBuf>, session: Option<String>) -> Option<PathBuf> {
    locate(&Target { id: 0, kind, transcript, session, since: 0 })
}

/// Everything a log has spent, read once from start to end.
pub fn spent_in(path: &Path) -> Spend {
    let mut log = Log::new(path.to_path_buf());
    log.advance();
    log.spend
}

/// `spend` without `base`, kind of call by kind of call: what a clone spent
/// itself, its log starting with its original's conversation.
pub fn less(spend: &Spend, base: &Spend) -> Spend {
    let mut out = spend.clone();
    for (b, t) in base {
        if let Some(have) = out.get_mut(b) {
            have.sub(t);
        }
    }
    out.retain(|_, t| t.total() > 0);
    out
}

/// Claude resumes only conversations logged under the folder it starts in:
/// `<claude>/projects/<cwd, each character but a letter or digit a dash>/`.
/// Where `cwd` would look for `session`.
pub fn claude_log_under(cwd: &Path, session: &str) -> PathBuf {
    let dir: String = cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    claude_dir().join("projects").join(dir).join(format!("{session}.jsonl"))
}

fn locate(t: &Target) -> Option<PathBuf> {
    // The agent said where its log is (Claude writes it only with the first
    // message): there's nothing to find anywhere else meanwhile.
    if let Some(p) = &t.transcript {
        return p.is_file().then(|| p.clone());
    }
    let s = t.session.as_deref()?;
    match t.kind {
        Kind::Claude => find_claude(&claude_dir(), s),
        Kind::Codex => find_codex(&codex_dir(), s),
        _ => None,
    }
}

/// Claude's subagents log apart, next to the conversation:
/// `<session>.jsonl` and `<session>/subagents/agent-*.jsonl`.
fn subagent_logs(main: &Path) -> Vec<PathBuf> {
    let dir = main.with_extension("").join("subagents");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    v.sort();
    v
}

/// One agent's logs being followed.
struct Watched {
    /// What the log was found from: a path, or a conversation id.
    key: Option<String>,
    log: Log,
    subagents: BTreeMap<PathBuf, Log>,
    /// Spent in the agent's earlier conversations (before a `/clear`).
    earlier: Spend,
}

impl Watched {
    fn spend(&self) -> Spend {
        let mut s = self.earlier.clone();
        merge(&mut s, &self.log.spend);
        for l in self.subagents.values() {
            merge(&mut s, &l.spend);
        }
        s
    }

    /// The agent is in another conversation now: what it spent in the one
    /// before stays its own.
    fn switch(&mut self, key: Option<String>, path: PathBuf) {
        if path != self.log.path {
            self.earlier = self.spend();
            self.subagents.clear();
            self.log = Log::new(path);
        }
        self.key = key;
    }

    fn advance(&mut self, kind: Kind) {
        self.log.advance();
        if kind == Kind::Claude {
            for p in subagent_logs(&self.log.path) {
                self.subagents.entry(p.clone()).or_insert_with(|| Log::new(p));
            }
            self.subagents.retain(|_, l| l.advance());
        }
    }
}

/// Follow every target's log; report each agent's context and spend when
/// they change.
pub fn spawn_watcher(targets: Targets, tx: Sender<AppEvent>) {
    let overridden = std::env::var("TRIX_CLAUDE_CONTEXT").ok().and_then(|s| s.trim().parse::<u64>().ok()).filter(|&n| n > 0);
    let _ = std::thread::Builder::new().name("trix-usage".into()).spawn(move || {
        let mut learned = crate::state::load_prefs().context_windows;
        let mut logs: HashMap<u32, Watched> = HashMap::new();
        let mut sent: HashMap<u32, Option<Context>> = HashMap::new();
        let mut sent_spend: HashMap<u32, Spend> = HashMap::new();
        // Logs looked for and not found: what was looked for, and when.
        let mut missed: HashMap<u32, (Option<String>, Instant)> = HashMap::new();
        // The month's record: what it holds is sent when this session adds to
        // it, and when another one has (looked at every half minute).
        let ledger = crate::ledger::dir();
        let _ = crate::ledger::prune(&ledger);
        let mut month: Option<(crate::ledger::Ym, Option<std::time::SystemTime>)> = None;
        let mut looked = Instant::now();
        loop {
            let mut reads = Vec::new();
            let list = targets.lock().unwrap().clone();
            logs.retain(|id, _| list.iter().any(|t| t.id == *id));
            sent.retain(|id, _| list.iter().any(|t| t.id == *id));
            sent_spend.retain(|id, _| list.iter().any(|t| t.id == *id));
            missed.retain(|id, _| list.iter().any(|t| t.id == *id));
            for t in list.iter().filter(|t| matches!(t.kind, Kind::Claude | Kind::Codex)) {
                let key = t.transcript.as_ref().map(|p| p.display().to_string()).or_else(|| t.session.clone());
                let stale = logs.get(&t.id).is_none_or(|w| w.key != key || !w.log.path.is_file());
                if stale {
                    // Finding a log by its id reads every Claude project, or
                    // every day of Codex sessions: not every 2 seconds. (A
                    // path the agent gave is one look, so that one is.)
                    if t.transcript.is_none() && missed.get(&t.id).is_some_and(|(k, at)| *k == key && at.elapsed() < RELOOK) {
                        continue;
                    }
                    let Some(path) = locate(t) else {
                        missed.insert(t.id, (key, Instant::now()));
                        continue;
                    };
                    missed.remove(&t.id);
                    match logs.get_mut(&t.id) {
                        Some(w) => w.switch(key, path),
                        None => {
                            logs.insert(t.id, Watched { key, log: Log::new(path), subagents: BTreeMap::new(), earlier: Spend::new() });
                        }
                    }
                }
                let w = logs.get_mut(&t.id).unwrap();
                w.advance(t.kind);
                reads.push(w.log.take(t.since));
                reads.extend(w.subagents.values_mut().map(|l| l.take(t.since)));
                let log = &w.log;
                let ctx = match t.kind {
                    Kind::Codex => log.codex.map(|(used, window)| Context { used, window, exact: true }),
                    _ => log.claude.as_ref().map(|(used, model)| {
                        let (window, exact) = claude_window(model, *used, overridden, &learned);
                        if !exact && window > learned.get(model).copied().unwrap_or(STANDARD) {
                            learned.insert(model.clone(), window);
                            let mut prefs = crate::state::load_prefs();
                            prefs.context_windows.insert(model.clone(), window);
                            let _ = crate::state::store_prefs(&prefs);
                        }
                        Context { used: *used, window, exact }
                    }),
                };
                if sent.get(&t.id) != Some(&ctx) {
                    sent.insert(t.id, ctx);
                    if tx.send(AppEvent::Context(t.id, ctx)).is_err() {
                        return;
                    }
                }
                let spend = w.spend();
                if sent_spend.get(&t.id) != Some(&spend) {
                    sent_spend.insert(t.id, spend.clone());
                    if tx.send(AppEvent::Spend(t.id, spend)).is_err() {
                        return;
                    }
                }
            }
            let wrote = crate::ledger::record(&ledger, &reads).map(|m| !m.is_empty()).unwrap_or(false);
            let ym = crate::ledger::Ym::now();
            if wrote || month.is_none_or(|(m, _)| m != ym) || looked.elapsed() >= Duration::from_secs(30) {
                looked = Instant::now();
                let at = crate::ledger::changed(&ledger, ym);
                if month != Some((ym, at)) {
                    month = Some((ym, at));
                    if tx.send(AppEvent::Month(ym, crate::ledger::load(&ledger, ym))).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("trix-test-usage-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn claude_turn(used: u64, model: &str) -> Value {
        json!({"type": "assistant", "isSidechain": false, "message": {"model": model, "usage": {
            "input_tokens": 2, "cache_creation_input_tokens": 100, "cache_read_input_tokens": used - 2 - 100 - 50, "output_tokens": 50}}})
    }
    fn codex_turn(used: u64, window: u64) -> Value {
        json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"total_tokens": used * 9}, "last_token_usage": {"total_tokens": used}, "model_context_window": window}}})
    }

    #[test]
    fn reads_claude_and_codex_lines() {
        assert_eq!(claude_line(&claude_turn(137_648, "claude-opus-5-5")), Some((137_648, "claude-opus-5-5".into())));
        let mut side = claude_turn(5_000, "claude-opus-5-5");
        side["isSidechain"] = json!(true);
        assert_eq!(claude_line(&side), None, "subagents have their own context");
        assert_eq!(claude_line(&json!({"type": "assistant", "message": {"model": "<synthetic>", "usage": {"input_tokens": 0}}})), None);
        assert_eq!(claude_line(&json!({"type": "user", "message": {"content": "hi"}})), None);
        assert_eq!(codex_line(&codex_turn(35_491, 258_400)), Some((35_491, 258_400)), "the last turn, not the running total");
        assert_eq!(codex_line(&json!({"payload": {"type": "token_count", "info": null}})), None);
        assert_eq!(codex_line(&claude_turn(10_000, "x")), None);
    }

    #[test]
    fn claude_windows() {
        let none = BTreeMap::new();
        assert_eq!(claude_window("claude-opus-5-5", 138_000, None, &none), (200_000, false));
        assert_eq!(claude_window("claude-opus-5-5", 309_000, None, &none), (1_000_000, false), "past 200K: must be 1M");
        let learned = BTreeMap::from([("claude-opus-5-5".to_string(), 1_000_000)]);
        assert_eq!(claude_window("claude-opus-5-5", 20_000, None, &learned), (1_000_000, false), "remembered per model");
        assert_eq!(claude_window("claude-haiku-5-5", 20_000, None, &learned), (200_000, false));
        assert_eq!(claude_window("claude-sonnet-5-5[1m]", 20_000, None, &none), (1_000_000, true));
        assert_eq!(claude_window("claude-opus-5-5", 20_000, Some(500_000), &none), (500_000, true), "the override wins");
    }

    #[test]
    fn follows_a_growing_log() {
        let dir = tmp();
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let mut log = Log::new(path.clone());
        assert!(log.advance());
        assert_eq!(log.claude, None);
        writeln!(f, "{}", claude_turn(10_000, "m")).unwrap();
        write!(f, "{}", &claude_turn(20_000, "m").to_string()[..30]).unwrap(); // half a line
        log.advance();
        assert_eq!(log.claude.as_ref().map(|c| c.0), Some(10_000), "a half-written line waits");
        writeln!(f, "{}", &claude_turn(20_000, "m").to_string()[30..]).unwrap();
        writeln!(f, "not json").unwrap();
        log.advance();
        assert_eq!(log.claude.as_ref().map(|c| c.0), Some(20_000));
        drop(f);
        std::fs::write(&path, format!("{}\n", claude_turn(3_000, "m"))).unwrap(); // /clear-style rewrite
        log.advance();
        assert_eq!(log.claude.as_ref().map(|c| c.0), Some(3_000), "a shorter file starts over");
        std::fs::remove_file(&path).unwrap();
        assert!(!log.advance());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_logs_are_read_whole_a_piece_at_a_time() {
        let dir = tmp();
        let path = dir.join("big.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let filler = format!("{{\"type\":\"user\",\"pad\":\"{}\"}}\n", "x".repeat(1000));
        writeln!(f, "{}", claude_turn(10_000, "m")).unwrap(); // long before the last piece
        for _ in 0..(CHUNK / 1000 + 100) {
            f.write_all(filler.as_bytes()).unwrap();
        }
        writeln!(f, "{}", claude_turn(42_000, "m")).unwrap();
        let mut log = Log::new(path);
        log.advance();
        assert_eq!(log.claude.as_ref().map(|c| c.0), Some(42_000), "the latest turn");
        assert_eq!(total(&log.spend).total(), 52_000, "and every turn counted");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn line(v: &Value) -> String {
        format!("{v}\n")
    }

    #[test]
    fn claude_turns_add_up_once_each() {
        let dir = tmp();
        let path = dir.join("s.jsonl");
        let turn = |id: &str, out: u64| {
            json!({"type": "assistant", "message": {"id": id, "model": "claude-opus-5-5", "usage": {
                "input_tokens": 10, "cache_creation_input_tokens": 300, "cache_read_input_tokens": 1000, "output_tokens": out,
                "cache_creation": {"ephemeral_5m_input_tokens": 100, "ephemeral_1h_input_tokens": 200}}}})
        };
        // A message logged once per content block, its output growing.
        let text = [turn("a", 5), turn("a", 7), turn("b", 1)].iter().map(line).collect::<String>();
        std::fs::write(&path, text).unwrap();
        let mut log = Log::new(path.clone());
        log.advance();
        let b = Bucket { model: "claude-opus-5-5".into(), fast: false, prompt: 0 };
        assert_eq!(log.spend.get(&b), Some(&Tokens { input: 20, cache_write_5m: 200, cache_write_1h: 400, cache_read: 2000, output: 8 }));
        // Fast mode and long prompts are priced apart; subagents count.
        let mut fast = turn("c", 1);
        fast["message"]["usage"]["speed"] = json!("fast");
        fast["isSidechain"] = json!(true);
        fast["message"]["usage"]["cache_read_input_tokens"] = json!(200_000);
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(line(&fast).as_bytes()).unwrap();
        log.advance();
        assert_eq!(log.spend.get(&Bucket { fast: true, prompt: 200, ..b.clone() }).map(|t| t.cache_read), Some(200_000));
        // No breakdown by TTL: a 5-minute write.
        let (_, _, t) = claude_spend(&json!({"type": "assistant", "message": {"model": "m", "usage": {"cache_creation_input_tokens": 9}}})).unwrap();
        assert_eq!((t.cache_write_5m, t.cache_write_1h), (9, 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn codex_totals_go_to_the_model_in_use() {
        let dir = tmp();
        let path = dir.join("r.jsonl");
        let ctx = |m: &str| json!({"type": "turn_context", "payload": {"model": m}});
        let count = |input: u64, cached: u64, output: u64| {
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {
                "input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0, "output_tokens": output, "total_tokens": input + output},
                "last_token_usage": {"total_tokens": 1}, "model_context_window": 100}}})
        };
        let text = [ctx("gpt-5-codex"), count(1000, 600, 50), count(1000, 600, 50), ctx("gpt-5.2"), count(3000, 2000, 80)].iter().map(line).collect::<String>();
        std::fs::write(&path, text).unwrap();
        let mut log = Log::new(path);
        log.advance();
        let b = |m: &str| Bucket { model: m.into(), fast: false, prompt: 0 };
        assert_eq!(log.spend.get(&b("gpt-5-codex")), Some(&Tokens { input: 400, cache_read: 600, output: 50, ..Default::default() }), "a count repeated isn't more");
        assert_eq!(log.spend.get(&b("gpt-5.2")), Some(&Tokens { input: 600, cache_read: 1400, output: 30, ..Default::default() }), "what was added since");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_clone_counts_only_what_it_spent_itself() {
        let b = |m: &str| Bucket { model: m.into(), fast: false, prompt: 0 };
        let t = |input: u64, output: u64| Tokens { input, output, ..Default::default() };
        let base: Spend = [(b("opus"), t(100, 10))].into();
        let spend: Spend = [(b("opus"), t(150, 25)), (b("haiku"), t(5, 1))].into();
        assert_eq!(less(&spend, &base), [(b("opus"), t(50, 15)), (b("haiku"), t(5, 1))].into());
        assert_eq!(less(&base, &base), Spend::new(), "nothing of its own yet");
        // The original's whole log, read once, is the base.
        let root = tmp();
        let log = root.join("s.jsonl");
        std::fs::write(&log, line(&claude_turn(1_000, "m")) + &line(&claude_turn(2_000, "m"))).unwrap();
        let both = total(&spent_in(&log)).total();
        std::fs::write(&log, line(&claude_turn(1_000, "m"))).unwrap();
        assert!(both > total(&spent_in(&log)).total() && total(&spent_in(&log)).total() > 0, "every turn in it");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn claude_resumes_from_its_folders_logs() {
        let p = claude_log_under(Path::new("/Users/me/Projects/trix/.trix/worktrees/a b"), "s1");
        assert!(p.ends_with("projects/-Users-me-Projects-trix--trix-worktrees-a-b/s1.jsonl"), "{}", p.display());
    }

    #[test]
    fn an_agent_keeps_what_it_spent_before() {
        let root = tmp();
        let first = root.join("s1.jsonl");
        std::fs::write(&first, line(&claude_turn(1_000, "m"))).unwrap();
        let subs = root.join("s1/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let mut side = claude_turn(500, "m");
        side["isSidechain"] = json!(true);
        std::fs::write(subs.join("agent-a1.jsonl"), line(&side)).unwrap();
        std::fs::write(subs.join("agent-a1.meta.json"), "{}").unwrap();
        let mut w = Watched { key: None, log: Log::new(first.clone()), subagents: BTreeMap::new(), earlier: Spend::new() };
        w.advance(Kind::Claude);
        assert_eq!(total(&w.spend()).total(), 1_500, "its subagents' too");
        assert_eq!(w.log.claude.as_ref().map(|c| c.0), Some(1_000), "but not in its context");
        // `/clear`: a new conversation, a new log.
        let second = root.join("s2.jsonl");
        std::fs::write(&second, line(&claude_turn(2_000, "m"))).unwrap();
        w.switch(Some("s2".into()), second);
        w.advance(Kind::Claude);
        assert_eq!(total(&w.spend()).total(), 3_500);
        // The same log found again isn't counted twice.
        w.switch(Some("again".into()), root.join("s2.jsonl"));
        w.advance(Kind::Claude);
        assert_eq!(total(&w.spend()).total(), 3_500);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finds_logs_by_conversation_id() {
        let root = tmp();
        let p = root.join("projects/-home-me-code-api");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("abc-123.jsonl"), "").unwrap();
        assert_eq!(find_claude(&root, "abc-123"), Some(p.join("abc-123.jsonl")));
        assert_eq!(find_claude(&root, "nope"), None);
        let d = root.join("sessions/2026/11/11");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("rollout-2026-11-11T15-19-07-01a1-thread.jsonl"), "").unwrap();
        assert_eq!(find_codex(&root, "01a1-thread"), Some(d.join("rollout-2026-11-11T15-19-07-01a1-thread.jsonl")));
        assert_eq!(find_codex(&root, "thread"), None, "the whole id, not a suffix of it");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_log_the_agent_named_is_only_looked_for_there() {
        let root = tmp();
        let log = root.join("s1.jsonl");
        // Unwritten yet, and not searched for by id meanwhile.
        let t = Target { id: 1, kind: Kind::Claude, transcript: Some(log.clone()), session: Some("s1".into()), since: 0 };
        assert_eq!(locate(&t), None);
        std::fs::write(&log, "").unwrap();
        assert_eq!(locate(&t), Some(log));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn each_turn_is_dated_for_the_month_and_counted_once() {
        let dir = tmp();
        let path = dir.join("s.jsonl");
        let ledger = dir.join("spend");
        let turn = |id: &str, out: u64, at: &str| format!("{}\n", json!({"type": "assistant", "timestamp": at, "message": {"id": id, "model": "claude-opus-5-5", "usage": {"input_tokens": 10, "output_tokens": out}}}));
        let (a, b, c) = (turn("a", 5, "2026-11-24T10:00:00Z"), turn("a", 7, "2026-11-24T10:00:01Z"), turn("c", 3, "2026-11-24T11:00:00Z"));
        // The last line is still being written: not read yet.
        std::fs::write(&path, format!("{a}{b}{}", &c[..20])).unwrap();
        let mut log = Log::new(path.clone());
        log.advance();
        let read = log.take(0);
        let at = crate::ledger::parse_time("2026-11-24T10:00:00Z").unwrap();
        let ends: Vec<(u64, u64, u64)> = read.entries.iter().map(|e| (e.end, e.at, e.tokens.total())).collect();
        // A message logged again adds only what grew: 2 more out.
        assert_eq!(ends, vec![(a.len() as u64, at, 15), ((a.len() + b.len()) as u64, at + 1, 2)]);
        assert_eq!(read.done, (a.len() + b.len()) as u64, "up to the last whole line");
        let (ym, _, _) = crate::ledger::local(at);
        let out = || crate::usage::total(&crate::ledger::load(&ledger, ym).total()).total();
        crate::ledger::record(&ledger, &[read]).unwrap();
        assert_eq!(out(), 17);
        // trix restarts and reads the log from the start: nothing more.
        let mut again = Log::new(path.clone());
        again.advance();
        crate::ledger::record(&ledger, &[again.take(0)]).unwrap();
        assert_eq!(out(), 17);
        // The new turn, whole: that and only that.
        std::fs::write(&path, format!("{a}{b}{c}")).unwrap();
        again.advance();
        crate::ledger::record(&ledger, &[again.take(0)]).unwrap();
        assert_eq!(out(), 30);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn token_counts_read_well() {
        assert_eq!(tokens(800), "800");
        assert_eq!(tokens(137_648), "138k");
        assert_eq!(tokens(690_540), "691k");
        assert_eq!(tokens(999_499), "999k");
        assert_eq!(tokens(999_500), "1M", "not \"1000k\"");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_234_000), "1.2M");
        let c = Context { used: 309_460, window: 1_000_000, exact: false };
        assert_eq!((c.used_percent(), c.left()), (31, 690_540));
    }
}
