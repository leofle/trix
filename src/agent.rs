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
use std::path::{Path, PathBuf};
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
    pub fn from_label(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.label() == s)
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
    /// Conversation to resume (Claude / Codex) instead of starting a new one.
    pub resume: Option<String>,
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
    /// Our own handle on the PTY's master side, to ask the terminal which
    /// process group is in the foreground.
    pty: std::fs::File,
    pub pid: u32,
    pub size: (u16, u16),

    pub state: State,
    pub state_since: Instant,
    pub created: Instant,
    /// Unseen completion / question — cleared when the human looks at it.
    pub unseen: bool,
    /// Lifecycle hooks are reporting: Claude's, or Codex's once trusted
    /// (spinner-aware heuristics apply).
    pub hooked: bool,
    /// A custom tool reports its own state via `trix hook <source>`; trust it
    /// and skip the output heuristics entirely.
    pub reported: bool,
    /// Last Esc/Enter the human pressed while this agent was blocked.
    pub dismissed_at: Option<Instant>,
    last_resize: Instant,
    pub activity: String,
    pub last_prompt: Option<String>,
    /// The conversation this agent is in, as its hooks last reported it.
    pub session: Option<String>,
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
    /// A prompt is on its way in (`trix send`, or a first prompt at launch)
    /// and no turn has visibly started since. `trix wait` treats that as
    /// work, so the end of the previous turn, or `SessionStart`, isn't taken
    /// for the end of this one.
    pub prompted: Option<Instant>,
    /// A hook has reported a turn. Until then a hooked agent can still be on
    /// a startup dialog (trust this folder?) that no hook reports.
    pub turn_seen: bool,
    /// The agent that started or last prompted this one through `trix`.
    pub managed_by: Option<String>,
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
        let first_prompt = spec.kind.is_agent() && spec.prompt.as_deref().is_some_and(|p| !p.trim().is_empty());
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
        env.insert("TRIX_SESSION".into(), crate::session::name().into());
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
        let master = pty.file().try_clone()?;

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
            pty: master,
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
            session: spec.resume,
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
            prompted: first_prompt.then(Instant::now),
            turn_seen: false,
            managed_by: None,
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
        // The turn a prompt was sent for has visibly begun (or ended, or it
        // is waiting on the human): idle alone isn't evidence of any of that.
        if s != State::Idle {
            self.prompted = None;
        }
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
        // `/clear` and friends start a new conversation: always track the latest.
        if let Some(id) = &msg.session {
            self.session = Some(id.clone());
        }
        match msg.source.as_str() {
            "claude" => self.hooked = true,
            // `notify` only covers finished turns. Anything else is one of
            // Codex's lifecycle hooks, which it runs once they're trusted.
            "codex" if msg.event != "agent-turn-complete" => self.hooked = true,
            "codex" => {}
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
                self.turn_seen = true;
                if let Some(p) = &msg.detail {
                    self.last_prompt = Some(p.clone());
                }
                self.activity = "thinking".into();
                self.tools_used = 0;
                self.set_state(State::Working, focused)
            }
            "PreToolUse" => {
                self.turn_seen = true;
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
            "Interrupt" => {
                self.activity = "interrupted".into();
                self.set_state(State::Idle, focused)
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
            // `SessionStart` comes before the trust-this-folder dialog, and no
            // hook reports the dialog. Before the first turn, the screen is
            // the only way to know; after it, prompts come through hooks, and
            // the agent's own prose ("would you like to…") mustn't count.
            if self.state == State::Idle
                && !self.turn_seen
                && quiet > Duration::from_millis(800)
                && self.state_since.elapsed() > Duration::from_secs(1)
                && screen_question()
            {
                self.activity = "waiting for an answer".into();
                return self.set_state(State::Blocked, focused);
            }
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

    /// Look at the bottom of what's on the live screen for approval /
    /// question UI. Inline TUIs draw from the top of a fresh terminal, so
    /// that's the last rows with anything on them, not the last rows.
    pub fn screen_needs_input(&self) -> bool {
        let term = self.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        let blank = |l: i32| (0..cols).all(|c| matches!(grid[Line(l)][Column(c)].c, ' ' | '\0'));
        let mut lines = grid.screen_lines() as i32;
        while lines > 0 && blank(lines - 1) {
            lines -= 1;
        }
        let mut text = String::new();
        for l in (lines - 14).max(0)..lines {
            let row = &grid[Line(l)];
            for c in 0..cols {
                text.push(row[Column(c)].c);
            }
            text.push('\n');
        }
        needs_input(&text)
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

    /// Is a shell running a command, rather than sitting at its prompt? The
    /// terminal's foreground process group says: the shell's own while at the
    /// prompt, the command's while one runs.
    pub fn running_command(&self) -> bool {
        crate::platform::foreground_group(&self.pty).is_some_and(|g| g != self.pid as i32)
    }

    pub fn live_cwd(&self) -> PathBuf {
        crate::platform::cwd_of(self.pid).unwrap_or_else(|| self.cwd.clone())
    }

    #[allow(dead_code)]
    pub fn cursor_point(&self) -> Point {
        self.term.lock().grid().cursor.point
    }
}

/// Does this screen text show an approval prompt or a question?
fn needs_input(screen: &str) -> bool {
    let t = screen.to_lowercase();
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

/// Codex lifecycle hooks, passed with `-c`. Codex only runs a hook once the
/// user has trusted it in its `/hooks` screen, and it records that trust
/// against a hash of this exact definition, one hook per event, by position.
/// Changing a byte here (or running trix from another path) asks for review
/// again; until then Codex skips them and `notify` still reports finished
/// turns.
pub const CODEX_HOOK_EVENTS: [&str; 8] =
    ["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse", "Stop", "Interrupt", "SessionEnd"];

fn codex_hooks(exe: &Path) -> String {
    let exe = exe.display().to_string();
    let command = format!("{} hook codex", shell_words::quote(&exe));
    // A JSON string is a valid TOML basic string, with escaping.
    let command = serde_json::to_string(&command).unwrap_or_default();
    let handler = format!("[{{hooks=[{{type=\"command\",command={command},timeout=3}}]}}]");
    let events: Vec<String> = CODEX_HOOK_EVENTS.iter().map(|e| format!("{e}={handler}")).collect();
    format!("hooks={{{}}}", events.join(","))
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
            if let Some(id) = &spec.resume {
                args.push("--resume".into());
                args.push(id.clone());
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
            if spec.resume.is_some() {
                args.push("resume".into());
            }
            args.push("-c".into());
            // A JSON string array is a valid TOML inline array, with escaping.
            let notify = serde_json::to_string(&[exe.display().to_string(), "hook".into(), "codex".into()]).unwrap_or_default();
            args.push(format!("notify={notify}"));
            args.push("-c".into());
            args.push(codex_hooks(exe));
            if let Some(extra) = spec.command.as_deref().filter(|c| !c.trim().is_empty()) {
                args.extend(split_args(extra));
            }
            if let Some(id) = &spec.resume {
                args.push(id.clone());
            } else if let Some(p) = spec.prompt.as_deref().filter(|p| !p.is_empty()) {
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

/// A real agent around `sleep`: a live PTY and emulator, but nothing on screen
/// and no output, so the state machine only sees what a test feeds it.
#[cfg(test)]
pub(crate) fn test_agent(kind: Kind) -> Agent {
    let (tx, _rx) = std::sync::mpsc::channel();
    let spec = Spawn {
        kind: Kind::Custom,
        name: "t".into(),
        cwd: std::env::temp_dir(),
        command: Some("exec sleep 60".into()),
        prompt: None,
        branch_note: None,
        resume: None,
    };
    let sock = std::env::temp_dir().join("trix-test.sock");
    let mut a = Agent::spawn(1, spec, (80, 24), tx, &sock, None, std::path::Path::new("/bin/true")).unwrap();
    a.kind = kind;
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(source: &str, event: &str, tool: Option<&str>, detail: Option<&str>) -> HookMsg {
        HookMsg {
            agent: 1,
            source: source.into(),
            event: event.into(),
            tool: tool.map(Into::into),
            detail: detail.map(Into::into),
            session: None,
        }
    }
    fn claude(event: &str, tool: Option<&str>, detail: Option<&str>) -> HookMsg {
        hook("claude", event, tool, detail)
    }
    fn ago(d: Duration) -> Instant {
        Instant::now().checked_sub(d).expect("uptime")
    }
    fn spec(kind: Kind) -> Spawn {
        Spawn { kind, name: "n".into(), cwd: "/tmp".into(), command: None, prompt: None, branch_note: None, resume: None }
    }

    #[test]
    fn foreground_job_from_the_terminal() {
        let a = test_agent(Kind::Shell);
        assert!(!a.running_command(), "sleep leads its own foreground group");
        a.shutdown();
        // A shell with job control hands the terminal to the command it runs.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut s = spec(Kind::Custom);
        s.command = Some("exec sh -mc 'sleep 5; true'".into());
        s.cwd = std::env::temp_dir();
        let a = Agent::spawn(1, s, (80, 24), tx, &std::env::temp_dir().join("trix-test.sock"), None, Path::new("/bin/true")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !a.running_command() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(a.running_command(), "sleep is in the foreground, not the shell");
        a.shutdown();
    }

    #[test]
    fn live_cwd_follows_the_process() {
        let a = test_agent(Kind::Shell);
        let real = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(real(&a.live_cwd()), real(&std::env::temp_dir()));
        a.shutdown();
    }

    // ── state transitions ──

    #[test]
    fn set_state_reports_changes_only() {
        let mut a = test_agent(Kind::Claude);
        assert_eq!(a.set_state(State::Working, true), Some(State::Starting));
        assert!(a.work_started.is_some());
        assert_eq!(a.set_state(State::Working, true), None);
        a.set_state(State::Blocked, true);
        assert!(a.work_started.is_some(), "blocked keeps the work timer");
        a.set_state(State::Idle, true);
        assert!(a.work_started.is_none());
    }

    #[test]
    fn unseen_only_when_not_looking() {
        let mut a = test_agent(Kind::Claude);
        a.set_state(State::Done, false);
        assert!(a.unseen);
        a.set_state(State::Working, false);
        a.set_state(State::Done, true);
        assert!(!a.unseen);
        a.set_state(State::Blocked, false);
        assert!(a.unseen);
    }

    // ── hooks ──

    #[test]
    fn a_full_claude_turn() {
        let mut a = test_agent(Kind::Claude);
        assert_eq!(a.on_hook(&claude("SessionStart", None, None), false), Some(State::Starting));
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "ready"));
        assert!(a.hooked);

        a.on_hook(&claude("UserPromptSubmit", None, Some("fix the tests")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "thinking"));
        assert_eq!(a.last_prompt.as_deref(), Some("fix the tests"));

        a.on_hook(&claude("PreToolUse", Some("Bash"), Some("cargo test")), false);
        assert_eq!((a.state, a.activity.as_str(), a.tools_used), (State::Working, "Bash cargo test", 1));

        a.on_hook(&claude("PermissionRequest", Some("Bash"), Some("rm -rf dist")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "approve Bash rm -rf dist"));
        assert!(a.unseen);

        a.on_hook(&claude("PostToolUse", Some("Bash"), None), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "Bash ✓"));

        a.on_hook(&claude("PreToolUse", Some("Read"), None), false);
        assert_eq!(a.activity, "Read", "no detail: just the tool");

        a.on_hook(&claude("Stop", None, None), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Done, "finished · 2 tools"));
    }

    #[test]
    fn a_reply_without_tools() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("UserPromptSubmit", None, None), true);
        a.on_hook(&claude("Stop", None, None), true);
        assert_eq!(a.activity, "replied");
        a.on_hook(&claude("UserPromptSubmit", None, None), true);
        a.on_hook(&claude("PreToolUse", Some("Edit"), Some("app.rs")), true);
        a.on_hook(&claude("Stop", None, None), true);
        assert_eq!(a.activity, "finished · 1 tool", "the count restarts each prompt");
    }

    #[test]
    fn questions_block() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("PreToolUse", Some("AskUserQuestion"), Some("Which db?")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Which db?"));
        let mut b = test_agent(Kind::Claude);
        b.on_hook(&claude("PreToolUse", Some("AskUserQuestion"), None), false);
        assert_eq!(b.activity, "has a question");
    }

    #[test]
    fn only_permission_notifications_block() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("UserPromptSubmit", None, None), true);
        assert_eq!(a.on_hook(&claude("Notification", None, Some("Claude is waiting for your input")), true), None);
        assert_eq!(a.state, State::Working);
        a.on_hook(&claude("Notification", None, Some("Claude needs your permission to use Bash")), true);
        assert_eq!(a.state, State::Blocked);
    }

    #[test]
    fn codex_turn_complete_uses_its_message() {
        let mut a = test_agent(Kind::Codex);
        a.set_state(State::Working, true);
        a.on_hook(&hook("codex", "agent-turn-complete", None, Some("All green")), true);
        assert_eq!((a.state, a.activity.as_str()), (State::Done, "All green"));
        assert!(!a.hooked && !a.reported, "codex keeps the heuristics");
    }

    #[test]
    fn latest_session_id_wins() {
        let mut a = test_agent(Kind::Claude);
        let mut m = claude("SessionStart", None, None);
        m.session = Some("first".into());
        a.on_hook(&m, true);
        let mut m = claude("Stop", None, None);
        a.on_hook(&m, true);
        assert_eq!(a.session.as_deref(), Some("first"), "a report without an id keeps the old one");
        m.session = Some("after-clear".into());
        a.on_hook(&m, true);
        assert_eq!(a.session.as_deref(), Some("after-clear"));
    }

    #[test]
    fn exited_agents_ignore_late_hooks() {
        let mut a = test_agent(Kind::Claude);
        a.set_state(State::Exited, true);
        assert_eq!(a.on_hook(&claude("SessionEnd", None, None), true), None);
        assert_eq!(a.state, State::Exited);
        assert_eq!(a.heuristics(true), None);
    }

    #[test]
    fn unknown_events_change_nothing() {
        let mut a = test_agent(Kind::Claude);
        a.set_state(State::Idle, true);
        assert_eq!(a.on_hook(&claude("SomethingNew", None, None), true), None);
        assert_eq!(a.state, State::Idle);
    }

    // ── heuristics ──

    #[test]
    fn starting_settles_to_idle() {
        let mut a = test_agent(Kind::Shell);
        assert_eq!(a.heuristics(true), None, "too early");
        a.created = ago(Duration::from_secs(2));
        a.last_output = ago(Duration::from_secs(1));
        assert_eq!(a.heuristics(true), Some(State::Starting));
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "ready"));
    }

    #[test]
    fn hooked_turn_that_went_silent_was_interrupted() {
        let mut a = test_agent(Kind::Claude);
        a.hooked = true;
        a.set_state(State::Working, true);
        a.last_output = ago(Duration::from_secs(3));
        assert_eq!(a.heuristics(true), None, "spinner pauses happen");
        a.last_output = ago(Duration::from_secs(7));
        a.heuristics(true);
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "interrupted"));
    }

    #[test]
    fn dismissed_prompt_stops_blocking() {
        let mut a = test_agent(Kind::Claude);
        a.hooked = true;
        a.set_state(State::Blocked, true);
        a.state_since = ago(Duration::from_secs(5));
        a.last_output = ago(Duration::from_secs(3));
        assert_eq!(a.heuristics(true), None, "nobody pressed anything");
        a.dismissed_at = Some(ago(Duration::from_secs(3)));
        a.heuristics(true);
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "dismissed"));
    }

    fn show(a: &Agent, text: &str) {
        let mut p: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        p.advance(&mut *a.term.lock(), text.replace('\n', "\r\n").as_bytes());
    }

    #[test]
    fn trust_dialog_after_session_start_needs_you() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("SessionStart", None, None), false);
        assert_eq!(a.state, State::Idle);
        show(&a, " Quick safety check: Is this a project you trust?\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel\n");
        a.state_since = ago(Duration::from_secs(2));
        a.last_output = ago(Duration::from_secs(1));
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "waiting for an answer"));
        a.shutdown();
    }

    #[test]
    fn prose_after_a_turn_is_not_a_question() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("SessionStart", None, None), false);
        a.on_hook(&claude("UserPromptSubmit", None, None), false);
        a.on_hook(&claude("Interrupt", None, None), false);
        assert_eq!(a.state, State::Idle);
        show(&a, "Would you like to add tests too? (yes/no)\n");
        a.state_since = ago(Duration::from_secs(2));
        a.last_output = ago(Duration::from_secs(1));
        assert_eq!(a.heuristics(false), None, "hooks report this agent's prompts");
        a.shutdown();
    }

    #[test]
    fn a_first_prompt_is_pending_until_its_turn_shows() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut s = spec(Kind::Custom);
        s.command = Some("exec sleep 60".into());
        s.prompt = Some("go".into());
        s.cwd = std::env::temp_dir();
        let mut a = Agent::spawn(1, s, (80, 24), tx, &std::env::temp_dir().join("trix-test.sock"), None, Path::new("/bin/true")).unwrap();
        assert!(a.prompted.is_some());
        a.set_state(State::Idle, false);
        assert!(a.prompted.is_some(), "SessionStart's idle is not the turn");
        a.set_state(State::Working, false);
        assert!(a.prompted.is_none());
        a.shutdown();
    }

    #[test]
    fn unhooked_long_work_is_done_short_work_is_idle() {
        let mut a = test_agent(Kind::Claude);
        a.set_state(State::Working, true);
        a.work_started = Some(ago(Duration::from_secs(10)));
        a.last_output = ago(Duration::from_secs(2));
        a.heuristics(true);
        assert_eq!(a.state, State::Done);
        assert!(a.activity.starts_with("finished after"), "{}", a.activity);

        let mut b = test_agent(Kind::Claude);
        b.set_state(State::Working, true);
        b.work_started = Some(ago(Duration::from_secs(1)));
        b.last_output = ago(Duration::from_secs(2));
        b.heuristics(true);
        assert_eq!(b.state, State::Idle);
    }

    #[test]
    fn sustained_output_means_working() {
        let mut a = test_agent(Kind::Shell);
        a.set_state(State::Idle, true);
        for _ in 0..5 {
            a.burst.push_back(Instant::now());
        }
        assert_eq!(a.heuristics(true), None, "a few bursts are not work yet");
        a.burst.push_back(Instant::now());
        a.last_output = Instant::now();
        a.heuristics(true);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "working"));
    }

    #[test]
    fn self_reporting_tools_skip_heuristics() {
        let mut a = test_agent(Kind::Custom);
        a.on_hook(&hook("mytool", "UserPromptSubmit", None, None), true);
        assert!(a.reported);
        a.last_output = ago(Duration::from_secs(60));
        a.work_started = Some(ago(Duration::from_secs(60)));
        assert_eq!(a.heuristics(true), None);
        assert_eq!(a.state, State::Working);
    }

    #[test]
    fn keystroke_echo_is_not_activity() {
        let mut a = test_agent(Kind::Shell);
        a.last_input = Instant::now();
        a.last_resize = ago(Duration::from_secs(10));
        a.on_output();
        assert!(a.burst.is_empty());
        a.last_input = ago(Duration::from_secs(10));
        a.on_output();
        assert_eq!(a.burst.len(), 1);
        assert_eq!(*a.spark.back().unwrap(), 2, "both still count for the sparkline");
    }

    // ── screen prompts ──

    #[test]
    fn prompts_on_screen_need_input() {
        assert!(needs_input("Do you want to proceed?\n ❯ 1. Yes\n   2. No"));
        assert!(needs_input("Allow command `rm -rf dist`? (y/N)"));
        assert!(needs_input("DO YOU TRUST THIS FOLDER?"));
        assert!(needs_input("Press Enter to confirm"));
    }

    #[test]
    fn ordinary_prose_does_not() {
        assert!(!needs_input(""));
        assert!(!needs_input("I'd approve of this approach; the yes branch is fine."));
        assert!(!needs_input("$ cargo test\n test result: ok"));
    }

    // ── command lines ──

    #[test]
    fn claude_command_line() {
        let mut s = spec(Kind::Claude);
        s.resume = Some("abc".into());
        s.command = Some(r#"--model opus --append-system-prompt "be terse""#.into());
        s.prompt = Some("fix it".into());
        let (prog, args) = command_for(&s, Some(Path::new("/run/s.json")), Path::new("/bin/trix"));
        assert_eq!(prog, "claude");
        assert_eq!(args, ["--settings", "/run/s.json", "--resume", "abc", "--model", "opus", "--append-system-prompt", "be terse", "fix it"]);

        let mut s = spec(Kind::Claude);
        s.command = Some("   ".into());
        s.prompt = Some(String::new());
        assert!(command_for(&s, None, Path::new("/bin/trix")).1.is_empty());
    }

    #[test]
    fn codex_command_line() {
        let notify = r#"notify=["/bin/trix","hook","codex"]"#;
        let hooks = codex_hooks(Path::new("/bin/trix"));
        let mut s = spec(Kind::Codex);
        s.prompt = Some("hi".into());
        let (prog, args) = command_for(&s, None, Path::new("/bin/trix"));
        assert_eq!(prog, "codex");
        assert_eq!(args, ["-c", notify, "-c", &hooks, "hi"]);

        // Resuming: the id goes last and a first prompt makes no sense.
        s.resume = Some("t-1".into());
        s.command = Some("--full-auto".into());
        assert_eq!(command_for(&s, None, Path::new("/bin/trix")).1, ["resume", "-c", notify, "-c", &hooks, "--full-auto", "t-1"]);
    }

    #[test]
    fn codex_hooks_match_what_was_trusted() {
        // Codex trusts a hook by a hash of its exact definition. This is the
        // string trusted with `/hooks` on 2026-11-11; if it changes, every
        // user has to review trix's hooks again.
        let h = r#"{hooks=[{type="command",command="/home/fleo/.cargo/bin/trix hook codex",timeout=3}]}"#;
        let want = format!(
            "hooks={{SessionStart=[{h}],UserPromptSubmit=[{h}],PreToolUse=[{h}],PermissionRequest=[{h}],PostToolUse=[{h}],Stop=[{h}],Interrupt=[{h}],SessionEnd=[{h}]}}"
        );
        assert_eq!(codex_hooks(Path::new("/home/fleo/.cargo/bin/trix")), want);
    }

    #[test]
    fn codex_hooks_quote_odd_paths() {
        let h = codex_hooks(Path::new("/opt/my tools/trix"));
        assert!(h.contains(r#"command="'/opt/my tools/trix' hook codex""#), "{h}");
        let parsed: toml::Table = h.parse().expect("valid TOML");
        let first = &parsed["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(first["command"].as_str(), Some("'/opt/my tools/trix' hook codex"));
        assert_eq!(first["timeout"].as_integer(), Some(3));
    }

    #[test]
    fn codex_hooks_take_over_from_heuristics() {
        let mut a = test_agent(Kind::Codex);
        a.on_hook(&hook("codex", "agent-turn-complete", None, None), true);
        assert!(!a.hooked, "notify alone isn't a lifecycle hook");
        a.on_hook(&hook("codex", "SessionStart", None, None), true);
        assert!(a.hooked && !a.reported);
        a.on_hook(&hook("codex", "UserPromptSubmit", None, Some("fix it")), true);
        a.on_hook(&hook("codex", "PermissionRequest", Some("exec_command"), Some("cargo test")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "approve exec_command cargo test"));
        a.on_hook(&hook("codex", "PostToolUse", Some("exec_command"), None), false);
        assert_eq!(a.state, State::Working);
        a.on_hook(&hook("codex", "Interrupt", None, None), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "interrupted"));
    }

    #[test]
    fn shell_and_custom_command_lines() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        assert_eq!(command_for(&spec(Kind::Shell), None, Path::new("/bin/trix")), (shell.clone(), vec![]));
        let mut s = spec(Kind::Custom);
        s.command = Some("npm run dev".into());
        assert_eq!(command_for(&s, None, Path::new("/bin/trix")), (shell, vec!["-lc".into(), "npm run dev".into()]));
    }

    #[test]
    fn unbalanced_quotes_fall_back_to_words() {
        assert_eq!(split_args(r#"--a "b c""#), ["--a", "b c"]);
        assert_eq!(split_args(r#"--a "b"#), ["--a", "\"b"]);
    }

    #[test]
    fn kind_labels_roundtrip() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_label(k.label()), Some(k));
        }
        assert_eq!(Kind::from_label("vim"), None);
        assert!(!Kind::Shell.is_agent() && Kind::Custom.is_agent());
    }
}
