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

    /// Start an agent, with the guard on its pane when `guard` says so
    /// (`push: guarded` and `gate_only`): an accidental push or `gh` call
    /// fails there.
    fn spawn_in(
        &mut self,
        name: &str,
        agent: &AgentSpec,
        cwd: &Path,
        prompt: &str,
        guard: bool,
    ) -> Result<(), String> {
        let _ = guard;
        self.spawn(name, agent, cwd, prompt)
    }
    /// Run a check: a command in a process of its own, with trix's
    /// variables removed, the guard on when `guard` says so, and its pid
    /// registered with the server so nothing it starts can drive trix.
    fn check(&mut self, cwd: &Path, argv: &[String], guard: bool) -> Result<CheckOut, String> {
        let _ = (cwd, argv, guard);
        Err("checks can't run here".into())
    }
    /// The last lines of an agent's screen: how a quota stop is told from
    /// an idle agent.
    fn screen(&mut self, name: &str) -> Option<String> {
        let _ = name;
        None
    }
    /// Tell you, on the desktop.
    fn notify(&mut self, title: &str, body: &str) {
        let _ = (title, body);
    }
    /// Run your `on_finish` command: a run ended.
    fn on_finish(&mut self, argv: &[String]) {
        let _ = argv;
    }
    /// Store nothing, start a run's pane: the run is already in its folder.
    fn open_run(&mut self, id: &str, repo: &Path) -> Result<(), String> {
        let _ = (id, repo);
        Err("runs can't be started here".into())
    }
}

/// What a check came to.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckOut {
    pub exit_code: i32,
    /// The end of what it printed.
    pub log_tail: String,
}

/// The end of a command's output, for a prompt: its last lines, bounded.
pub fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(40);
    let t = lines[from..].join("\n");
    let cut = t.len().saturating_sub(4000);
    t[t.ceil_char_boundary(cut)..].to_string()
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
    fn spawn_in(
        &mut self,
        name: &str,
        agent: &AgentSpec,
        cwd: &Path,
        prompt: &str,
        guard: bool,
    ) -> Result<(), String> {
        if !guard {
            return self.spawn(name, agent, cwd, prompt);
        }
        let command = (!agent.args.is_empty()).then(|| shell_words::join(&agent.args));
        control(crate::control::Request::SpawnGuarded {
            kind: agent.agent.clone(),
            name: Some(name.into()),
            cwd: cwd.to_path_buf(),
            command,
            prompt: Some(prompt.into()),
        })
        .map(|_| ())
    }
    fn check(&mut self, cwd: &Path, argv: &[String], guard: bool) -> Result<CheckOut, String> {
        let Some((program, rest)) = argv.split_first() else {
            return Err("the check has no command".into());
        };
        let mut c = std::process::Command::new(program);
        c.current_dir(cwd)
            .args(rest)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        // No pane's: it finds no trix to talk to as one.
        for k in super::guard::TRIX_VARS {
            c.env_remove(k);
        }
        if guard {
            super::guard::apply(&mut c, &super::guard::empty_dir());
        }
        let child = c.spawn().map_err(|e| format!("{program}: {e}"))?;
        // The server is told whose this is before it can ask anything: a
        // request from it, or from what it starts, is refused.
        control(crate::control::Request::CheckPid { pid: child.id() })?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{program}: {e}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(CheckOut {
            exit_code: out.status.code().unwrap_or(-1),
            log_tail: tail(&text),
        })
    }
    fn screen(&mut self, name: &str) -> Option<String> {
        control(crate::control::Request::Read {
            target: name.into(),
            lines: 15,
        })
        .ok()
    }
    fn notify(&mut self, title: &str, body: &str) {
        crate::platform::notify(title, body, false);
    }
    fn on_finish(&mut self, argv: &[String]) {
        if let Some((program, rest)) = argv.split_first() {
            let program = crate::app::expand(program);
            let _ = std::process::Command::new(program)
                .args(rest)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
    fn open_run(&mut self, id: &str, repo: &Path) -> Result<(), String> {
        let exe = crate::platform::trix_exe().map_err(|e| e.to_string())?;
        let command = shell_words::join([
            exe.display().to_string(),
            "workflow".into(),
            "pane".into(),
            id.to_string(),
        ]);
        control(crate::control::Request::Spawn {
            kind: "custom".into(),
            name: Some(id.into()),
            cwd: repo.to_path_buf(),
            command: Some(command),
            prompt: None,
            worktree: false,
        })
        .map(|_| ())
    }
}
