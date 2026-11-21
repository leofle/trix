//! Runnable worktrees: what a new worktree needs besides its files.
//!
//! A repository says it in `.trix.toml`, at its root and committed:
//!
//! ```toml
//! [worktree]
//! setup = "npm ci"            # run in the new worktree, in the agent's pane
//! copy = [".env", ".env.*"]   # gitignored files to bring from the main checkout
//! ports = 10                  # how many ports each agent gets
//! ```
//!
//! That file comes with the repository, so it's someone else's commands
//! until you've read them: nothing in it is used until you trust it, in the
//! new-agent form, and any change to it asks again. Only that dialog grants
//! trust. There's no command for it, so an agent can't talk its way past it.
//!
//! Ports are trix's own: each worktree holds a block of them (`TRIX_PORT`…),
//! in a ledger every session shares, for as long as an agent is in it.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::WorktreeOpts;

/// The repository's file, in its main checkout. Not under `.trix/`, which
/// trix keeps out of git.
pub const FILE: &str = ".trix.toml";

/// How much `copy` brings over: it runs before the agent starts, on the
/// server's thread, and it's meant for `.env`s, not for build output.
const MAX_FILES: usize = 1000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PORTS: u16 = 100;

/// What an agent's start adds for its worktree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Runtime {
    pub ports: Option<Ports>,
    /// Runs once, in the pane, before the agent.
    pub setup: Option<Setup>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    pub script: String,
    /// The main checkout, as `TRIX_ROOT`.
    pub root: PathBuf,
    /// Written when the script succeeds, in the worktree's own git folder.
    /// It outlasts a server that died meanwhile and never heard.
    #[serde(default)]
    pub done: Option<PathBuf>,
}

impl Setup {
    pub fn finished(&self) -> bool {
        self.done.as_deref().is_some_and(Path::exists)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ports {
    pub base: u16,
    pub count: u16,
}

impl Ports {
    pub fn last(self) -> u16 {
        self.base + (self.count - 1)
    }

    /// `TRIX_PORT` is the first, for the one server most projects have.
    pub fn env(self) -> [(&'static str, String); 3] {
        [("TRIX_PORT", self.base.to_string()), ("TRIX_PORT_END", self.last().to_string()), ("TRIX_PORT_COUNT", self.count.to_string())]
    }

    fn overlaps(self, other: Ports) -> bool {
        self.base <= other.last() && other.base <= self.last()
    }
}

// ───────────────────────────── the repo's file ─────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoFile {
    #[serde(default)]
    worktree: RepoSection,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RepoSection {
    setup: Option<String>,
    #[serde(default)]
    copy: Vec<String>,
    ports: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RepoConfig {
    pub root: PathBuf,
    /// The whole file, as trusted: any change to it asks again.
    pub text: String,
    pub setup: Option<String>,
    pub copy: Vec<String>,
    pub ports: Option<u16>,
}

impl RepoConfig {
    /// The file of the repository `dir` is in, from its main checkout, so
    /// what an agent edits in its own worktree doesn't count. `None`: not a
    /// repository, no file, or a file that asks for nothing.
    pub fn load(dir: &Path) -> Result<Option<RepoConfig>, String> {
        let Some(root) = crate::app::main_checkout(dir) else { return Ok(None) };
        let path = root.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{FILE}: {e}")),
        };
        Self::parse(root, text)
    }

    fn parse(root: PathBuf, text: String) -> Result<Option<RepoConfig>, String> {
        let f: RepoFile = toml::from_str(&text).map_err(|e| format!("{FILE}: {}", e.message()))?;
        let w = f.worktree;
        for p in &w.copy {
            check_pattern(p)?;
        }
        let ports = match w.ports {
            None => None,
            Some(n) if (1..=MAX_PORTS as i64).contains(&n) => Some(n as u16),
            Some(n) => return Err(format!("{FILE}: worktree.ports: {n} is not from 1 to {MAX_PORTS}")),
        };
        let setup = w.setup.filter(|s| !s.trim().is_empty());
        if setup.is_none() && w.copy.is_empty() && ports.is_none() {
            return Ok(None);
        }
        Ok(Some(RepoConfig { root, text, setup, copy: w.copy, ports }))
    }

    pub fn trusted(&self) -> bool {
        is_trusted_in(&trust_file(), &self.root, &self.text)
    }

    pub fn trust(&self) -> std::io::Result<()> {
        grant_in(&trust_file(), &self.root, &self.text)
    }

    /// The trust dialog's text. Exactly what would run: nothing hidden,
    /// nothing a terminal would act on.
    pub fn describe(&self) -> Vec<String> {
        let mut out = vec![format!("{} in {} asks for this in each new worktree:", FILE, crate::app::tilde(&self.root))];
        if let Some(s) = &self.setup {
            out.push(String::new());
            out.push("run, as you:".into());
            out.extend(s.trim_end().lines().map(|l| format!("  {}", visible(l))));
        }
        if !self.copy.is_empty() {
            out.push(String::new());
            out.push("copy from the main checkout, if git ignores them:".into());
            out.extend(self.copy.iter().map(|p| format!("  {}", visible(p))));
        }
        if let Some(n) = self.ports {
            out.push(String::new());
            out.push(format!("reserve {n} port{}", if n == 1 { "" } else { "s" }));
        }
        out
    }
}

/// `s`, with anything that could move the cursor, recolor the text or
/// reorder it shown as a replacement character.
fn visible(s: &str) -> String {
    let hidden = |c: char| c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
    s.chars().map(|c| if c == '\t' { ' ' } else if hidden(c) { '\u{fffd}' } else { c }).collect()
}

/// A path inside the repository, with `*` and `?` in its parts.
fn check_pattern(p: &str) -> Result<(), String> {
    let bad = p.is_empty() || p.starts_with('/') || p.starts_with('~') || p.split('/').any(|c| c.is_empty() || c == "." || c == "..");
    if bad {
        return Err(format!("{FILE}: worktree.copy: '{p}' must be a path inside the repository"));
    }
    Ok(())
}

// ───────────────────────────── trust ─────────────────────────────

fn state_dir() -> PathBuf {
    // Tests never read or grant the developer's own trust, or take their ports.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("trix-test-state-{}", std::process::id()));
    }
    crate::state::xdg_dir("XDG_STATE_HOME", ".local/state").join("trix")
}

fn trust_file() -> PathBuf {
    state_dir().join("trusted-repos.json")
}

fn read_trust(file: &Path) -> BTreeMap<String, String> {
    std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The text itself is kept, not a hash of it: the comparison is exact.
fn is_trusted_in(file: &Path, root: &Path, text: &str) -> bool {
    read_trust(file).get(&root.display().to_string()).is_some_and(|t| t == text)
}

fn grant_in(file: &Path, root: &Path, text: &str) -> std::io::Result<()> {
    // Sessions are separate servers: one's grant mustn't undo another's.
    let _lock = LedgerLock::take(file.parent().unwrap_or(Path::new(".")))?;
    let mut all = read_trust(file);
    all.insert(root.display().to_string(), text.to_string());
    crate::state::write_atomic(file, &serde_json::to_vec_pretty(&all).unwrap_or_default())
}

// ───────────────────────────── copying ─────────────────────────────

#[derive(Debug, Default, PartialEq)]
pub struct Copied {
    pub files: usize,
    /// Each file left behind, and why.
    pub skipped: Vec<String>,
}

fn glob(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|i| glob(rest, &name[i..])),
        Some(('?', rest)) => !name.is_empty() && glob(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

/// The regular files a pattern names. Symlinks are never followed, so a
/// pattern can't reach outside the checkout, and git's and trix's own
/// folders aren't looked in.
fn expand(root: &Path, rel: PathBuf, parts: &[&str], out: &mut Vec<PathBuf>) {
    let Some((head, rest)) = parts.split_first() else { return };
    // Names needn't be UTF-8: a wildcard matches them as they'd be shown.
    let names: Vec<OsString> = if head.contains(['*', '?']) {
        let want: Vec<char> = head.chars().collect();
        let mut found: Vec<OsString> = std::fs::read_dir(root.join(&rel))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| glob(&want, &n.to_string_lossy().chars().collect::<Vec<_>>()))
            .collect();
        found.sort();
        found
    } else {
        vec![OsString::from(head)]
    };
    for name in names {
        if name == ".git" || name == ".trix" {
            continue;
        }
        let next = rel.join(&name);
        let Ok(meta) = std::fs::symlink_metadata(root.join(&next)) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if rest.is_empty() {
            if meta.is_file() {
                out.push(next);
            }
        } else if meta.is_dir() {
            expand(root, next, rest, out);
        }
    }
}

fn ignored(root: &Path, rel: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--"])
        .arg(rel)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Which of `paths` git ignores in `dir`, in one git process: this runs on
/// the server's thread. A path git refuses outright (inside a submodule, say)
/// fails the whole batch, so then each is asked about alone.
fn ignored_among(dir: &Path, paths: &[PathBuf]) -> HashSet<PathBuf> {
    if paths.is_empty() {
        return HashSet::new();
    }
    let batch = || {
        let mut child = Command::new("git").arg("-C").arg(dir).args(["check-ignore", "-z", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let input: Vec<u8> = paths.iter().flat_map(|p| p.as_os_str().as_bytes().iter().copied().chain([0])).collect();
        // Written beside the read: git answers as it goes, and a full pipe
        // each way would leave both waiting.
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let out = child.wait_with_output().ok()?;
        let _ = writer.join();
        // 0: some are ignored. 1: none are. Anything else is a failure.
        matches!(out.status.code(), Some(0 | 1)).then(|| out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p))).collect::<HashSet<_>>())
    };
    batch().unwrap_or_else(|| paths.iter().filter(|p| ignored(dir, p)).cloned().collect())
}

/// Something other than a folder is on the way to `rel` under `dest`.
fn parents_blocked(dest: &Path, rel: &Path) -> bool {
    let mut at = dest.to_path_buf();
    rel.parent().into_iter().flat_map(Path::components).any(|part| {
        at.push(part);
        std::fs::symlink_metadata(&at).is_ok_and(|m| !m.is_dir())
    })
}

/// Make `rel`'s folders under `dest`, refusing to go through a symlink.
fn make_parents(dest: &Path, rel: &Path) -> std::io::Result<()> {
    let mut at = dest.to_path_buf();
    for part in rel.parent().into_iter().flat_map(Path::components) {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(std::io::Error::other("a file or symlink is in the way")),
            Err(_) => std::fs::create_dir(&at)?,
        }
    }
    Ok(())
}

/// Copy what the patterns name from `root` to `dest`. Only files git ignores
/// on both sides: anything else is either in the worktree already or would
/// end up in the agent's commit, as one ignored only by an uncommitted
/// `.gitignore` in the main checkout would. A file already there is kept. A
/// tracked file a wide pattern also names is no news, and counts for nothing.
pub fn copy_files(root: &Path, dest: &Path, patterns: &[String]) -> Copied {
    let mut found = Vec::new();
    for p in patterns {
        let parts: Vec<&str> = p.split('/').collect();
        expand(root, PathBuf::new(), &parts, &mut found);
    }
    found.sort();
    found.dedup();
    let mut done = Copied::default();
    let there = |rel: &Path| std::fs::symlink_metadata(dest.join(rel)).is_ok();
    let in_root = ignored_among(root, &found);
    let mut wanted = Vec::new();
    for rel in found {
        if in_root.contains(&rel) {
            if parents_blocked(dest, &rel) {
                done.skipped.push(format!("{}: a file or symlink is in the way", rel.display()));
            } else {
                wanted.push(rel);
            }
        } else if !there(&rel) {
            done.skipped.push(format!("{}: git doesn't ignore it", rel.display()));
        }
    }
    let in_dest = ignored_among(dest, &wanted);
    let mut bytes = 0u64;
    for rel in &wanted {
        let shown = rel.display();
        if !in_dest.contains(rel) {
            if !there(rel) {
                done.skipped.push(format!("{shown}: git doesn't ignore it in the new worktree, so it could be committed"));
            }
            continue;
        }
        if there(rel) {
            done.skipped.push(format!("{shown}: already there"));
            continue;
        }
        if done.files >= MAX_FILES {
            done.skipped.push(format!("{shown}: over {MAX_FILES} files"));
            continue;
        }
        let size = std::fs::metadata(root.join(rel)).map_or(0, |m| m.len());
        if bytes + size > MAX_BYTES {
            done.skipped.push(format!("{shown}: over {} MB in all", MAX_BYTES / (1024 * 1024)));
            continue;
        }
        match make_parents(dest, rel).and_then(|()| std::fs::copy(root.join(rel), dest.join(rel))) {
            Ok(n) => {
                bytes += n;
                done.files += 1;
            }
            Err(e) => done.skipped.push(format!("{shown}: {e}")),
        }
    }
    done
}

/// What was left behind, short enough for a toast.
fn skipped_note(skipped: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut note = format!("not copied: {}", skipped[..skipped.len().min(SHOWN)].join("; "));
    if skipped.len() > SHOWN {
        note.push_str(&format!("; and {} more", skipped.len() - SHOWN));
    }
    note
}

// ───────────────────────────── ports ─────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Lease {
    #[serde(flatten)]
    ports: Ports,
    /// The sessions with an agent in this worktree.
    sessions: BTreeSet<String>,
}

type Ledger = BTreeMap<String, Lease>;

/// The worktree trix made that `cwd` is in. Only those get ports.
pub fn managed_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|p| p.parent().is_some_and(|d| d.ends_with(".trix/worktrees"))).map(Path::to_path_buf)
}

/// Held while the ledger is read and written: sessions are separate servers.
struct LedgerLock(std::fs::File);

impl LedgerLock {
    fn take(dir: &Path) -> std::io::Result<LedgerLock> {
        std::fs::create_dir_all(dir)?;
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("ports.lock"))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(LedgerLock(f))
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn read_ledger(dir: &Path) -> Ledger {
    std::fs::read(dir.join("ports.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_ledger(dir: &Path, ledger: &Ledger) -> std::io::Result<()> {
    crate::state::write_atomic(&dir.join("ports.json"), &serde_json::to_vec_pretty(ledger).unwrap_or_default())
}

/// Nothing is listening on it, on either loopback.
fn port_free(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};
    [IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)]
        .into_iter()
        .all(|ip| !matches!(TcpListener::bind((ip, port)), Err(e) if e.kind() == std::io::ErrorKind::AddrInUse))
}

fn pick_block(ledger: &Ledger, base: u16, count: u16, free: &dyn Fn(u16) -> bool) -> Option<Ports> {
    let mut at = base as u32;
    while at + count as u32 - 1 <= u16::MAX as u32 {
        let block = Ports { base: at as u16, count };
        if !ledger.values().any(|l| l.ports.overlaps(block)) && (block.base..=block.last()).all(free) {
            return Some(block);
        }
        at += count as u32;
    }
    None
}

fn lease_in(dir: &Path, worktree: &Path, session: &str, base: u16, count: u16, free: &dyn Fn(u16) -> bool) -> std::io::Result<Option<Ports>> {
    let _lock = LedgerLock::take(dir)?;
    let mut ledger = read_ledger(dir);
    // A worktree that's gone doesn't need its ports (removed by hand).
    ledger.retain(|path, _| Path::new(path).exists());
    let key = worktree.display().to_string();
    let ports = match ledger.get_mut(&key) {
        // The same worktree keeps its ports: a restart finds its server's.
        Some(l) => {
            l.sessions.insert(session.to_string());
            l.ports
        }
        None => {
            let Some(ports) = pick_block(&ledger, base, count, free) else { return Ok(None) };
            ledger.insert(key, Lease { ports, sessions: BTreeSet::from([session.to_string()]) });
            ports
        }
    };
    write_ledger(dir, &ledger)?;
    Ok(Some(ports))
}

/// This session has no agent left in the worktree.
fn release_in(dir: &Path, worktree: &Path, session: &str) -> std::io::Result<()> {
    if !dir.join("ports.json").exists() {
        return Ok(());
    }
    let _lock = LedgerLock::take(dir)?;
    let mut ledger = read_ledger(dir);
    let key = worktree.display().to_string();
    let Some(l) = ledger.get_mut(&key) else { return Ok(()) };
    l.sessions.remove(session);
    if l.sessions.is_empty() {
        ledger.remove(&key);
    }
    write_ledger(dir, &ledger)
}

/// This session now holds only the worktrees in `keep`: what it had in the
/// rest went with a workspace that won't come back.
fn keep_only_in(dir: &Path, session: &str, keep: &BTreeSet<PathBuf>) -> std::io::Result<()> {
    if !dir.join("ports.json").exists() {
        return Ok(());
    }
    let _lock = LedgerLock::take(dir)?;
    let mut ledger = read_ledger(dir);
    let before = ledger.clone();
    ledger.retain(|path, l| {
        if !keep.contains(Path::new(path)) {
            l.sessions.remove(session);
        }
        !l.sessions.is_empty()
    });
    if ledger == before {
        return Ok(());
    }
    write_ledger(dir, &ledger)
}

/// The worktree is gone, whoever was in it.
fn forget_in(dir: &Path, worktree: &Path) -> std::io::Result<()> {
    if !dir.join("ports.json").exists() {
        return Ok(());
    }
    let _lock = LedgerLock::take(dir)?;
    let mut ledger = read_ledger(dir);
    if ledger.remove(&worktree.display().to_string()).is_none() {
        return Ok(());
    }
    write_ledger(dir, &ledger)
}

/// Who a lease is held for: this session. Tests share one ledger and run at
/// once, so each thread counts as a session, or one settling its restore
/// would give back the ports of all the others.
fn lease_holder() -> String {
    if cfg!(test) {
        return format!("test-{:?}", std::thread::current().id());
    }
    crate::session::name().to_string()
}

/// `cwd`'s worktree's ports, reserved now if it has none. `None`: it isn't a
/// worktree of trix's, ports are off, or there's no block left.
pub fn lease(cwd: &Path, opts: &WorktreeOpts, count: Option<u16>) -> Option<Ports> {
    if !opts.ports {
        return None;
    }
    let worktree = managed_root(cwd)?;
    // One coming back to a worktree gets the size its repository asks for.
    let count = count.or_else(|| if read_ledger(&state_dir()).contains_key(&worktree.display().to_string()) { None } else { trusted_ports(&worktree) }).unwrap_or(opts.port_count);
    match lease_in(&state_dir(), &worktree, &lease_holder(), opts.port_base, count, &port_free) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("trix: couldn't reserve ports for {}: {e}", worktree.display());
            None
        }
    }
}

fn trusted_ports(worktree: &Path) -> Option<u16> {
    RepoConfig::load(worktree).ok().flatten().filter(RepoConfig::trusted).and_then(|r| r.ports)
}

/// Said when a worktree that should have ports got none.
pub fn no_ports(cwd: &Path, opts: &WorktreeOpts, ports: Option<Ports>) -> Option<String> {
    (ports.is_none() && opts.ports && managed_root(cwd).is_some()).then(|| format!("no free block of {} ports from {}: started without TRIX_PORT", opts.port_count, opts.port_base))
}

/// Give back this session's blocks, but for the worktrees in `keep`.
pub fn keep_only(keep: impl IntoIterator<Item = PathBuf>) {
    let keep: BTreeSet<PathBuf> = keep.into_iter().filter_map(|p| managed_root(&p)).collect();
    if let Err(e) = keep_only_in(&state_dir(), &lease_holder(), &keep) {
        eprintln!("trix: couldn't release ports: {e}");
    }
}

/// The agents in `json` (a `trix ls --json` reply) inside `worktree`.
fn agents_in(reply: &str, worktree: &Path) -> Option<Vec<String>> {
    let reply: crate::control::Reply = serde_json::from_str(reply).ok()?;
    let list: serde_json::Value = serde_json::from_str(&reply.out).ok()?;
    let agents = list.get("agents")?.as_array()?;
    Some(agents.iter().filter(|a| a.get("cwd").and_then(|c| c.as_str()).is_some_and(|c| Path::new(c).starts_with(worktree))).filter_map(|a| a.get("name")?.as_str().map(String::from)).collect())
}

/// Why another session keeps this worktree from being removed, if it does.
/// A session that can't say counts as using it: removing a folder from under
/// an agent is worse than asking you to look.
pub fn used_elsewhere(worktree: &Path) -> Option<String> {
    use crate::session::ClientMsg;
    if cfg!(test) {
        return None;
    }
    let me = crate::session::name();
    let held: BTreeSet<String> = read_ledger(&state_dir()).get(&worktree.display().to_string()).map(|l| l.sessions.clone()).unwrap_or_default();
    let mut sessions: BTreeSet<String> = crate::session::candidates().into_iter().collect();
    sessions.extend(held.iter().cloned());
    for s in sessions.iter().filter(|s| *s != me) {
        let ask = ClientMsg::Control { protocol: crate::control::CONTROL, from: None, req: crate::control::Request::List { json: true } };
        match crate::session::ask(s, &ask) {
            Ok(Some(text)) => match agents_in(&text, worktree) {
                Some(names) if names.is_empty() => {}
                Some(names) => return Some(format!("{} in session {s} still use this worktree: close them first", names.join(", "))),
                None => return Some(format!("session {s} didn't say whether it uses this worktree: try again")),
            },
            Err(()) => return Some(format!("session {s} didn't say whether it uses this worktree: try again")),
            // No server, but agents its crash left running may be in here.
            Ok(None) if held.contains(s) && !crate::holder::discover(s).is_empty() => {
                return Some(format!("session {s} has agents left running: open it, or `trix kill -s {s}`, first"));
            }
            Ok(None) => {}
        }
    }
    None
}

pub fn release(cwd: &Path) {
    if let Some(worktree) = managed_root(cwd)
        && let Err(e) = release_in(&state_dir(), &worktree, &lease_holder())
    {
        eprintln!("trix: couldn't release the ports of {}: {e}", worktree.display());
    }
}

/// The block the ledger holds for this worktree.
#[cfg(test)]
pub fn leased(worktree: &Path) -> Option<Ports> {
    read_ledger(&state_dir()).get(&worktree.display().to_string()).map(|l| l.ports)
}

pub fn forget(worktree: &Path) {
    if let Err(e) = forget_in(&state_dir(), worktree) {
        eprintln!("trix: couldn't release the ports of {}: {e}", worktree.display());
    }
}

// ───────────────────────────── a new worktree ─────────────────────────────

/// What getting a worktree ready came to.
#[derive(Debug, Default)]
pub struct Prepared {
    pub runtime: Runtime,
    /// For the agent's card: "copied 2 files · ports 20000–20009".
    pub note: Option<String>,
    /// Something you'd want to know was left undone.
    pub warning: Option<String>,
}

/// Ready a worktree: its files and its setup if this is a new one and the
/// repository's file is trusted, and its ports either way. `skip`: you said
/// not to use the file this time.
pub fn prepare(path: &Path, repo: Option<&RepoConfig>, reused: bool, skip: bool, opts: &WorktreeOpts) -> Prepared {
    let trusted = repo.filter(|r| !skip && r.trusted());
    let mut out = Prepared { runtime: Runtime { ports: lease(path, opts, trusted.and_then(|r| r.ports)), setup: None }, ..Prepared::default() };
    let mut notes = Vec::new();
    if !reused {
        if let Some(r) = trusted {
            let copied = copy_files(&r.root, path, &r.copy);
            if copied.files > 0 {
                notes.push(format!("copied {} file{}", copied.files, if copied.files == 1 { "" } else { "s" }));
            }
            if !copied.skipped.is_empty() {
                out.warning = Some(skipped_note(&copied.skipped));
            }
            out.runtime.setup = r.setup.clone().map(|script| Setup { script, root: r.root.clone(), done: done_marker(path) });
        } else if repo.is_some() && !skip {
            out.warning = Some(format!("{FILE} isn't trusted yet, so its setup and copies didn't run: start an agent with Worktree from the new-agent form to read and trust it"));
        }
    }
    if let Some(p) = out.runtime.ports {
        notes.push(format!("ports {}–{}", p.base, p.last()));
    }
    if let Some(w) = no_ports(path, opts, out.runtime.ports) {
        out.warning = Some(out.warning.take().map_or(w.clone(), |had| format!("{had} · {w}")));
    }
    out.note = (!notes.is_empty()).then(|| notes.join(" · "));
    out
}

/// Where a worktree's setup says it finished: its private git folder, so it
/// can't be committed and goes when the worktree does.
fn done_marker(worktree: &Path) -> Option<PathBuf> {
    let out = Command::new("git").arg("-C").arg(worktree).args(["rev-parse", "--absolute-git-dir"]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| PathBuf::from(std::ffi::OsStr::from_bytes(out.stdout.trim_ascii_end())).join("trix-setup-done"))
}

// ───────────────────────────── the setup, in the pane ─────────────────────────────

/// The pane's command that runs the setup and then becomes `program`.
/// `installs`: what follows is `trix install`, which says when it's done.
pub fn wrap(exe: &Path, setup: &Setup, installs: bool, program: String, args: Vec<String>) -> (String, Vec<String>) {
    let marker = setup.done.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
    let mut all = vec!["worktree-setup".to_string(), if installs { "quiet" } else { "tell" }.to_string(), marker, setup.script.clone(), "--".to_string(), program];
    all.extend(args);
    (exe.display().to_string(), all)
}

/// `trix worktree-setup`. Returns the exit code; on success it never
/// returns: it becomes the agent. A setup that fails leaves the pane exited,
/// so nothing starts on a broken worktree, and Enter tries it again.
pub fn run_setup(args: &[String]) -> i32 {
    let [mode, marker, script, dashes, program, agent_args @ ..] = args else {
        eprintln!("usage: trix worktree-setup <tell|quiet> <done-file> <script> -- <command> [args…]   (internal: trix runs it in a pane)");
        return 2;
    };
    if dashes != "--" {
        eprintln!("trix worktree-setup: expected -- before the command");
        return 2;
    }
    let marker = (!marker.is_empty()).then(|| Path::new(marker));
    if let Err(code) = run_script(script, marker) {
        return code;
    }
    if mode == "tell" {
        crate::hooks::tell("trix", "Installed");
    }
    let err = Command::new(program).args(agent_args).exec();
    eprintln!("trix: couldn't start {program}: {err}");
    126
}

/// The script itself: `Err` is the exit code to leave the pane with.
fn run_script(script: &str, marker: Option<&Path>) -> Result<(), i32> {
    if let Some(m) = marker {
        let _ = std::fs::remove_file(m);
    }
    println!("\x1b[1mtrix:\x1b[0m setting up this worktree ({FILE}):\n");
    // `-e`: a script's first failure is the setup's. No stdin: a question
    // here would wait unseen behind a pane that reads as starting.
    match Command::new("sh").arg("-ec").arg(script).stdin(Stdio::null()).status() {
        Ok(s) if s.success() => {
            // Before the agent, and whether or not a server is there to hear.
            if let Some(m) = marker {
                let _ = std::fs::write(m, "");
            }
            println!("\n\x1b[1mtrix:\x1b[0m setup finished. Starting the agent…\n");
            Ok(())
        }
        Ok(s) => {
            eprintln!("\ntrix: the worktree's setup failed ({s}). Fix it and press Enter to run it again. To go on without it, close this pane and start an agent with the same name and Worktree: an existing worktree isn't set up again.");
            Err(s.code().unwrap_or(1))
        }
        Err(e) => {
            eprintln!("\ntrix: couldn't run the worktree's setup: {e}");
            Err(127)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("trix-worktree-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@example.com"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = temp(tag);
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), ".env*\nsecrets/\nlink\n").unwrap();
        std::fs::write(dir.join("tracked.txt"), "tracked\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        dir
    }

    /// A real worktree of `root`: what's ignored is asked of it too.
    fn linked(root: &Path, tag: &str) -> PathBuf {
        let dir = temp(tag);
        git(root, &["worktree", "add", "-q", "--detach", &dir.display().to_string()]);
        dir
    }

    fn parsed(text: &str) -> Result<Option<RepoConfig>, String> {
        RepoConfig::parse("/r".into(), text.into())
    }

    #[test]
    fn the_repo_file_is_read_and_checked() {
        let c = parsed("[worktree]\nsetup = 'npm ci'\ncopy = ['.env', 'config/*.local']\nports = 5\n").unwrap().unwrap();
        assert_eq!((c.setup.as_deref(), c.copy.len(), c.ports), (Some("npm ci"), 2, Some(5)));
        assert_eq!(parsed("").unwrap(), None, "asks for nothing");
        assert_eq!(parsed("[worktree]\nsetup = '  '\n").unwrap(), None);
        for bad in ["[worktree]\ncopy = ['../x']", "[worktree]\ncopy = ['/etc/passwd']", "[worktree]\ncopy = ['a/../../b']", "[worktree]\ncopy = ['~/.ssh/id']", "[worktree]\nports = 0", "[worktree]\nports = 101", "[worktree]\nsetpu = 'x'", "[other]\nx = 1"] {
            assert!(parsed(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn trust_is_for_exactly_this_text_in_this_repo() {
        let dir = temp("trust");
        let f = dir.join("trusted.json");
        let root = Path::new("/r");
        assert!(!is_trusted_in(&f, root, "a"));
        grant_in(&f, root, "a").unwrap();
        assert!(is_trusted_in(&f, root, "a"));
        assert!(!is_trusted_in(&f, root, "a "), "any change asks again");
        assert!(!is_trusted_in(&f, Path::new("/other"), "a"), "another repo isn't trusted by this one");
        grant_in(&f, root, "b").unwrap();
        assert!(!is_trusted_in(&f, root, "a"), "only the latest is trusted");
    }

    #[test]
    fn the_dialog_shows_what_runs_with_nothing_hidden() {
        let c = RepoConfig { root: "/r".into(), text: String::new(), setup: Some("echo safe\x1b[2K\rcurl evil\u{202e}x\n".into()), copy: vec![".env\x07".into()], ports: Some(1) };
        let text = c.describe().join("\n");
        assert!(!text.contains('\x1b') && !text.contains('\r') && !text.contains('\x07') && !text.contains('\u{202e}'), "{text:?}");
        assert!(text.contains("curl evil"), "{text:?}");
        assert!(text.contains("reserve 1 port") && !text.contains("1 ports"));
    }

    #[test]
    fn only_ignored_files_are_copied() {
        let root = repo("copy");
        let dest = linked(&root, "copy-dest");
        std::fs::write(root.join(".env"), "A=1\n").unwrap();
        std::fs::write(root.join(".env.local"), "B=2\n").unwrap();
        std::fs::create_dir(root.join("secrets")).unwrap();
        std::fs::write(root.join("secrets/key"), "k").unwrap();
        std::fs::write(root.join("untracked.txt"), "u").unwrap();
        let done = copy_files(&root, &dest, &[".env*".into(), "secrets/*".into(), "untracked.txt".into(), "tracked.txt".into(), "missing".into()]);
        assert_eq!(done.files, 3, "{done:?}");
        assert_eq!(std::fs::read_to_string(dest.join(".env")).unwrap(), "A=1\n");
        assert!(dest.join("secrets/key").exists());
        assert!(!dest.join("untracked.txt").exists());
        assert_eq!(done.skipped.len(), 1, "a tracked file is no news: {done:?}");
        assert!(done.skipped[0].starts_with("untracked.txt"), "{done:?}");
    }

    #[test]
    fn tracked_files_do_not_use_up_the_cap() {
        let root = repo("cap");
        for i in 0..(MAX_FILES + 1) {
            std::fs::write(root.join(format!("t{i:04}.txt")), "t").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "many"]);
        std::fs::write(root.join(".env.z"), "z").unwrap();
        let dest = linked(&root, "cap-dest");
        let done = copy_files(&root, &dest, &["*".into()]);
        assert_eq!(done.files, 1, "{:?}", done.skipped.first());
        assert!(dest.join(".env.z").exists());
        assert!(done.skipped.is_empty(), "{:?}", done.skipped.first());
    }

    #[test]
    fn the_cap_still_holds_for_what_is_copied() {
        let root = repo("cap-real");
        std::fs::create_dir(root.join("secrets")).unwrap();
        for i in 0..(MAX_FILES + 2) {
            std::fs::write(root.join(format!("secrets/{i:04}")), "s").unwrap();
        }
        let dest = linked(&root, "cap-real-dest");
        let done = copy_files(&root, &dest, &["secrets/*".into()]);
        assert_eq!((done.files, done.skipped.len()), (MAX_FILES, 2));
        assert!(skipped_note(&done.skipped).contains("over 1000 files"));
    }

    #[test]
    fn a_long_list_of_what_stayed_behind_is_shortened() {
        let all: Vec<String> = (0..5).map(|i| format!("f{i}: why")).collect();
        let note = skipped_note(&all);
        assert!(note.contains("f2") && !note.contains("f3") && note.ends_with("and 2 more"), "{note}");
    }

    #[test]
    fn ignored_files_are_asked_about_in_one_batch_and_a_refused_path_falls_back() {
        let root = repo("batch");
        let names: Vec<PathBuf> = [".env", "tracked.txt", "odd name\nwith newline.env"].iter().map(PathBuf::from).collect();
        std::fs::write(root.join(".env"), "x").unwrap();
        assert_eq!(ignored_among(&root, &names), HashSet::from([PathBuf::from(".env")]));
        // git refuses the whole batch for a path through a symlink.
        std::os::unix::fs::symlink("/tmp", root.join("through")).unwrap();
        let mixed = [PathBuf::from(".env"), PathBuf::from("through/x")];
        assert_eq!(ignored_among(&root, &mixed), HashSet::from([PathBuf::from(".env")]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_copied() {
        use std::os::unix::ffi::OsStringExt;
        let root = repo("bytes");
        let name = OsString::from_vec(b".env-\xff".to_vec());
        std::fs::write(root.join(&name), "b").unwrap();
        let dest = linked(&root, "bytes-dest");
        let done = copy_files(&root, &dest, &[".env*".into()]);
        assert_eq!(done.files, 1, "{done:?}");
        assert_eq!(std::fs::read(dest.join(&name)).unwrap(), b"b");
    }

    #[test]
    fn a_tracked_example_file_raises_no_warning() {
        let root = repo("example");
        std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
        std::fs::write(root.join(".env.example"), "A=\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "example"]);
        std::fs::write(root.join(".env"), "A=1\n").unwrap();
        let dest = linked(&root, "example-dest");
        let done = copy_files(&root, &dest, &[".env".into(), ".env.*".into()]);
        assert_eq!((done.files, done.skipped.len()), (1, 0), "{done:?}");
    }

    #[test]
    fn a_file_the_new_worktree_would_not_ignore_stays_behind() {
        let root = repo("uncommitted");
        let dest = linked(&root, "uncommitted-dest");
        // Ignored only by an edit the new worktree's branch doesn't have.
        std::fs::write(root.join(".gitignore"), ".env*\nsecrets/\nlink\nlocal.key\n").unwrap();
        std::fs::write(root.join("local.key"), "k").unwrap();
        let done = copy_files(&root, &dest, &["local.key".into()]);
        assert_eq!(done.files, 0, "{done:?}");
        assert!(!dest.join("local.key").exists());
        assert!(done.skipped[0].contains("could be committed"), "{done:?}");
    }

    #[test]
    fn copying_keeps_what_is_there_and_a_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = repo("keep");
        let dest = linked(&root, "keep-dest");
        std::fs::write(root.join(".env"), "new").unwrap();
        std::fs::write(root.join(".env.x"), "x").unwrap();
        std::fs::set_permissions(root.join(".env.x"), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(dest.join(".env"), "mine").unwrap();
        let done = copy_files(&root, &dest, &[".env*".into()]);
        assert_eq!(done.files, 1);
        assert_eq!(std::fs::read_to_string(dest.join(".env")).unwrap(), "mine");
        assert_eq!(std::fs::metadata(dest.join(".env.x")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn copying_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;
        let root = repo("links");
        let outside = temp("links-outside");
        std::fs::write(outside.join("secret"), "s").unwrap();
        symlink(outside.join("secret"), root.join(".env")).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let dest = linked(&root, "links-dest");
        assert_eq!(copy_files(&root, &dest, &[".env".into(), "link/secret".into(), "link/*".into()]).files, 0);

        // Nor out of the worktree, through a link already in it.
        std::fs::create_dir(root.join("secrets")).unwrap();
        std::fs::write(root.join("secrets/key"), "k").unwrap();
        let elsewhere = temp("links-elsewhere");
        symlink(&elsewhere, dest.join("secrets")).unwrap();
        assert_eq!(copy_files(&root, &dest, &["secrets/key".into()]).files, 0);
        assert!(!elsewhere.join("key").exists());
    }

    #[test]
    fn git_and_trix_folders_are_never_copied() {
        let root = repo("own");
        std::fs::create_dir_all(root.join(".trix/worktrees")).unwrap();
        std::fs::write(root.join(".trix/x"), "x").unwrap();
        let dest = linked(&root, "own-dest");
        assert_eq!(copy_files(&root, &dest, &["*/*".into(), ".git/config".into(), ".trix/x".into()]).files, 0);
    }

    #[test]
    fn only_trix_worktrees_are_managed() {
        assert_eq!(managed_root(Path::new("/r/.trix/worktrees/api")), Some("/r/.trix/worktrees/api".into()));
        assert_eq!(managed_root(Path::new("/r/.trix/worktrees/api/src/x")), Some("/r/.trix/worktrees/api".into()));
        assert_eq!(managed_root(Path::new("/r/.trix/worktrees")), None);
        assert_eq!(managed_root(Path::new("/r/src")), None);
    }

    #[test]
    fn each_worktree_gets_its_own_block_and_keeps_it() {
        let dir = temp("ports");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let free = |_: u16| true;
        let pa = lease_in(&dir, &a, "s", 20000, 10, &free).unwrap().unwrap();
        let pb = lease_in(&dir, &b, "s", 20000, 10, &free).unwrap().unwrap();
        assert_eq!((pa.base, pa.last(), pb.base), (20000, 20009, 20010));
        assert_eq!(lease_in(&dir, &a, "s", 20000, 10, &free).unwrap(), Some(pa), "a restart keeps them");
        // Freed, the block is offered again.
        release_in(&dir, &a, "s").unwrap();
        let c = dir.join("c");
        std::fs::create_dir_all(&c).unwrap();
        assert_eq!(lease_in(&dir, &c, "s", 20000, 10, &free).unwrap(), Some(pa));
    }

    #[test]
    fn a_block_in_use_is_passed_over() {
        let dir = temp("busy");
        let a = dir.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let p = lease_in(&dir, &a, "s", 20000, 10, &|port| port != 20003).unwrap().unwrap();
        assert_eq!(p.base, 20010);
    }

    #[test]
    fn blocks_of_different_sizes_do_not_overlap() {
        let dir = temp("sizes");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let free = |_: u16| true;
        let big = lease_in(&dir, &a, "s", 20000, 25, &free).unwrap().unwrap();
        let small = lease_in(&dir, &b, "s", 20000, 10, &free).unwrap().unwrap();
        assert!(!big.overlaps(small), "{big:?} {small:?}");
    }

    #[test]
    fn the_range_running_out_is_no_ports_not_a_wrap() {
        let dir = temp("end");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let free = |_: u16| true;
        assert_eq!(lease_in(&dir, &a, "s", 65530, 6, &free).unwrap(), Some(Ports { base: 65530, count: 6 }));
        assert_eq!(lease_in(&dir, &b, "s", 65530, 6, &free).unwrap(), None);
    }

    #[test]
    fn a_lease_lasts_until_the_last_session_leaves() {
        let dir = temp("sessions");
        let a = dir.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let free = |_: u16| true;
        let p = lease_in(&dir, &a, "one", 20000, 10, &free).unwrap();
        assert_eq!(lease_in(&dir, &a, "two", 20000, 10, &free).unwrap(), p);
        release_in(&dir, &a, "one").unwrap();
        assert!(read_ledger(&dir).contains_key(&a.display().to_string()));
        release_in(&dir, &a, "two").unwrap();
        assert!(read_ledger(&dir).is_empty());
    }

    #[test]
    fn a_session_keeps_only_the_worktrees_it_can_come_back_to() {
        let dir = temp("keep-only");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let free = |_: u16| true;
        lease_in(&dir, &a, "one", 20000, 10, &free).unwrap();
        lease_in(&dir, &b, "one", 20000, 10, &free).unwrap();
        lease_in(&dir, &b, "two", 20000, 10, &free).unwrap();
        keep_only_in(&dir, "one", &BTreeSet::new()).unwrap();
        let ledger = read_ledger(&dir);
        assert!(!ledger.contains_key(&a.display().to_string()), "nobody else had it");
        assert_eq!(ledger[&b.display().to_string()].sessions, BTreeSet::from(["two".to_string()]), "another session's stays");
    }

    #[test]
    fn another_sessions_agents_are_found_in_a_worktree() {
        let out = r#"{"agents":[{"name":"rb","cwd":"/r/.trix/worktrees/w/src"},{"name":"x","cwd":"/r/.trix/worktrees/w2"}]}"#;
        let reply = serde_json::to_string(&crate::control::Reply::ok(out)).unwrap();
        assert_eq!(agents_in(&reply, Path::new("/r/.trix/worktrees/w")), Some(vec!["rb".to_string()]));
        assert_eq!(agents_in(&reply, Path::new("/r/.trix/worktrees/none")), Some(vec![]));
        assert_eq!(agents_in("", Path::new("/r")), None, "no answer isn't \"nobody\"");
    }

    #[test]
    fn running_out_of_ports_is_said() {
        let opts = WorktreeOpts::default();
        assert!(no_ports(Path::new("/r/.trix/worktrees/w"), &opts, None).unwrap().contains("without TRIX_PORT"));
        assert_eq!(no_ports(Path::new("/r/.trix/worktrees/w"), &opts, Some(Ports { base: 20000, count: 10 })), None);
        assert_eq!(no_ports(Path::new("/r/src"), &opts, None), None, "not a worktree of trix's");
        assert_eq!(no_ports(Path::new("/r/.trix/worktrees/w"), &WorktreeOpts { ports: false, ..opts }, None), None, "turned off");
    }

    #[test]
    fn coming_back_to_a_released_worktree_gets_the_repos_size() {
        let root = repo("size");
        std::fs::write(root.join(FILE), "[worktree]\nports = 5\n").unwrap();
        let cfg = RepoConfig::load(&root).unwrap().unwrap();
        cfg.trust().unwrap();
        let tree = root.join(".trix/worktrees/w");
        git(&root, &["worktree", "add", "-q", "--detach", &tree.display().to_string()]);
        let p = lease(&tree, &WorktreeOpts::default(), None).unwrap();
        assert_eq!(p.count, 5);
        release(&tree);
        assert_eq!(lease(&tree, &WorktreeOpts::default(), None).unwrap().count, 5);
        release(&tree);
    }

    #[test]
    fn a_setup_says_it_finished_where_a_new_server_can_read_it() {
        let root = repo("marker");
        let tree = linked(&root, "marker-tree");
        let m = done_marker(&tree).unwrap();
        assert!(m.starts_with(root.join(".git")), "{}", m.display());
        let s = Setup { script: "true".into(), root: root.clone(), done: Some(m.clone()) };
        assert!(!s.finished());
        // Not through `run_setup`: on success that becomes the agent.
        assert_eq!(run_script("true", Some(&m)), Ok(()));
        assert!(s.finished());
        assert_eq!(run_script("exit 3", Some(&m)), Err(3));
        assert!(!s.finished(), "a retry that fails takes it back");
    }

    #[test]
    fn a_removed_worktree_gives_its_ports_back() {
        let dir = temp("gone");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let free = |_: u16| true;
        let p = lease_in(&dir, &a, "s", 20000, 10, &free).unwrap();
        std::fs::remove_dir(&a).unwrap();
        assert_eq!(lease_in(&dir, &b, "s", 20000, 10, &free).unwrap(), p, "its folder is gone");

        forget_in(&dir, &b).unwrap();
        assert!(read_ledger(&dir).is_empty());
    }

    #[test]
    fn a_setup_that_fails_never_reaches_the_agent() {
        let dir = temp("setup");
        let marker = dir.join("agent-ran");
        let args = |script: &str| ["tell".to_string(), String::new(), script.to_string(), "--".to_string(), "touch".to_string(), marker.display().to_string()];
        assert_eq!(run_setup(&args("exit 7")), 7);
        assert_eq!(run_setup(&args("false; echo not reached")), 1, "the first failure is the setup's");
        assert!(!marker.exists());
        assert_eq!(run_setup(&["tell".into(), "true".into()]), 2, "malformed");
    }

    #[test]
    fn the_wrapped_command_keeps_every_argument() {
        let setup = Setup { script: "npm ci".into(), root: "/r".into(), done: Some("/g/done".into()) };
        let (exe, args) = wrap(Path::new("/bin/trix"), &setup, false, "claude".into(), vec!["--flag".into(), "--".into()]);
        assert_eq!(exe, "/bin/trix");
        assert_eq!(args, ["worktree-setup", "tell", "/g/done", "npm ci", "--", "claude", "--flag", "--"]);
        assert_eq!(wrap(Path::new("t"), &setup, true, "p".into(), vec![]).1[1], "quiet");
    }
}
