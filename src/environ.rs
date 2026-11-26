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
//! So a server started under a trix holder or server takes that one's
//! environment instead, the one its session was started with: what the
//! agent added goes, and what it took away comes back, whichever agent it
//! is. Only when the command was typed into a shell pane (nothing but
//! shells between it and its holder) does it keep its own, since what's
//! there is what you exported. Either way, what trix set for the pane and
//! what an agent is known to set for its commands are left out
//! (`AGENT_SET`, `PANE_SET`): that covers the rest, a server whose holder
//! can't be read, and a session that an older trix started from an agent.
//! Names an agent shares with people (`AGENT`) go only in the first case.
//! This replaces #113's list of Claude's variables (Yoav), which is part of
//! `AGENT_SET` now.

use std::ffi::OsString;

pub type Env = Vec<(OsString, OsString)>;

/// What each agent trix runs sets for the commands it runs, and nobody else
/// would: its session, thread, request and tool call, its tokens and
/// sockets. Measured from each one's shell tool (`env` there against a
/// plain shell's). Settings a person may export (`CLAUDE_EFFORT`,
/// `CLAUDE_CODE_USE_BEDROCK`, `NO_COLOR`, `GIT_ASKPASS`, proxies…) aren't
/// here: they go only by taking the holder's environment.
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
/// too: `AGENT` (opencode sets `AGENT=1`, amp sets it as well). One in the
/// holder's environment is the session's, so it stays; one only an agent's
/// command has goes.
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
/// commands. `from_holder`: it's the session's own, so the names agents
/// share with people (`SHARED`) are yours there and stay.
fn clean(env: Env, from_holder: bool) -> Env {
    let agents = |k: &str| AGENT_SET.iter().any(|(_, vars)| vars.contains(&k)) && !(from_holder && SHARED.contains(&k));
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
    resolve(&args, || std::env::vars_os().collect(), |h| crate::platform::env_of(pids[h]))
}

/// `own` is this process's environment, `host_env` reads the one of the
/// forebear at that place.
fn resolve(forebears: &[Option<Vec<String>>], own: impl FnOnce() -> Env, host_env: impl FnOnce(usize) -> Option<Env>) -> Env {
    match source(forebears) {
        Source::Host(h) => match host_env(h) {
            Some(env) => clean(env, true),
            // Under an agent, with nothing to compare with: what's ours is
            // the agent's command's.
            None => clean(own(), false),
        },
        // Yours: a shell pane's, or a terminal's outside trix.
        Source::Own => clean(own(), true),
    }
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

    /// What a session was started with, as the holder has it.
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
    fn under_an_agent_the_server_takes_the_holders_environment() {
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

    #[test]
    fn under_every_agent_the_server_gets_the_sessions_environment_back() {
        for (agent, added, removed) in MEASURED {
            let got = resolve(&run_by(agent), || under(agent), |h| (h == 3).then(|| env(SESSION)));
            assert_eq!(names(&got), names(&env(SESSION)), "{agent} added {added:?}, removed {removed:?}");
        }
    }

    #[test]
    fn when_the_holder_cant_be_read_what_the_agent_set_still_goes() {
        // A holder that isn't ours to read, or a session an older trix
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
        // In the holder's (the session's) environment: yours.
        let got = resolve(&run_by("opencode"), || panic!("not ours"), |_| Some(yours.clone()));
        assert_eq!(names(&got), ["AGENT", "PATH"]);
        // Typed into a shell pane, or outside trix: yours too.
        assert_eq!(names(&resolve(&[a(&["trix"]), a(&["-zsh"]), a(&["trix", "holder"])], || yours.clone(), |_| None)), ["AGENT", "PATH"]);
        // Under an agent, with no holder to read: the agent's.
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
