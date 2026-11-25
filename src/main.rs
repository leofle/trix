mod agent;
mod app;
mod case;
mod cleanup;
mod config;
mod cost;
mod control;
mod events;
mod folders;
mod holder;
mod hooks;
mod idle;
mod input;
mod install;
mod instructions;
mod layout;
mod ledger;
mod modes;
#[cfg(target_os = "macos")]
mod notifier;
mod offer;
mod platform;
mod prices;
mod review;
mod search;
mod session;
mod settings;
mod state;
mod theme;
mod ui;
mod usage;
mod worktree;
mod worktrees;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use crossterm::{execute, queue};
use ratatui::Terminal;
use std::os::unix::net::UnixStream;
use std::io::{Write, stdout};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use agent::{Kind, Spawn};
use app::{App, AppEvent};
use session::{ClientMsg, Ended, Mode, Out, RemoteBackend, SessionEvent};

const HELP: &str = "\
trix — a workspace where coding agents live

usage:
  trix                      open the workspace (attaches if it is already running)
  trix claude codex shell   open, and start these agents in the current folder
  trix attach               attach to the running workspace, never start one
  trix view                 watch the running workspace read-only (q leaves)
  trix ls [--json] [--all]  list agents and their state without attaching;
                            --all lists every session, even from inside a pane
  trix triage <PR>          a case: a reviewer, a fixer and a gatekeeper take the PR
                            from findings to a merge you approve (trix case --help)
  trix spend [YYYY-MM] [--json]
                            what agents spent in a month (this one by default),
                            per model, every session's, against monthly_budget
  trix worktrees [--all] [--json]
                            trix's worktrees in this repo (--all: every repo trix
                            knows): who uses each, what it holds, if it's merged
  trix worktrees prune [--folders] [--min-age 1d] [--yes]
                            remove clean, merged, unused worktrees, merged trix/*
                            branches and month-old kept tips; --folders also
                            unmerged folders, keeping the branch. Shows the plan
                            and asks; a dry run without a terminal unless --yes
  trix worktrees rm <name> [--keep-branch] [--yes]
                            remove one, as the review's X does; says why not
  trix forget -s <name> [--yes]
                            drop a stopped session's saved workspace, so it
                            stops keeping its worktrees in use; asks first
  trix kill                 stop the workspace and every agent in it
  trix kill --all           stop every running session

agent control (for scripts, and for agents managing other agents):
  trix spawn <kind> [--name N] [--cwd DIR] [--worktree] [--prompt P] [--args A]
                            start claude, codex, grok, opencode, cursor (cursor-agent),
                            copilot, muse, gemini, antigravity, amp, shell or custom; prints
                            its name. --worktree uses the repo's .trix.toml only
                            once it's trusted in the new-agent form (alt+n)
  trix send <agent> <prompt…>
                            type a prompt into an agent and submit it (prompt
                            from stdin when omitted). Refuses agents that need
                            you, shells and starting agents unless --force
  trix read <agent> [-n N]  the last N lines of its screen and scrollback (60)
  trix wait <agent>… [--all] [-t SECS]
                            block until one (or --all) stops working: done,
                            idle, needs-you or exited. Exits 124 after -t
                            seconds (300; 0 waits forever)
  trix close <agent> [--force] [--clean]
                            close it; --force if it is working or needs you.
                            --clean then removes its worktree if nothing keeps
                            it (exits 1 with why, keeping it, otherwise)
  trix args <agent> [<args…> | --clear]
                            show or change the extra arguments it starts with
                            (e.g. --model claude-opus-5-5). Used from its next
                            start: a restore, or a saved layout
  trix events [agent…] [--json]
                            follow what agents do, live: one line per change
                            (spawned, state, renamed, closed). --json for
                            scripts: one JSON object per line
  <agent> is a name, or its number in `trix ls`.

layouts (named sets of tabs, in ~/.config/trix/layouts/<name>.toml):
  trix layout save <name> [--force]
                            save the tabs open now: kind, name, folder, args
  trix layout open <name> [--resume]
                            open them again (fresh conversations; --resume
                            picks up the saved ones). Tabs already open are
                            skipped. From outside trix, starts it and attaches
  trix layout ls            list saved layouts
  trix layout rm <name>     remove one

  trix config               check ~/.config/trix/config.toml (keys, sounds,
                            notifications); trix reloads it on every save.
                            Also ~/.config/trix/instructions.md: what you
                            tell every agent, read as each one starts

  trix hook <source>        (internal) agent lifecycle reporter
  trix hook amp --plugin    print trix's plugin for Amp, to save where Amp reads
                            plugins (see the README)
  trix install <kind> [--stdin <text>] -- <command…>
                            (internal) install a missing agent in its pane, then
                            run it, with <text> as what it reads on stdin first
  trix worktree-setup <tell|quiet> <done-file> <script> -- <command…>
                            (internal) run a repo's trusted .trix.toml setup in
                            a new worktree's pane, then run the agent
  trix server               (internal) the background workspace
  trix holder               (internal) holds one agent's terminal for the server

  -s, --session <name>      work with a named session instead of the default
                            one, e.g. `trix -s work`, `trix ls -s work`

Agents live in a background server: closing the terminal or pressing alt+d
detaches, and they keep working. alt+q quits and stops them. One terminal
is in control at a time; `trix view` lets any number of others watch. Each named
session is its own workspace, with its own agents and saved layout. Without
-s, `trix ls` shows every running session, how long each has been left
alone, and the stopped ones that still have a saved workspace.

keys inside trix (the defaults): alt+n new agent · alt+a who needs you · alt+g grid · alt+d detach · alt+? all keys
";

/// Pull `-s <name>` / `--session <name>` / `--session=<name>` out of `args`.
fn take_session(args: &mut Vec<String>) -> Result<Option<String>, String> {
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        // Past `--`, everything is positional: a prompt may say "-s".
        if args[i] == "--" {
            break;
        }
        // Another flag's value (`--args '-s x'`, `--prompt -s`) isn't ours.
        if control::VALUE_FLAGS.contains(&args[i].as_str()) {
            i += 2;
            continue;
        }
        let value = if args[i] == "-s" || args[i] == "--session" {
            if i + 1 >= args.len() {
                return Err(format!("{} needs a session name", args[i]));
            }
            let v = args.remove(i + 1);
            args.remove(i);
            v
        } else if let Some(v) = args[i].strip_prefix("--session=") {
            let v = v.to_string();
            args.remove(i);
            v
        } else {
            i += 1;
            continue;
        };
        session::check_name(&value)?;
        if found.as_ref().is_some_and(|f| f != &value) {
            return Err("more than one session given".into());
        }
        found = Some(value);
    }
    Ok(found)
}

/// The sessions `trix ls` lists: inside a pane or with `-s`, that one
/// (`explicit`), unless `--all`; otherwise every one running.
fn ls_sessions(explicit: bool, all: bool, name: &str, candidates: Vec<String>) -> Vec<String> {
    if explicit && !all { vec![name.to_string()] } else { candidates }
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // The copy of trix that posts macOS notifications, and nothing else.
    #[cfg(target_os = "macos")]
    if notifier::is_app() {
        std::process::exit(notifier::run(&args));
    }
    if args.first().map(String::as_str) == Some("hook") {
        let source = args.get(1).cloned().unwrap_or_else(|| "custom".into());
        hooks::run_client(&source, args.get(2).cloned());
        return Ok(());
    }
    // Check the runtime folder now, while the terminal is still normal.
    hooks::runtime_dir();
    if args.first().map(String::as_str) == Some("holder") {
        std::process::exit(holder::main());
    }
    // Before the session flags: everything after `--` is the agent's.
    if args.first().map(String::as_str) == Some("install") {
        std::process::exit(install::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("worktree-setup") {
        std::process::exit(worktree::run_setup(&args[1..]));
    }
    let flag = match take_session(&mut args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("trix: {e}");
            std::process::exit(2);
        }
    };
    // Inside a pane, `trix ls` and friends mean the session the pane is in.
    let from_env = std::env::var("TRIX_SESSION").ok().filter(|n| session::check_name(n).is_ok());
    let flagged = flag.is_some();
    let explicit = flagged || from_env.is_some();
    session::set_name(flag.or(from_env).unwrap_or_else(|| session::DEFAULT.into()));
    let missing = || format!("trix: {}", session::missing());

    match args.first().map(String::as_str) {
        Some("server") => return server(),
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("trix {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("ls" | "list") if args.iter().skip(1).any(|a| a == "--json") => {
            if !args.iter().skip(1).any(|a| a == "--all" || a == "-a") {
                control::run(control::Request::List { json: true });
            }
            // Every running session's listing, as one array.
            let ask = ClientMsg::Control { protocol: control::CONTROL, from: None, req: control::Request::List { json: true } };
            let lists: Vec<serde_json::Value> = session::candidates()
                .iter()
                .filter_map(|n| session::ask(n, &ask).ok().flatten())
                .filter_map(|text| serde_json::from_str::<control::Reply>(&text).ok())
                .filter_map(|r| serde_json::from_str(&r.out).ok())
                .collect();
            println!("{}", serde_json::to_string_pretty(&lists).unwrap_or_default());
            return Ok(());
        }
        Some("layout") => {
            let cmd = match layout::parse(&args[1..]) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("trix: {e}");
                    std::process::exit(2);
                }
            };
            match layout::run(cmd) {
                Ok(true) => return client(Mode::Control(Vec::new()), false),
                Ok(false) => return Ok(()),
                Err(e) => {
                    eprintln!("trix: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(cmd) if control::COMMANDS.contains(&cmd) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
            match control::parse(&args, cwd) {
                Ok(control::Request::Events { targets }) => events::run(targets, args.iter().any(|a| a == "--json")),
                Ok(req) => control::run(req),
                Err(e) => {
                    eprintln!("trix: {e}\n\n{HELP}");
                    std::process::exit(2);
                }
            }
        }
        Some("ls" | "list") => {
            let all = args.iter().skip(1).any(|a| a == "--all" || a == "-a");
            let names = ls_sessions(explicit, all, session::name(), session::candidates());
            let mut texts = Vec::new();
            let mut running = Vec::new();
            // Sessions with no server, but with agents one that died left
            // running: said, or nobody would know they are.
            let mut left = Vec::new();
            for n in &names {
                match session::query(n, &ClientMsg::List) {
                    Some(text) => {
                        texts.push(text);
                        running.push(n.clone());
                    }
                    None => left.extend(session::left_running(n).map(|note| (n, note))),
                }
            }
            // Stopped sessions whose saved workspace still holds worktrees:
            // when every session is listed.
            let footer = if !explicit || all {
                let age = |s: &str| std::fs::metadata(state::path_for(s)).and_then(|m| m.modified()).ok().and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
                idle::footer(&idle::stopped(&state::saved_workspaces(), &running, &age))
            } else {
                None
            };
            if texts.is_empty() {
                // Says so of this session itself.
                left.retain(|(n, _)| *n != session::name());
            }
            for (n, note) in &left {
                if *n == session::DEFAULT {
                    eprintln!("trix: {note}");
                } else {
                    eprintln!("trix: session '{n}': {note}");
                }
            }
            if texts.is_empty() {
                eprintln!("{}", missing());
                if let Some(f) = footer {
                    print!("{f}");
                }
                std::process::exit(1);
            }
            print!("{}", texts.join("\n"));
            if let Some(f) = footer {
                print!("\n{f}");
            }
            return Ok(());
        }
        Some("worktrees") => std::process::exit(worktrees::run(&args[1..])),
        Some("spend") => std::process::exit(ledger::run(&args[1..])),
        Some("triage") => std::process::exit(case::triage(&args[1..])),
        Some("case") => std::process::exit(case::run(&args[1..])),
        Some("forget") => {
            let yes = args.iter().skip(1).any(|a| a == "--yes" || a == "-y");
            if !flagged || args.iter().skip(1).any(|a| a != "--yes" && a != "-y") {
                eprintln!("trix: forget takes a session: `trix forget -s <name> [--yes]`\n\n{HELP}");
                std::process::exit(2);
            }
            let name = session::name();
            // Held until it's done: no server starts and reads the file meanwhile.
            let (_locks, served) = session::claim(Some(name));
            let holders = holder::discover(name).len();
            let mut ask = |what: &str| -> bool {
                eprint!("{what}");
                if yes {
                    return true;
                }
                use std::io::IsTerminal;
                if !std::io::stdin().is_terminal() {
                    eprintln!("trix: no terminal to ask: run it again with --yes to forget it");
                    return false;
                }
                eprint!("Forget it? Its agents won't be offered back; their folders and branches stay. [y/N] ");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                matches!(line.trim(), "y" | "Y" | "yes")
            };
            match state::forget(name, !served.is_empty(), holders, &mut ask) {
                Ok(done) => {
                    worktree::release_session(name);
                    eprintln!("trix: {done}");
                    return Ok(());
                }
                Err(why) => {
                    eprintln!("trix: {why}");
                    std::process::exit(1);
                }
            }
        }
        Some("config") => {
            if args.len() > 1 {
                eprintln!("trix: config takes no arguments\n\n{HELP}");
                std::process::exit(2);
            }
            let path = app::tilde(&config::path());
            match (config::load(), config::stamp()) {
                (Ok(_), None) => println!("{path}: no file, so the defaults. See the README's Config section."),
                (Ok(_), Some(_)) => println!("{path}: ok"),
                (Err(e), _) => {
                    eprintln!("trix: {path}: {e}");
                    std::process::exit(1);
                }
            }
            let shown = app::tilde(&instructions::path());
            match instructions::user() {
                Ok(None) => println!("{shown}: none, so agents get trix's instructions only"),
                Ok(Some(t)) => println!("{shown}: ok, {} lines every agent is given", t.lines().count()),
                Err(e) => {
                    eprintln!("trix: {e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        Some("kill") => {
            if matches!(args.get(1).map(String::as_str), Some("-a" | "--all")) {
                let mut killed = 0;
                for n in session::candidates() {
                    match session::send_once(&n, &ClientMsg::Kill) {
                        Ok(true) => killed += 1,
                        Ok(false) => {}
                        Err(why) => {
                            eprintln!("{why}");
                            std::process::exit(1);
                        }
                    }
                }
                // And whatever servers that died hard left running.
                let orphans = holder::kill_orphans(None);
                if killed == 0 && orphans == 0 {
                    eprintln!("trix: no workspace running");
                    std::process::exit(1);
                }
                return Ok(());
            }
            match session::send_once(session::name(), &ClientMsg::Kill) {
                Ok(true) => {}
                // No server, but maybe agents one that died hard left behind.
                Ok(false) if holder::kill_orphans(Some(session::name())) > 0 => {}
                Ok(false) => {
                    eprintln!("{}", missing());
                    std::process::exit(1);
                }
                Err(why) => {
                    eprintln!("{why}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        _ => {}
    }
    if std::env::var_os("TRIX").is_some() && std::env::var_os("TRIX_ALLOW_NESTED").is_none() {
        eprintln!("trix: already inside trix (set TRIX_ALLOW_NESTED=1 to nest)");
        std::process::exit(1);
    }
    if matches!(args.first().map(String::as_str), Some("attach" | "a")) {
        if args.len() > 1 {
            eprintln!("trix: attach takes no arguments\n\n{HELP}");
            std::process::exit(2);
        }
        return client(Mode::Control(Vec::new()), false);
    }
    if matches!(args.first().map(String::as_str), Some("view" | "watch")) {
        if args.len() > 1 {
            eprintln!("trix: view takes no arguments\n\n{HELP}");
            std::process::exit(2);
        }
        return client(Mode::View, false);
    }
    let mut initial = Vec::new();
    for a in &args {
        let kind = match a.as_str() {
            "claude" => Kind::Claude,
            "codex" => Kind::Codex,
            "grok" => Kind::Grok,
            "opencode" => Kind::OpenCode,
            // Not `agent`: Grok's installer links that name too.
            "cursor" | "cursor-agent" => Kind::Cursor,
            "copilot" => Kind::Copilot,
            "muse" => Kind::Muse,
            "gemini" => Kind::Gemini,
            "antigravity" | "agy" => Kind::Antigravity,
            "amp" => Kind::Amp,
            "shell" | "sh" => Kind::Shell,
            other => {
                eprintln!("trix: unknown argument '{other}'\n\n{HELP}");
                std::process::exit(2);
            }
        };
        initial.push(kind.label().to_string());
    }
    client(Mode::Control(initial), true)
}

fn restore() {
    let mut out = stdout();
    let _ = execute!(
        out,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}

/// A viewer only needs raw keys and the alternate screen: no mouse capture,
/// so text can still be selected the terminal's own way.
fn restore_view() {
    let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

/// The terminal side: connect (starting the server if allowed), take over the
/// terminal, and relay until detached.
fn client(mode: Mode, start: bool) -> anyhow::Result<()> {
    let view = matches!(mode, Mode::View);
    let stream = match session::connect() {
        Some(s) => s,
        None if start => session::start_server()?,
        None => {
            if session::blocked() {
                eprintln!("trix: {}", session::missing());
            } else {
                eprintln!("trix: {} — start it with `{}`", session::missing(), session::command());
            }
            std::process::exit(1);
        }
    };

    let reset = if view { restore_view } else { restore };
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        reset();
        prev_hook(info);
    }));

    enable_raw_mode()?;
    let mut out = stdout();
    if view {
        queue!(out, EnterAlternateScreen)?;
    } else {
        queue!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
        // With the shifted character too (REPORT_ALTERNATE_KEYS): without it,
        // a terminal speaking the kitty protocol (iTerm2, Ghostty, kitty, WezTerm)
        // sends alt+$ as alt+shift+4, which reads as alt+4, a jump to agent 4,
        // and a pane gets alt+4 for it too.
        if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
            queue!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS))?;
        }
    }
    out.flush()?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    let ended = session::run_client(stream, cols, rows, mode);
    reset();
    match ended? {
        Ended::Bye(msg) => {
            if !msg.is_empty() {
                println!("{msg}");
            }
            Ok(())
        }
        Ended::Lost => {
            eprintln!("trix: lost the workspace server (log: {})", session::log_path().display());
            std::process::exit(1);
        }
        Ended::Refused if view => {
            eprintln!("trix: the running workspace was started by an older trix that can't be watched — restart it (alt+q, then `{}`) to use `trix view`", session::command());
            std::process::exit(1);
        }
        Ended::Refused => {
            eprintln!("trix: the workspace server hung up (log: {})", session::log_path().display());
            std::process::exit(1);
        }
    }
}

/// The background workspace. Owns every agent; renders into whichever client
/// is attached, or nowhere.
fn server() -> anyhow::Result<()> {
    let Some(lock) = session::lock()? else {
        // Another server won the race; the client will connect to that one.
        return Ok(());
    };
    platform::raise_fd_limit();
    let exe = std::env::current_exe()?;
    let (tx, rx) = mpsc::channel::<AppEvent>();
    hooks::sweep_stale();
    let socket = hooks::socket_path();
    hooks::serve(&socket, tx.clone())?;
    let integrations = hooks::Integrations::write(&exe);
    let control = session::control_path();
    session::listen(&control, tx.clone())?;

    let out = Out::default();
    let mut terminal = Terminal::new(RemoteBackend::new(out.clone()))?;
    let mut app = App::new(tx.clone(), socket.clone(), integrations.clone(), exe, out.clone());
    app.term_focused = false;
    app.holders = true;
    // Agents the last server left running (it crashed, or was killed).
    let back = app.reattach();
    if back > 0 {
        eprintln!("trix: reattached {back} agent{} left running", if back == 1 { "" } else { "s" });
    }

    const FRAME: Duration = Duration::from_millis(8); // ~120 fps ceiling
    const ANIM: Duration = Duration::from_millis(90);
    const TICK: Duration = Duration::from_millis(250);
    let mut last_draw = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
    let mut last_tick = Instant::now();
    let mut last_slow = Instant::now();
    // `trix kill` connections, closed once everything has stopped.
    let mut killed: Vec<UnixStream> = Vec::new();

    loop {
        let now = Instant::now();
        let attached = out.attached();
        // Animations draw when the picture changes, not on a fixed beat: a
        // blinking "needs you" alone means a frame every 540 ms, not 11 a second.
        let anim = if attached { app.next_frame_change() } else { None };
        let frame_due = if attached && app.dirty {
            last_draw + FRAME
        } else if let Some(t) = anim {
            t.max(last_draw + ANIM)
        } else {
            now + Duration::from_secs(5)
        };
        let deadline = frame_due.min(last_tick + TICK).min(app.search_due.unwrap_or(frame_due));
        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(ev) => {
                dispatch(ev, &mut app, &mut terminal, &out, &mut killed);
                // Coalesce bursts, but never starve the renderer.
                for _ in 0..2048 {
                    match rx.try_recv() {
                        Ok(ev) => dispatch(ev, &mut app, &mut terminal, &out, &mut killed),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !killed.is_empty() {
            out.bye_all("[trix stopped by `trix kill`]");
            break;
        }
        if app.quit {
            out.bye("");
            out.bye_all("[trix quit]");
            break;
        }
        if app.detach {
            app.detach = false;
            out.bye(&format!("[detached from trix · run `{}` to come back]", session::command()));
            app.on_detach();
        }
        app.run_due_search();
        let now = Instant::now();
        if now >= last_tick + TICK {
            last_tick = now;
            app.tick();
            control::poll(&mut app);
            if now >= last_slow + Duration::from_secs(1) {
                last_slow = now;
                app.slow_tick();
                events::prune(&mut app);
            }
        }
        // Whatever changed this turn, from any source.
        events::publish(&mut app);
        if !out.attached() {
            app.dirty = false;
            continue;
        }
        if anim.is_some_and(|t| now >= t) && now >= last_draw + ANIM {
            app.dirty = true;
        }
        if app.dirty && now >= last_draw + FRAME {
            app.dirty = false;
            last_draw = now;
            // Output errors only ever mean the client left; `Out` handles it.
            if let Ok(frame) = terminal.draw(|f| ui::draw(f, &mut app)) {
                let t = &app.theme;
                out.mirror(frame.buffer, ratatui::style::Style::new().fg(t.bg).bg(t.accent));
            }
        }
    }

    // Save before stopping: the agents' exits must not read as "closed".
    app.persist();
    app.release_unsaved_ports();
    control::stop_all(&mut app, "the workspace stopped");
    app.shutdown_all();
    let _ = std::fs::remove_file(&control);
    let _ = std::fs::remove_file(&socket);
    integrations.remove();
    // Free the session for a new server, then let `trix kill` return.
    drop(lock);
    drop(killed);
    Ok(())
}

fn viewers_changed(app: &mut App, out: &Out, what: &str) {
    app.viewers = out.viewers();
    // Newcomers need the title too, not just the next change of it.
    app.last_title.clear();
    if out.controlled() {
        let n = app.viewers;
        let count = if n == 0 { String::new() } else { format!(" · {n} watching") };
        app.toast(format!("{what}{count}"), app::ToastKind::Info);
    }
    app.dirty = true;
}

fn dispatch(ev: AppEvent, app: &mut App, terminal: &mut Terminal<RemoteBackend>, out: &Out, killed: &mut Vec<UnixStream>) {
    match ev {
        AppEvent::Session(s) => on_session(s, app, terminal, out, killed),
        ev => app.handle(ev),
    }
}

fn on_session(ev: SessionEvent, app: &mut App, terminal: &mut Terminal<RemoteBackend>, out: &Out, killed: &mut Vec<UnixStream>) {
    match ev {
        SessionEvent::Attach { id, stream, cols, rows, cwd, spawn } => {
            let kicked = out.attach(id, stream, (cols, rows));
            // The new terminal is blank: forget the last frame, repaint all.
            let _ = terminal.clear();
            if app.agents.is_empty() {
                app.pane_size = (cols.saturating_sub(34 + 4).max(20), rows.saturating_sub(4).max(5));
            }
            app.on_attach(cwd.clone());
            if kicked {
                app.toast("took over from another terminal", app::ToastKind::Info);
            }
            for label in spawn {
                let Some(kind) = Kind::from_label(&label) else { continue };
                let spec = Spawn { kind, name: app.unique_name(kind.label()), cwd: cwd.clone(), command: None, prompt: None, branch_note: None, resume: None, fork: false, runtime: Default::default() };
                if let Err(e) = app.spawn(spec) {
                    app.toast(format!("could not start {}: {e}", kind.label()), app::ToastKind::Bad);
                }
            }
        }
        SessionEvent::Resize(id, cols, rows) => {
            if out.resize(id, cols, rows) {
                app.handle(AppEvent::Resize);
            }
        }
        SessionEvent::View { id, stream, cols, rows } => {
            out.add_viewer(id, stream, (cols, rows));
            viewers_changed(app, out, "someone started watching");
        }
        SessionEvent::ViewResize(id, cols, rows) => {
            out.resize_viewer(id, cols, rows);
            app.dirty = true;
        }
        SessionEvent::Unview(id) => {
            if out.bye_viewer(id, "[stopped watching trix]") {
                viewers_changed(app, out, "a viewer left");
            }
        }
        SessionEvent::Detached(id) => {
            if out.lost(id) {
                app.on_detach();
            }
            // Compare with what was last shown, not with just now: a viewer
            // whose write failed was already removed before this arrived.
            if out.viewers() < app.viewers {
                viewers_changed(app, out, "a viewer left");
            }
        }
        SessionEvent::List(mut stream) => {
            let _ = session::write_frame(&mut stream, session::TEXT, app.listing().as_bytes());
        }
        SessionEvent::Control { stream, from, req, sandboxed } => control::handle(app, stream, from, req, sandboxed),
        SessionEvent::Kill(stream) => killed.push(stream),
    }
}

#[cfg(test)]
mod tests {
    use super::take_session;

    #[test]
    fn ls_all_lists_every_session_even_from_a_pane() {
        let all = || vec!["default".to_string(), "sidebar".to_string()];
        assert_eq!(super::ls_sessions(true, false, "default", all()), ["default"], "a pane sees its own");
        assert_eq!(super::ls_sessions(true, true, "default", all()), ["default", "sidebar"], "--all sees every one");
        assert_eq!(super::ls_sessions(false, false, "default", all()), ["default", "sidebar"]);
    }

    fn run(args: &[&str]) -> (Result<Option<String>, String>, Vec<String>) {
        let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let r = take_session(&mut v);
        (r, v)
    }

    #[test]
    fn session_flag_anywhere() {
        assert_eq!(run(&["-s", "work", "ls"]), (Ok(Some("work".into())), vec!["ls".into()]));
        assert_eq!(run(&["ls", "-s", "work"]), (Ok(Some("work".into())), vec!["ls".into()]));
        assert_eq!(run(&["attach", "--session", "w2"]), (Ok(Some("w2".into())), vec!["attach".into()]));
        assert_eq!(run(&["--session=w3", "claude", "codex"]), (Ok(Some("w3".into())), vec!["claude".into(), "codex".into()]));
    }

    #[test]
    fn session_flag_is_not_taken_from_prompts() {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(run(&["send", "web", "--", "-s", "quiet"]), (Ok(None), v(&["send", "web", "--", "-s", "quiet"])));
        assert_eq!(run(&["spawn", "custom", "--args", "-s", "x"]), (Ok(None), v(&["spawn", "custom", "--args", "-s", "x"])));
        assert_eq!(run(&["-s", "w", "send", "web", "--", "-s", "q"]), (Ok(Some("w".into())), v(&["send", "web", "--", "-s", "q"])));
    }

    #[test]
    fn no_flag_leaves_args_alone() {
        assert_eq!(run(&["claude", "shell"]), (Ok(None), vec!["claude".into(), "shell".into()]));
        assert_eq!(run(&[]), (Ok(None), vec![]));
    }

    #[test]
    fn repeating_the_same_session_is_fine() {
        assert_eq!(run(&["-s", "a", "ls", "--session=a"]).0, Ok(Some("a".into())));
    }

    #[test]
    fn bad_flags_are_errors() {
        assert!(run(&["-s"]).0.is_err());
        assert!(run(&["ls", "--session"]).0.is_err());
        assert!(run(&["-s", "a", "-s", "b"]).0.is_err());
        assert!(run(&["-s", "a/b"]).0.is_err());
        assert!(run(&["--session="]).0.is_err());
        // A missing name must not swallow the next flag as one.
        assert!(run(&["-s", "--session=x"]).0.is_err());
    }
}
