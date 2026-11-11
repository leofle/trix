//! Session restore: what the workspace looked like, kept on disk so a new
//! server (after `trix kill`, alt+q, a crash or a reboot) can bring it back.
//!
//! Lives under `$XDG_STATE_HOME/trix` (default `~/.local/state/trix`), not in
//! the runtime dir, because that one is wiped on reboot. Claude and Codex
//! agents keep the id of the conversation they were in, so restoring resumes
//! that exact conversation instead of the most recent one.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

pub fn path() -> PathBuf {
    dir().join("workspace.json")
}

pub fn load() -> Option<Saved> {
    let text = std::fs::read_to_string(path()).ok()?;
    let saved: Saved = serde_json::from_str(&text).ok()?;
    (saved.version == VERSION && !saved.agents.is_empty()).then_some(saved)
}

/// Write atomically, so a crash mid-write never leaves half a file. An empty
/// workspace removes the file: there is nothing to offer next time.
pub fn store(saved: &Saved) -> std::io::Result<()> {
    if saved.agents.is_empty() {
        return match std::fs::remove_file(path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir())?;
    let tmp = dir().join(format!("workspace.json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(saved)?)?;
    std::fs::rename(&tmp, path())
}
