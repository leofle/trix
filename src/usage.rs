//! Context gauge: how full each agent's context window is.
//!
//! Claude and Codex both log every turn's token usage in their conversation
//! file. A background thread follows those files, reading only what was
//! appended since the last look, and reports each agent's latest figure.
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
use std::time::Duration;

use crate::agent::Kind;
use crate::app::AppEvent;

const STANDARD: u64 = 200_000;
const LONG: u64 = 1_000_000;
/// How much of a long log to read the first time: the latest turn is at the end.
const TAIL: u64 = 4 << 20;

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
        1_000..1_000_000 => format!("{}k", (n as f64 / 1_000.0).round() as u64),
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
}

pub type Targets = Arc<Mutex<Vec<Target>>>;

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
}

impl Log {
    fn new(path: PathBuf) -> Log {
        Log { path, ..Log::default() }
    }

    /// Read what was appended since last time. False when the file is gone.
    fn advance(&mut self) -> bool {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return false };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Log::new(std::mem::take(&mut self.path)); // rewritten
        }
        let first = self.offset == 0 && self.partial.is_empty();
        if first && len > TAIL {
            self.offset = len - TAIL;
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return true;
        }
        let mut buf = Vec::new();
        if f.take(len - self.offset).read_to_end(&mut buf).is_err() {
            return true;
        }
        self.offset += buf.len() as u64;
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(&buf);
        let mut lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
        let rest = lines.pop().unwrap_or_default().to_vec();
        if first && self.offset > buf.len() as u64 && !lines.is_empty() {
            lines.remove(0); // started mid-line
        }
        for line in lines {
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            if let Some(c) = claude_line(&v) {
                self.claude = Some(c);
            } else if let Some(c) = codex_line(&v) {
                self.codex = Some(c);
            }
        }
        self.partial = rest;
        true
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

fn locate(t: &Target) -> Option<PathBuf> {
    if let Some(p) = t.transcript.as_ref().filter(|p| p.is_file()) {
        return Some(p.clone());
    }
    let s = t.session.as_deref()?;
    match t.kind {
        Kind::Claude => find_claude(&claude_dir(), s),
        Kind::Codex => find_codex(&codex_dir(), s),
        _ => None,
    }
}

/// Follow every target's log; report each agent's context when it changes.
pub fn spawn_watcher(targets: Targets, tx: Sender<AppEvent>) {
    let overridden = std::env::var("TRIX_CLAUDE_CONTEXT").ok().and_then(|s| s.trim().parse::<u64>().ok()).filter(|&n| n > 0);
    let _ = std::thread::Builder::new().name("trix-usage".into()).spawn(move || {
        let mut learned = crate::state::load_prefs().context_windows;
        let mut logs: HashMap<u32, (Option<String>, Log)> = HashMap::new();
        let mut sent: HashMap<u32, Option<Context>> = HashMap::new();
        loop {
            let list = targets.lock().unwrap().clone();
            logs.retain(|id, _| list.iter().any(|t| t.id == *id));
            sent.retain(|id, _| list.iter().any(|t| t.id == *id));
            for t in list.iter().filter(|t| matches!(t.kind, Kind::Claude | Kind::Codex)) {
                let key = t.transcript.as_ref().map(|p| p.display().to_string()).or_else(|| t.session.clone());
                let stale = logs.get(&t.id).is_none_or(|(k, log)| *k != key || !log.path.is_file());
                if stale {
                    let Some(path) = locate(t) else { continue };
                    logs.insert(t.id, (key, Log::new(path)));
                }
                let (_, log) = logs.get_mut(&t.id).unwrap();
                log.advance();
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
    fn long_logs_are_read_from_the_tail() {
        let dir = tmp();
        let path = dir.join("big.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let filler = format!("{{\"type\":\"user\",\"pad\":\"{}\"}}\n", "x".repeat(1000));
        writeln!(f, "{}", codex_turn(1, 1)).unwrap(); // only in the skipped head
        for _ in 0..(TAIL / 1000 + 100) {
            f.write_all(filler.as_bytes()).unwrap();
        }
        writeln!(f, "{}", codex_turn(42_000, 258_400)).unwrap();
        let mut log = Log::new(path);
        log.advance();
        assert_eq!(log.codex, Some((42_000, 258_400)));
        let _ = std::fs::remove_dir_all(dir);
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
    fn token_counts_read_well() {
        assert_eq!(tokens(800), "800");
        assert_eq!(tokens(137_648), "138k");
        assert_eq!(tokens(690_540), "691k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_234_000), "1.2M");
        let c = Context { used: 309_460, window: 1_000_000, exact: false };
        assert_eq!((c.used_percent(), c.left()), (31, 690_540));
    }
}
