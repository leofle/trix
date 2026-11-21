//! Agents that aren't installed yet install in their own pane, then start.
//!
//! When an agent's command isn't on `PATH`, `Agent::spawn_in` runs
//! `trix install <kind> -- <command> <args…>` in the pane instead. That
//! installs the agent with the first tool you have (mise, Homebrew, npm, then
//! the vendor's own install script), with its output on screen, tells the
//! server the install is over, and becomes the agent (`exec`): the same
//! process, with the same arguments the agent would have had.
//!
//! An agent that reads its first prompt from stdin (Amp) starts this way
//! too, installed or not: `--stdin <prompt>` puts the prompt there as trix
//! becomes the agent, with no shell in between.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent::Kind;

/// One way to install an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `mise use -g`, the way Omarchy installs agents.
    Mise(&'static str),
    Brew(&'static str),
    BrewCask(&'static str),
    Npm(&'static str),
    /// The vendor's install script, fetched with curl and run by bash. It
    /// puts the agent in `~/.local/bin`.
    Script(&'static str),
}

impl Via {
    /// What has to be on `PATH` for this way to work.
    fn tools(self) -> &'static [&'static str] {
        match self {
            Via::Mise(_) => &["mise"],
            Via::Brew(_) | Via::BrewCask(_) => &["brew"],
            Via::Npm(_) => &["npm"],
            Via::Script(_) => &["curl", "bash"],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Via::Mise(_) => "mise",
            Via::Brew(_) | Via::BrewCask(_) => "brew",
            Via::Npm(_) => "npm",
            Via::Script(_) => "its install script",
        }
    }

    fn argv(self) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect();
        match self {
            Via::Mise(p) => v(&["mise", "use", "-g", p]),
            Via::Brew(p) => v(&["brew", "install", p]),
            Via::BrewCask(p) => v(&["brew", "install", "--cask", p]),
            Via::Npm(p) => v(&["npm", "install", "-g", p]),
            Via::Script(url) => vec!["bash".into(), "-c".into(), format!("curl -fsSL {url} | bash")],
        }
    }
}

/// The ways to install each agent, best first. Each is the agent's own
/// package; mise's are the ones Omarchy uses.
fn ways(kind: Kind) -> &'static [Via] {
    match kind {
        Kind::Claude => &[Via::Mise("claude"), Via::BrewCask("claude-code"), Via::Npm("@anthropic-ai/claude-code"), Via::Script("https://claude.ai/install.sh")],
        Kind::Codex => &[Via::Mise("codex"), Via::BrewCask("codex"), Via::Npm("@openai/codex")],
        Kind::OpenCode => &[Via::Mise("opencode"), Via::Brew("opencode"), Via::Npm("opencode-ai")],
        // Homebrew's `grok` is another program (a regex tool).
        Kind::Grok => &[Via::Mise("npm:@xai-official/grok"), Via::Npm("@xai-official/grok")],
        Kind::Cursor => &[Via::BrewCask("cursor-cli"), Via::Script("https://cursor.com/install")],
        Kind::Copilot => &[Via::Mise("npm:@github/copilot"), Via::BrewCask("copilot-cli"), Via::Npm("@github/copilot"), Via::Script("https://gh.io/copilot-install")],
        // Meta ships Muse Code only as its install script: npm's
        // `@muse-code/sdk` is its SDK, not the CLI.
        Kind::Muse => &[Via::Script("https://dev.meta.ai/install.sh")],
        Kind::Gemini => &[Via::Mise("npm:@google/gemini-cli"), Via::Brew("gemini-cli"), Via::Npm("@google/gemini-cli")],
        // Google ships Antigravity CLI only as its install script.
        Kind::Antigravity => &[Via::Script("https://antigravity.google/cli/install.sh")],
        // npm's package was `@sourcegraph/amp` until May 2026. Its install
        // script puts Amp in `~/.amp/bin` and links it into `~/.local/bin`.
        Kind::Amp => &[Via::Mise("npm:@ampcode/cli"), Via::Npm("@ampcode/cli"), Via::Script("https://ampcode.com/install.sh")],
        Kind::Shell | Kind::Custom => &[],
    }
}

/// The first way to install `kind` that this `PATH` has the tools for.
pub fn pick(kind: Kind, path: Option<&OsStr>) -> Option<Via> {
    ways(kind).iter().copied().find(|w| w.tools().iter().all(|t| find(t, path).is_some()))
}

/// What any way of installing `kind` would need, for when there is none:
/// "mise, brew or npm".
pub fn needs(kind: Kind) -> String {
    let mut tools: Vec<&str> = Vec::new();
    for w in ways(kind) {
        let t = w.tools()[0];
        if !tools.contains(&t) {
            tools.push(t);
        }
    }
    match tools.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => "nothing".into(),
    }
}

/// Where `name` would run from with this `PATH`, as a shell would find it.
pub fn find(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return runnable(&p).then_some(p);
    }
    std::env::split_paths(path?).filter(|d| !d.as_os_str().is_empty()).map(|d| d.join(name)).find(|p| runnable(p))
}

fn runnable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Is `program` missing from trix's own `PATH`, which its panes inherit?
pub fn missing(program: &str) -> bool {
    find(program, std::env::var_os("PATH").as_deref()).is_none()
}

/// The pane's command that installs `kind` if it's missing and then runs
/// `program args`, with `stdin`, if any, as what it reads there first.
pub fn wrap(exe: &Path, kind: Kind, stdin: Option<&str>, program: String, args: Vec<String>) -> (String, Vec<String>) {
    let mut all = vec!["install".to_string(), kind.label().into()];
    if let Some(text) = stdin {
        all.extend(["--stdin".into(), text.into()]);
    }
    all.extend(["--".into(), program]);
    all.extend(args);
    (exe.display().to_string(), all)
}

/// `text` as a program's stdin: a pipe holding all of it, its writing end
/// closed, so the program reads the text and then the end. Text too long
/// for a pipe to hold goes in a file with no name left instead.
fn stdin_holding(text: &str) -> std::io::Result<std::fs::File> {
    use std::io::{Seek, Write};
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == 0 {
        // Owned from here: both close when dropped, and neither is left
        // open in the agent (its stdin is a copy of the reading end).
        let (read, mut write) = unsafe { (std::fs::File::from_raw_fd(fds[0]), std::fs::File::from_raw_fd(fds[1])) };
        // Nothing reads yet: a write that would wait for room never ends.
        unsafe {
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
        }
        if write.write_all(text.as_bytes()).is_ok() {
            return Ok(read);
        }
    }
    use std::os::unix::fs::OpenOptionsExt;
    let path = crate::hooks::runtime_dir().join(format!("stdin-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&path)?;
    let _ = std::fs::remove_file(&path);
    f.write_all(text.as_bytes())?;
    f.rewind()?;
    Ok(f)
}

/// Give `cmd` the first prompt as its stdin. Without it, the agent still
/// starts, with the terminal as its stdin as usual.
fn feed(cmd: &mut Command, program: &str, text: &str) {
    match stdin_holding(text) {
        Ok(f) => {
            cmd.stdin(f);
        }
        Err(e) => eprintln!("trix: couldn't hand {program} its first prompt ({e}): starting it without."),
    }
}

/// `<kind> [--stdin <text>] -- <command> [args…]`: the kind, what its stdin
/// starts with, its command and arguments.
fn parse(args: &[String]) -> Option<(Kind, Option<&str>, &str, &[String])> {
    let kind = args.first().and_then(|k| Kind::from_label(k)).filter(|k| !ways(*k).is_empty())?;
    let (stdin, rest) = match args.get(1).map(String::as_str) {
        Some("--stdin") => (Some(args.get(2)?.as_str()), &args[3..]),
        _ => (None, &args[1..]),
    };
    let (program, agent_args) = rest.strip_prefix(&["--".to_string()])?.split_first()?;
    Some((kind, stdin, program, agent_args))
}

/// `PATH`, with `~/.local/bin` (where install scripts put agents) last if
/// it isn't there already.
fn with_local_bin(path: Option<&OsStr>) -> OsString {
    let mut dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        let local = Path::new(&home).join(".local/bin");
        if !dirs.contains(&local) {
            dirs.push(local);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| path.map(OsStr::to_os_string).unwrap_or_default())
}

/// Hold this while installing `kind`, so two panes starting the same agent
/// don't install it on top of each other: the second waits, then finds it.
fn lock(kind: Kind) -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    let f = std::fs::File::create(crate::hooks::runtime_dir().join(format!("install-{}.lock", kind.label()))).ok()?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        println!("trix: another pane is installing {}; waiting for it…", kind.label());
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return None;
        }
    }
    Some(f)
}

/// `trix install <kind> [--stdin <text>] -- <command> [args…]`, run as a
/// pane's command. Returns the exit code; on success it never returns: it
/// becomes the agent.
pub fn run(args: &[String]) -> i32 {
    let usage = || {
        eprintln!("usage: trix install <kind> [--stdin <text>] -- <command> [args…]   (internal: trix runs it in a pane)");
        2
    };
    let Some((kind, stdin, program, agent_args)) = parse(args) else { return usage() };

    let path = std::env::var_os("PATH");
    let held = lock(kind);
    let mut used = None;
    if find(program, path.as_deref()).is_none() {
        let Some(via) = pick(kind, path.as_deref()) else {
            eprintln!("trix: {program} isn't installed, and nothing on your PATH can install it (it needs {}).", needs(kind));
            return 127;
        };
        let argv = via.argv();
        println!("\x1b[1mtrix:\x1b[0m {program} isn't installed yet. Installing it with {}:\n  {}\n", via.label(), argv.join(" "));
        // Homebrew 7 asks before it installs (or upgrades a dependency), and
        // a question in a pane that reads as "installing" waits unseen. The
        // variable, not `--yes`: older Homebrew has no such flag.
        match Command::new(&argv[0]).args(&argv[1..]).env("HOMEBREW_NO_ASK", "1").status() {
            Ok(s) if s.success() => println!("\n\x1b[1mtrix:\x1b[0m {program} is installed. Starting it…\n"),
            Ok(s) => {
                eprintln!("\ntrix: installing {program} failed ({s}). Press Enter to try again, or install it yourself and then press Enter.");
                return s.code().unwrap_or(1);
            }
            Err(e) => {
                eprintln!("\ntrix: couldn't run {}: {e}", argv[0]);
                return 127;
            }
        }
        used = Some(via);
    }
    drop(held);
    // The pane stops holding at "installing" and starts watching the agent.
    crate::hooks::tell("trix", "Installed");

    let path = with_local_bin(path.as_deref());
    let mut cmd = if let Some(bin) = find(program, Some(&path)) {
        let mut c = Command::new(bin);
        c.arg0(program).args(agent_args);
        c
    } else if let Some(Via::Mise(pkg)) = used {
        // mise installed it, but its shims aren't on this PATH: run it
        // through mise, as Omarchy's wrappers do.
        let mut c = Command::new("mise");
        c.args(["x", pkg, "--", program]).args(agent_args);
        c
    } else {
        eprintln!("trix: {program} was installed, but it isn't on your PATH. Add its folder to PATH, restart trix, and press Enter here.");
        return 127;
    };
    // The installer above had the terminal: only the agent reads the text.
    if let Some(text) = stdin {
        feed(&mut cmd, program, text);
    }
    let err = cmd.env("PATH", &path).exec();
    eprintln!("trix: couldn't start {program}: {err}");
    126
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of fake programs: `tools` are runnable, `plain` are files
    /// without the execute bit.
    fn bin_dir(tag: &str, tools: &[&str], plain: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trix-install-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (names, mode) in [(tools, 0o755), (plain, 0o644)] {
            for n in names {
                let p = dir.join(n);
                std::fs::write(&p, "#!/bin/sh\n").unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            }
        }
        dir
    }

    #[test]
    fn find_wants_a_runnable_file() {
        let dir = bin_dir("find", &["grok"], &["opencode"]);
        let path = std::env::join_paths([dir.join("nope"), dir.clone()]).unwrap();
        assert_eq!(find("grok", Some(&path)), Some(dir.join("grok")));
        assert_eq!(find("opencode", Some(&path)), None, "not executable");
        assert_eq!(find("cursor-agent", Some(&path)), None);
        assert_eq!(find("grok", None), None);
        let full = dir.join("grok");
        assert_eq!(find(full.to_str().unwrap(), None), Some(full));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn picks_the_first_way_whose_tools_are_there() {
        let dir = bin_dir("pick", &["brew", "npm", "curl", "bash"], &[]);
        let path = dir.clone().into_os_string();
        let p = Some(path.as_os_str());
        assert_eq!(pick(Kind::OpenCode, p), Some(Via::Brew("opencode")));
        assert_eq!(pick(Kind::Cursor, p), Some(Via::BrewCask("cursor-cli")));
        assert_eq!(pick(Kind::Copilot, p), Some(Via::BrewCask("copilot-cli")));
        assert_eq!(pick(Kind::Muse, p), Some(Via::Script("https://dev.meta.ai/install.sh")));
        // Never Homebrew's grok, which is something else.
        assert_eq!(pick(Kind::Grok, p), Some(Via::Npm("@xai-official/grok")));
        assert_eq!(pick(Kind::Shell, p), None);
        assert_eq!(pick(Kind::Custom, p), None);
        let _ = std::fs::remove_dir_all(dir);

        let dir = bin_dir("pick-mise", &["mise", "brew", "npm"], &[]);
        let path = dir.clone().into_os_string();
        assert_eq!(pick(Kind::Grok, Some(&path)), Some(Via::Mise("npm:@xai-official/grok")));
        assert_eq!(pick(Kind::OpenCode, Some(&path)), Some(Via::Mise("opencode")));
        assert_eq!(pick(Kind::Copilot, Some(&path)), Some(Via::Mise("npm:@github/copilot")));
        assert_eq!(pick(Kind::Amp, Some(&path)), Some(Via::Mise("npm:@ampcode/cli")));
        // Meta's install script is the only Muse Code: no mise, Homebrew or npm package.
        assert_eq!(pick(Kind::Muse, Some(&path)), None);
        // mise has no Cursor of Omarchy's to install.
        assert_eq!(pick(Kind::Cursor, Some(&path)), Some(Via::BrewCask("cursor-cli")));
        let _ = std::fs::remove_dir_all(dir);

        // The install script needs bash as well as curl.
        let dir = bin_dir("pick-curl", &["curl"], &[]);
        let path = dir.clone().into_os_string();
        assert_eq!(pick(Kind::Cursor, Some(&path)), None);
        let _ = std::fs::remove_dir_all(dir);

        let dir = bin_dir("pick-npm", &["npm", "curl", "bash"], &[]);
        let path = dir.clone().into_os_string();
        assert_eq!(pick(Kind::Copilot, Some(&path)), Some(Via::Npm("@github/copilot")));
        assert_eq!(pick(Kind::Amp, Some(&path)), Some(Via::Npm("@ampcode/cli")));
        let _ = std::fs::remove_dir_all(dir);
        let dir = bin_dir("pick-script", &["curl", "bash"], &[]);
        let path = dir.clone().into_os_string();
        assert_eq!(pick(Kind::Copilot, Some(&path)), Some(Via::Script("https://gh.io/copilot-install")));
        assert_eq!(pick(Kind::Muse, Some(&path)), Some(Via::Script("https://dev.meta.ai/install.sh")));
        // Google's install script is the only Antigravity CLI.
        assert_eq!(pick(Kind::Antigravity, Some(&path)), Some(Via::Script("https://antigravity.google/cli/install.sh")));
        assert_eq!(pick(Kind::Amp, Some(&path)), Some(Via::Script("https://ampcode.com/install.sh")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn needs_names_each_tool_once() {
        assert_eq!(needs(Kind::Grok), "mise or npm");
        assert_eq!(needs(Kind::OpenCode), "mise, brew or npm");
        assert_eq!(needs(Kind::Cursor), "brew or curl");
        assert_eq!(needs(Kind::Copilot), "mise, brew, npm or curl");
        assert_eq!(needs(Kind::Muse), "curl");
        assert_eq!(needs(Kind::Antigravity), "curl");
        assert_eq!(needs(Kind::Amp), "mise, npm or curl");
    }

    #[test]
    fn wrap_keeps_the_agents_own_command_line() {
        let (prog, args) = wrap(Path::new("/x/trix"), Kind::Grok, None, "grok".into(), vec!["--session-id".into(), "u".into(), "-- hi".into()]);
        assert_eq!(prog, "/x/trix");
        assert_eq!(args, ["install", "grok", "--", "grok", "--session-id", "u", "-- hi"]);
        // A first prompt for the agent's stdin, whatever it starts with.
        let (_, args) = wrap(Path::new("/x/trix"), Kind::Amp, Some("-- fix\nit"), "amp".into(), vec!["--fast".into()]);
        assert_eq!(args, ["install", "amp", "--stdin", "-- fix\nit", "--", "amp", "--fast"]);
    }

    /// What the agent reads on its stdin: the text, then the end.
    #[test]
    fn stdin_holds_the_prompt_and_ends() {
        use std::io::Read;
        for text in ["fix the login\nthen run the tests", "", &"a long prompt, longer than a pipe holds. ".repeat(40_000)] {
            let mut read = String::new();
            stdin_holding(text).unwrap().read_to_string(&mut read).unwrap();
            assert_eq!(read.len(), text.len());
            assert!(read == text);
        }
    }

    /// The agent `trix install` becomes reads the prompt on its stdin, and
    /// keeps its own arguments.
    #[test]
    fn the_agent_starts_with_its_prompt_on_stdin() {
        let dir = bin_dir("stdin", &[], &[]);
        let out = dir.join("out");
        let script = dir.join("amp");
        std::fs::write(&script, format!("#!/bin/sh\ncat > '{0}'\nprintf ' [%s]' \"$@\" >> '{0}'\n", out.display())).unwrap();
        // Through sh, not exec'd: a child another test is spawning may hold
        // the descriptor it was written with until its own exec, and a file
        // open for writing can't be exec'd (ETXTBSY).
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&script).args(["--fast", "-x y"]);
        feed(&mut cmd, "amp", "- fix it\nplease");
        assert!(cmd.status().unwrap().success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "- fix it\nplease [--fast] [-x y]");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn install_arguments() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let args = v(&["grok", "--", "grok", "--session-id", "u"]);
        assert_eq!(parse(&args), Some((Kind::Grok, None, "grok", &args[3..])));
        // The prompt is one argument, whatever it holds.
        let args = v(&["amp", "--stdin", "-- fix it", "--", "amp", "--fast"]);
        assert_eq!(parse(&args), Some((Kind::Amp, Some("-- fix it"), "amp", &args[5..])));
        let args = v(&["amp", "--stdin", "--", "--", "amp"]);
        assert_eq!(parse(&args), Some((Kind::Amp, Some("--"), "amp", &args[5..])));
        for bad in [&["amp"][..], &["amp", "--"], &["amp", "--stdin"], &["amp", "--stdin", "hi", "amp"], &["shell", "--", "sh"], &["gpt", "--", "gpt"]] {
            assert_eq!(parse(&v(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn local_bin_is_added_once() {
        let Some(home) = std::env::var_os("HOME") else { return };
        let local = Path::new(&home).join(".local/bin");
        let p = with_local_bin(Some(OsStr::new("/usr/bin")));
        assert_eq!(std::env::split_paths(&p).collect::<Vec<_>>(), [PathBuf::from("/usr/bin"), local.clone()]);
        let again = with_local_bin(Some(&p));
        assert_eq!(again, p);
    }
}
