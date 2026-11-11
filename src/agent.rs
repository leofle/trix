//! One agent = one PTY + one terminal emulator + a state machine that turns
//! hook reports and screen/activity signals into a human-readable status.

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::app::AppEvent;
use crate::hooks::{self, HookMsg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Claude,
    Codex,
    Shell,
    Custom,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Claude, Kind::Codex, Kind::Shell, Kind::Custom];
    pub fn label(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Shell => "shell",
            Kind::Custom => "custom",
        }
    }
    pub fn is_agent(self) -> bool {
        !matches!(self, Kind::Shell)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Idle,
    Working,
    Blocked,
    Done,
    Exited,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Idle => "idle",
            State::Working => "working",
            State::Blocked => "needs you",
            State::Done => "done",
            State::Exited => "exited",
        }
    }
}

#[derive(Clone)]
pub struct Listener {
    id: u32,
    tx: Sender<AppEvent>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let ev = match event {
            TermEvent::Wakeup => AppEvent::Output(self.id),
            TermEvent::PtyWrite(s) => AppEvent::PtyWrite(self.id, s),
            TermEvent::Title(t) => AppEvent::Title(self.id, t),
            TermEvent::Bell => AppEvent::Bell(self.id),
            TermEvent::ChildExit(status) => AppEvent::Exit(self.id, status.code()),
            TermEvent::ClipboardStore(_, s) => AppEvent::Clipboard(s),
            TermEvent::TextAreaSizeRequest(f) => {
                // Respond with an approximate cell size; good enough for apps that ask.
                AppEvent::PtyWriteFn(self.id, f)
            }
            _ => return,
        };
        let _ = self.tx.send(ev);
    }
}

pub struct TermSize {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Spawn {
    pub kind: Kind,
    pub name: String,
    pub cwd: PathBuf,
    pub command: Option<String>,
    pub prompt: Option<String>,
    pub branch_note: Option<String>,
}

pub struct Agent {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    pub cwd: PathBuf,
    #[allow(dead_code)]
    pub command_line: String,
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    pub pid: u32,
    pub size: (u16, u16),

    pub state: State,
    pub state_since: Instant,
    pub created: Instant,
    /// Unseen completion / question — cleared when the human looks at it.
    pub unseen: bool,
    /// Claude lifecycle hooks are reporting (spinner-aware heuristics apply).
    pub hooked: bool,
    /// A custom tool reports its own state via `trix hook <source>`; trust it
    /// and skip the output heuristics entirely.
    pub reported: bool,
    /// Last Esc/Enter the human pressed while this agent was blocked.
    pub dismissed_at: Option<Instant>,
    last_resize: Instant,
    pub activity: String,
    pub last_prompt: Option<String>,
    pub title: Option<String>,
    pub exit_code: Option<i32>,

    pub last_output: Instant,
    pub last_input: Instant,
    /// Output events per second, newest last.
    pub spark: VecDeque<u32>,
    spark_tick: Instant,
    burst: VecDeque<Instant>,
    pub work_started: Option<Instant>,
    pub tools_used: u32,

    pub branch: Option<String>,
    pub diff: Option<(u32, u32, u32)>,
}

pub const SPARK_LEN: usize = 48;

impl Agent {
    pub fn spawn(
        id: u32,
        spec: Spawn,
        size: (u16, u16),
        tx: Sender<AppEvent>,
        socket: &std::path::Path,
        claude_settings: Option<&std::path::Path>,
        exe: &std::path::Path,
    ) -> anyhow::Result<Agent> {
        let (program, args) = command_for(&spec, claude_settings, exe);
        let command_line = std::iter::once(program.clone())
            .chain(args.iter().map(|a| if a.contains(' ') { format!("{a:?}") } else { a.clone() }))
            .collect::<Vec<_>>()
            .join(" ");

        let mut env = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "trix".into());
        env.insert("TRIX".into(), "1".into());
        env.insert("TRIX_SOCKET".into(), socket.display().to_string());
        env.insert("TRIX_AGENT_ID".into(), id.to_string());
        env.insert("TRIX_AGENT_NAME".into(), spec.name.clone());
        if spec.kind == Kind::Codex {
            if let Some(chain) = codex_user_notify() {
                env.insert("TRIX_CODEX_NOTIFY".into(), chain);
            }
        }

        let opts = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            working_directory: Some(spec.cwd.clone()),
            drain_on_exit: true,
            env,
        };
        let ws = window_size(size);
        let pty = tty::new(&opts, ws, id as u64)?;
        let pid = pty.child().id();

        let listener = Listener { id, tx };
        let config = Config { scrolling_history: 10_000, ..Config::default() };
        let tsize = TermSize { cols: size.0 as usize, lines: size.1 as usize };
        let term = Arc::new(FairMutex::new(Term::new(config, &tsize, listener.clone())));
        let ev = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = ev.channel();
        ev.spawn();

        let now = Instant::now();
        let mut spark = VecDeque::with_capacity(SPARK_LEN);
        spark.extend(std::iter::repeat_n(0, SPARK_LEN));
        Ok(Agent {
            id,
            name: spec.name,
            kind: spec.kind,
            cwd: spec.cwd,
            command_line,
            term,
            sender,
            pid,
            size,
            state: State::Starting,
            state_since: now,
            created: now,
            unseen: false,
            hooked: false,
            reported: false,
            dismissed_at: None,
            last_resize: now,
            activity: spec.branch_note.unwrap_or_else(|| "launching".into()),
            last_prompt: spec.prompt,
            title: None,
            exit_code: None,
            last_output: now,
            last_input: now - Duration::from_secs(60),
            spark,
            spark_tick: now,
            burst: VecDeque::new(),
            work_started: None,
            tools_used: 0,
            branch: None,
            diff: None,
        })
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let b = bytes.into();
        if !b.is_empty() {
            let _ = self.sender.send(Msg::Input(b));
        }
    }

    pub fn write_later(&self, bytes: &'static [u8], delay: Duration) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = sender.send(Msg::Input(Cow::Borrowed(bytes)));
        });
    }

    pub fn user_input(&mut self, bytes: Vec<u8>) {
        self.last_input = Instant::now();
        self.write(bytes);
    }

    pub fn resize(&mut self, cols: u16, lines: u16) {
        let (cols, lines) = (cols.max(2), lines.max(1));
        if self.size == (cols, lines) {
            return;
        }
        self.size = (cols, lines);
        self.last_resize = Instant::now();
        let _ = self.sender.send(Msg::Resize(window_size((cols, lines))));
        self.term.lock().resize(TermSize { cols: cols as usize, lines: lines as usize });
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(Msg::Shutdown);
    }

    pub fn set_state(&mut self, s: State, focused: bool) -> Option<State> {
        if self.state == s {
            return None;
        }
        let prev = self.state;
        self.state = s;
        self.state_since = Instant::now();
        match s {
            State::Working => {
                if self.work_started.is_none() {
                    self.work_started = Some(Instant::now());
                }
            }
            State::Done | State::Blocked => {
                self.unseen = !focused;
            }
            _ => {}
        }
        if !matches!(s, State::Working | State::Blocked) {
            self.work_started = None;
        }
        Some(prev)
    }

    /// Called on every terminal wakeup.
    pub fn on_output(&mut self) {
        let now = Instant::now();
        self.last_output = now;
        self.roll_spark(now);
        if let Some(b) = self.spark.back_mut() {
            *b += 1;
        }
        // Ignore output that is just the echo of keystrokes, or the redraw a
        // resize provokes (sidebar / grid / attention banner toggles).
        if now.duration_since(self.last_input) > Duration::from_millis(350)
            && now.duration_since(self.last_resize) > Duration::from_millis(400)
        {
            self.burst.push_back(now);
        }
        while self.burst.front().is_some_and(|t| now.duration_since(*t) > Duration::from_millis(2500)) {
            self.burst.pop_front();
        }
    }

    pub fn roll_spark(&mut self, now: Instant) {
        while now.duration_since(self.spark_tick) >= Duration::from_secs(1) {
            self.spark_tick += Duration::from_secs(1);
            self.spark.pop_front();
            self.spark.push_back(0);
        }
    }

    /// Apply a hook report. Returns the previous state when it changed.
    pub fn on_hook(&mut self, msg: &HookMsg, focused: bool) -> Option<State> {
        // A late report (e.g. SessionEnd racing ChildExit) must not revive a
        // dead pane: Enter-to-restart only works while it reads as exited.
        if self.state == State::Exited {
            return None;
        }
        match msg.source.as_str() {
            "claude" => self.hooked = true,
            "codex" => {} // notify only covers turn completion; keep heuristics
            _ => self.reported = true,
        }
        let tool = msg.tool.clone().unwrap_or_default();
        let described = |d: &Option<String>| match d {
            Some(d) if !d.is_empty() => format!("{tool} {d}"),
            _ => tool.clone(),
        };
        match msg.event.as_str() {
            "SessionStart" => {
                self.activity = "ready".into();
                self.set_state(State::Idle, focused)
            }
            "UserPromptSubmit" => {
                if let Some(p) = &msg.detail {
                    self.last_prompt = Some(p.clone());
                }
                self.activity = "thinking".into();
                self.tools_used = 0;
                self.set_state(State::Working, focused)
            }
            "PreToolUse" => {
                self.tools_used += 1;
                if tool == "AskUserQuestion" {
                    self.activity = msg.detail.clone().unwrap_or_else(|| "has a question".into());
                    return self.set_state(State::Blocked, focused);
                }
                self.activity = described(&msg.detail);
                self.set_state(State::Working, focused)
            }
            "PostToolUse" => {
                if self.state == State::Blocked {
                    self.activity = format!("{tool} ✓");
                }
                self.set_state(State::Working, focused)
            }
            "PermissionRequest" => {
                self.activity = format!("approve {}", described(&msg.detail));
                self.set_state(State::Blocked, focused)
            }
            "Notification" => {
                let m = msg.detail.clone().unwrap_or_default();
                if m.to_lowercase().contains("permission") || m.to_lowercase().contains("approv") {
                    if self.state != State::Blocked {
                        self.activity = hooks::one_line(&m, 60);
                    }
                    self.set_state(State::Blocked, focused)
                } else {
                    None
                }
            }
            "Stop" | "agent-turn-complete" => {
                self.activity = match self.tools_used {
                    0 => "replied".into(),
                    n => format!("finished · {n} tool{}", if n == 1 { "" } else { "s" }),
                };
                if let Some(d) = &msg.detail {
                    if msg.event == "agent-turn-complete" {
                        self.activity = d.clone();
                    }
                }
                self.set_state(State::Done, focused)
            }
            "SessionEnd" => self.set_state(State::Idle, focused),
            _ => None,
        }
    }

    /// Heuristic pass, run on a timer. Hooks win when present; the screen and
    /// output cadence fill the gaps (interrupted turns, unhooked agents).
    pub fn heuristics(&mut self, focused: bool) -> Option<State> {
        if self.state == State::Exited {
            return None;
        }
        let now = Instant::now();
        self.roll_spark(now);
        while self.burst.front().is_some_and(|t| now.duration_since(*t) > Duration::from_millis(2500)) {
            self.burst.pop_front();
        }
        let quiet = now.duration_since(self.last_output);
        let busy = self.burst.len() >= 6 && quiet < Duration::from_millis(1200);

        if self.state == State::Starting {
            if self.created.elapsed() > Duration::from_millis(1500) && quiet > Duration::from_millis(600) {
                if self.kind.is_agent() && self.screen_needs_input() {
                    self.activity = "waiting for an answer".into();
                    return self.set_state(State::Blocked, focused);
                }
                self.activity = "ready".into();
                return self.set_state(State::Idle, focused);
            }
            return None;
        }

        if self.reported {
            return None;
        }

        let screen_question = || self.screen_needs_input();

        if self.hooked {
            // Claude redraws its spinner every second while working. A long
            // silence means the turn was interrupted without a Stop hook.
            if self.state == State::Working && quiet > Duration::from_secs(6) {
                if screen_question() {
                    self.activity = "waiting for an answer".into();
                    return self.set_state(State::Blocked, focused);
                }
                self.activity = "interrupted".into();
                return self.set_state(State::Idle, focused);
            }
            if self.state == State::Blocked && busy && !screen_question() {
                return self.set_state(State::Working, focused);
            }
            // Denied / cancelled a prompt: no PostToolUse or Stop follows, and
            // the redraw lands inside the keystroke-echo window. If the human
            // pressed Esc/Enter, things went quiet and the prompt is gone, it
            // no longer needs them.
            if self.state == State::Blocked
                && self.dismissed_at.is_some_and(|t| t > self.state_since && t.elapsed() > Duration::from_secs(2))
                && quiet > Duration::from_secs(2)
                && !screen_question()
            {
                self.activity = "dismissed".into();
                return self.set_state(State::Idle, focused);
            }
            return None;
        }

        match self.state {
            State::Working if !busy && quiet > Duration::from_millis(1500) => {
                if screen_question() {
                    self.activity = "waiting for an answer".into();
                    self.set_state(State::Blocked, focused)
                } else {
                    let worked = self.work_started.map(|t| t.elapsed()).unwrap_or_default();
                    let long = worked > Duration::from_secs(if self.kind.is_agent() { 3 } else { 8 });
                    if long {
                        self.activity = format!("finished after {}", crate::ui::fmt_dur(worked));
                        self.set_state(State::Done, focused)
                    } else {
                        self.activity = "idle".into();
                        self.set_state(State::Idle, focused)
                    }
                }
            }
            State::Blocked if busy && !screen_question() => {
                self.activity = "working".into();
                self.set_state(State::Working, focused)
            }
            // Unhooked agents only become blocked from what the screen shows,
            // so once the prompt is gone and output settles, they're not.
            State::Blocked if quiet > Duration::from_secs(2) && !screen_question() => {
                self.activity = "idle".into();
                self.set_state(State::Idle, focused)
            }
            State::Idle | State::Done if busy => {
                self.activity = self.title.clone().unwrap_or_else(|| "working".into());
                self.set_state(State::Working, focused)
            }
            State::Idle | State::Done if quiet > Duration::from_millis(800) && self.kind.is_agent() => {
                if self.state != State::Blocked && screen_question() && self.state_since.elapsed() > Duration::from_secs(1) {
                    self.activity = "waiting for an answer".into();
                    self.set_state(State::Blocked, focused)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Look at the bottom of the live screen for approval / question UI.
    pub fn screen_needs_input(&self) -> bool {
        let term = self.term.lock();
        let grid = term.grid();
        let lines = grid.screen_lines() as i32;
        let cols = grid.columns();
        let mut text = String::new();
        for l in (lines - 14).max(0)..lines {
            let row = &grid[Line(l)];
            for c in 0..cols {
                text.push(row[Column(c)].c);
            }
            text.push('\n');
        }
        let t = text.to_lowercase();
        // Only phrasings of actual prompts — a bare word like "approve" shows
        // up in ordinary agent prose and code.
        const PATTERNS: [&str; 13] = [
            "enter to confirm",
            "trust this folder",
            "(yes/no)",
            "do you want to",
            "would you like to",
            "allow command",
            "allow this",
            "(y/n)",
            "[y/n]",
            "yes, and don't ask",
            "yes, proceed",
            "press enter to confirm",
            "❯ 1. yes",
        ];
        PATTERNS.iter().any(|p| t.contains(p))
    }

    /// Plain-text dump of the visible screen (used for broadcast previews & copy).
    #[allow(dead_code)]
    pub fn screen_text(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let mut out = String::new();
        for l in 0..grid.screen_lines() as i32 {
            let row = &grid[Line(l)];
            let line: String = (0..grid.columns()).map(|c| row[Column(c)].c).collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    pub fn live_cwd(&self) -> PathBuf {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid)).unwrap_or_else(|_| self.cwd.clone())
    }

    #[allow(dead_code)]
    pub fn cursor_point(&self) -> Point {
        self.term.lock().grid().cursor.point
    }
}

fn window_size((cols, lines): (u16, u16)) -> WindowSize {
    WindowSize { num_cols: cols, num_lines: lines, cell_width: 9, cell_height: 18 }
}

/// Split user-typed extra flags the way a shell would, so quoted values like
/// `--append-system-prompt "be terse"` stay one argument.
fn split_args(s: &str) -> Vec<String> {
    shell_words::split(s).unwrap_or_else(|_| s.split_whitespace().map(String::from).collect())
}

/// The user's own Codex `notify` command (JSON array), so `trix hook codex`
/// can call it after reporting — our `-c notify=` would otherwise replace it.
fn codex_user_notify() -> Option<String> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let argv: Vec<String> = table
        .get("notify")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let is_trix = argv.first().is_some_and(|p| p.rsplit('/').next() == Some("trix")) && argv.get(1).map(String::as_str) == Some("hook");
    (!argv.is_empty() && !is_trix).then(|| serde_json::to_string(&argv).ok()).flatten()
}

fn command_for(spec: &Spawn, claude_settings: Option<&std::path::Path>, exe: &std::path::Path) -> (String, Vec<String>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let mut args = Vec::new();
    match spec.kind {
        Kind::Claude => {
            if let Some(s) = claude_settings {
                args.push("--settings".into());
                args.push(s.display().to_string());
            }
            if let Some(extra) = spec.command.as_deref().filter(|c| !c.trim().is_empty()) {
                args.extend(split_args(extra));
            }
            if let Some(p) = spec.prompt.as_deref().filter(|p| !p.is_empty()) {
                args.push(p.into());
            }
            ("claude".into(), args)
        }
        Kind::Codex => {
            args.push("-c".into());
            // A JSON string array is a valid TOML inline array, with escaping.
            let notify = serde_json::to_string(&[exe.display().to_string(), "hook".into(), "codex".into()]).unwrap_or_default();
            args.push(format!("notify={notify}"));
            if let Some(extra) = spec.command.as_deref().filter(|c| !c.trim().is_empty()) {
                args.extend(split_args(extra));
            }
            if let Some(p) = spec.prompt.as_deref().filter(|p| !p.is_empty()) {
                args.push(p.into());
            }
            ("codex".into(), args)
        }
        Kind::Shell => (shell, vec![]),
        Kind::Custom => {
            let cmd = spec.command.clone().unwrap_or_default();
            (shell, vec!["-lc".into(), cmd])
        }
    }
}
