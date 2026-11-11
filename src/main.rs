mod agent;
mod app;
mod hooks;
mod input;
mod theme;
mod ui;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use crossterm::{execute, queue};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{Write, stdout};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use agent::{Kind, Spawn};
use app::{App, AppEvent};

const HELP: &str = "\
trix — a workspace where coding agents live

usage:
  trix                      open the workspace
  trix claude codex shell   open with these agents started in the current folder
  trix hook <source>        (internal) agent lifecycle reporter

keys inside trix: alt+n new agent · alt+a who needs you · alt+g grid · alt+? all keys
";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => {
            let source = args.get(1).cloned().unwrap_or_else(|| "custom".into());
            hooks::run_client(&source, args.get(2).cloned());
            return Ok(());
        }
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("trix {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    if std::env::var_os("TRIX").is_some() && std::env::var_os("TRIX_ALLOW_NESTED").is_none() {
        eprintln!("trix: already inside trix (set TRIX_ALLOW_NESTED=1 to nest)");
        std::process::exit(1);
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
        initial.push(kind);
    }
    run(initial)
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

fn run(initial: Vec<Kind>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let (tx, rx) = mpsc::channel::<AppEvent>();
    hooks::sweep_stale();
    let socket = hooks::socket_path();
    hooks::serve(&socket, tx.clone())?;
    let claude_settings = hooks::write_claude_settings(&exe).ok();

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
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Terminal input thread.
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("trix-input".into()).spawn(move || {
            while let Ok(ev) = crossterm::event::read() {
                let ev = match ev {
                    Event::Key(k) => AppEvent::Key(k),
                    Event::Mouse(m) => AppEvent::Mouse(m),
                    Event::Paste(s) => AppEvent::Paste(s),
                    Event::Resize(..) => AppEvent::Resize,
                    Event::FocusGained => AppEvent::FocusGained,
                    Event::FocusLost => AppEvent::FocusLost,
                };
                if tx.send(ev).is_err() {
                    break;
                }
            }
        })?;
    }

    let mut app = App::new(tx.clone(), socket.clone(), claude_settings.clone(), exe);
    let size = terminal.size()?;
    app.pane_size = (size.width.saturating_sub(34 + 4).max(20), size.height.saturating_sub(4).max(5));
    let cwd = app.start_dir.clone();
    for kind in initial {
        let name = format!("{}{}", kind.label(), if app.agents.iter().any(|a| a.kind == kind) { "-2" } else { "" });
        let spec = Spawn { kind, name, cwd: cwd.clone(), command: None, prompt: None, branch_note: None };
        if let Err(e) = app.spawn(spec) {
            app.toast(format!("could not start {}: {e}", kind.label()), app::ToastKind::Bad);
        }
    }
    if !app.agents.is_empty() {
        app.set_focus(0);
    }

    const FRAME: Duration = Duration::from_millis(8); // ~120 fps ceiling
    const ANIM: Duration = Duration::from_millis(90);
    const TICK: Duration = Duration::from_millis(250);
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut last_tick = Instant::now();
    let mut last_slow = Instant::now();

    let result: anyhow::Result<()> = loop {
        let now = Instant::now();
        let anim = app.animating();
        let frame_due = if app.dirty {
            last_draw + FRAME
        } else if anim {
            last_draw + ANIM
        } else {
            now + Duration::from_secs(5)
        };
        let deadline = frame_due.min(last_tick + TICK);
        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(ev) => {
                app.handle(ev);
                // Coalesce bursts, but never starve the renderer.
                for _ in 0..2048 {
                    match rx.try_recv() {
                        Ok(ev) => app.handle(ev),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
        if app.quit {
            break Ok(());
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
        if anim && now >= last_draw + ANIM {
            app.dirty = true;
        }
        if app.dirty && now >= last_draw + FRAME {
            app.dirty = false;
            last_draw = now;
            if let Err(e) = terminal.draw(|f| ui::draw(f, &mut app)) {
                break Err(e.into());
            }
        }
    };

    app.shutdown_all();
    restore();
    let _ = std::fs::remove_file(&socket);
    if let Some(p) = claude_settings {
        let _ = std::fs::remove_file(p);
    }
    result
}
