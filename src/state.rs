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
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("trix")
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
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(saved)?)?;
    std::fs::rename(&tmp, path)
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
        SavedAgent { kind: kind.into(), name: name.into(), cwd: "/tmp".into(), command: None, session: session.map(Into::into) }
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
