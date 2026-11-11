//! Agents driving agents.
//!
//! `trix ls --json`, `trix send`, `trix read`, `trix wait`, `trix spawn` and
//! `trix close` let anything with a shell (most usefully, an agent in another
//! pane) work the workspace the way the human does. They talk to the server
//! over the control socket: one `ClientMsg::Control` request, one JSON
//! `Reply` back in a `TEXT` frame, and the client prints it and exits with its
//! code. `wait` is the only one that takes its time: the server keeps the
//! connection and answers from its tick once a target settles.
//!
//! The guard rails live here, server-side, so every client gets them:
//! - `send` never types into an agent that is sitting on a prompt (the Enter
//!   would answer it: approve a `rm -rf`), into a shell (it would run as a
//!   command), or into one still starting (it may be on a trust dialog),
//!   unless `--force` says so.
//! - An agent can't send to, wait on or close itself.
//! - `close` won't stop an agent that is working or needs you without
//!   `--force`.
//! - Agents a client started or prompted show who did it in the sidebar, and
//!   every send is announced with a toast, so the human can tell.

use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agent::{Agent, Kind, Spawn, State};
use crate::app::{self, App, ToastKind};
use crate::session::{self, ClientMsg, TEXT};

/// Version of the control requests below. A server that doesn't know
/// `ClientMsg::Control` at all just hangs up, which the client reports.
pub const CONTROL: u32 = 1;

/// How long after a `send` an agent that hasn't moved still counts as busy:
/// enough for any agent to show it started, so `send` then `wait` doesn't
/// return before the turn begins, short enough that a prompt that never
/// started a turn doesn't hang `wait`.
const SEND_GRACE: Duration = Duration::from_secs(15);

/// `read` without `--lines`.
const READ_LINES: usize = 60;

/// `wait` without `--timeout`: below the 10-minute ceiling of agents' own
/// shell tools, so a waiting agent gets an answer instead of being killed.
const WAIT_SECS: u64 = 300;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Request {
    List { json: bool },
    Send { target: String, text: String, force: bool },
    Read { target: String, lines: usize },
    Wait { targets: Vec<String>, all: bool, timeout_secs: u64 },
    Spawn { kind: String, name: Option<String>, cwd: PathBuf, command: Option<String>, prompt: Option<String>, worktree: bool },
    Close { target: String, force: bool },
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Reply {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Reply {
    fn ok(out: impl Into<String>) -> Reply {
        Reply { code: 0, out: out.into(), err: String::new() }
    }
    fn fail(err: impl Into<String>) -> Reply {
        Reply { code: 1, out: String::new(), err: err.into() }
    }
}

// ───────────────────────────── client side ─────────────────────────────

pub const COMMANDS: [&str; 5] = ["send", "read", "wait", "spawn", "close"];

/// Parse `trix <command> …` (session flags already taken out).
pub fn parse(args: &[String], cwd: PathBuf) -> Result<Request, String> {
    let (cmd, rest) = args.split_first().ok_or("no command")?;
    let mut pos = Vec::new();
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let takes_value = |f: &str| matches!(f, "--lines" | "-n" | "--timeout" | "-t" | "--name" | "--cwd" | "--prompt" | "--args");
    let mut it = rest.iter();
    let mut only_pos = false;
    while let Some(a) = it.next() {
        if only_pos || !a.starts_with('-') || a == "-" {
            pos.push(a.clone());
        } else if a == "--" {
            only_pos = true;
        } else if let Some((f, v)) = a.split_once('=').filter(|_| a.starts_with("--")) {
            flags.push((f.to_string(), Some(v.to_string())));
        } else if takes_value(a) {
            let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
            flags.push((a.clone(), Some(v.clone())));
        } else {
            flags.push((a.clone(), None));
        }
    }
    let allowed: &[&str] = match cmd.as_str() {
        "send" => &["--force", "-f"],
        "read" => &["--lines", "-n"],
        "wait" => &["--all", "--timeout", "-t"],
        "spawn" => &["--name", "--cwd", "--prompt", "--args", "--worktree"],
        "close" => &["--force", "-f"],
        other => return Err(format!("unknown command '{other}'")),
    };
    if let Some((f, _)) = flags.iter().find(|(f, _)| !allowed.contains(&f.as_str())) {
        return Err(format!("{cmd}: unknown option '{f}'"));
    }
    let has = |names: &[&str]| flags.iter().any(|(f, _)| names.contains(&f.as_str()));
    let value = |names: &[&str]| flags.iter().rev().find(|(f, _)| names.contains(&f.as_str())).and_then(|(_, v)| v.clone());
    let number = |names: &[&str], default: u64| -> Result<u64, String> {
        match value(names) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("{}: '{v}' is not a number", names[0])),
        }
    };
    let one = |what: &str| -> Result<String, String> {
        match pos.as_slice() {
            [t] => Ok(t.clone()),
            [] => Err(format!("{cmd} needs {what}")),
            _ => Err(format!("{cmd} takes one {what}")),
        }
    };
    Ok(match cmd.as_str() {
        "send" => {
            let [target, text @ ..] = pos.as_slice() else { return Err("send needs an agent and a prompt".into()) };
            let text = if text.is_empty() || text == ["-"] {
                let mut s = String::new();
                io::stdin().read_to_string(&mut s).map_err(|e| format!("reading the prompt from stdin: {e}"))?;
                s
            } else {
                text.join(" ")
            };
            let text = text.trim_end_matches('\n').to_string();
            if text.trim().is_empty() {
                return Err("send: the prompt is empty".into());
            }
            Request::Send { target: target.clone(), text, force: has(&["--force", "-f"]) }
        }
        "read" => Request::Read { target: one("agent")?, lines: number(&["--lines", "-n"], READ_LINES as u64)?.clamp(1, 10_000) as usize },
        "wait" => {
            if pos.is_empty() {
                return Err("wait needs at least one agent".into());
            }
            Request::Wait { targets: pos.clone(), all: has(&["--all"]), timeout_secs: number(&["--timeout", "-t"], WAIT_SECS)? }
        }
        "spawn" => {
            let kind = one("agent kind (claude, codex, shell or custom)")?;
            let cwd = value(&["--cwd"]).map(|d| absolute(app::expand(&d), &cwd)).unwrap_or(cwd);
            Request::Spawn {
                kind,
                name: value(&["--name"]).filter(|n| !n.trim().is_empty()),
                cwd,
                command: value(&["--args"]).filter(|s| !s.trim().is_empty()),
                prompt: value(&["--prompt"]).filter(|s| !s.trim().is_empty()),
                worktree: has(&["--worktree"]),
            }
        }
        "close" => Request::Close { target: one("agent")?, force: has(&["--force", "-f"]) },
        _ => unreachable!(),
    })
}

fn absolute(p: PathBuf, cwd: &std::path::Path) -> PathBuf {
    if p.is_absolute() { p } else { cwd.join(p) }
}

/// The pane this process runs in, when it is one of this session's.
fn caller() -> Option<u32> {
    if std::env::var("TRIX_SESSION").ok()? != session::name() {
        return None;
    }
    std::env::var("TRIX_AGENT_ID").ok()?.parse().ok()
}

/// Send one request, print the reply, exit with its code.
pub fn run(req: Request) -> ! {
    let wait = match &req {
        Request::Wait { timeout_secs, .. } => Some(*timeout_secs),
        _ => None,
    };
    let reply = match session::control(&ClientMsg::Control { protocol: CONTROL, from: caller(), req }, wait) {
        Err(e) => Reply { code: 1, err: e, ..Reply::default() },
        Ok(r) => r,
    };
    print!("{}", reply.out);
    if !reply.err.is_empty() {
        eprintln!("trix: {}", reply.err);
    }
    std::process::exit(reply.code);
}

// ───────────────────────────── server side ─────────────────────────────

/// A `trix wait` that hasn't been answered yet.
pub struct Waiter {
    stream: UnixStream,
    agents: Vec<u32>,
    all: bool,
    deadline: Option<Instant>,
}

pub fn reply(stream: &mut UnixStream, r: &Reply) {
    let json = serde_json::to_vec(r).unwrap_or_default();
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let _ = session::write_frame(stream, TEXT, &json);
}

pub fn handle(app: &mut App, mut stream: UnixStream, from: Option<u32>, req: Request) {
    if let Request::Wait { targets, all, timeout_secs } = req {
        match wait(app, from, &targets) {
            Ok(agents) => {
                let deadline = (timeout_secs > 0).then(|| Instant::now() + Duration::from_secs(timeout_secs));
                app.waiters.push(Waiter { stream, agents, all, deadline });
                poll(app);
            }
            Err(e) => reply(&mut stream, &Reply::fail(e)),
        }
        return;
    }
    let r = match req {
        Request::List { json: true } => Reply::ok(listing_json(app, from)),
        Request::List { json: false } => Reply::ok(app.listing()),
        Request::Send { target, text, force } => send(app, from, &target, &text, force),
        Request::Read { target, lines } => read(app, &target, lines),
        Request::Spawn { kind, name, cwd, command, prompt, worktree } => spawn(app, from, &kind, name, cwd, command, prompt, worktree),
        Request::Close { target, force } => close(app, from, &target, force),
        Request::Wait { .. } => unreachable!(),
    };
    reply(&mut stream, &r);
}

fn caller_name(app: &App, from: Option<u32>) -> Option<String> {
    let id = from?;
    app.agents.iter().find(|a| a.id == id).map(|a| a.name.clone())
}

/// An agent by exact name, or by its number in the sidebar.
pub fn resolve<'a>(names: impl ExactSizeIterator<Item = &'a str> + Clone, target: &str) -> Result<usize, String> {
    let exact: Vec<usize> = names.clone().enumerate().filter(|(_, n)| *n == target).map(|(i, _)| i).collect();
    match exact.as_slice() {
        [i] => return Ok(*i),
        [] => {}
        _ => return Err(format!("{} agents are named '{target}' — rename one (alt+r), or use its number", exact.len())),
    }
    let len = names.len();
    if let Ok(n) = target.parse::<usize>()
        && (1..=len).contains(&n)
    {
        return Ok(n - 1);
    }
    if len == 0 {
        return Err(format!("no agent '{target}': the workspace is empty"));
    }
    let have: Vec<&str> = names.collect();
    Err(format!("no agent '{target}' (agents: {})", have.join(", ")))
}

fn find(app: &App, target: &str) -> Result<usize, String> {
    resolve(app.agents.iter().map(|a| a.name.as_str()), target)
}

fn not_self(a: &Agent, from: Option<u32>, what: &str) -> Result<(), String> {
    if from == Some(a.id) { Err(format!("'{}' is you — an agent can't {what} itself", a.name)) } else { Ok(()) }
}

/// Why `send` won't type into this agent, if it won't.
pub fn send_refusal(a: &Agent) -> Option<String> {
    if !a.kind.is_agent() {
        return Some(format!("'{}' is a shell: the prompt would run as a command (--force to type it anyway)", a.name));
    }
    match a.state {
        State::Idle | State::Working | State::Done => None,
        State::Blocked => Some(format!(
            "'{}' needs you ({}): Enter would answer that prompt — leave it to the human, or --force",
            a.name,
            crate::hooks::one_line(&a.activity, 60)
        )),
        State::Starting => Some(format!("'{}' is still starting (it may be on a trust dialog) — `trix wait` for it first", a.name)),
        State::Exited => Some(format!("'{}' has exited", a.name)),
    }
}

fn send(app: &mut App, from: Option<u32>, target: &str, text: &str, force: bool) -> Reply {
    let i = match find(app, target) {
        Ok(i) => i,
        Err(e) => return Reply::fail(e),
    };
    if let Err(e) = not_self(&app.agents[i], from, "send to") {
        return Reply::fail(e);
    }
    let a = &app.agents[i];
    if let Some(why) = send_refusal(a)
        && (!force || a.state == State::Exited)
    {
        return Reply::fail(why);
    }
    let by = caller_name(app, from);
    let a = &mut app.agents[i];
    let mode = *a.term.lock().mode();
    a.user_input(crate::input::encode_paste(text, mode));
    // Submit a beat later so TUIs see the paste and Enter as separate events.
    a.write_later(&b"\r"[..], Duration::from_millis(80));
    a.prompted = Some(Instant::now());
    if by.is_some() {
        a.managed_by = by.clone();
    }
    let name = a.name.clone();
    let who = by.unwrap_or_else(|| "trix send".into());
    app.toast(format!("{who} → {name}: {}", crate::hooks::one_line(text, 50)), ToastKind::Info);
    Reply::ok(format!("sent to {name}\n"))
}

fn read(app: &App, target: &str, lines: usize) -> Reply {
    match find(app, target) {
        Ok(i) => {
            let a = &app.agents[i];
            let mut text = crate::search::tail(&a.term.lock(), lines).join("\n");
            text.push('\n');
            Reply::ok(text)
        }
        Err(e) => Reply::fail(e),
    }
}

fn wait(app: &App, from: Option<u32>, targets: &[String]) -> Result<Vec<u32>, String> {
    let mut ids = Vec::new();
    for t in targets {
        let a = &app.agents[find(app, t)?];
        not_self(a, from, "wait for")?;
        if !ids.contains(&a.id) {
            ids.push(a.id);
        }
    }
    Ok(ids)
}

/// Has this agent stopped working, for `wait`? A prompt just sent counts as
/// work until the agent shows it started (or clearly never will).
pub fn settled(a: &Agent, now: Instant) -> bool {
    if a.state == State::Exited {
        return true;
    }
    if !matches!(a.state, State::Idle | State::Done | State::Blocked) {
        return false;
    }
    let since = a.prompted.map(|t| now.saturating_duration_since(t));
    // A shell is done when it is back at its prompt; give a typed command a
    // moment to start first.
    if a.kind == Kind::Shell {
        return since.is_none_or(|s| s > Duration::from_millis(500)) && !a.running_command();
    }
    let Some(since) = since else { return true };
    if a.prompted.is_some_and(|t| a.state_since > t) {
        return true;
    }
    if a.hooked || a.reported {
        // It will say when its turn starts.
        return since > SEND_GRACE;
    }
    // Nothing reports for it: done once its output has settled.
    since > Duration::from_secs(2) && now.saturating_duration_since(a.last_output) > Duration::from_secs(2)
}

fn status_line(a: &Agent) -> String {
    let state = match a.state {
        State::Blocked => "needs-you",
        s => s.label(),
    };
    format!("{}\t{state}\t{}\n", a.name, crate::hooks::one_line(&a.activity, 80))
}

/// Answer every `wait` whose agents have settled, timed out or gone away.
pub fn poll(app: &mut App) {
    if app.waiters.is_empty() {
        return;
    }
    let now = Instant::now();
    let waiters = std::mem::take(&mut app.waiters);
    for mut w in waiters {
        let present: Vec<&Agent> = w.agents.iter().filter_map(|id| app.agents.iter().find(|a| a.id == *id)).collect();
        let gone = w.agents.len() - present.len();
        let done: Vec<&Agent> = present.iter().copied().filter(|a| settled(a, now)).collect();
        let finished = if w.all { done.len() == present.len() } else { !done.is_empty() || present.is_empty() };
        if finished {
            let out: String = (if w.all { &present } else { &done }).iter().map(|a| status_line(a)).collect();
            let r = match gone {
                0 => Reply::ok(out),
                n => Reply { code: 1, out, err: format!("{n} of the agents waited for {} closed", if n == 1 { "was" } else { "were" }) },
            };
            reply(&mut w.stream, &r);
        } else if w.deadline.is_some_and(|d| now >= d) {
            let out: String = present.iter().map(|a| status_line(a)).collect();
            let busy: Vec<&str> = present.iter().filter(|a| !settled(a, now)).map(|a| a.name.as_str()).collect();
            let err = format!("timed out with {} still working — wait again", busy.join(", "));
            reply(&mut w.stream, &Reply { code: 124, out, err });
        } else if hung_up(&w.stream) {
            // The waiting client gave up (^C, its tool timed out).
        } else {
            app.waiters.push(w);
        }
    }
}

/// The workspace is going away: let every waiting client know why.
pub fn stop_all(app: &mut App, why: &str) {
    for mut w in std::mem::take(&mut app.waiters) {
        reply(&mut w.stream, &Reply::fail(why));
    }
}

/// Did the other end close? (EOF on a peek; a waiting client never writes.)
fn hung_up(s: &UnixStream) -> bool {
    let mut b = [0u8; 1];
    let n = unsafe { libc::recv(s.as_raw_fd(), b.as_mut_ptr().cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    n == 0 || (n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock)
}

#[allow(clippy::too_many_arguments)]
fn spawn(app: &mut App, from: Option<u32>, kind: &str, name: Option<String>, cwd: PathBuf, command: Option<String>, prompt: Option<String>, worktree: bool) -> Reply {
    let Some(kind) = Kind::from_label(kind).or_else(|| (kind == "sh").then_some(Kind::Shell)) else {
        return Reply::fail(format!("unknown agent kind '{kind}' (claude, codex, shell or custom)"));
    };
    if !cwd.is_dir() {
        return Reply::fail(format!("folder not found: {}", app::tilde(&cwd)));
    }
    if kind == Kind::Custom && command.is_none() {
        return Reply::fail("custom agents need a command: --args '<command>'");
    }
    if prompt.is_some() && !kind.is_agent() {
        return Reply::fail("a shell takes no prompt (start it, then `trix send --force`)");
    }
    let name = match name {
        Some(n) => {
            let n = n.trim().to_string();
            if app.agents.iter().any(|a| a.name == n) {
                return Reply::fail(format!("there is already an agent named '{n}'"));
            }
            n
        }
        None => {
            let base = cwd.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| kind.label().into());
            app.unique_name(&app::slug(&base))
        }
    };
    let mut cwd = cwd;
    let mut note = None;
    if worktree {
        match app::make_worktree(&cwd, &app::slug(&name)) {
            Ok((path, branch, reused)) => {
                if let Some(other) = app.agents.iter().find(|a| a.cwd == path) {
                    return Reply::fail(format!("{branch} is already {}'s worktree — pick another --name", other.name));
                }
                cwd = path;
                note = Some(format!("{} worktree on {branch}", if reused { "reusing" } else { "new" }));
            }
            Err(e) => return Reply::fail(e.to_string()),
        }
    }
    let by = caller_name(app, from);
    let spec = Spawn { kind, name: name.clone(), cwd: cwd.clone(), command, prompt, branch_note: note, resume: None };
    match app.spawn_quiet(spec) {
        Ok(id) => {
            if let Some(a) = app.agents.iter_mut().find(|a| a.id == id) {
                a.managed_by = by.clone();
            }
            let who = by.unwrap_or_else(|| "trix spawn".into());
            app.toast(format!("{who} started {name}"), ToastKind::Info);
            Reply::ok(format!("{name}\t{}\n", app::tilde(&cwd)))
        }
        Err(e) => Reply::fail(format!("could not start {}: {e}", kind.label())),
    }
}

fn close(app: &mut App, from: Option<u32>, target: &str, force: bool) -> Reply {
    let i = match find(app, target) {
        Ok(i) => i,
        Err(e) => return Reply::fail(e),
    };
    let a = &app.agents[i];
    if let Err(e) = not_self(a, from, "close") {
        return Reply::fail(e);
    }
    if !force && matches!(a.state, State::Working | State::Blocked | State::Starting) {
        let what = if a.state == State::Blocked { "needs you" } else { a.state.label() };
        return Reply::fail(format!("'{}' is {what} — --force to stop it anyway", a.name));
    }
    let (id, name) = (a.id, a.name.clone());
    app.close(id);
    Reply::ok(format!("closed {name}\n"))
}

#[derive(Serialize)]
struct JsonAgent<'a> {
    id: u32,
    number: usize,
    name: &'a str,
    kind: &'a str,
    state: &'static str,
    activity: &'a str,
    seconds_in_state: u64,
    unseen: bool,
    focused: bool,
    you: bool,
    cwd: String,
    branch: Option<&'a str>,
    diff: Option<JsonDiff>,
    last_prompt: Option<&'a str>,
    exit_code: Option<i32>,
    managed_by: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonDiff {
    files: u32,
    added: u32,
    removed: u32,
}

#[derive(Serialize)]
struct JsonList<'a> {
    session: &'a str,
    viewers: usize,
    agents: Vec<JsonAgent<'a>>,
}

fn listing_json(app: &App, from: Option<u32>) -> String {
    let agents = app
        .agents
        .iter()
        .enumerate()
        .map(|(i, a)| JsonAgent {
            id: a.id,
            number: i + 1,
            name: &a.name,
            kind: a.kind.label(),
            state: match a.state {
                State::Blocked => "needs-you",
                s => s.label(),
            },
            activity: &a.activity,
            seconds_in_state: a.state_since.elapsed().as_secs(),
            unseen: a.unseen,
            focused: i == app.focus,
            you: from == Some(a.id),
            cwd: a.cwd.display().to_string(),
            branch: a.branch.as_deref(),
            diff: a.diff.map(|(files, added, removed)| JsonDiff { files, added, removed }),
            last_prompt: a.last_prompt.as_deref(),
            exit_code: a.exit_code,
            managed_by: a.managed_by.as_deref(),
        })
        .collect();
    let mut s = serde_json::to_string_pretty(&JsonList { session: session::name(), viewers: app.viewers, agents }).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_agent;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }
    fn p(s: &[&str]) -> Result<Request, String> {
        parse(&args(s), PathBuf::from("/work"))
    }

    #[test]
    fn parses_commands() {
        assert_eq!(p(&["send", "web", "fix", "the", "tests"]), Ok(Request::Send { target: "web".into(), text: "fix the tests".into(), force: false }));
        assert_eq!(p(&["send", "-f", "web", "--", "--help me"]), Ok(Request::Send { target: "web".into(), text: "--help me".into(), force: true }));
        assert_eq!(p(&["read", "web"]), Ok(Request::Read { target: "web".into(), lines: READ_LINES }));
        assert_eq!(p(&["read", "-n", "5", "web"]), Ok(Request::Read { target: "web".into(), lines: 5 }));
        assert_eq!(p(&["read", "--lines=0", "web"]), Ok(Request::Read { target: "web".into(), lines: 1 }), "at least one line");
        assert_eq!(p(&["wait", "a", "b", "--all", "-t", "0"]), Ok(Request::Wait { targets: args(&["a", "b"]), all: true, timeout_secs: 0 }));
        assert_eq!(p(&["wait", "a"]), Ok(Request::Wait { targets: args(&["a"]), all: false, timeout_secs: WAIT_SECS }));
        assert_eq!(p(&["close", "--force", "a"]), Ok(Request::Close { target: "a".into(), force: true }));
        assert_eq!(
            p(&["spawn", "claude", "--name", "api", "--cwd", "sub", "--worktree", "--prompt=hi there"]),
            Ok(Request::Spawn {
                kind: "claude".into(),
                name: Some("api".into()),
                cwd: "/work/sub".into(),
                command: None,
                prompt: Some("hi there".into()),
                worktree: true
            })
        );
        assert_eq!(
            p(&["spawn", "shell"]),
            Ok(Request::Spawn { kind: "shell".into(), name: None, cwd: "/work".into(), command: None, prompt: None, worktree: false })
        );
    }

    #[test]
    fn rejects_bad_commands() {
        assert!(p(&["send", "web", "--force-it", "x"]).unwrap_err().contains("unknown option"));
        assert!(p(&["read"]).unwrap_err().contains("needs"));
        assert!(p(&["read", "a", "b"]).unwrap_err().contains("one"));
        assert!(p(&["read", "-n", "lots", "a"]).unwrap_err().contains("not a number"));
        assert!(p(&["read", "a", "-n"]).unwrap_err().contains("needs a value"));
        assert!(p(&["wait"]).is_err());
        assert!(p(&["wait", "--all", "a"]).is_ok());
        assert!(p(&["close", "--all"]).is_err());
        assert!(p(&["send", "web", "   "]).unwrap_err().contains("empty"));
    }

    #[test]
    fn resolves_names_then_numbers() {
        let names = ["web", "api", "2", "api-2"];
        let r = |t: &str| resolve(names.iter().copied(), t);
        assert_eq!(r("api"), Ok(1));
        assert_eq!(r("2"), Ok(2), "a name wins over a number");
        assert_eq!(r("4"), Ok(3));
        assert_eq!(r("1"), Ok(0));
        assert!(r("5").unwrap_err().contains("agents: web, api, 2, api-2"));
        assert!(r("0").is_err());
        assert!(r("ap").is_err(), "no prefix matching");
        let twins = ["x", "x"];
        assert!(resolve(twins.iter().copied(), "x").unwrap_err().contains("2 agents"));
        assert!(resolve([].iter().copied(), "x").unwrap_err().contains("empty"));
    }

    #[test]
    fn send_never_answers_a_prompt() {
        let mut a = test_agent(Kind::Claude);
        for (s, ok) in [
            (State::Starting, false),
            (State::Idle, true),
            (State::Working, true),
            (State::Blocked, false),
            (State::Done, true),
            (State::Exited, false),
        ] {
            a.set_state(s, true);
            assert_eq!(send_refusal(&a).is_none(), ok, "{s:?}");
        }
        a.set_state(State::Idle, true);
        a.kind = Kind::Shell;
        assert!(send_refusal(&a).unwrap().contains("shell"));
        a.shutdown();
    }

    #[test]
    fn a_sent_prompt_counts_as_work_until_it_starts() {
        let mut a = test_agent(Kind::Claude);
        a.hooked = true;
        a.set_state(State::Done, true);
        let now = Instant::now();
        assert!(settled(&a, now));
        a.prompted = Some(now);
        assert!(!settled(&a, now), "still Done from the last turn");
        assert!(settled(&a, now + SEND_GRACE + Duration::from_secs(1)), "never started: don't hang");
        a.set_state(State::Working, true);
        assert!(!settled(&a, Instant::now()));
        a.set_state(State::Blocked, true);
        assert!(settled(&a, Instant::now()), "needing you ends a wait");
        a.set_state(State::Done, true);
        assert!(settled(&a, Instant::now()));
        a.shutdown();
    }

    #[test]
    fn unreported_agents_settle_when_output_does() {
        let mut a = test_agent(Kind::Custom);
        a.set_state(State::Idle, true);
        let now = Instant::now();
        a.prompted = Some(now);
        a.last_output = now;
        assert!(!settled(&a, now + Duration::from_secs(1)), "too soon after the send");
        assert!(settled(&a, now + Duration::from_secs(3)), "quiet since");
        a.last_output = now + Duration::from_secs(2);
        assert!(!settled(&a, now + Duration::from_secs(3)), "output a second ago");
        a.shutdown();
    }

    #[test]
    fn shells_settle_at_their_prompt() {
        let mut a = test_agent(Kind::Shell);
        a.set_state(State::Idle, true);
        let now = Instant::now();
        assert!(settled(&a, now));
        a.prompted = Some(now);
        assert!(!settled(&a, now), "give the command a moment to start");
        assert!(settled(&a, now + Duration::from_secs(1)));
        a.shutdown();
    }

    #[test]
    fn replies_round_trip() {
        let r = Reply { code: 124, out: "a\tworking\n".into(), err: "timed out".into() };
        let back: Reply = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
