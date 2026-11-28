//! Everything a runner does outside itself, behind one trait, so a test, a
//! dry run or a replay can play the world.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// An agent as the runner sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub name: String,
    /// `trix ls --json`'s: working, needs-you, done, idle, starting, exited.
    pub state: String,
    pub cost: f64,
}

/// The agent a node starts: its kind, and its extra arguments.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub agent: String,
    pub args: Vec<String>,
}

/// Everything the runner does outside itself, so a test can play it.
pub trait Env {
    fn now(&self) -> u64;
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, String>;
    fn gh(&mut self, repo: &Path, args: &[&str]) -> Result<String, String>;
    fn spawn(
        &mut self,
        name: &str,
        agent: &AgentSpec,
        cwd: &Path,
        prompt: &str,
    ) -> Result<(), String>;
    /// Store a new triage case and start its pane.
    fn open_case(&mut self, c: &crate::case::Case) -> Result<(), String>;
    fn send(&mut self, name: &str, text: &str) -> Result<(), String>;
    /// Close an agent; `Err` while it's still working.
    fn close(&mut self, name: &str, clean: bool) -> Result<(), String>;
    fn agents(&mut self) -> Result<Vec<Seen>, String>;
    /// A key typed into the pane: yours, or `approve` / `abort`'s, which
    /// the server types there.
    fn answer(&mut self) -> Option<char>;
    /// The pane's state and activity: `Working`, `Question` or `Finished`.
    fn say(&mut self, event: &str, detail: &str);
    fn print(&mut self, line: &str);
}

/// A command run in `repo`, never prompting: its output, or what it said
/// when it failed.
pub fn run_cmd(prog: &str, repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        })
    }
}

/// A request to this session's server, from the pane this runs in.
pub fn control(req: crate::control::Request) -> Result<String, String> {
    let msg = crate::session::ClientMsg::Control {
        protocol: crate::control::CONTROL,
        from: crate::control::caller(),
        req,
    };
    match crate::session::control(&msg, Some(50)) {
        Ok(r) if r.code == 0 => Ok(r.out),
        Ok(r) => Err(r.err.trim().to_string()),
        Err(e) => Err(e),
    }
}

/// The runner in its pane: git, gh and the workspace for real.
pub struct Live {
    keys: std::sync::mpsc::Receiver<char>,
    /// Who the pane says it is to the server: `triage`, `workflow`.
    source: &'static str,
    /// What the pane last said: said again only when it changes.
    said: (String, String),
}

impl Live {
    /// Takes its answers from the pane's stdin: a key, then enter.
    pub fn in_pane(source: &'static str) -> Live {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                if let Some(k) = line.trim().chars().next() {
                    let _ = tx.send(k.to_ascii_lowercase());
                }
                line.clear();
            }
        });
        Live {
            keys: rx,
            source,
            said: Default::default(),
        }
    }

    /// For a command that only looks: it's never typed into.
    pub fn looking(source: &'static str) -> Live {
        Live {
            keys: std::sync::mpsc::channel().1,
            source,
            said: Default::default(),
        }
    }
}

impl Env for Live {
    fn now(&self) -> u64 {
        crate::prices::now()
    }
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, String> {
        run_cmd("git", repo, args)
    }
    fn gh(&mut self, repo: &Path, args: &[&str]) -> Result<String, String> {
        run_cmd("gh", repo, args)
    }
    fn spawn(
        &mut self,
        name: &str,
        agent: &AgentSpec,
        cwd: &Path,
        prompt: &str,
    ) -> Result<(), String> {
        let command = (!agent.args.is_empty()).then(|| shell_words::join(&agent.args));
        control(crate::control::Request::Spawn {
            kind: agent.agent.clone(),
            name: Some(name.into()),
            cwd: cwd.to_path_buf(),
            command,
            prompt: Some(prompt.into()),
            worktree: false,
        })
        .map(|_| ())
    }
    fn open_case(&mut self, c: &crate::case::Case) -> Result<(), String> {
        crate::case::store(&crate::case::root(), c)?;
        crate::case::case_pane(&crate::case::root(), &c.id, &c.repo)
    }
    fn send(&mut self, name: &str, text: &str) -> Result<(), String> {
        control(crate::control::Request::Send {
            target: name.into(),
            text: text.into(),
            force: false,
        })
        .map(|_| ())
    }
    fn close(&mut self, name: &str, clean: bool) -> Result<(), String> {
        control(crate::control::Request::Close {
            target: name.into(),
            force: false,
            clean,
        })
        .map(|_| ())
    }
    fn agents(&mut self) -> Result<Vec<Seen>, String> {
        let out = control(crate::control::Request::List { json: true })?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        Ok(v["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| Seen {
                name: a["name"].as_str().unwrap_or_default().into(),
                state: a["state"].as_str().unwrap_or_default().into(),
                cost: a["spend"]["cost_usd"].as_f64().unwrap_or(0.0),
            })
            .collect())
    }
    fn answer(&mut self) -> Option<char> {
        self.keys.try_recv().ok()
    }
    fn say(&mut self, event: &str, detail: &str) {
        if self.said != (event.to_string(), detail.to_string()) {
            self.said = (event.into(), detail.into());
            crate::hooks::say(self.source, event, detail);
        }
    }
    fn print(&mut self, line: &str) {
        println!("{line}");
    }
}
