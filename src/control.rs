//! Agents driving agents.
//!
//! `trix ls --json`, `trix send`, `trix read`, `trix wait`, `trix spawn`,
//! `trix close` and `trix args` let anything with a shell (most usefully, an agent in another
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
use std::io::{self, IsTerminal, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::agent::{Agent, Kind, Spawn, State};
use crate::app::{self, App, ToastKind};
use crate::session::{self, ClientMsg, TEXT};

/// Version of the control requests below: bump it whenever an existing
/// `Request` or `Reply` changes shape. A new variant needs no bump: a server
/// that can't parse a request (older than `ClientMsg::Control`, or than the
/// request's variant) just hangs up, which the client reports, while every
/// other command keeps working with it.
pub const CONTROL: u32 = 2;

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
    /// Show (`set: None`, `clear: false`), replace or clear the extra
    /// arguments an agent starts with next time (`trix args`).
    Args { target: String, set: Option<String>, clear: bool },
    /// The agents open now, as JSON `SavedAgent`s (`trix layout save`).
    Layout,
    /// Start a saved layout's agents (`trix layout open`).
    Open { name: String, agents: Vec<crate::layout::LayoutAgent>, resume: bool },
    /// Follow what these agents (none: all of them) do (`trix events`).
    Events { targets: Vec<String> },
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Reply {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Reply {
    pub fn ok(out: impl Into<String>) -> Reply {
        Reply { code: 0, out: out.into(), err: String::new() }
    }
    fn fail(err: impl Into<String>) -> Reply {
        Reply { code: 1, out: String::new(), err: err.into() }
    }
}

// ───────────────────────────── client side ─────────────────────────────

pub const COMMANDS: [&str; 7] = ["send", "read", "wait", "spawn", "close", "events", "args"];

/// Flags whose next argument is their value, whatever it looks like.
pub const VALUE_FLAGS: &[&str] = &["--lines", "-n", "--timeout", "-t", "--name", "--cwd", "--prompt", "--args"];

/// Parse `trix <command> …` (session flags already taken out).
pub fn parse(args: &[String], cwd: PathBuf) -> Result<Request, String> {
    let (cmd, rest) = args.split_first().ok_or("no command")?;
    if cmd == "args" {
        return parse_args(rest);
    }
    let mut pos = Vec::new();
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let takes_value = |f: &str| VALUE_FLAGS.contains(&f);
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
        "events" => &["--json"],
        other => return Err(format!("unknown command '{other}'")),
    };
    if let Some((f, _)) = flags.iter().find(|(f, _)| !allowed.contains(&f.as_str())) {
        return Err(format!("{cmd}: unknown option '{f}'"));
    }
    if let Some((f, _)) = flags.iter().find(|(f, v)| v.is_some() && ["--force", "--all", "--worktree"].contains(&f.as_str())) {
        // `--force=false` would otherwise count as --force.
        return Err(format!("{cmd}: {f} takes no value"));
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
                if text.is_empty() && io::stdin().is_terminal() {
                    return Err("send needs a prompt: put it after the agent, or pipe it in".into());
                }
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
            let kind = one("agent kind (claude, codex, grok, opencode, cursor, copilot, muse, gemini, antigravity, amp, shell or custom)")?;
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
        "events" => Request::Events { targets: pos.clone() },
        _ => unreachable!(),
    })
}

/// `trix args <agent> [--clear | <args…>]`. Everything after the agent is
/// the new arguments, flags included (they are the point: `--model …`), so
/// this doesn't go through the option parser. One word is taken as already
/// written for a shell (`trix args web "--model x --effort high"`); several
/// are quoted back together, so `-- --allow-tool 'shell(trix:*)'` survives.
fn parse_args(rest: &[String]) -> Result<Request, String> {
    let rest = match rest.first() {
        Some(f) if f == "--" => &rest[1..],
        _ => rest,
    };
    let (target, words) = rest.split_first().ok_or("args needs an agent (and the new arguments, or --clear)")?;
    let (words, raw) = match words.first() {
        Some(f) if f == "--" => (&words[1..], true),
        _ => (words, false),
    };
    let (set, clear) = match words {
        [] => (None, false),
        [c] if c == "--clear" && !raw => (None, true),
        [one] => (Some(one.trim().to_string()), false),
        many => (Some(shell_words::join(many)), false),
    };
    if let Some(s) = &set {
        if s.is_empty() {
            return Err("args: the arguments are empty (use --clear to remove them)".into());
        }
        shell_words::split(s).map_err(|e| format!("args: can't split '{s}': {e}"))?;
    }
    Ok(Request::Args { target: target.clone(), set, clear })
}

fn absolute(p: PathBuf, cwd: &std::path::Path) -> PathBuf {
    if p.is_absolute() { p } else { cwd.join(p) }
}

/// The pane this process runs in, when it is one of this session's.
pub fn caller() -> Option<u32> {
    if std::env::var("TRIX_SESSION").ok()? != session::name() {
        return None;
    }
    std::env::var("TRIX_AGENT_ID").ok()?.parse().ok()
}

/// Send one request, print the reply, exit with its code.
pub fn run(req: Request) -> ! {
    let wait = match &req {
        Request::Wait { timeout_secs, .. } => Some(*timeout_secs),
        // Making a worktree in a big repo can take a while.
        Request::Spawn { .. } => Some(50),
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

pub fn handle(app: &mut App, mut stream: UnixStream, from: Option<u32>, req: Request, sandboxed: bool) {
    if sandboxed && let Some(what) = outside_the_sandbox(&req) {
        let err = format!("a command in a sandbox can't {what}: that would run outside the sandbox. Ask the user to do it, or run it outside the sandbox");
        return reply(&mut stream, &Reply::fail(err));
    }
    if let Request::Events { targets } = &req {
        return crate::events::subscribe(app, stream, targets);
    }
    if let Request::Wait { targets, all, timeout_secs } = req {
        match wait(app, from, &targets) {
            Ok(agents) => {
                // Too far off to represent is the same as no deadline.
                let deadline = (timeout_secs > 0).then(|| Instant::now().checked_add(Duration::from_secs(timeout_secs))).flatten();
                app.waiters.push(Waiter { stream, agents, all, deadline });
                poll(app);
            }
            Err(e) => reply(&mut stream, &Reply::fail(e)),
        }
        return;
    }
    // Someone is using the session, if not from a terminal.
    if matches!(req, Request::Send { .. } | Request::Spawn { .. }) {
        app.clock.active(std::time::SystemTime::now());
    }
    let r = match req {
        Request::List { json: true } => Reply::ok(listing_json(app, from)),
        Request::List { json: false } => Reply::ok(app.listing()),
        Request::Send { target, text, force } => send(app, from, &target, &text, force),
        Request::Read { target, lines } => read(app, &target, lines),
        Request::Spawn { kind, name, cwd, command, prompt, worktree } => spawn(app, from, &kind, name, cwd, command, prompt, worktree),
        Request::Close { target, force } => close(app, from, &target, force),
        Request::Args { target, set, clear } => extra_args(app, from, &target, set, clear),
        Request::Layout => Reply::ok(serde_json::to_string(&app.open_agents()).unwrap_or_default()),
        Request::Open { name, agents, resume } => crate::layout::open(app, from, &name, agents, resume),
        Request::Wait { .. } | Request::Events { .. } => unreachable!(),
    };
    reply(&mut stream, &r);
}

pub(crate) fn caller_name(app: &App, from: Option<u32>) -> Option<String> {
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
        State::Starting => Some(match &a.installing {
            Some(what) => format!("'{}' is still {what} — `trix wait` for it first", a.name),
            None => format!("'{}' is still starting (it may be on a trust dialog) — `trix wait` for it first", a.name),
        }),
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
    a.submit(crate::input::encode_paste(text, mode));
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
        // Without --all, a target closed while waited on has stopped too.
        let finished = if w.all { done.len() == present.len() } else { !done.is_empty() || gone > 0 };
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
    crate::events::stop_all(app);
}

/// Did the other end close? (EOF on a peek; a waiting client never writes.)
pub(crate) fn hung_up(s: &UnixStream) -> bool {
    let mut b = [0u8; 1];
    let n = unsafe { libc::recv(s.as_raw_fd(), b.as_mut_ptr().cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    n == 0 || (n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock)
}

/// What a command inside an agent's sandbox may not ask trix for, because
/// trix would do it outside the sandbox: run a shell or any command, type
/// into a permission prompt, change how agents start, take over or stop the
/// workspace. Reading, waiting, sending prompts and starting agents as they
/// normally start stay open, so sandboxed agents can still work with others.
fn outside_the_sandbox(req: &Request) -> Option<&'static str> {
    match req {
        Request::Send { force: true, .. } => Some("send --force (it could answer another agent's permission prompt)"),
        Request::Spawn { kind, .. } if matches!(kind_of(kind), Some(Kind::Shell | Kind::Custom)) => Some("start a shell or a custom command"),
        Request::Spawn { command: Some(c), .. } if !c.trim().is_empty() => Some("start an agent with --args"),
        Request::Close { force: true, .. } => Some("close --force"),
        Request::Args { set, clear, .. } if set.is_some() || *clear => Some("change the arguments an agent starts with"),
        Request::Open { .. } => Some("open a layout (its agents can have any arguments and commands)"),
        _ => None,
    }
}

fn kind_of(label: &str) -> Option<Kind> {
    Kind::from_label(label).or(match label {
        "sh" => Some(Kind::Shell),
        "cursor-agent" => Some(Kind::Cursor),
        "agy" => Some(Kind::Antigravity),
        _ => None,
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn(app: &mut App, from: Option<u32>, kind: &str, name: Option<String>, cwd: PathBuf, command: Option<String>, prompt: Option<String>, worktree: bool) -> Reply {
    let Some(kind) = kind_of(kind) else {
        return Reply::fail(format!("unknown agent kind '{kind}' (claude, codex, grok, opencode, cursor, copilot, muse, gemini, antigravity, amp, shell or custom)"));
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
    if prompt.is_some() && kind == Kind::Custom {
        return Reply::fail("a custom command takes no prompt: put it in --args, or start it and `trix send` it");
    }
    let name = match name {
        Some(n) => {
            let n = n.trim().to_string();
            if app::only_digits(&n) {
                return Reply::fail(format!("'{n}' can't be a name: a number means the agent at that position in `trix ls`"));
            }
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
    let mut runtime = crate::worktree::Runtime::default();
    let mut warning = String::new();
    if worktree {
        let repo = match crate::worktree::RepoConfig::load(&cwd) {
            Ok(r) => r,
            Err(e) => return Reply::fail(e),
        };
        match app::make_worktree(&cwd, &app::slug(&name)) {
            Ok((path, branch, reused)) => {
                if let Some(other) = app.agents.iter().find(|a| a.cwd == path) {
                    return Reply::fail(format!("{branch} is already {}'s worktree — pick another --name", other.name));
                }
                // Trust is only ever given in the new-agent form: a script
                // or an agent gets the worktree, and is told what didn't run.
                let ready = crate::worktree::prepare(&path, repo.as_ref(), reused, false, &app.config.worktree);
                cwd = path;
                let made = format!("{} worktree on {branch}", if reused { "reusing" } else { "new" });
                note = Some(ready.note.map_or(made.clone(), |n| format!("{made} · {n}")));
                runtime = ready.runtime;
                if let Some(w) = ready.warning {
                    app.toast(format!("{name}: {w}"), ToastKind::Bad);
                    warning = w;
                }
            }
            Err(e) => return Reply::fail(e.to_string()),
        }
    }
    let by = caller_name(app, from);
    let spec = Spawn { kind, name: name.clone(), cwd: cwd.clone(), command, prompt, branch_note: note, resume: None, fork: false, runtime };
    match app.spawn_quiet(spec) {
        Ok(id) => {
            if let Some(a) = app.agents.iter_mut().find(|a| a.id == id) {
                a.managed_by = by.clone();
            }
            let who = by.unwrap_or_else(|| "trix spawn".into());
            app.toast(format!("{who} started {name}"), ToastKind::Info);
            Reply { err: warning, ..Reply::ok(format!("{name}\t{}\n", app::tilde(&cwd))) }
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
        let what = if a.state == State::Blocked { "needs you" } else { &format!("is {}", a.state.label()) };
        return Reply::fail(format!("'{}' {what} — --force to stop it anyway", a.name));
    }
    let (id, name) = (a.id, a.name.clone());
    app.close(id);
    Reply::ok(format!("closed {name}\n"))
}

/// Show or change the extra arguments an agent starts with. A change is
/// saved with the workspace and used the next time trix starts the agent
/// (a restore, or `trix layout save`); the agent running now keeps the ones
/// it started with. An agent may change its own: fixing your own launch
/// flags is the main use.
fn extra_args(app: &mut App, from: Option<u32>, target: &str, set: Option<String>, clear: bool) -> Reply {
    let i = match find(app, target) {
        Ok(i) => i,
        Err(e) => return Reply::fail(e),
    };
    let (id, name, kind) = (app.agents[i].id, app.agents[i].name.clone(), app.agents[i].kind);
    if kind == Kind::Shell {
        return Reply::fail(format!("'{name}' is a shell: shells take no extra arguments"));
    }
    if kind == Kind::Custom && clear {
        return Reply::fail(format!("'{name}' is a custom command: its arguments are the command, so it can't go without"));
    }
    let Some(args) = app.extra_args_mut(id) else {
        return Reply::fail(format!("'{name}' has no saved launch to change"));
    };
    if set.is_none() && !clear {
        return Reply::ok(args.as_deref().map(|a| format!("{a}\n")).unwrap_or_default());
    }
    let old = std::mem::replace(args, set.clone());
    let what = set.as_deref().unwrap_or("no extra arguments");
    let was = old.map(|o| format!(" (was: {o})")).unwrap_or_default();
    if from != Some(id) {
        let who = caller_name(app, from).unwrap_or_else(|| "trix args".into());
        app.toast(format!("{who} → {name}: args {what}"), ToastKind::Info);
    }
    Reply::ok(format!("{name} will start with: {what}{was}\n"))
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
    /// Where it is now: a shell that cd'd elsewhere, or an agent that did.
    live_cwd: String,
    branch: Option<&'a str>,
    diff: Option<JsonDiff>,
    last_prompt: Option<&'a str>,
    exit_code: Option<i32>,
    managed_by: Option<&'a str>,
    /// How full its context window is (Claude and Codex), when known.
    context: Option<JsonContext>,
    /// Its tokens and what they cost (Claude and Codex), once it has used any.
    spend: Option<JsonSpend>,
}

#[derive(Serialize)]
struct JsonPrices {
    /// "live": the fetched list, ahead of trix's own; "built-in": trix's
    /// own only. `[cost.prices]` in config.toml comes first either way.
    source: &'static str,
    /// The list is fetched (`[cost] live_prices`).
    fetching: bool,
    url: String,
    /// When the list in use was fetched (Unix seconds), and how long ago.
    fetched_at: Option<u64>,
    age_seconds: Option<u64>,
    models: Option<usize>,
    /// The last fetch's failure, until one succeeds; the last good list stays in use.
    last_error: Option<String>,
}

impl JsonPrices {
    fn new(app: &App) -> JsonPrices {
        let c = &app.config.cost;
        let live = app.live_prices();
        JsonPrices {
            source: if live.is_some() { "live" } else { "built-in" },
            fetching: c.live_prices,
            url: live.map_or(c.prices_url.clone(), |l| l.url.clone()),
            fetched_at: live.map(|l| l.fetched_at),
            age_seconds: live.map(|l| crate::prices::now().saturating_sub(l.fetched_at)),
            models: live.map(|l| l.models.len()),
            last_error: app.price_feed.error.clone().filter(|_| c.live_prices),
        }
    }
}

#[derive(Serialize)]
struct JsonSpend {
    input_tokens: u64,
    cache_write_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    /// At list prices; for a subscription, what the same work would cost.
    cost_usd: f64,
    /// False when some tokens are from a model with no price: they're
    /// counted, and left out of the cost.
    cost_complete: bool,
    budget_usd: Option<f64>,
    over_budget: bool,
}

impl JsonSpend {
    fn new(app: &App, spend: &crate::usage::Spend, budget: Option<f64>) -> JsonSpend {
        let t = crate::usage::total(spend);
        let c = app.cost_of(spend);
        JsonSpend {
            input_tokens: t.input,
            cache_write_tokens: t.cache_write(),
            cache_read_tokens: t.cache_read,
            output_tokens: t.output,
            total_tokens: t.total(),
            cost_usd: (c.usd * 10_000.0).round() / 10_000.0,
            cost_complete: c.unpriced == 0,
            budget_usd: budget,
            over_budget: budget.is_some_and(|b| c.usd >= b),
        }
    }
}

#[derive(Serialize)]
struct JsonContext {
    used: u64,
    window: u64,
    left: u64,
    used_percent: u8,
    /// The window size came from the agent (Codex) or TRIX_CLAUDE_CONTEXT,
    /// rather than being inferred.
    window_exact: bool,
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
    /// A terminal is in control: someone is there, so it's never idle.
    attached: bool,
    /// Since the controller left; null while one is attached, or if none ever was.
    detached_seconds: Option<u64>,
    /// Since anyone was there or anything happened; null while attached.
    idle_seconds: Option<u64>,
    started_seconds: u64,
    /// Every agent's spend added up, agents closed since the server started too.
    totals: JsonSpend,
    /// Where model prices come from.
    prices: JsonPrices,
    agents: Vec<JsonAgent<'a>>,
    /// The restore offer nobody has answered yet: what this session would
    /// bring back, and so what it still uses.
    pending_restore: Vec<JsonPending<'a>>,
}

#[derive(Serialize)]
struct JsonPending<'a> {
    name: &'a str,
    cwd: String,
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
            live_cwd: a.live_cwd().display().to_string(),
            branch: a.branch.as_deref(),
            diff: a.diff.map(|(files, added, removed)| JsonDiff { files, added, removed }),
            last_prompt: a.last_prompt.as_deref(),
            exit_code: a.exit_code,
            managed_by: a.managed_by.as_deref(),
            context: a.context.map(|c| JsonContext { used: c.used, window: c.window, left: c.left(), used_percent: c.used_percent(), window_exact: c.exact }),
            spend: (crate::usage::total(&a.spend).total() > 0).then(|| JsonSpend::new(app, &a.spend, app.config.cost.agent_budget)),
        })
        .collect();
    let totals = JsonSpend::new(app, &app.totals(), app.config.cost.budget);
    let prices = JsonPrices::new(app);
    let pending_restore = app.pending_restore.iter().flat_map(|p| &p.agents).map(|a| JsonPending { name: &a.name, cwd: a.cwd.display().to_string() }).collect();
    let now = std::time::SystemTime::now();
    let (idle, detached) = app.left_alone(now);
    let list = JsonList {
        session: session::name(),
        viewers: app.viewers,
        attached: app.controlled(),
        detached_seconds: detached.map(|d| d.as_secs()),
        idle_seconds: idle.map(|d| d.as_secs()),
        started_seconds: app.clock.started_for(now).as_secs(),
        totals,
        prices,
        agents,
        pending_restore,
    };
    let mut s = serde_json::to_string_pretty(&list).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_agent;

    #[test]
    fn ls_json_has_spend_and_totals() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        app.config.cost.budget = Some(5.0);
        for (id, model) in [(1, "claude-opus-5-5"), (2, "gpt-6.1-sol"), (3, "")] {
            let mut a = test_agent(Kind::Claude);
            a.id = id;
            if !model.is_empty() {
                let b = crate::usage::Bucket { model: model.into(), fast: false, prompt: 0 };
                a.spend.insert(b, crate::usage::Tokens { input: 1_000_000, cache_read: 500_000, cache_write_1h: 10_000, output: 100_000, ..Default::default() });
            }
            app.agents.push(a);
        }
        let v: serde_json::Value = serde_json::from_str(&listing_json(&app, None)).unwrap();
        let s = &v["agents"][0]["spend"];
        // 4 + 0.1 + 0.08 + 2.
        assert_eq!(s["cost_usd"], 6.18);
        assert_eq!((s["input_tokens"].as_u64(), s["cache_read_tokens"].as_u64(), s["cache_write_tokens"].as_u64(), s["output_tokens"].as_u64()), (Some(1_000_000), Some(500_000), Some(10_000), Some(100_000)));
        assert_eq!((s["total_tokens"].as_u64(), s["cost_complete"].as_bool(), s["budget_usd"].is_null()), (Some(1_610_000), Some(true), true));
        assert_eq!(v["agents"][1]["spend"]["cost_complete"], false, "no price for its model");
        assert!(v["agents"][2]["spend"].is_null(), "nothing spent yet");
        assert_eq!((v["attached"].as_bool(), v["detached_seconds"].is_null()), (Some(false), true), "nobody ever attached");
        assert!(v["idle_seconds"].as_u64().is_some() && v["started_seconds"].as_u64().is_some());
        assert_eq!(v["pending_restore"], serde_json::json!([]));
        let t = &v["totals"];
        assert_eq!((t["total_tokens"].as_u64(), t["cost_usd"].as_f64()), (Some(3_220_000), Some(6.18)));
        assert_eq!((t["cost_complete"].as_bool(), t["budget_usd"].as_f64(), t["over_budget"].as_bool()), (Some(false), Some(5.0), Some(true)));
        let p = &v["prices"];
        assert_eq!((p["source"].as_str(), p["fetching"].as_bool(), p["fetched_at"].is_null()), (Some("built-in"), Some(true), true), "nothing fetched yet");
        // A list arrives: gpt-6.1-sol gets its price, and the source says so.
        let sol = crate::cost::Tiered { base: crate::cost::Price::openai(2.0, 10.0, 0.1), above: vec![] };
        let fetched = crate::prices::now() - 3600;
        let live = crate::prices::Live::from_models("https://example.com/p.json", fetched, [("gpt-6.1-sol".to_string(), sol)]);
        app.handle(crate::app::AppEvent::Prices(crate::prices::Feed { live: Some(std::sync::Arc::new(live)), error: Some("curl: (6) Could not resolve host".into()) }));
        let v: serde_json::Value = serde_json::from_str(&listing_json(&app, None)).unwrap();
        let p = &v["prices"];
        assert_eq!((p["source"].as_str(), p["url"].as_str(), p["fetched_at"].as_u64(), p["models"].as_u64()), (Some("live"), Some("https://example.com/p.json"), Some(fetched), Some(1)));
        assert!(p["age_seconds"].as_u64().is_some_and(|a| (3600..3700).contains(&a)), "{p}");
        assert_eq!(p["last_error"], "curl: (6) Could not resolve host", "the last good list stays in use");
        // 2 + 0.05 + 0.02 + 1.
        assert_eq!((v["agents"][1]["spend"]["cost_usd"].as_f64(), v["agents"][1]["spend"]["cost_complete"].as_bool()), (Some(3.07), Some(true)));
        // Turned off: trix's own prices only.
        app.config.cost.live_prices = false;
        let v: serde_json::Value = serde_json::from_str(&listing_json(&app, None)).unwrap();
        assert_eq!((v["prices"]["source"].as_str(), v["prices"]["fetching"].as_bool(), v["prices"]["last_error"].is_null()), (Some("built-in"), Some(false), true));
        assert_eq!(v["agents"][1]["spend"]["cost_complete"], false);
    }

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }
    fn p(s: &[&str]) -> Result<Request, String> {
        parse(&args(s), PathBuf::from("/work"))
    }

    #[test]
    fn what_a_sandbox_may_ask_for() {
        let open = |a: &[&str]| outside_the_sandbox(&p(a).unwrap()).is_none();
        assert!(outside_the_sandbox(&Request::List { json: true }).is_none());
        for ok in [&["send", "web", "hi"][..], &["read", "web"], &["wait", "web"], &["events"], &["args", "web"], &["close", "web"],
                   &["spawn", "claude"], &["spawn", "codex", "--prompt", "fix it", "--worktree"], &["spawn", "claude", "--args", " "]] {
            assert!(open(ok), "{ok:?} stays open");
        }
        for no in [&["send", "-f", "web", "y"][..], &["spawn", "shell"], &["spawn", "sh"], &["spawn", "custom", "--args", "id"],
                   &["spawn", "claude", "--args", "--dangerously-skip-permissions"], &["close", "--force", "web"],
                   &["args", "web", "--", "--yolo"], &["args", "web", "--clear"]] {
            assert!(!open(no), "{no:?} is refused");
        }
        assert!(outside_the_sandbox(&Request::Open { name: "work".into(), agents: vec![], resume: false }).is_some());
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
    fn a_spawn_into_an_untrusted_repo_warns_and_runs_nothing() {
        let dir = std::env::temp_dir().join(format!("trix-control-untrusted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&dir).args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join(".env"), "A=1\n").unwrap();
        std::fs::write(dir.join(".trix.toml"), "[worktree]\nsetup = 'touch ran'\ncopy = ['.env']\n").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let r = spawn(&mut app, None, "custom", Some("w".into()), dir.clone(), Some("exec sleep 60".into()), None, true);
        assert!(r.err.contains("isn't trusted"), "{}", r.err);
        assert!(app.agents[0].installing.is_none(), "no setup is held in front of it");
        assert!(!dir.join(".trix/worktrees/w/.env").exists());
        app.agents[0].shutdown();
    }

    #[test]
    fn rejects_bad_commands() {
        assert!(p(&["send", "web", "--force-it", "x"]).unwrap_err().contains("unknown option"));
        assert!(p(&["read"]).unwrap_err().contains("needs"));
        for bad in [&["close", "web", "--force=false"][..], &["wait", "a", "--all=0"], &["spawn", "shell", "--worktree=no"]] {
            assert!(p(bad).unwrap_err().contains("takes no value"), "{bad:?}");
        }
        assert!(p(&["wait", "a", "--timeout=5"]).is_ok(), "value flags keep their =value");
        assert!(p(&["read", "a", "b"]).unwrap_err().contains("one"));
        assert!(p(&["read", "-n", "lots", "a"]).unwrap_err().contains("not a number"));
        assert!(p(&["read", "a", "-n"]).unwrap_err().contains("needs a value"));
        assert!(p(&["wait"]).is_err());
        assert!(p(&["wait", "--all", "a"]).is_ok());
        assert!(p(&["close", "--all"]).is_err());
        assert!(p(&["send", "web", "   "]).unwrap_err().contains("empty"));
    }

    #[test]
    fn parses_args_with_their_flags() {
        let args = |t: &str, set: Option<&str>, clear| Ok(Request::Args { target: t.into(), set: set.map(String::from), clear });
        assert_eq!(p(&["args", "web"]), args("web", None, false), "no arguments: show them");
        assert_eq!(p(&["args", "web", "--clear"]), args("web", None, true));
        assert_eq!(p(&["args", "web", "--model claude-opus-5-5 --effort xhigh"]), args("web", Some("--model claude-opus-5-5 --effort xhigh"), false), "one word is taken as written");
        assert_eq!(p(&["args", "alpha dog", "--model", "x"]), args("alpha dog", Some("--model x"), false), "flags are the arguments, not trix's options");
        assert_eq!(p(&["args", "web", "--", "--allow-tool", "shell(trix:*)"]), args("web", Some("--allow-tool 'shell(trix:*)'"), false), "quoted back together");
        assert_eq!(p(&["args", "web", "--", "--clear"]), args("web", Some("--clear"), false), "after --, --clear is an argument");
        assert!(p(&["args"]).unwrap_err().contains("needs an agent"));
        assert!(p(&["args", "web", "  "]).unwrap_err().contains("empty"));
        assert!(p(&["args", "web", "--append-system-prompt \"unclosed"]).unwrap_err().contains("can't split"));
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
        a.set_state(State::Idle, true);
        assert!(!settled(&a, now), "idle (SessionStart) is not the turn");
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
