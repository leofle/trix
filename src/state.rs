//! Session restore: what the workspace looked like, kept on disk so a new
//! server (after `trix kill`, alt+q, a crash or a reboot) can bring it back.
//!
//! Lives under `$XDG_STATE_HOME/trix` (default `~/.local/state/trix`), not in
//! the runtime dir, because that one is wiped on reboot. Claude and Codex
//! agents keep the id of the conversation they were in, so restoring resumes
//! that exact conversation instead of the most recent one.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedAgent {
    pub kind: String,
    pub name: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub command: Option<String>,
    /// Claude `session_id` / Codex `thread-id`.
    #[serde(default)]
    pub session: Option<String>,
    /// Pinned to the top of the list.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Saved {
    pub version: u32,
    #[serde(default)]
    pub focus: usize,
    pub agents: Vec<SavedAgent>,
}

impl Saved {
    pub fn new(focus: usize, agents: Vec<SavedAgent>) -> Saved {
        Saved { version: VERSION, focus, agents }
    }

    /// One line for the restore prompt, e.g. "3 agents · 2 conversations resume".
    pub fn summary(&self) -> String {
        let n = self.agents.len();
        let resumable = self.agents.iter().filter(|a| a.session.is_some()).count();
        let mut s = format!("{n} agent{} from your last session", if n == 1 { "" } else { "s" });
        if resumable > 0 {
            s.push_str(&format!(" · {resumable} conversation{} resume", if resumable == 1 { "" } else { "s" }));
        }
        s
    }
}

fn dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("trix")
}

/// An XDG base folder: `$var` when it's an absolute path, else `~/<fallback>`.
pub fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback))
}

/// Write `bytes` to `path` through a temporary file and a rename, so a crash
/// mid-write never leaves half a file. Creates the folder; a failed write
/// leaves no temporary file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    // Two writers in one process (the usage thread and the UI both store
    // prefs) must not share a temporary file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    tmp.push(format!(".{}.{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let tmp = PathBuf::from(tmp);
    let res = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// `write_atomic` through a symlink: a config file linked from a dotfiles
/// checkout stays linked, and the checkout gets the change.
pub fn write_through(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let linked = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let real = if linked { std::fs::canonicalize(path)? } else { path.to_path_buf() };
    write_atomic(&real, bytes)
}

/// One file per session; the default one keeps the original name.
pub fn path() -> PathBuf {
    if crate::session::is_default() {
        dir().join("workspace.json")
    } else {
        dir().join(format!("workspace-{}.json", crate::session::name()))
    }
}

pub fn load() -> Option<Saved> {
    load_from(&path())
}

fn load_from(path: &Path) -> Option<Saved> {
    let text = std::fs::read_to_string(path).ok()?;
    let saved: Saved = serde_json::from_str(&text).ok()?;
    (saved.version == VERSION && !saved.agents.is_empty()).then_some(saved)
}

/// Write atomically, so a crash mid-write never leaves half a file. An empty
/// workspace removes the file: there is nothing to offer next time.
pub fn store(saved: &Saved) -> std::io::Result<()> {
    store_at(&path(), saved)
}

fn store_at(path: &Path, saved: &Saved) -> std::io::Result<()> {
    if saved.agents.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    write_atomic(path, &serde_json::to_vec_pretty(saved)?)
}

/// How you like trix to look, whatever the session or workspace. Kept apart
/// from the saved workspace, which is per session, only read when you
/// restore, and gone when the workspace is empty.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Prefs {
    /// alt+m: the narrow sidebar.
    #[serde(default)]
    pub sidebar_mini: bool,
    /// alt+c: one-line sidebar cards.
    #[serde(default)]
    pub sidebar_compact: bool,
    /// The kind of agent started last from alt+n, for `[new] agent = "last"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent: Option<String>,
    /// Claude context windows learned per model (see usage.rs).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub context_windows: std::collections::BTreeMap<String, u64>,
}

fn prefs_path() -> PathBuf {
    dir().join("prefs.json")
}

pub fn load_prefs() -> Prefs {
    // Nor read them: tests start from the defaults, whatever this machine has.
    if cfg!(test) {
        return Prefs::default();
    }
    load_prefs_from(&prefs_path())
}

fn load_prefs_from(path: &Path) -> Prefs {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn store_prefs(prefs: &Prefs) -> std::io::Result<()> {
    // Tests that flip the sidebar leave the developer's own alone.
    if cfg!(test) {
        return Ok(());
    }
    store_prefs_at(&prefs_path(), prefs)
}

fn store_prefs_at(path: &Path, prefs: &Prefs) -> std::io::Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(prefs)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn tmp() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("trix-test-state-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn agent(kind: &str, name: &str, session: Option<&str>) -> SavedAgent {
        SavedAgent { kind: kind.into(), name: name.into(), cwd: "/tmp".into(), command: None, session: session.map(Into::into), pinned: false }
    }

    #[test]
    fn summary_counts_agents_and_conversations() {
        assert_eq!(Saved::new(0, vec![agent("shell", "s", None)]).summary(), "1 agent from your last session");
        assert_eq!(
            Saved::new(0, vec![agent("claude", "a", Some("x")), agent("shell", "s", None)]).summary(),
            "2 agents from your last session · 1 conversation resume"
        );
        assert_eq!(
            Saved::new(0, vec![agent("claude", "a", Some("x")), agent("codex", "b", Some("y")), agent("shell", "s", None)]).summary(),
            "3 agents from your last session · 2 conversations resume"
        );
    }

    #[test]
    fn store_then_load() {
        let dir = tmp();
        let path = dir.join("nested/workspace.json");
        let mut custom = agent("custom", "dev", None);
        custom.command = Some("npm run dev".into());
        let saved = Saved::new(1, vec![agent("claude", "web", Some("abc")), custom]);
        store_at(&path, &saved).unwrap();
        assert_eq!(load_from(&path), Some(saved));
        // Written atomically: no temp file left next to it.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["workspace.json"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_workspace_removes_the_file() {
        let dir = tmp();
        let path = dir.join("workspace.json");
        store_at(&path, &Saved::new(0, vec![agent("shell", "s", None)])).unwrap();
        assert!(path.exists());
        store_at(&path, &Saved::new(0, vec![])).unwrap();
        assert!(!path.exists());
        // And again, with nothing there to remove.
        store_at(&path, &Saved::new(0, vec![])).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unusable_files_load_as_nothing() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.json");
        assert_eq!(load_from(&path), None, "missing");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_from(&path), None, "garbage");
        std::fs::write(&path, r#"{"version":99,"agents":[{"kind":"shell","name":"s","cwd":"/"}]}"#).unwrap();
        assert_eq!(load_from(&path), None, "other version");
        std::fs::write(&path, r#"{"version":1,"agents":[]}"#).unwrap();
        assert_eq!(load_from(&path), None, "no agents");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn write_atomic_leaves_no_temporary_file() {
        let dir = tmp();
        let path = dir.join("sub/file.json");
        write_atomic(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one", "creates the folder");
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // Renaming over a non-empty folder fails after the write succeeded.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("inside")).unwrap();
        assert!(write_atomic(&blocked, b"x").is_err());
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "only sub/ and blocked/: {left:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prefs_round_trip_and_default() {
        let dir = tmp();
        let path = dir.join("prefs.json");
        assert_eq!(load_prefs_from(&path), Prefs::default(), "no file yet: full sidebar");
        store_prefs_at(&path, &Prefs { sidebar_mini: true, ..Prefs::default() }).unwrap();
        assert!(load_prefs_from(&path).sidebar_mini);
        store_prefs_at(&path, &Prefs { sidebar_mini: false, ..Prefs::default() }).unwrap();
        assert!(!load_prefs_from(&path).sidebar_mini);
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(load_prefs_from(&path), Prefs::default(), "older file without the field");
        std::fs::write(&path, r#"{"sidebar_mini":true,"from_a_newer_trix":1}"#).unwrap();
        assert!(load_prefs_from(&path).sidebar_mini, "unknown fields don't throw the rest away");
        let mut learned = Prefs::default();
        learned.context_windows.insert("claude-opus-5-5".into(), 1_000_000);
        store_prefs_at(&path, &learned).unwrap();
        assert_eq!(load_prefs_from(&path), learned, "learned context windows are kept");
        store_prefs_at(&path, &Prefs::default()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("context_windows"), "and left out when there are none");
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(load_prefs_from(&path), Prefs::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn optional_fields_default() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.json");
        std::fs::write(&path, r#"{"version":1,"agents":[{"kind":"shell","name":"s","cwd":"/"}]}"#).unwrap();
        let saved = load_from(&path).unwrap();
        assert_eq!(saved.focus, 0);
        assert_eq!(saved.agents[0].command, None);
        assert_eq!(saved.agents[0].session, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
