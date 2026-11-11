mod agent;
mod app;
mod hooks;
mod input;
mod session;
mod state;
mod theme;
mod ui;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use crossterm::{execute, queue};
use ratatui::Terminal;
use std::io::{Write, stdout};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use agent::{Kind, Spawn};
use app::{App, AppEvent};
use session::{ClientMsg, Ended, Out, RemoteBackend, SessionEvent};

const HELP: &str = "\
trix — a workspace where coding agents live

usage:
  trix                      open the workspace (attaches if it is already running)
  trix claude codex shell   open, and start these agents in the current folder
  trix attach               attach to the running workspace, never start one
  trix ls                   list agents and their state without attaching
  trix kill                 stop the workspace and every agent in it
  trix kill --all           stop every running session
  trix hook <source>        (internal) agent lifecycle reporter
  trix server               (internal) the background workspace

  -s, --session <name>      work with a named session instead of the default
                            one, e.g. `trix -s work`, `trix ls -s work`

Agents live in a background server: closing the terminal or pressing alt+d
detaches, and they keep working. alt+q quits and stops them. Each named
session is its own workspace, with its own agents and saved layout. Without
-s, `trix ls` shows every running session.

keys inside trix: alt+n new agent · alt+a who needs you · alt+g grid · alt+d detach · alt+? all keys
";

/// Pull `-s <name>` / `--session <name>` / `--session=<name>` out of `args`.
fn take_session(args: &mut Vec<String>) -> Result<Option<String>, String> {
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
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

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("hook") {
        let source = args.get(1).cloned().unwrap_or_else(|| "custom".into());
        hooks::run_client(&source, args.get(2).cloned());
        return Ok(());
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
    let explicit = flag.is_some() || from_env.is_some();
    session::set_name(flag.or(from_env).unwrap_or_else(|| session::DEFAULT.into()));
    let missing = || {
        if session::is_default() { "trix: no workspace running".to_string() } else { format!("trix: no session '{}' running", session::name()) }
    };

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
        Some("ls" | "list") => {
            let names = if explicit { vec![session::name().to_string()] } else { session::candidates() };
            let texts: Vec<String> = names.iter().filter_map(|n| session::query(n, &ClientMsg::List)).collect();
            if texts.is_empty() {
                eprintln!("{}", missing());
                std::process::exit(1);
            }
            print!("{}", texts.join("\n"));
            return Ok(());
        }
        Some("kill") => {
            if matches!(args.get(1).map(String::as_str), Some("-a" | "--all")) {
                let killed = session::candidates().iter().filter(|n| session::send_once(n, &ClientMsg::Kill)).count();
                if killed == 0 {
                    eprintln!("trix: no workspace running");
                    std::process::exit(1);
                }
                return Ok(());
            }
            if !session::send_once(session::name(), &ClientMsg::Kill) {
                eprintln!("{}", missing());
                std::process::exit(1);
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
        return client(Vec::new(), false);
    }
    let mut initial = Vec::new();
    for a in &args {
        let kind = match a.as_str() {
            "claude" => Kind::Claude,
            "codex" => Kind::Codex,
            "shell" | "sh" => Kind::Shell,
            other => {
                eprintln!("trix: unknown argument '{other}'\n\n{HELP}");
                std::process::exit(2);
            }
        };
        initial.push(kind.label().to_string());
    }
    client(initial, true)
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

/// The terminal side: connect (starting the server if allowed), take over the
/// terminal, and relay until detached.
fn client(spawn: Vec<String>, start: bool) -> anyhow::Result<()> {
    let stream = match session::connect() {
        Some(s) => s,
        None if start => session::start_server()?,
        None => {
            let what = if session::is_default() { "no workspace running".to_string() } else { format!("no session '{}' running", session::name()) };
            eprintln!("trix: {what} — start it with `{}`", session::command());
            std::process::exit(1);
        }
    };

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev_hook(info);
    }));

    enable_raw_mode()?;
    let mut out = stdout();
    queue!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        queue!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    out.flush()?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    let ended = session::run_client(stream, cols, rows, spawn);
    restore();
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
    }
}

/// The background workspace. Owns every agent; renders into whichever client
/// is attached, or nowhere.
fn server() -> anyhow::Result<()> {
    let Some(_lock) = session::lock()? else {
        // Another server won the race; the client will connect to that one.
        return Ok(());
    };
    let exe = std::env::current_exe()?;
    let (tx, rx) = mpsc::channel::<AppEvent>();
    hooks::sweep_stale();
    let socket = hooks::socket_path();
    hooks::serve(&socket, tx.clone())?;
    let claude_settings = hooks::write_claude_settings(&exe).ok();
    let control = session::control_path();
    session::listen(&control, tx.clone())?;

    let out = Out::default();
    let mut terminal = Terminal::new(RemoteBackend::new(out.clone()))?;
    let mut app = App::new(tx.clone(), socket.clone(), claude_settings.clone(), exe, out.clone());
    app.term_focused = false;

    const FRAME: Duration = Duration::from_millis(8); // ~120 fps ceiling
    const ANIM: Duration = Duration::from_millis(90);
    const TICK: Duration = Duration::from_millis(250);
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut last_tick = Instant::now();
    let mut last_slow = Instant::now();
    let mut killed = false;

    loop {
        let now = Instant::now();
        let attached = out.attached();
        let anim = attached && app.animating();
        let frame_due = if attached && app.dirty {
            last_draw + FRAME
        } else if anim {
            last_draw + ANIM
        } else {
            now + Duration::from_secs(5)
        };
        let deadline = frame_due.min(last_tick + TICK);
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
        if killed {
            out.bye("[trix stopped by `trix kill`]");
            break;
        }
        if app.quit {
            out.bye("");
            break;
        }
        if app.detach {
            app.detach = false;
            out.bye(&format!("[detached from trix · run `{}` to come back]", session::command()));
            app.on_detach();
        }
        let now = Instant::now();
        if now >= last_tick + TICK {
            last_tick = now;
            app.tick();
            if now >= last_slow + Duration::from_secs(1) {
                last_slow = now;
                app.slow_tick();
            }
        }
        if !out.attached() {
            app.dirty = false;
            continue;
        }
        if anim && now >= last_draw + ANIM {
            app.dirty = true;
        }
        if app.dirty && now >= last_draw + FRAME {
            app.dirty = false;
            last_draw = now;
            // Output errors only ever mean the client left; `Out` handles it.
            let _ = terminal.draw(|f| ui::draw(f, &mut app));
        }
    }

    // Save before stopping: the agents' exits must not read as "closed".
    app.persist();
    app.shutdown_all();
    let _ = std::fs::remove_file(&control);
    let _ = std::fs::remove_file(&socket);
    if let Some(p) = claude_settings {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

fn dispatch(ev: AppEvent, app: &mut App, terminal: &mut Terminal<RemoteBackend>, out: &Out, killed: &mut bool) {
    match ev {
        AppEvent::Session(s) => on_session(s, app, terminal, out, killed),
        ev => app.handle(ev),
    }
}

fn on_session(ev: SessionEvent, app: &mut App, terminal: &mut Terminal<RemoteBackend>, out: &Out, killed: &mut bool) {
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
                let spec = Spawn { kind, name: app.unique_name(kind.label()), cwd: cwd.clone(), command: None, prompt: None, branch_note: None, resume: None };
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
        SessionEvent::Detached(id) => {
            out.lost(id);
            if !out.attached() {
                app.on_detach();
            }
        }
        SessionEvent::List(mut stream) => {
            let _ = session::write_frame(&mut stream, session::TEXT, app.listing().as_bytes());
        }
        SessionEvent::Kill => *killed = true,
    }
}
