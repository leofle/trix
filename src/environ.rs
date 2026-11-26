//! The environment a new server starts with.
//!
//! A server started from inside a pane (`trix -s work layout open …`, as a
//! lead agent does) would otherwise run with whatever the command that
//! started it had: the pane's agent's environment, which is the pane's plus
//! what that agent set for the commands it runs, less what it took away.
//! Every agent trix runs changes it, and a new server hands it on to every
//! agent it starts:
//!
//! - its own session, thread and tool call ids, tokens and sockets
//!   (Claude's `CLAUDE_CODE_CHILD_SESSION` turns off saving the transcript,
//!   so a restore can't resume the agent);
//! - settings for its own commands: `NO_COLOR` (Codex, amp, Cursor),
//!   `LC_ALL` and pagers (Codex, Gemini), `NONINTERACTIVE` (amp), git's
//!   askpass and config (Copilot, Cursor), its sandbox's proxies (Muse);
//! - and what it removed: Antigravity drops `SSH_AUTH_SOCK`.
//!
//! So a server started under an agent undoes what that agent did: the
//! agent's program started with the pane's environment, and the difference
//! between that and what reached the first process below it that isn't the
//! agent's own (its shell, a tool's server) is what the agent changed. An
//! agent may be several processes (Codex's npm wrapper and its binary,
//! opencode's `run`, `serve` and `opencode-pty`): they're told by name. Whatever the agent set there
//! and is still unchanged goes; what it changed gets the pane's value back;
//! what it took away comes back. Changes made further down are kept, since
//! someone meant them: a script's or a tool's own `HOME`, `XDG_RUNTIME_DIR`
//! and `PATH` (a sandbox's), an agent's `FOO=1 trix …`. Only when the
//! command was typed into a shell pane (nothing but shells between it and
//! its holder) is its environment kept whole. Either way, what trix set for
//! the pane and what an agent is known to set for its commands are left out
//! (`AGENT_SET`, `PANE_SET`): that covers environments that can't be read
//! (an agent's sandbox may not let trix look), and a session that an older
//! trix started from an agent. Names an
//! agent shares with people (`AGENT`) go by that list only when nothing
//! could be read. This replaces #113's list of Claude's variables (Yoav),
//! which is part of `AGENT_SET` now.

use std::ffi::OsString;

pub type Env = Vec<(OsString, OsString)>;

/// What each agent trix runs sets for the commands it runs, and nobody else
/// would: its session, thread, request and tool call, its tokens and
/// sockets. Measured from each one's shell tool (`env` there against a
/// plain shell's). Settings a person may export (`CLAUDE_EFFORT`,
/// `CLAUDE_CODE_USE_BEDROCK`, `NO_COLOR`, `GIT_ASKPASS`, proxies…) aren't
/// here: they go only when the agent is seen setting them (`undo`).
pub const AGENT_SET: &[(&str, &[&str])] = &[
    (
        "claude",
        &[
            "CLAUDECODE",
            "CLAUDE_PID",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_EXECPATH",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ATTENDED",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_CHILD_NOTES_DIR",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_TOOL_USE_ID",
        ],
    ),
    (
        "codex",
        &[
            "CODEX_SESSION_ID",
            "CODEX_THREAD_ID",
            "CODEX_TOOL_CALL_ID",
            "CODEX_SANDBOX",
            "CODEX_SANDBOX_NETWORK_DISABLED",
            "CODEX_CI",
            "CODEX_VERSION",
            "CODEX_MANAGED_BY_NPM",
            "CODEX_MANAGED_PACKAGE_ROOT",
            "CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS",
            "NODE_REPL_TRUSTED_BROWSER_CLIENT_SHA256S",
        ],
    ),
    ("gemini", &["GEMINI_CLI"]),
    ("opencode", &["OPENCODE", "OPENCODE_SESSION_ID", "OPENCODE_TERMINAL", "AGENT"]),
    ("amp", &["AGENT", "AGENT_THREAD_ID", "AMP_CURRENT_THREAD_ID"]),
    (
        "antigravity",
        &[
            "ANTIGRAVITY_AGENT",
            "ANTIGRAVITY_AGENTAPI_EXE",
            "ANTIGRAVITY_APP_DATA_DIR",
            "ANTIGRAVITY_CONVERSATION_ID",
            "ANTIGRAVITY_CSRF_TOKEN",
            "ANTIGRAVITY_LS_ADDRESS",
            "ANTIGRAVITY_LS_VERSION",
            "ANTIGRAVITY_PROJECT_ID",
            "ANTIGRAVITY_SOURCE_METADATA",
            "ANTIGRAVITY_TRAJECTORY_ID",
        ],
    ),
    ("muse", &["MUSE_SESSION_ID", "MUSE_THREAD_ID", "MUSE_TOOL_USE_ID", "MUSE_RELEASE_INFO", "CLAUDE_CODE_TOOL_USE_ID", "TRACEPARENT"]),
    ("copilot", &["COPILOT_AGENT_SESSION_ID", "COPILOT_CLI", "COPILOT_CLI_BINARY_VERSION", "COPILOT_CLI_PATH", "COPILOT_CLI_RESOLVED_DIST_DIR"]),
    (
        "cursor",
        &[
            "CURSOR_AGENT",
            "CURSOR_CONVERSATION_ID",
            "CURSOR_REQUEST_ID",
            "CURSOR_INVOKED_AS",
            "CURSOR_RIPGREP_PATH",
            "AGENT_TRANSCRIPTS",
            "GLOBAL_AGENT_ENVIRONMENT_VARIABLE_NAMESPACE",
            "__CURSOR_SANDBOX_ENV_RESTORE",
        ],
    ),
];

/// Names in `AGENT_SET` that aren't any one agent's, and a person might set
/// too: `AGENT` (opencode sets `AGENT=1`, amp sets it as well). They go when
/// the agent is seen setting them, and by name only when nothing could be
/// read.
const SHARED: &[&str] = &["AGENT", "TRACEPARENT"];

/// What trix gives a pane of its own (agent.rs): that pane's socket, id,
/// name, root and ports. A new server's panes get their own.
pub const PANE_SET: &[&str] =
    &["TRIX", "TRIX_SOCKET", "TRIX_AGENT_ID", "TRIX_AGENT_NAME", "TRIX_SESSION", "TRIX_ROOT", "TRIX_PORT", "TRIX_PORT_COUNT", "TRIX_PORT_END", "TRIX_CODEX_NOTIFY"];

/// The programs that only run what a person types: with nothing else between
/// a command and its holder, the command was typed into a shell pane.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh"];

/// Where a new server's environment comes from.
#[derive(Debug, PartialEq)]
enum Source {
    /// The trix holder or server at this place among the forebears.
    Host(usize),
    /// This process's own: it isn't under trix, or a person typed it.
    Own,
}

/// Runtimes an agent's program may run under: its name is then the script's.
const RUNTIMES: &[&str] = &["node", "bun", "deno", "python", "python3", "ruby"];

/// A program's name, for telling an agent's own processes apart: `codex`
/// for `/…/bin/codex` and for `node /…/bin/codex.js` alike.
fn program(args: &[String]) -> Option<String> {
    let base = |a: &str| std::path::Path::new(a.trim_start_matches('-')).file_stem().map(|n| n.to_string_lossy().into_owned());
    let first = base(args.first()?)?;
    if RUNTIMES.contains(&first.as_str()) {
        return args[1..].iter().find(|a| !a.starts_with('-')).and_then(|a| base(a)).or(Some(first));
    }
    Some(first)
}

/// Where the agent's own processes end, going down from its program at
/// `top`: the first process below that isn't the agent's, by its name
/// (Codex's npm wrapper and its binary, opencode's `run`, `serve` and
/// `opencode-pty`, Gemini starting itself again). That one got what the
/// agent gave its commands. A tool the agent started (another program)
/// ends it there, so what the tool set for its own commands is kept.
fn below_agent(forebears: &[Option<Vec<String>>], top: usize) -> usize {
    let Some(agent) = forebears[top].as_deref().and_then(program) else { return top - 1 };
    let mut i = top - 1;
    while i > 0 && forebears[i].as_deref().and_then(program).is_some_and(|p| p.starts_with(&agent)) {
        i -= 1;
    }
    i
}

fn is_shell(args: &[String]) -> bool {
    let name = args.first().and_then(|a| std::path::Path::new(a.trim_start_matches('-')).file_name().map(|n| n.to_string_lossy().into_owned()));
    name.is_some_and(|n| SHELLS.contains(&n.as_str()))
}

/// `forebears` are the arguments of this process and its parents in turn
/// (`None` where they can't be read): this one first.
fn source(forebears: &[Option<Vec<String>>]) -> Source {
    let host = forebears.iter().position(|a| a.as_deref().is_some_and(crate::control::trix_host));
    match host {
        None => Source::Own,
        // Itself a holder or server (h is 0), or under one by way of shells only.
        Some(h) if forebears.get(1..h).unwrap_or_default().iter().all(|a| a.as_deref().is_some_and(is_shell)) => Source::Own,
        Some(h) => Source::Host(h),
    }
}

/// `env` without what trix set for a pane and what an agent set for its
/// commands. `seen`: what the agent did was undone already, so the names
/// agents share with people (`SHARED`) left are yours and stay.
fn clean(env: Env, seen: bool) -> Env {
    let agents = |k: &str| AGENT_SET.iter().any(|(_, vars)| vars.contains(&k)) && !(seen && SHARED.contains(&k));
    let drop = |k: &OsString| k.to_str().is_some_and(|k| PANE_SET.contains(&k) || agents(k));
    env.into_iter().filter(|(k, _)| !drop(k)).collect()
}

/// This process and its parents, then its session's leader and that one's
/// parents, if the first walk didn't pass it (a command whose parent left).
fn forebears() -> Vec<u32> {
    let chain = |from: u32| {
        let mut out = vec![from];
        while out.len() < 64 {
            match out.last().and_then(|&p| crate::platform::parent_of(p)).filter(|&p| p > 1) {
                Some(p) => out.push(p),
                None => break,
            }
        }
        out
    };
    let mut out = chain(std::process::id());
    if let Some(leader) = crate::platform::session_of(std::process::id()).filter(|&s| s > 1 && !out.contains(&s)) {
        out.extend(chain(leader));
    }
    out
}

/// The environment a server started from here should run with.
pub fn for_new_server() -> Env {
    let pids = forebears();
    let args: Vec<Option<Vec<String>>> = pids.iter().map(|&p| crate::platform::args_of(p)).collect();
    // macOS keeps its own programs' environment (`/bin/zsh`, `/bin/sh`) to
    // themselves: an empty one is one we couldn't read.
    resolve(&args, || std::env::vars_os().collect(), |i| crate::platform::env_of(pids[i]).filter(|e| !e.is_empty()))
}

/// `own` is this process's environment, `env_at` reads the one the forebear
/// at that place started with.
fn resolve(forebears: &[Option<Vec<String>>], own: impl FnOnce() -> Env, env_at: impl Fn(usize) -> Option<Env>) -> Env {
    match source(forebears) {
        // The agent's program is right under its host, and what it gave
        // its commands is in the first process below it that isn't its own
        // (`source` leaves at least one between).
        Source::Host(h) if h >= 2 => match (env_at(h - 1), gave(forebears, below_agent(forebears, h - 1), &env_at)) {
            (Some(started), Some((gave, meant))) => clean(undo(own(), &started, &gave, &meant), true),
            // Nothing to compare with: what's here is the agent's command's.
            _ => clean(own(), false),
        },
        Source::Host(_) => clean(own(), false),
        // Yours: a shell pane's, or a terminal's outside trix.
        Source::Own => clean(own(), true),
    }
}

/// What the agent gave its commands, from the process at `from` or, when
/// that one can't be read (a shell of macOS's own), the next one below that
/// can, with the names set on the way, in the unread processes' command
/// lines (`FOO=1 trix …`, `export FOO=…`): someone meant those.
fn gave(forebears: &[Option<Vec<String>>], from: usize, env_at: &impl Fn(usize) -> Option<Env>) -> Option<(Env, Vec<String>)> {
    let mut meant = Vec::new();
    for i in (0..=from).rev() {
        if let Some(env) = env_at(i) {
            return Some((env, meant));
        }
        meant.extend(forebears[i].iter().flatten().flat_map(|a| assigned(a)));
    }
    None
}

/// The names a shell command line assigns: `FOO=1 cmd`, `export FOO=1`,
/// `a; BAR=2 b`.
fn assigned(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || ";&|()'\"".contains(c))
        .filter_map(|w| w.split_once('=').map(|(name, _)| name))
        .filter(|n| n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(String::from)
        .collect()
}

/// `env` with what an agent did undone: `started` is what its program
/// started with, `gave` what it gave the first process that isn't its own. A variable it set
/// or changed goes, or gets `started`'s value back, unless something since
/// changed it again; one it took away comes back unless it was set since.
/// `meant`: names set on purpose since, left as they are.
fn undo(env: Env, started: &Env, gave: &Env, meant: &[String]) -> Env {
    let get = |e: &Env, k: &OsString| e.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let mut out: Env = env
        .into_iter()
        .filter_map(|(k, v)| {
            let agents = get(gave, &k).filter(|g| get(started, &k).as_ref() != Some(g) && !meant.iter().any(|m| k == m.as_str()));
            match agents {
                Some(g) if g == v => get(started, &k).map(|s| (k, s)),
                _ => Some((k, v)),
            }
        })
        .collect();
    for (k, v) in started {
        if get(gave, k).is_none() && get(&out, k).is_none() {
            out.push((k.clone(), v.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(args: &[&str]) -> Option<Vec<String>> {
        Some(args.iter().map(|s| s.to_string()).collect())
    }

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
    }

    fn names(env: &Env) -> Vec<String> {
        let mut n: Vec<String> = env.iter().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        n.sort();
        n
    }

    /// What a session was started with.
    const SESSION: &[(&str, &str)] = &[
        ("HOME", "/Users/you"),
        ("PATH", "/opt/homebrew/bin:/usr/bin:/bin"),
        ("SSH_AUTH_SOCK", "/private/tmp/launchd/Listeners"),
        ("LANG", "en_US.UTF-8"),
        ("CLAUDE_EFFORT", "high"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ("ANTHROPIC_API_KEY", "yours"),
        ("TRIX_ALLOW_NESTED", "1"),
    ];

    /// What each agent did to the environment of the commands it runs, as
    /// measured: (agent, added, removed). The agent-specific names are in
    /// `AGENT_SET`; the rest are settings a person might export too.
    const MEASURED: &[(&str, &[&str], &[&str])] = &[
        ("claude", &["CLAUDECODE", "CLAUDE_PID", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_CHILD_NOTES_DIR", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN"], &[]),
        ("codex", &["CODEX_SESSION_ID", "CODEX_THREAD_ID", "CODEX_TOOL_CALL_ID", "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED", "CODEX_CI", "CODEX_VERSION", "CODEX_MANAGED_BY_NPM", "CODEX_MANAGED_PACKAGE_ROOT", "CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS", "NODE_REPL_TRUSTED_BROWSER_CLIENT_SHA256S", "NO_COLOR", "LC_ALL", "LC_CTYPE", "GIT_PAGER", "GH_PAGER"], &[]),
        ("gemini", &["GEMINI_CLI", "PAGER", "GIT_PAGER"], &[]),
        ("opencode", &["OPENCODE", "OPENCODE_SESSION_ID", "OPENCODE_TERMINAL", "AGENT", "NO_PROXY", "no_proxy"], &[]),
        ("amp", &["AGENT", "AGENT_THREAD_ID", "AMP_CURRENT_THREAD_ID", "NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "NONINTERACTIVE", "DEBIAN_FRONTEND"], &[]),
        ("antigravity", &["ANTIGRAVITY_AGENT", "ANTIGRAVITY_AGENTAPI_EXE", "ANTIGRAVITY_APP_DATA_DIR", "ANTIGRAVITY_CONVERSATION_ID", "ANTIGRAVITY_CSRF_TOKEN", "ANTIGRAVITY_LS_ADDRESS", "ANTIGRAVITY_LS_VERSION", "ANTIGRAVITY_PROJECT_ID", "ANTIGRAVITY_SOURCE_METADATA", "ANTIGRAVITY_TRAJECTORY_ID", "GIT_PAGER", "JJ_PAGER", "JJ_EDITOR", "HGEDITOR"], &["SSH_AUTH_SOCK"]),
        ("muse", &["MUSE_SESSION_ID", "MUSE_THREAD_ID", "MUSE_TOOL_USE_ID", "MUSE_RELEASE_INFO", "CLAUDE_CODE_TOOL_USE_ID", "TRACEPARENT", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "FTP_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "all_proxy", "ftp_proxy", "no_proxy", "NODE_USE_ENV_PROXY"], &[]),
        ("copilot", &["COPILOT_AGENT_SESSION_ID", "COPILOT_CLI", "COPILOT_CLI_BINARY_VERSION", "COPILOT_CLI_PATH", "COPILOT_CLI_RESOLVED_DIST_DIR", "GCM_INTERACTIVE", "GIT_ASKPASS", "SSH_ASKPASS", "GIT_TERMINAL_PROMPT", "GIT_OPTIONAL_LOCKS", "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"], &[]),
        ("cursor", &["CURSOR_AGENT", "CURSOR_CONVERSATION_ID", "CURSOR_REQUEST_ID", "CURSOR_INVOKED_AS", "CURSOR_RIPGREP_PATH", "AGENT_TRANSCRIPTS", "GLOBAL_AGENT_ENVIRONMENT_VARIABLE_NAMESPACE", "__CURSOR_SANDBOX_ENV_RESTORE", "NODE_DISABLE_COMPILE_CACHE", "_ZO_DOCTOR", "NO_COLOR", "FORCE_COLOR", "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"], &[]),
    ];

    /// The pane's environment: the session's plus what trix gave the pane.
    fn pane() -> Env {
        let mut e = env(SESSION);
        e.extend(env(&[("TRIX", "1"), ("TRIX_SOCKET", "/tmp/trix.sock"), ("TRIX_AGENT_ID", "3"), ("TRIX_AGENT_NAME", "alpha dog"), ("TRIX_SESSION", "default"), ("TRIX_PORT", "4100"), ("TERM", "xterm-256color")]));
        e
    }

    /// What an agent's command sees: the pane's, changed as `MEASURED` says.
    fn under(agent: &str) -> Env {
        let (_, added, removed) = MEASURED.iter().find(|(a, _, _)| *a == agent).unwrap();
        let mut e: Env = pane().into_iter().filter(|(k, _)| !removed.iter().any(|r| k == *r)).collect();
        e.extend(added.iter().map(|k| (OsString::from(k), OsString::from("x"))));
        e
    }

    #[test]
    fn every_agent_trix_runs_is_measured_and_listed() {
        for kind in ["claude", "codex", "gemini", "opencode", "amp", "antigravity", "muse", "copilot", "cursor"] {
            assert!(MEASURED.iter().any(|(a, _, _)| *a == kind), "{kind} measured");
            assert!(AGENT_SET.iter().any(|(a, _)| *a == kind), "{kind} listed");
        }
    }

    #[test]
    fn under_an_agent_it_finds_the_agent_under_its_holder() {
        let claude = [a(&["trix", "-s", "work", "layout", "open"]), a(&["/bin/zsh", "-c", "trix -s work layout open"]), a(&["claude"]), a(&["/opt/homebrew/bin/trix", "holder"]), a(&["/sbin/launchd"])];
        assert_eq!(source(&claude), Source::Host(3));
        // Under the server, when the pane's holder couldn't start.
        let unheld = [a(&["trix", "-s", "work"]), a(&["node", "/usr/local/bin/codex"]), a(&["trix", "-s", "default", "server"])];
        assert_eq!(source(&unheld), Source::Host(2));
        // A command whose arguments can't be read isn't taken for a shell.
        assert_eq!(source(&[a(&["trix"]), None, a(&["trix", "holder"])]), Source::Host(2));
    }

    #[test]
    fn typed_into_a_shell_pane_or_outside_trix_it_keeps_its_own() {
        let typed = [a(&["trix", "-s", "work"]), a(&["-zsh"]), a(&["/opt/homebrew/bin/trix", "holder"])];
        assert_eq!(source(&typed), Source::Own);
        let script = [a(&["trix", "-s", "work"]), a(&["/bin/bash", "./start.sh"]), a(&["/bin/zsh", "-l"]), a(&["trix", "holder"])];
        assert_eq!(source(&script), Source::Own);
        let outside = [a(&["trix", "-s", "work"]), a(&["-zsh"]), a(&["/Applications/Ghostty.app/Contents/MacOS/ghostty"])];
        assert_eq!(source(&outside), Source::Own);
        // A server or holder of trix's own, and a case runner straight under its holder.
        assert_eq!(source(&[a(&["trix", "-s", "triage-1", "server"]), a(&["trix", "case", "run"])]), Source::Own);
        assert_eq!(source(&[a(&["trix", "case", "run", "triage-1"]), a(&["trix", "holder"])]), Source::Own);
    }

    /// An agent's command, as its forebears are: trix, the agent's shell,
    /// the agent, the holder.
    fn run_by(agent: &str) -> Vec<Option<Vec<String>>> {
        vec![a(&["trix", "-s", "work", "layout", "open"]), a(&["/bin/zsh", "-c", "trix -s work layout open"]), a(&[agent]), a(&["/opt/homebrew/bin/trix", "holder"])]
    }

    /// `env_at` for `run_by`: the agent started with the pane's, and gave its
    /// shell `gave`.
    fn seen(gave: Env) -> impl Fn(usize) -> Option<Env> {
        move |i| match i {
            2 => Some(pane()),
            1 => Some(gave.clone()),
            _ => None,
        }
    }

    #[test]
    fn under_every_agent_the_server_gets_the_panes_environment_back() {
        for (agent, added, removed) in MEASURED {
            let got = resolve(&run_by(agent), || under(agent), seen(under(agent)));
            assert_eq!(got.iter().filter(|(k, _)| k != "TERM").count(), env(SESSION).len(), "{agent}");
            assert_eq!(names(&got), names(&clean(pane(), true)), "{agent} added {added:?}, removed {removed:?}");
            // With the values the pane had: SSH_AUTH_SOCK, Antigravity took.
            assert_eq!(got.iter().find(|(k, _)| k == "SSH_AUTH_SOCK").map(|(_, v)| v.clone()), Some("/private/tmp/launchd/Listeners".into()), "{agent}");
        }
    }

    #[test]
    fn an_agents_own_processes_are_told_by_name() {
        assert_eq!(program(&["/usr/local/bin/codex".into()]).as_deref(), Some("codex"));
        assert_eq!(program(&["node".into(), "--no-warnings".into(), "/x/@openai/codex/bin/codex.js".into()]).as_deref(), Some("codex"));
        assert_eq!(program(&["-zsh".into()]).as_deref(), Some("zsh"));
        assert_eq!(program(&["node".into()]).as_deref(), Some("node"));
        // Codex on an older Node: its wrapper, then its binary, then the shell.
        let codex = [a(&["trix", "-s", "work"]), a(&["/bin/zsh", "-lc", "trix -s work"]), a(&["/x/vendor/aarch64-apple-darwin/codex/codex", "exec"]), a(&["node", "/x/bin/codex.js"]), a(&["trix", "holder"])];
        assert_eq!(below_agent(&codex, 3), 1);
        // opencode: run, serve, its pty daemon, then the shell.
        let opencode = [a(&["trix"]), a(&["sh", "-c", "trix"]), a(&["/x/opencode-pty", "daemon"]), a(&["/x/opencode", "serve"]), a(&["opencode"]), a(&["trix", "holder"])];
        assert_eq!(below_agent(&opencode, 4), 1);
        // A tool's server under the agent ends the agent there.
        let probe = [a(&["trix"]), a(&["python3", "-m", "probe"]), a(&["claude"]), a(&["trix", "holder"])];
        assert_eq!(below_agent(&probe, 2), 1);
        // An agent that runs trix itself, no shell between.
        assert_eq!(below_agent(&[a(&["trix"]), a(&["claude"]), a(&["trix", "holder"])], 1), 0);
        // Arguments it can't read: just its first process.
        assert_eq!(below_agent(&[a(&["trix"]), a(&["zsh"]), None, a(&["trix", "holder"])], 2), 1);
    }

    #[test]
    fn what_a_later_process_of_the_agent_set_goes_too() {
        // Codex's binary, under its wrapper, sets NO_COLOR and its pagers:
        // the wrapper's environment has none of it, the shell's has.
        let codex = vec![a(&["trix", "-s", "work"]), a(&["/bin/zsh", "-lc", "trix -s work"]), a(&["/x/codex"]), a(&["node", "/x/bin/codex.js"]), a(&["trix", "holder"])];
        let mut wrapper = pane();
        wrapper.push(("CODEX_MANAGED_BY_NPM".into(), "1".into()));
        let shell = under("codex");
        let got = resolve(&codex, || shell.clone(), |i| match i {
            3 => Some(pane()),
            2 => Some(wrapper.clone()),
            1 => Some(shell.clone()),
            _ => None,
        });
        assert_eq!(names(&got), names(&clean(pane(), true)));
        assert!(!got.iter().any(|(k, _)| k == "NO_COLOR" || k == "LC_ALL" || k == "GIT_PAGER"));
    }

    #[test]
    fn past_a_shell_that_cant_be_read_what_it_was_told_to_set_stays() {
        assert_eq!(assigned("XDG_CONFIG_HOME=/c trix -s x; export FOO=1 && (BAR=2 b) 'Q=3' --flag=x a=b=c"), ["XDG_CONFIG_HOME", "FOO", "BAR", "Q", "a"]);
        // Claude runs its command through /bin/zsh, which macOS won't let
        // trix read: what reached trix itself stands in for it.
        let mut own = under("claude");
        own.push(("NO_COLOR".into(), "1".into()));
        own.push(("XDG_CONFIG_HOME".into(), "/c".into()));
        let forebears = vec![a(&["trix", "-s", "x"]), a(&["/bin/zsh", "-c", "XDG_CONFIG_HOME=/c trix -s x"]), a(&["claude"]), a(&["trix", "holder"])];
        let got = resolve(&forebears, || own.clone(), |i| match i {
            2 => Some(pane()),
            0 => Some(own.clone()),
            _ => None,
        });
        let has = |k: &str| got.iter().any(|(n, _)| n == k);
        assert!(has("XDG_CONFIG_HOME"), "set on the command line: meant");
        assert!(!has("NO_COLOR") && !has("CLAUDE_CODE_CHILD_SESSION"), "the agent's");
        assert!(has("SSH_AUTH_SOCK") && has("HOME"));
    }

    #[test]
    fn what_an_agent_changed_gets_its_value_back() {
        let mut gave = pane();
        for (k, v) in gave.iter_mut() {
            if k == "TERM" {
                *v = "dumb".into();
            }
        }
        let got = resolve(&run_by("gemini"), || gave.clone(), seen(gave.clone()));
        assert_eq!(got.iter().find(|(k, _)| k == "TERM").map(|(_, v)| v.clone()), Some("xterm-256color".into()));
    }

    #[test]
    fn what_was_changed_after_the_agent_stays() {
        // A tool's sandbox (or a script, or `FOO=1 trix`) under the agent:
        // its own HOME, runtime folder and PATH, and the agent's ids, which
        // it passed on untouched.
        let mut own = under("claude");
        own.retain(|(k, _)| k != "HOME" && k != "PATH");
        own.extend(env(&[("HOME", "/tmp/sandbox/home"), ("PATH", "/tmp/sandbox/bin:/usr/bin"), ("XDG_RUNTIME_DIR", "/tmp/sandbox/run"), ("CLAUDE_EFFORT", "low")]));
        own.retain(|(k, v)| !(k == "CLAUDE_EFFORT" && v == "high"));
        let got = resolve(&run_by("claude"), || own.clone(), seen(under("claude")));
        let val = |k: &str| got.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_string_lossy().into_owned());
        assert_eq!(val("HOME").as_deref(), Some("/tmp/sandbox/home"));
        assert_eq!(val("PATH").as_deref(), Some("/tmp/sandbox/bin:/usr/bin"));
        assert_eq!(val("XDG_RUNTIME_DIR").as_deref(), Some("/tmp/sandbox/run"));
        assert_eq!(val("CLAUDE_EFFORT").as_deref(), Some("low"));
        assert_eq!(val("CLAUDE_CODE_CHILD_SESSION"), None);
        // The tool's own server sits between the agent and the command: its
        // first process is that one, not a shell.
        let probe = vec![a(&["trix"]), a(&["python3", "-m", "probe"]), a(&["claude"]), a(&["trix", "holder"])];
        let got = resolve(&probe, || own.clone(), seen(under("claude")));
        assert!(got.iter().any(|(k, v)| k == "HOME" && v == "/tmp/sandbox/home"));
    }

    #[test]
    fn when_nothing_can_be_read_what_the_agent_set_still_goes() {
        // Processes that aren't ours to read, or a session an older trix
        // started from an agent: what that agent set for its commands goes,
        // your settings stay.
        for (agent, _, _) in MEASURED {
            let got = names(&resolve(&run_by(agent), || under(agent), |_| None));
            for (_, vars) in AGENT_SET {
                for v in *vars {
                    assert!(!got.iter().any(|g| g == v), "{agent}: {v} passed on");
                }
            }
            for keep in ["HOME", "PATH", "CLAUDE_EFFORT", "CLAUDE_CODE_USE_BEDROCK", "ANTHROPIC_API_KEY", "TRIX_ALLOW_NESTED"] {
                assert!(got.iter().any(|g| g == keep), "{agent}: {keep} dropped");
            }
        }
    }

    #[test]
    fn typed_into_a_shell_pane_what_you_exported_stays() {
        let typed = [a(&["trix", "-s", "work"]), a(&["-zsh"]), a(&["trix", "holder"])];
        let mut own = pane();
        own.push((OsString::from("MY_SETTING"), OsString::from("1")));
        let got = names(&resolve(&typed, || own, |_| panic!("not the holder's")));
        assert!(got.iter().any(|g| g == "MY_SETTING"));
        assert!(!got.iter().any(|g| g == "TRIX_AGENT_ID"));
    }

    /// #113's: Claude's session goes, your settings for it stay.
    #[test]
    fn a_server_started_in_an_agent_drops_that_agents_session_not_your_settings() {
        let all = env(&[("PATH", "/bin"), ("CLAUDECODE", "1"), ("CLAUDE_CODE_CHILD_SESSION", "1"), ("CLAUDE_CODE_MESSAGING_TOKEN", "t"), ("CLAUDE_CODE_SESSION_ID", "s"), ("CLAUDE_CODE_USE_BEDROCK", "1"), ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "9"), ("ANTHROPIC_API_KEY", "k")]);
        for from_holder in [true, false] {
            assert_eq!(names(&clean(all.clone(), from_holder)), ["ANTHROPIC_API_KEY", "CLAUDE_CODE_MAX_OUTPUT_TOKENS", "CLAUDE_CODE_USE_BEDROCK", "PATH"]);
        }
        assert_eq!(names(&clean(env(&[("PATH", "/bin"), ("HOME", "/h")]), false)), ["HOME", "PATH"]);
    }

    #[test]
    fn an_agent_variable_of_yours_stays_unless_only_the_agents_command_has_it() {
        let yours = env(&[("PATH", "/bin"), ("AGENT", "mine"), ("OPENCODE_SESSION_ID", "s")]);
        // In the pane's environment, and left alone by the agent: yours.
        let started = env(&[("PATH", "/bin"), ("AGENT", "mine")]);
        let got = resolve(&run_by("opencode"), || yours.clone(), |i| [None, Some(yours.clone()), Some(started.clone())].get(i).cloned().flatten());
        assert_eq!(names(&got), ["AGENT", "PATH"]);
        // Set by the agent: its.
        let gave = env(&[("PATH", "/bin"), ("AGENT", "1")]);
        let got = resolve(&run_by("opencode"), || gave.clone(), |i| [None, Some(gave.clone()), Some(env(&[("PATH", "/bin")]))].get(i).cloned().flatten());
        assert_eq!(names(&got), ["PATH"]);
        // Typed into a shell pane, or outside trix: yours too.
        assert_eq!(names(&resolve(&[a(&["trix"]), a(&["-zsh"]), a(&["trix", "holder"])], || yours.clone(), |_| None)), ["AGENT", "PATH"]);
        // Under an agent, with nothing to read: the agent's.
        assert_eq!(names(&resolve(&run_by("opencode"), || yours.clone(), |_| None)), ["PATH"]);
    }

    #[test]
    fn what_trix_gave_the_pane_stays_with_the_pane() {
        let got = names(&clean(pane(), true));
        for v in PANE_SET {
            assert!(!got.iter().any(|g| g == v), "{v} passed on");
        }
        assert!(got.iter().any(|g| g == "TRIX_ALLOW_NESTED"), "a setting of yours");
    }

    /// The real thing, wherever the tests run (in a trix pane, or not).
    #[test]
    fn a_new_servers_environment_has_no_agents_variables() {
        let got = names(&for_new_server());
        assert!(got.iter().any(|g| g == "PATH"));
        for (_, vars) in AGENT_SET {
            for v in vars.iter().filter(|v| !SHARED.contains(v)) {
                assert!(!got.iter().any(|g| g == v), "{v} passed on");
            }
        }
    }
}
