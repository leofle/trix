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
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use crate::app::AppEvent;
use crate::hooks::{self, HookMsg, Integrations};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Claude,
    Codex,
    /// xAI's Grok Build CLI (`grok`).
    Grok,
    /// OpenCode (`opencode`).
    OpenCode,
    /// Cursor's CLI agent, started as `cursor-agent`: newer installs also
    /// name it `agent`, but Grok's installer claims that name too.
    Cursor,
    /// GitHub Copilot CLI (`copilot`).
    Copilot,
    Shell,
    Custom,
}

impl Kind {
    pub const ALL: [Kind; 8] = [Kind::Claude, Kind::Codex, Kind::Grok, Kind::OpenCode, Kind::Cursor, Kind::Copilot, Kind::Shell, Kind::Custom];
    pub fn label(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Grok => "grok",
            Kind::OpenCode => "opencode",
            Kind::Cursor => "cursor",
            Kind::Copilot => "copilot",
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
    /// A known coding agent, so typing a prompt into it is safe. A custom
    /// command may be anything (bash, ssh, psql), so it is not.
    pub fn takes_prompts(self) -> bool {
        matches!(self, Kind::Claude | Kind::Codex | Kind::Grok | Kind::OpenCode | Kind::Cursor | Kind::Copilot)
    }
    /// It can be started again in the conversation it was in.
    pub fn resumes(self) -> bool {
        self.takes_prompts()
    }
    /// No hook reports its approval prompts, so while it works, its own
    /// prompt UI on screen is how trix knows it needs you.
    fn prompts_only_on_screen(self) -> bool {
        matches!(self, Kind::Grok | Kind::Cursor)
    }
    /// Kinds whose own approval wording `kind_prompt` knows, so a prompt on
    /// screen turns them blocked mid-turn. OpenCode's come as events too:
    /// the screen is a second source, never the only one.
    fn reads_prompts_on_screen(self) -> bool {
        self.prompts_only_on_screen() || self == Kind::OpenCode
    }
    /// The new-agent dialog's letter for this kind, on its main row or under
    /// "more ▸". Not `h` or `l`, which step through the kinds.
    pub fn shortcut(self) -> Option<char> {
        match self {
            Kind::Claude => Some('c'),
            Kind::Codex => Some('x'),
            Kind::Grok => Some('g'),
            Kind::OpenCode => Some('o'),
            Kind::Cursor => Some('u'),
            Kind::Copilot => Some('p'),
            Kind::Shell => Some('s'),
            Kind::Custom => None,
        }
    }
    pub fn from_shortcut(c: char) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.shortcut() == Some(c))
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
            TermEvent::ColorRequest(index, f) => AppEvent::ColorRequest(self.id, index, f),
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

#[derive(Debug)]
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
    /// Writes due later, sent in order by one thread (see `submit`).
    later: Sender<(Instant, Cow<'static, [u8]>)>,
    /// When the last submission's Enter goes out.
    submit_free: Instant,
    /// `screen_needs_input`'s last answer, and the (last output, last
    /// resize) it was for.
    screen_seen: std::cell::Cell<Option<ScreenSeen>>,
    /// It has been hung up on (`hang_up`): stopping it again does nothing.
    stopping: std::cell::Cell<bool>,
    /// Anything has ever been asked of it: a first prompt, `trix send`, or
    /// keys typed into it. Until then, an agent's output is it starting up
    /// (Codex redraws for seconds), never finished work.
    asked: bool,
    /// The folder its process was last seen in, for once it has exited.
    last_cwd: std::cell::RefCell<Option<PathBuf>>,
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
    /// Its conversation log (Claude reports it in every hook).
    pub transcript: Option<PathBuf>,
    /// How full its context window is, from that log.
    pub context: Option<crate::usage::Context>,
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
    /// When the newest hook applied had fired (`HookMsg::seq`), so a report
    /// that was overtaken on its way here is dropped, not applied late.
    last_hook_seq: u64,
    /// Grok: when its terminal title last said "Action Required". The label
    /// blinks while Grok thinks its terminal is unfocused, so it counts for
    /// a moment after it goes.
    title_attention: Option<Instant>,
    /// State changes `trix events` hasn't told about yet: (from, to, the
    /// activity then). Two in one turn of the server loop both show.
    pub changes: Vec<(State, State, String)>,
    /// `session` is an id trix made up for a new Grok conversation, not one
    /// it resumed: no conversation exists under it until a turn.
    pub session_named_by_trix: bool,
    /// The agent wasn't installed, and its pane is installing it ("installing
    /// grok (npm)"). It stays starting until the installer says it's done:
    /// install output isn't the agent at work. See `install`.
    pub installing: Option<String>,
    /// Copilot: the tool its last `PreToolUse` named, until it ran. Its
    /// permission prompt comes as a notification with no tool in it, so
    /// this says what the approval is for.
    pending_tool: Option<String>,
}

pub const SPARK_LEN: usize = 48;

/// How far apart, in µs, two hook reports can arrive out of order: separate
/// processes, read on separate threads, so milliseconds in practice.
const HOOK_REORDER_US: u64 = 5_000_000;

/// How long a program may take to exit once hung up on, and then once told
/// to terminate, before it's made to.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// One thread per agent that sends each write when it's due, in the order
/// queued. It ends when the agent (the only sender) goes away.
fn delayed_writer(sender: EventLoopSender) -> Sender<(Instant, Cow<'static, [u8]>)> {
    let (tx, rx) = mpsc::channel::<(Instant, Cow<'static, [u8]>)>();
    let _ = std::thread::Builder::new().name("trix-later".into()).spawn(move || {
        for (due, bytes) in rx {
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            if sender.send(Msg::Input(bytes)).is_err() {
                break;
            }
        }
    });
    tx
}

impl Agent {
    pub fn spawn(
        id: u32,
        spec: Spawn,
        size: (u16, u16),
        tx: Sender<AppEvent>,
        socket: &std::path::Path,
        integrations: &Integrations,
        exe: &std::path::Path,
    ) -> anyhow::Result<Agent> {
        // Grok and Copilot take the id of a new conversation from their
        // caller, so trix knows what to resume without asking them.
        let fresh = new_conversation(&spec);
        let (program, args) = command_for(&spec, integrations, exe, fresh.as_deref());
        let first_prompt = spec.kind.is_agent() && spec.prompt.as_deref().is_some_and(|p| !p.trim().is_empty());
        let command_line = std::iter::once(program.clone())
            .chain(args.iter().map(|a| if a.contains(' ') { format!("{a:?}") } else { a.clone() }))
            .collect::<Vec<_>>()
            .join(" ");
        // An agent that isn't installed installs in its pane first, then runs.
        let installing = if spec.kind.takes_prompts() && crate::install::missing(&program) {
            let via = crate::install::pick(spec.kind, std::env::var_os("PATH").as_deref())
                .ok_or_else(|| anyhow::anyhow!("{program} isn't installed, and installing it needs {}", crate::install::needs(spec.kind)))?;
            Some(format!("installing {program} ({})", via.label()))
        } else {
            None
        };
        let (program, args) = if installing.is_some() { crate::install::wrap(exe, spec.kind, program, args) } else { (program, args) };

        let mut env = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "trix".into());
        env.insert("TRIX".into(), "1".into());
        env.insert("TRIX_SOCKET".into(), socket.display().to_string());
        env.insert("TRIX_AGENT_ID".into(), id.to_string());
        env.insert("TRIX_AGENT_NAME".into(), spec.name.clone());
        env.insert("TRIX_SESSION".into(), crate::session::name().into());
        if spec.kind == Kind::Codex
            && let Some(chain) = codex_user_notify()
        {
            env.insert("TRIX_CODEX_NOTIFY".into(), chain);
        }
        if spec.kind == Kind::OpenCode {
            let theirs = std::env::var("OPENCODE_CONFIG_CONTENT").ok();
            if let Some(c) = integrations.opencode_plugin_for_version().and_then(|p| hooks::opencode_config(p, theirs.as_deref())) {
                env.insert("OPENCODE_CONFIG_CONTENT".into(), c);
            }
        }
        for (k, v) in theme_env(spec.kind, integrations.themed, |k| std::env::var_os(k).is_some()) {
            env.insert(k.into(), v.into());
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
        let later = delayed_writer(sender.clone());

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
            later,
            submit_free: Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now),
            screen_seen: std::cell::Cell::new(None),
            stopping: std::cell::Cell::new(false),
            asked: first_prompt,
            last_cwd: std::cell::RefCell::new(None),
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
            activity: installing.clone().or(spec.branch_note).unwrap_or_else(|| "launching".into()),
            last_prompt: spec.prompt,
            // Copilot is restored with `--session-id`, which resumes the
            // conversation or starts one under that id: always worth saving.
            session_named_by_trix: fresh.is_some() && spec.kind == Kind::Grok,
            session: spec.resume.or(fresh),
            title: None,
            exit_code: None,
            last_output: now,
            last_input: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            spark,
            spark_tick: now,
            burst: VecDeque::new(),
            work_started: None,
            tools_used: 0,
            branch: None,
            diff: None,
            transcript: None,
            context: None,
            prompted: first_prompt.then(Instant::now),
            turn_seen: false,
            managed_by: None,
            last_hook_seq: 0,
            title_attention: None,
            changes: Vec::new(),
            installing,
            pending_tool: None,
        })
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let b = bytes.into();
        if !b.is_empty() {
            let _ = self.sender.send(Msg::Input(b));
        }
    }

    /// Type `paste` and press Enter a beat later, so TUIs see the paste and
    /// the Enter as separate events. A submission made before the previous
    /// one's Enter has gone out waits for it: two quick `trix send`s stay two
    /// prompts instead of pasting both and then pressing Enter twice.
    pub fn submit(&mut self, paste: Vec<u8>) {
        const ENTER_AFTER: Duration = Duration::from_millis(80);
        const GAP: Duration = Duration::from_millis(30);
        let now = Instant::now();
        self.last_input = now;
        self.note_asked();
        let start = now.max(self.submit_free + GAP);
        let enter = start + ENTER_AFTER;
        let _ = self.later.send((start, Cow::Owned(paste)));
        let _ = self.later.send((enter, Cow::Borrowed(&b"\r"[..])));
        self.submit_free = enter;
    }

    /// Input counts as asking it something, except an answer to a prompt
    /// or dialog it's waiting on (trust this folder? these hooks?).
    fn note_asked(&mut self) {
        if self.state != State::Blocked {
            self.asked = true;
        }
    }

    pub fn user_input(&mut self, bytes: Vec<u8>) {
        self.last_input = Instant::now();
        self.note_asked();
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

    /// Stop it: hang up, and if its program ignores that, end it anyway in
    /// the background (see `platform::stop`).
    pub fn shutdown(&self) {
        if self.hang_up() {
            let pid = self.pid;
            let _ = std::thread::Builder::new().name("trix-stop".into()).spawn(move || crate::platform::stop(&[pid], STOP_GRACE));
        }
    }

    /// Close its terminal, which hangs up on its program. False if that
    /// was done already.
    pub fn hang_up(&self) -> bool {
        if self.stopping.replace(true) {
            return false;
        }
        let _ = self.sender.send(Msg::Shutdown);
        true
    }

    pub fn set_state(&mut self, s: State, focused: bool) -> Option<State> {
        if self.state == s {
            return None;
        }
        let prev = self.state;
        self.state = s;
        self.state_since = Instant::now();
        // Bounded: without subscribers, nothing drains it but the server loop.
        if self.changes.len() >= 64 {
            self.changes.remove(0);
        }
        self.changes.push((prev, s, self.activity.clone()));
        // The turn a prompt was sent for has visibly begun (or it is waiting
        // on the human, or gone). Idle isn't evidence of that, and neither is
        // Done: a prompt sent mid-turn waits for that turn to end first.
        if !matches!(s, State::Idle | State::Done) {
            self.prompted = None;
        }
        match s {
            State::Working => {
                if self.work_started.is_none() {
                    self.work_started = Some(Instant::now());
                }
            }
            State::Done => self.unseen = !focused,
            State::Blocked => {
                self.unseen = !focused;
                // Output from before it asked (a spinner, streamed text) is
                // not the agent going on: only what comes after may count.
                self.burst.clear();
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

    /// Its install is over and the agent itself is starting: it gets the
    /// usual start-up time from now.
    fn installed(&mut self) {
        if self.installing.take().is_some() {
            self.created = Instant::now();
            self.activity = "launching".into();
        }
    }

    /// Apply a hook report. Returns the previous state when it changed.
    pub fn on_hook(&mut self, msg: &HookMsg, focused: bool) -> Option<State> {
        // A late report (e.g. SessionEnd racing ChildExit) must not revive a
        // dead pane: Enter-to-restart only works while it reads as exited.
        if self.state == State::Exited {
            return None;
        }
        // trix's own installer is done: the agent starts now.
        if msg.source == "trix" {
            if msg.event == "Installed" {
                self.installed();
            }
            return None;
        }
        // The agent reporting at all means it is installed and running.
        self.installed();
        // Hooks are separate processes read on separate threads, so a
        // PostToolUse can land after the Stop that followed it and flip a
        // finished agent back to Working.
        // The stamps are wall-clock time, though: one far older than the
        // newest wasn't overtaken, the clock went back (NTP, a resume), and
        // dropping it would drop every report until the clock caught up.
        if msg.seq != 0 {
            if msg.seq < self.last_hook_seq && self.last_hook_seq - msg.seq < HOOK_REORDER_US {
                return None;
            }
            self.last_hook_seq = msg.seq;
        }
        // `/clear` and friends start a new conversation: always track the latest.
        if let Some(id) = &msg.session {
            self.session = Some(id.clone());
        }
        if let Some(p) = &msg.transcript {
            self.transcript = Some(PathBuf::from(p));
        }
        match msg.source.as_str() {
            "claude" => self.hooked = true,
            // `notify` only covers finished turns. Anything else is one of
            // Codex's lifecycle hooks, which it runs once they're trusted.
            "codex" if msg.event != "agent-turn-complete" => self.hooked = true,
            "codex" => {}
            // trix's own plugin (OpenCode, Copilot) or the user's hooks (Grok).
            "opencode" | "grok" | "copilot" => self.hooked = true,
            // Cursor runs only `sessionStart` and the tool hooks from trix's
            // plugin (cursor-agent 2026.11.11): no prompt, no turn end. Its
            // screen says when a turn starts and ends; its hooks add what it
            // ran and which conversation it is.
            "cursor" => {}
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
                self.pending_tool = None;
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
                self.pending_tool = Some(self.activity.clone());
                self.set_state(State::Working, focused)
            }
            "PostToolUse" => {
                self.pending_tool = None;
                if self.state == State::Blocked {
                    self.activity = format!("{tool} ✓");
                }
                self.set_state(State::Working, focused)
            }
            // Grok's permission prompt carries a message, not a tool: show it as is.
            "PermissionRequest" if msg.source == "grok" && msg.detail.as_deref().is_some_and(|d| !d.is_empty()) => {
                self.activity = hooks::one_line(msg.detail.as_deref().unwrap_or_default(), 60);
                self.set_state(State::Blocked, focused)
            }
            // Copilot's permission prompt comes as a notification naming no
            // tool: the tool it was about to run is what it asks about.
            "PermissionRequest" if msg.source == "copilot" => {
                self.activity = match (&self.pending_tool, msg.detail.as_deref().filter(|d| !d.is_empty())) {
                    (Some(t), _) => format!("approve {t}"),
                    (None, Some(m)) => hooks::one_line(m, 60),
                    (None, None) => "needs your approval".into(),
                };
                self.set_state(State::Blocked, focused)
            }
            // Claude asks permission to show its own question: the question
            // is what you're being asked.
            "PermissionRequest" if tool == "AskUserQuestion" => {
                self.activity = msg.detail.clone().unwrap_or_else(|| "has a question".into());
                self.set_state(State::Blocked, focused)
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
            // OpenCode reports its session idle at start too, and the new
            // agents' turn ends mean nothing before a turn began.
            "Stop" if matches!(msg.source.as_str(), "opencode" | "cursor" | "grok" | "copilot") && !self.turn_seen => None,
            "Stop" | "agent-turn-complete" => {
                self.pending_tool = None;
                self.activity = match self.tools_used {
                    0 => "replied".into(),
                    n => format!("finished · {n} tool{}", if n == 1 { "" } else { "s" }),
                };
                if let Some(d) = &msg.detail
                    && msg.event == "agent-turn-complete"
                {
                    self.activity = d.clone();
                }
                self.set_state(State::Done, focused)
            }
            "Interrupt" => {
                self.activity = "interrupted".into();
                self.set_state(State::Idle, focused)
            }
            // A tool that already ran, from agents that report tools only
            // once they're done (Cursor). Cursor's screen says whether it's
            // working: this names what it did, and one that lands after the
            // turn ended changes nothing.
            "ToolDone" => {
                self.turn_seen = true;
                self.tools_used += 1;
                if self.state == State::Working {
                    self.activity = format!("{} ✓", described(&msg.detail));
                }
                None
            }
            // Copilot's `ask_user` is on screen already (its `PreToolUse`
            // named the question): its dialog's notification adds nothing.
            "Question" if msg.source == "copilot" && self.state == State::Blocked => None,
            // The agent asks the human something (OpenCode's question tool,
            // a Copilot dialog).
            "Question" => {
                self.activity = msg.detail.clone().unwrap_or_else(|| "has a question".into());
                self.set_state(State::Blocked, focused)
            }
            // A permission or a question was answered: back to work.
            "Answered" => {
                self.activity = "working".into();
                self.set_state(State::Working, focused)
            }
            // Busy without a prompt seen (a queued message, a retry).
            "Busy" if self.state != State::Working && self.state != State::Blocked => {
                self.turn_seen = true;
                self.activity = "working".into();
                self.set_state(State::Working, focused)
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
            if self.installing.is_some() {
                return None;
            }
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

        // Grok's title says it's idle: output now is a redraw, not a turn.
        // Right after a turn, Grok names the conversation and redraws, which
        // would otherwise read as a new turn and undo its Done.
        let mut title_idle = false;
        if self.kind == Kind::Grok {
            // Its own question or approval UI on screen outranks the title,
            // which keeps spinning (so the screen is never quiet) while it
            // waits. Answered, the UI goes, and the title takes over again.
            if matches!(self.state, State::Working | State::Blocked) && self.screen_kind_prompt() {
                if self.state == State::Blocked {
                    return None;
                }
                self.activity = "needs your approval".into();
                return self.set_state(State::Blocked, focused);
            }
        }
        // A blink of the label that has gone counts only while it still
        // waits, or if it came since the state last changed (the label
        // blinked off before a tick saw it). Not once the approval is
        // answered: that would turn it straight back to blocked.
        let latched = self
            .title_attention
            .is_some_and(|t| t.elapsed() < GROK_BLINK && (self.state == State::Blocked || t > self.state_since));
        if self.kind == Kind::Grok
            && let Some(sig) = grok_title(self.title.as_deref(), latched)
        {
            title_idle = sig == TitleSignal::Idle;
            if let Some(s) = self.apply_title(sig, quiet, focused) {
                return s;
            }
        }

        // Grok and Cursor have no hook for their approval prompts: the
        // prompt on screen is the sign, read only from their own wording,
        // once the screen has been still for half a second. OpenCode's TUI
        // may keep redrawing under its prompt, so it gets no quiet spell:
        // the wording alone is specific enough.
        if self.kind.reads_prompts_on_screen()
            && self.state == State::Working
            && (self.kind == Kind::OpenCode || quiet > Duration::from_millis(500))
            && self.screen_kind_prompt()
        {
            self.activity = "needs your approval".into();
            return self.set_state(State::Blocked, focused);
        }
        if self.kind.prompts_only_on_screen() {
            // The agent's prose ("would you like to…") can stay on screen
            // after the prompt goes: only its own prompt wording keeps it.
            if self.hooked && self.state == State::Blocked && quiet > Duration::from_secs(2) && !self.screen_kind_prompt() {
                self.activity = "working".into();
                return self.set_state(State::Working, focused);
            }
        }

        // Grok and Cursor ask only through their own prompt UI. Their prose
        // ("would you like me to…?") at the end of a turn is not a prompt.
        let screen_question = || if self.kind.prompts_only_on_screen() { self.screen_kind_prompt() } else { self.screen_needs_input() };

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
            // OpenCode reports an interrupted turn itself.
            if self.state == State::Working && quiet > Duration::from_secs(6) && self.kind != Kind::OpenCode {
                if screen_question() {
                    self.activity = "waiting for an answer".into();
                    return self.set_state(State::Blocked, focused);
                }
                self.activity = "interrupted".into();
                return self.set_state(State::Idle, focused);
            }
            // OpenCode reports the answer, and redraws while it waits.
            if self.state == State::Blocked && busy && self.kind != Kind::OpenCode && !screen_question() {
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
                    // An agent nobody has asked anything was starting up.
                    if long && (self.asked || !self.kind.takes_prompts()) {
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
            State::Idle | State::Done if busy && !title_idle => {
                // Cursor's title names the conversation, not what it does.
                self.activity = self.title.clone().filter(|_| self.kind != Kind::Cursor).unwrap_or_else(|| "working".into());
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
        self.screen_prompts().0
    }

    /// Does the screen show this agent's own approval prompt, by wording only
    /// it uses? Stricter than `screen_needs_input`: safe mid-turn, when the
    /// agent's prose is on screen too.
    fn screen_kind_prompt(&self) -> bool {
        self.screen_prompts().1
    }

    fn screen_prompts(&self) -> (bool, bool) {
        // Only output or a resize changes the screen: while an agent sits
        // quiet, the heuristics ask this 4 times a second, for nothing.
        let key = (self.last_output, self.last_resize);
        if let Some((k, v)) = self.screen_seen.get()
            && k == key
        {
            return v;
        }
        let text = self.screen_bottom();
        let own = kind_prompt(self.kind, &text);
        let v = (own || needs_input(&text), own);
        self.screen_seen.set(Some((key, v)));
        v
    }

    /// A new terminal title. Grok's says when it's busy and when it waits
    /// on an approval.
    pub fn on_title(&mut self, t: String) {
        if self.kind == Kind::Grok && t.contains(GROK_ATTENTION) {
            self.title_attention = Some(Instant::now());
        }
        self.title = Some(t);
    }

    /// Apply what Grok's title says. `None`: it doesn't say, carry on with
    /// the other signals.
    fn apply_title(&mut self, sig: TitleSignal, quiet: Duration, focused: bool) -> Option<Option<State>> {
        match sig {
            TitleSignal::Attention if self.state == State::Blocked => Some(None),
            TitleSignal::Attention => {
                self.activity = "needs your approval".into();
                Some(self.set_state(State::Blocked, focused))
            }
            // Blocked by the title (seen as it turned Blocked, or since), and
            // the label gone for good: the approval was answered.
            _ if self.state == State::Blocked
                && self.title_attention.is_some_and(|t| self.state_since.checked_sub(Duration::from_secs(1)).is_none_or(|s| t >= s)) =>
            {
                if matches!(sig, TitleSignal::Busy(_)) {
                    self.activity = "working".into();
                    return Some(self.set_state(State::Working, focused));
                }
                // Idle again: the turn ended with (or right after) the answer.
                Some(self.end_turn(focused))
            }
            // The user's own Grok hooks report the turn: the title only adds
            // approvals.
            _ if self.hooked => None,
            TitleSignal::Busy(what) => {
                self.turn_seen = true;
                if self.state == State::Working {
                    if let Some(w) = what {
                        self.activity = w;
                    }
                    return Some(None);
                }
                self.activity = what.unwrap_or_else(|| "working".into());
                Some(self.set_state(State::Working, focused))
            }
            TitleSignal::Idle if self.state == State::Working && quiet > Duration::from_millis(500) => Some(self.end_turn(focused)),
            // Idle: dialogs at start (trust this folder) are on screen only.
            TitleSignal::Idle => None,
        }
    }

    /// Grok's title went idle after work: done if the turn was long enough
    /// to be worth a look, idle if not.
    fn end_turn(&mut self, focused: bool) -> Option<State> {
        let worked = self.work_started.map(|t| t.elapsed()).unwrap_or_default();
        if worked > Duration::from_secs(3) {
            self.activity = format!("finished after {}", crate::ui::fmt_dur(worked));
            self.set_state(State::Done, focused)
        } else {
            self.activity = "idle".into();
            self.set_state(State::Idle, focused)
        }
    }

    fn screen_bottom(&self) -> String {
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
        text
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

    /// Where its process is now. Once it has exited, its pid may be another
    /// process's: where it was last seen, or where it started.
    pub fn live_cwd(&self) -> PathBuf {
        let last = || self.last_cwd.borrow().clone().unwrap_or_else(|| self.cwd.clone());
        if self.state == State::Exited {
            return last();
        }
        match crate::platform::cwd_of(self.pid) {
            Some(p) => {
                *self.last_cwd.borrow_mut() = Some(p.clone());
                p
            }
            None => last(),
        }
    }

    #[allow(dead_code)]
    pub fn cursor_point(&self) -> Point {
        self.term.lock().grid().cursor.point
    }
}

/// An agent that goes away takes its process and terminal with it. The app
/// shuts agents down before it drops them anyway; this covers the rest
/// (tests above all), where the event loop would otherwise keep the PTY
/// open until the program inside exits on its own.
impl Drop for Agent {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Does this screen text show an approval prompt or a question?
fn needs_input(screen: &str) -> bool {
    let t = screen.to_lowercase();
    // Only phrasings of actual prompts — a bare word like "approve" shows
    // up in ordinary agent prose and code.
    const PATTERNS: [&str; 14] = [
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
        // Claude Code's question dialog (AskUserQuestion).
        "enter to select",
    ];
    PATTERNS.iter().any(|p| t.contains(p))
}

/// Wording only one agent's own approval prompts use, from their sources:
/// Grok Build's permission footer and question dialog
/// (`xai-grok-pager`), OpenCode's permission prompt
/// (`packages/tui/src/routes/session/permission.tsx`), Cursor CLI's
/// approval prompts and workspace-trust screen, Copilot CLI's folder trust
/// and approval prompt, and Codex's folder-trust
/// and hooks-review dialogs (as Codex 0.167.2 draws them).
fn kind_prompt(kind: Kind, screen: &str) -> bool {
    let t = screen.to_lowercase();
    let any = |ps: &[&str]| ps.iter().any(|p| t.contains(p));
    match kind {
        Kind::Grok => {
            any(&["ctrl+o:always-approve", "ctrl+o:yolo", "no, reject (type to add feedback)", "tab:next answer", "x:dismiss"])
                || (t.contains("yes, proceed") && t.contains("no, quit"))
        }
        Kind::OpenCode => t.contains("△ permission required") || (t.contains("allow once") && t.contains("allow always")),
        Kind::Cursor => {
            any(&["skip (esc or n)", "waiting for approval...", "workspace trust required", "do you trust the contents of this directory"])
                || (t.contains("run (once)") && t.contains("skip"))
        }
        // Copilot CLI 1.0.110: folder trust at start (as it draws it), and
        // the way out of its approval prompt (as `copilot help permissions`
        // words it).
        Kind::Copilot => any(&["confirm folder trust", "do you trust the files in this folder", "no, and tell copilot what to do differently"]),
        // Codex 0.167's start-up dialogs: trusting the folder, then
        // trix's hooks (and any of yours) once they're new or changed.
        Kind::Codex => any(&["trust and continue", "your trust decision will be saved", "hooks need review", "need review before they can run", "continue without trusting"]),
        _ => false,
    }
}

/// Grok Build's title (`notifications/title.rs`): "⚠ Action Required" while
/// a permission waits, a braille spinner and what it does while busy, then
/// the session name and "grok", parts joined by " - ".
const GROK_ATTENTION: &str = "Action Required";
/// Unfocused, Grok blinks the "Action Required" label: off for half a second.
const GROK_BLINK: Duration = Duration::from_millis(1500);

/// The screen last read, by (last output, last resize): (any prompt, the
/// agent's own prompt).
type ScreenSeen = ((Instant, Instant), (bool, bool));

#[derive(Debug, PartialEq)]
enum TitleSignal {
    Attention,
    Busy(Option<String>),
    Idle,
}

/// What Grok's title says, if it is one of Grok's titles.
fn grok_title(title: Option<&str>, attention: bool) -> Option<TitleSignal> {
    let title = title?;
    if attention || title.contains(GROK_ATTENTION) {
        return Some(TitleSignal::Attention);
    }
    let parts: Vec<&str> = title.split(" - ").map(str::trim).collect();
    let spinner = |p: &str| {
        let mut cs = p.chars();
        matches!((cs.next(), cs.next()), (Some('\u{2801}'..='\u{28ff}'), None))
    };
    if let Some(i) = parts.iter().position(|p| spinner(p)) {
        let what = parts.get(i + 1).filter(|p| **p != "grok").map(|p| hooks::one_line(p, 60));
        return Some(TitleSignal::Busy(what));
    }
    (parts.last() == Some(&"grok")).then_some(TitleSignal::Idle)
}

/// A random (version 4) UUID, for a new Grok conversation.
fn new_session_id() -> Option<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).ok()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

/// Do the user's extra Grok flags pick the conversation themselves?
fn grok_picks_session(extra: Option<&str>) -> bool {
    let Some(extra) = extra else { return false };
    split_args(extra).iter().any(|a| {
        let flag = a.split('=').next().unwrap_or(a);
        matches!(flag, "-c" | "--continue" | "-r" | "--resume" | "-s" | "--session-id" | "--fork-session")
    })
}

/// The id trix picks for a new Grok or Copilot conversation: none when
/// it resumes one, or the user's extra flags pick it.
fn new_conversation(spec: &Spawn) -> Option<String> {
    let picks = match spec.kind {
        Kind::Grok => grok_picks_session(spec.command.as_deref()),
        Kind::Copilot => copilot_picks_session(spec.command.as_deref()),
        _ => return None,
    };
    (spec.resume.is_none() && !picks).then(new_session_id).flatten()
}

/// Do the user's extra Copilot flags pick the conversation themselves?
fn copilot_picks_session(extra: Option<&str>) -> bool {
    let Some(extra) = extra else { return false };
    split_args(extra).iter().any(|a| {
        let flag = a.split('=').next().unwrap_or(a);
        matches!(flag, "--continue" | "-r" | "--resume" | "--session-id" | "--connect")
    })
}

fn window_size((cols, lines): (u16, u16)) -> WindowSize {
    WindowSize { num_cols: cols, num_lines: lines, cell_width: 9, cell_height: 18 }
}

/// What an agent is started with while trix paints the panes with a theme
/// of its own (`themed`): Grok's and OpenCode's themes that take the
/// terminal's colors, the ones trix paints. Claude's is in its settings
/// overlay. A variable already `set` in trix's environment is the user's
/// choice, and stays.
fn theme_env(kind: Kind, themed: bool, set: impl Fn(&str) -> bool) -> Vec<(&'static str, &'static str)> {
    let ask: &[(&'static str, &'static str)] = match (themed, kind) {
        // The terminal theme is still rolling out: the flag turns it on.
        (true, Kind::Grok) => &[("GROK_THEME", "terminal"), ("GROK_TERMINAL_THEME", "1")],
        (true, Kind::OpenCode) => &[("OPENCODE_CLI_CONFIG_CONTENT", r#"{"theme":{"name":"system"}}"#)],
        _ => &[],
    };
    ask.iter().copied().filter(|(k, _)| !set(k)).collect()
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

fn command_for(spec: &Spawn, integrations: &Integrations, exe: &std::path::Path, new_session: Option<&str>) -> (String, Vec<String>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let mut args = Vec::new();
    let extra = spec.command.as_deref().filter(|c| !c.trim().is_empty());
    let prompt = spec.prompt.as_deref().filter(|p| !p.is_empty());
    match spec.kind {
        // `grok [PROMPT]`; `--session-id` names a new conversation.
        Kind::Grok => {
            if let Some(id) = &spec.resume {
                args.extend(["--resume".into(), id.clone()]);
            } else if let Some(id) = new_session {
                args.extend(["--session-id".into(), id.into()]);
            }
            args.extend(extra.map(split_args).unwrap_or_default());
            if let Some(p) = prompt {
                push_prompt(&mut args, p);
            }
            ("grok".into(), args)
        }
        // trix's plugin rides in `OPENCODE_CONFIG_CONTENT` (see `spawn`).
        Kind::OpenCode => {
            let theirs = extra.map(split_args).unwrap_or_default();
            // Its own server, with this pane's environment, unless you
            // picked one (see `Integrations::opencode_standalone`).
            if integrations.opencode_standalone() && !theirs.iter().any(|a| a == "--standalone" || a == "--server" || a.starts_with("--server=")) {
                args.push("--standalone".into());
            }
            if let Some(id) = &spec.resume {
                args.extend(["--session".into(), id.clone()]);
            }
            args.extend(theirs);
            if let Some(p) = prompt {
                args.push(format!("--prompt={p}"));
            }
            ("opencode".into(), args)
        }
        Kind::Cursor => {
            if let Some(dir) = &integrations.cursor_plugin {
                args.extend(["--plugin-dir".into(), dir.display().to_string()]);
            }
            if let Some(id) = &spec.resume {
                args.extend(["--resume".into(), id.clone()]);
            }
            args.extend(extra.map(split_args).unwrap_or_default());
            if let Some(p) = prompt {
                push_prompt(&mut args, p);
            }
            ("cursor-agent".into(), args)
        }
        // `copilot [-i PROMPT]`: no positional prompt. trix's plugin rides
        // on `--plugin-dir`. `--session-id` resumes the conversation with
        // that id, or starts one under it: both new and restored panes.
        Kind::Copilot => {
            if let Some(dir) = &integrations.copilot_plugin {
                args.extend(["--plugin-dir".into(), dir.display().to_string()]);
            }
            if let Some(id) = spec.resume.as_deref().or(new_session) {
                args.push(format!("--session-id={id}"));
            }
            args.extend(extra.map(split_args).unwrap_or_default());
            if let Some(p) = prompt {
                // With `=`, a prompt starting with `-` stays the value.
                args.push(format!("--interactive={p}"));
            }
            ("copilot".into(), args)
        }
        Kind::Claude => {
            if let Some(s) = &integrations.claude_settings {
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
                push_prompt(&mut args, p);
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
                push_prompt(&mut args, p);
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

/// The first prompt, last on the command line. One starting with `-` (a
/// markdown list, "--help is broken") goes after `--`, or the CLI would take
/// it for an option.
fn push_prompt(args: &mut Vec<String>, prompt: &str) {
    if prompt.starts_with('-') {
        args.push("--".into());
    }
    args.push(prompt.into());
}

/// A real agent around `sleep`: a live PTY and emulator, but nothing on screen
/// and no output, so the state machine only sees what a test feeds it.
#[cfg(test)]
pub(crate) fn test_agent(kind: Kind) -> Agent {
    let (tx, _rx) = std::sync::mpsc::channel();
    test_agent_on(kind, tx)
}

/// `test_agent`, its events sent to `tx`.
#[cfg(test)]
pub(crate) fn test_agent_on(kind: Kind, tx: Sender<AppEvent>) -> Agent {
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
    let mut a = Agent::spawn(1, spec, (80, 24), tx, &sock, &Integrations::default(), std::path::Path::new("/bin/true")).unwrap();
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
            transcript: None,
            seq: 0,
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
        // An interactive shell hands the terminal to the command it runs.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut s = spec(Kind::Custom);
        s.command = Some("exec bash --norc --noprofile -i".into());
        s.cwd = std::env::temp_dir();
        let a = Agent::spawn(1, s, (80, 24), tx, &std::env::temp_dir().join("trix-test.sock"), &Integrations::default(), Path::new("/bin/true")).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!a.running_command(), "at the prompt");
        a.write(&b"sleep 5\r"[..]);
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
    fn a_late_hook_does_not_undo_a_newer_one() {
        let mut a = test_agent(Kind::Claude);
        let at = |mut m: HookMsg, seq: u64| {
            m.seq = seq;
            m
        };
        a.on_hook(&at(claude("UserPromptSubmit", None, None), 10), true);
        a.on_hook(&at(claude("Stop", None, None), 30), true);
        // The PostToolUse fired before the Stop but was read after it.
        a.on_hook(&at(claude("PostToolUse", Some("Bash"), None), 20), true);
        assert_eq!(a.state, State::Done, "the stale report is dropped");
        a.on_hook(&at(claude("UserPromptSubmit", None, None), 40), true);
        assert_eq!(a.state, State::Working, "newer ones still apply");
        a.on_hook(&claude("Stop", None, None), true);
        assert_eq!(a.state, State::Done, "no stamp: always applied");
    }

    #[test]
    fn a_clock_that_went_back_does_not_mute_the_hooks() {
        let mut a = test_agent(Kind::Claude);
        let at = |mut m: HookMsg, seq: u64| {
            m.seq = seq;
            m
        };
        let now = 1_800_000_000_000_000;
        a.on_hook(&at(claude("UserPromptSubmit", None, None), now), true);
        // NTP set the clock back a minute: the next turn's reports are older.
        let back = now - 60_000_000;
        a.on_hook(&at(claude("Stop", None, None), back), true);
        assert_eq!(a.state, State::Done, "not overtaken, just stamped by a clock that moved");
        a.on_hook(&at(claude("UserPromptSubmit", None, None), back + 1_000), true);
        assert_eq!(a.state, State::Working, "and the ones after it count from there");
        a.on_hook(&at(claude("Stop", None, None), back + 3_000), true);
        a.on_hook(&at(claude("PostToolUse", Some("Bash"), None), back + 2_000), true);
        assert_eq!(a.state, State::Done, "a report overtaken on its way here is still dropped");
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
    fn a_question_stays_asked_while_claude_draws_it() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("UserPromptSubmit", None, None), false);
        redraw(&mut a); // its spinner, while it thinks
        a.on_hook(&claude("PreToolUse", Some("AskUserQuestion"), Some("Tea or coffee?")), false);
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Tea or coffee?"), "output from before it asked isn't the turn going on");
        // It draws the dialog, in many writes, as Claude Code 2.x does.
        show(&a, "Tea or coffee?\n❯ 1. Tea\n  2. Coffee\n  3. Type something.\nEnter to select · ↑/↓ to navigate · Esc to cancel\n");
        redraw(&mut a);
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked, "its dialog is on screen");
        // Then it asks to show it and says it needs you: still the one question.
        assert_eq!(a.on_hook(&claude("PermissionRequest", Some("AskUserQuestion"), Some("Tea or coffee?")), false), None);
        assert_eq!(a.on_hook(&claude("Notification", None, Some("Claude needs your permission to use AskUserQuestion")), false), None);
        assert_eq!(a.activity, "Tea or coffee?");
        // Answered.
        a.on_hook(&claude("PostToolUse", Some("AskUserQuestion"), None), false);
        assert_eq!(a.state, State::Working);
        a.shutdown();
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

    /// A program that ignores both the hang-up and SIGTERM, as OpenCode 2
    /// ignores the hang-up, is still stopped, children and all.
    #[test]
    fn a_program_that_ignores_the_hang_up_is_stopped_anyway() {
        let (tx, _rx) = std::sync::mpsc::channel();
        // It says when its traps are set: a login shell may take a while to
        // get there (CI's bash reads /etc/profile first), and a hang-up
        // before that would end it for real.
        let ready = std::env::temp_dir().join(format!("trix-stop-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&ready);
        let mut s = spec(Kind::Custom);
        s.cwd = std::env::temp_dir();
        s.command = Some(format!("trap '' HUP TERM; : > '{}'; while :; do sleep 0.1; done", ready.display()));
        let a = Agent::spawn(1, s, (80, 24), tx, &std::env::temp_dir().join("trix-test.sock"), &Integrations::default(), Path::new("/bin/true")).unwrap();
        let pid = a.pid;
        let by = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ready.exists(), "its traps were never set");
        let _ = std::fs::remove_file(&ready);
        assert!(a.hang_up());
        assert!(!a.hang_up(), "once");
        std::thread::sleep(Duration::from_millis(300));
        assert!(crate::platform::alive(pid), "it ignored the hang-up");
        crate::platform::stop(&[pid], Duration::from_millis(200));
        let gone = Instant::now() + Duration::from_secs(3);
        while crate::platform::alive(pid) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!crate::platform::alive(pid), "killed, and reaped by its event loop");
    }

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
    fn installing_stays_starting_until_the_installer_is_done() {
        let mut a = test_agent(Kind::Grok);
        a.installing = Some("installing grok (npm)".into());
        a.created = ago(Duration::from_secs(60));
        a.last_output = ago(Duration::from_secs(5));
        assert_eq!(a.heuristics(true), None, "a quiet download isn't a ready agent");
        assert_eq!(a.state, State::Starting);
        // Not the user's own tool reporting: nothing becomes `reported`.
        assert_eq!(a.on_hook(&hook("trix", "Installed", None, None), true), None);
        assert!(a.installing.is_none() && !a.reported && !a.hooked);
        assert_eq!(a.activity, "launching");
        // The agent gets its own start-up time from here.
        assert_eq!(a.heuristics(true), None, "just started");
        a.created = ago(Duration::from_secs(2));
        assert_eq!(a.heuristics(true), Some(State::Starting));
        assert_eq!(a.state, State::Idle);
    }

    #[test]
    fn the_agents_own_report_ends_installing_too() {
        let mut a = test_agent(Kind::OpenCode);
        a.installing = Some("installing opencode (brew)".into());
        a.on_hook(&hook("opencode", "Busy", None, None), true);
        assert!(a.installing.is_none());
        assert_eq!(a.state, State::Working);
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
    fn broadcast_skips_a_dialog_the_state_has_not_caught_up_with() {
        use crate::app::{BroadcastTarget, broadcast_reaches};
        let mut a = test_agent(Kind::Claude);
        a.set_state(State::Working, false);
        assert!(broadcast_reaches(&a, BroadcastTarget::All), "working, nothing on screen");
        // The permission dialog is up; its hook has not arrived, so the state still says Working.
        show(&a, "Do you want to proceed?\n ❯ 1. Yes\n   2. No\n");
        a.on_output();
        assert_eq!(a.state, State::Working);
        assert!(!broadcast_reaches(&a, BroadcastTarget::All), "the screen shows a prompt");
        a.shutdown();
    }

    #[test]
    fn the_screen_is_read_again_only_after_output() {
        let mut a = test_agent(Kind::Codex);
        assert!(!a.screen_needs_input());
        show(&a, "Do you want to proceed?\n ❯ 1. Yes\n   2. No\n");
        assert!(!a.screen_needs_input(), "no output reported since: the last answer stands");
        a.on_output();
        assert!(a.screen_needs_input(), "output: read again");
        a.shutdown();
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
        let mut a = Agent::spawn(1, s, (80, 24), tx, &std::env::temp_dir().join("trix-test.sock"), &Integrations::default(), Path::new("/bin/true")).unwrap();
        assert!(a.prompted.is_some());
        a.set_state(State::Idle, false);
        assert!(a.prompted.is_some(), "SessionStart's idle is not the turn");
        a.set_state(State::Working, false);
        assert!(a.prompted.is_none());
        // Sent while it works: the end of this turn isn't the prompt's turn.
        a.prompted = Some(Instant::now());
        a.set_state(State::Done, false);
        assert!(a.prompted.is_some(), "the turn it was sent during ended");
        a.set_state(State::Working, false);
        assert!(a.prompted.is_none(), "its own turn began");
        a.shutdown();
    }

    /// Codex 0.167 redraws for seconds as it starts. Nobody has asked it
    /// anything yet: that's it starting up, not a finished turn.
    #[test]
    fn output_before_anything_was_asked_is_not_done() {
        let mut a = test_agent(Kind::Codex);
        a.set_state(State::Working, true);
        a.work_started = Some(ago(Duration::from_secs(4)));
        a.last_output = ago(Duration::from_secs(2));
        a.heuristics(true);
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "idle"));
        // Answering its trust dialog isn't asking it anything either.
        a.set_state(State::Blocked, true);
        a.user_input(b"\r".to_vec());
        assert!(!a.asked, "an answer, not a request");
        // A prompt is: what follows is a turn, and it ends done.
        a.set_state(State::Idle, true);
        a.user_input(b"fix the tests\r".to_vec());
        assert!(a.asked);
        a.set_state(State::Working, true);
        a.work_started = Some(ago(Duration::from_secs(4)));
        a.last_output = ago(Duration::from_secs(2));
        a.heuristics(true);
        assert_eq!(a.state, State::Done);
        // A custom command does its work unasked (a dev server starting).
        let mut c = test_agent(Kind::Custom);
        c.set_state(State::Working, true);
        c.work_started = Some(ago(Duration::from_secs(10)));
        c.last_output = ago(Duration::from_secs(2));
        c.heuristics(true);
        assert_eq!(c.state, State::Done);
        a.shutdown();
        c.shutdown();
    }

    /// Codex 0.167.2's start-up dialogs, as drawn: they need you.
    #[test]
    fn codex_trust_and_hooks_dialogs_need_you() {
        let trust = "  permission settings. Folder settings can\n  run code automatically, even without a\n  model request. Continue only if you trust\n  these files. Your trust decision will be\n  saved.\n› 1. Trust and continue\n  2. Back to Agent Command Center\n  enter continue · esc back\n";
        let hooks = "  Hooks need review\n  9 hooks are new or changed.\n  Hooks can run outside the sandbox after you trust them.\n› 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n  enter confirm · esc skip\n";
        let list = "  Hooks\n  Lifecycle hooks from config and enabled plugins.\n  ⚠ 9 hooks need review before they can run.\n  t trust all · enter review · esc close\n";
        for screen in [trust, hooks, list] {
            assert!(kind_prompt(Kind::Codex, screen), "{screen}");
        }
        assert!(!kind_prompt(Kind::Codex, "› Ask Codex to do anything\n  gpt-5 · ~/Projects/trix\n"), "its prompt bar");
        assert!(!kind_prompt(Kind::Claude, hooks), "Codex's own wording");
        // From idle, on screen: needs you.
        let mut a = test_agent(Kind::Codex);
        a.set_state(State::Idle, false);
        a.state_since = ago(Duration::from_secs(2));
        show(&a, hooks);
        a.on_output();
        a.last_output = ago(Duration::from_secs(1));
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "waiting for an answer"));
        a.shutdown();
    }

    #[test]
    fn unhooked_long_work_is_done_short_work_is_idle() {
        let mut a = test_agent(Kind::Claude);
        a.asked = true;
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
        let (prog, args) = command_for(&s, &Integrations { claude_settings: Some("/run/s.json".into()), ..Default::default() }, Path::new("/bin/trix"), None);
        assert_eq!(prog, "claude");
        assert_eq!(args, ["--settings", "/run/s.json", "--resume", "abc", "--model", "opus", "--append-system-prompt", "be terse", "fix it"]);

        let mut s = spec(Kind::Claude);
        s.command = Some("   ".into());
        s.prompt = Some(String::new());
        assert!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None).1.is_empty());

        let mut s = spec(Kind::Claude);
        s.prompt = Some("- fix lint\n- run tests".into());
        assert_eq!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None).1, ["--", "- fix lint\n- run tests"], "not an option");
        let mut s = spec(Kind::Codex);
        s.prompt = Some("--help is broken".into());
        assert_eq!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None).1[4..], ["--", "--help is broken"]);
    }

    #[test]
    fn codex_command_line() {
        let notify = r#"notify=["/bin/trix","hook","codex"]"#;
        let hooks = codex_hooks(Path::new("/bin/trix"));
        let mut s = spec(Kind::Codex);
        s.prompt = Some("hi".into());
        let (prog, args) = command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None);
        assert_eq!(prog, "codex");
        assert_eq!(args, ["-c", notify, "-c", &hooks, "hi"]);

        // Resuming: the id goes last and a first prompt makes no sense.
        s.resume = Some("t-1".into());
        s.command = Some("--full-auto".into());
        assert_eq!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None).1, ["resume", "-c", notify, "-c", &hooks, "--full-auto", "t-1"]);
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
        assert_eq!(command_for(&spec(Kind::Shell), &Integrations::default(), Path::new("/bin/trix"), None), (shell.clone(), vec![]));
        let mut s = spec(Kind::Custom);
        s.command = Some("npm run dev".into());
        assert_eq!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None), (shell, vec!["-lc".into(), "npm run dev".into()]));
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
        assert!(Kind::Claude.takes_prompts() && Kind::Codex.takes_prompts() && !Kind::Shell.takes_prompts() && !Kind::Custom.takes_prompts());
        assert!([Kind::Grok, Kind::OpenCode, Kind::Cursor].iter().all(|k| k.takes_prompts() && k.resumes() && k.is_agent()));
        assert_eq!((Kind::Grok.label(), Kind::OpenCode.label(), Kind::Cursor.label()), ("grok", "opencode", "cursor"));
    }

    fn ints() -> Integrations {
        ints_for(false)
    }
    fn ints_for(opencode_standalone: bool) -> Integrations {
        Integrations {
            claude_settings: Some("/run/s.json".into()),
            opencode_plugin: Some("/run/opencode-plugin-1.js".into()),
            opencode_plugin_dir: Some("/run/opencode-plugin-1".into()),
            cursor_plugin: Some("/run/cursor-plugin-1".into()),
            copilot_plugin: Some("/run/copilot-plugin-1".into()),
            ..Integrations::with_standalone(opencode_standalone)
        }
    }

    #[test]
    fn grok_and_opencode_take_the_painted_colors() {
        let none = |_: &str| false;
        assert_eq!(theme_env(Kind::Grok, true, none), [("GROK_THEME", "terminal"), ("GROK_TERMINAL_THEME", "1")]);
        assert_eq!(theme_env(Kind::OpenCode, true, none), [("OPENCODE_CLI_CONFIG_CONTENT", r#"{"theme":{"name":"system"}}"#)]);
        assert!(theme_env(Kind::Claude, true, none).is_empty(), "Claude's is in its settings overlay");
        assert!(theme_env(Kind::Shell, true, none).is_empty());
        assert!(theme_env(Kind::Grok, false, none).is_empty(), "no theme of trix's own: their own themes");
        let theirs = |k: &str| k == "GROK_THEME";
        assert_eq!(theme_env(Kind::Grok, true, theirs), [("GROK_TERMINAL_THEME", "1")], "the user's GROK_THEME stays");
    }

    #[test]
    fn a_program_asking_its_colors_hears_the_theme() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx_agent = tx.clone();
        let mut app = crate::app::App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        let a = test_agent_on(Kind::OpenCode, tx_agent);
        let id = a.id;
        app.agents.push(a);
        let mut p: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        // Ask the background, then set color 1 and ask it: answered from what the program set.
        let ask = |p: &mut alacritty_terminal::vte::ansi::Processor, app: &crate::app::App, q: &[u8]| p.advance(&mut *app.agents[0].term.lock(), q);
        let answers = |app: &mut crate::app::App| {
            let mut out = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                if let crate::app::AppEvent::ColorRequest(i, index, f) = &ev {
                    assert_eq!(*i, id);
                    out.push((*index, app.pane_color(0, *index).map(|c| f(c))));
                }
            }
            out
        };
        app.theme_source = crate::theme::Source::Builtin;
        ask(&mut p, &app, b"\x1b]11;?\x07");
        assert_eq!(answers(&mut app), [(257, None)], "no theme file: no answer, as before");
        app.theme_source = crate::theme::Source::File("/x/colors.toml".into());
        app.theme = crate::theme::Theme::default();
        ask(&mut p, &app, b"\x1b]11;?\x07\x1b]10;?\x1b\\");
        assert_eq!(
            answers(&mut app),
            [(257, Some("\x1b]11;rgb:1a1a/1b1b/2626\x07".to_string())), (256, Some("\x1b]10;rgb:a9a9/b1b1/d6d6\x1b\\".to_string()))]
        );
        // Panes left to the terminal (`[theme] panes = false`): its colors,
        // which trix doesn't know; with Omarchy's theme, Omarchy's.
        app.config.theme.panes = false;
        ask(&mut p, &app, b"\x1b]11;?\x07");
        assert_eq!(answers(&mut app), [(257, None)], "panes in the terminal's colors: unknown");
        app.theme_source = crate::theme::Source::Omarchy;
        ask(&mut p, &app, b"\x1b]11;?\x07");
        assert_eq!(answers(&mut app), [(257, Some("\x1b]11;rgb:1a1a/1b1b/2626\x07".to_string()))], "Omarchy themes the terminal");
        app.config.theme.panes = true;
        app.theme_source = crate::theme::Source::File("/x/colors.toml".into());
        ask(&mut p, &app, b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07");
        assert_eq!(answers(&mut app), [(1, Some("\x1b]4;1;rgb:1212/3434/5656\x07".to_string()))], "a color it set itself");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn grok_argv_names_or_resumes_the_conversation() {
        let mut s = spec(Kind::Grok);
        s.prompt = Some("fix it".into());
        let id = "0b5e2c1a-7f3d-4c2e-9a1b-2d3e4f5a6b7c";
        assert_eq!(command_for(&s, &ints(), Path::new("/bin/trix"), Some(id)), ("grok".into(), vec!["--session-id".into(), id.into(), "fix it".into()]));
        s.resume = Some("abc".into());
        s.command = Some("--model grok-4".into());
        s.prompt = Some("- go on".into());
        assert_eq!(command_for(&s, &ints(), Path::new("/bin/trix"), None).1, ["--resume", "abc", "--model", "grok-4", "--", "- go on"]);
    }

    #[test]
    fn grok_leaves_the_conversation_to_flags_that_pick_one() {
        assert!(!grok_picks_session(None));
        assert!(!grok_picks_session(Some("--model grok-4")));
        for f in ["-c", "--continue", "-r", "--resume=x", "-s abc", "--session-id abc", "--fork-session"] {
            assert!(grok_picks_session(Some(f)), "{f}");
        }
    }

    #[test]
    fn new_session_ids_are_uuid_v4() {
        let id = new_session_id().unwrap();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4') && "89ab".contains(&parts[3][..1]));
        assert_ne!(id, new_session_id().unwrap());
    }

    #[test]
    fn opencode_argv_resumes_and_prompts_with_a_flag() {
        let mut s = spec(Kind::OpenCode);
        s.prompt = Some("-v is broken".into());
        assert_eq!(command_for(&s, &ints(), Path::new("/bin/trix"), None), ("opencode".into(), vec!["--prompt=-v is broken".into()]));
        s.resume = Some("ses_1".into());
        s.prompt = None;
        s.command = Some("--model anthropic/claude".into());
        assert_eq!(command_for(&s, &ints(), Path::new("/bin/trix"), None).1, ["--session", "ses_1", "--model", "anthropic/claude"]);
    }

    #[test]
    fn opencode_2_gets_its_own_server_unless_you_pick_one() {
        let two = ints_for(true);
        let mut s = spec(Kind::OpenCode);
        s.resume = Some("ses_1".into());
        assert_eq!(command_for(&s, &two, Path::new("/bin/trix"), None).1, ["--standalone", "--session", "ses_1"]);
        for theirs in ["--standalone", "--server http://localhost:4096", "--server=http://localhost:4096"] {
            s.command = Some(theirs.into());
            // Yours stands; trix adds none of its own.
            assert_eq!(command_for(&s, &two, Path::new("/bin/trix"), None).1.iter().filter(|a| *a == "--standalone").count(), usize::from(theirs == "--standalone"));
        }
    }

    #[test]
    fn cursor_argv_loads_the_plugin_and_resumes() {
        let mut s = spec(Kind::Cursor);
        s.prompt = Some("hi".into());
        assert_eq!(command_for(&s, &ints(), Path::new("/bin/trix"), None), ("cursor-agent".into(), vec!["--plugin-dir".into(), "/run/cursor-plugin-1".into(), "hi".into()]));
        s.resume = Some("chat-1".into());
        s.prompt = None;
        assert_eq!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None).1, ["--resume", "chat-1"]);
    }

    #[test]
    fn claude_and_codex_argv_ignore_the_new_integrations() {
        let s = spec(Kind::Claude);
        assert_eq!(command_for(&s, &ints(), Path::new("/bin/trix"), None).1, ["--settings", "/run/s.json"]);
        let c = command_for(&spec(Kind::Codex), &ints(), Path::new("/bin/trix"), Some("x")).1;
        assert!(!c.iter().any(|a| a.contains("plugin") || a == "x"));
    }

    #[test]
    fn kind_prompts_read_only_their_own_wording() {
        assert!(kind_prompt(Kind::Grok, "Allow bash?\n (○) Yes\n (○) No, reject (type to add feedback)\n1-3:select  Tab:next option  Ctrl+o:always-approve  Ctrl+c:cancel"));
        assert!(kind_prompt(Kind::OpenCode, "△ Permission required\n  Allow once   Allow always   Reject"));
        assert!(kind_prompt(Kind::Cursor, "Run this command?\n  ls -la\n → Run (once) (y)\n   Skip (esc or n)"));
        assert!(kind_prompt(Kind::Cursor, "⚠ Workspace Trust Required\nDo you trust the contents of this directory?"));
        // As Copilot CLI 1.0.110 draws its folder trust, and as its help
        // words the approval prompt's way out.
        assert!(kind_prompt(Kind::Copilot, "│ Confirm folder trust │\n│ Do you trust the files in this folder? │\n│ ❯ 1. Yes │"));
        assert!(kind_prompt(Kind::Copilot, "Run cargo test?\n❯ 1. Yes\n  2. Yes, and don't ask again for cargo in this repo\n  3. No, and tell Copilot what to do differently (Esc)"));
        // Prose that only the generic patterns would catch, or another agent's prompt.
        let prose = "Would you like to add tests? I can run them (y/n) next.\nDo you want to proceed?";
        for k in Kind::ALL {
            assert!(!kind_prompt(k, prose), "{k:?}");
        }
        assert!(!kind_prompt(Kind::Grok, "Run this command?\n Run (once) (y)\n Skip (esc or n)"));
        assert!(!kind_prompt(Kind::Claude, "△ Permission required\nAllow once Allow always"));
        assert!(!kind_prompt(Kind::Cursor, "I ran the command and skipped the lint step."));
    }

    /// Output the way an agent redraws while it works: a burst, none of it
    /// the echo of keystrokes or a resize's redraw.
    fn redraw(a: &mut Agent) {
        a.last_input = ago(Duration::from_secs(5));
        a.last_resize = ago(Duration::from_secs(5));
        for _ in 0..8 {
            a.on_output();
        }
    }

    #[test]
    fn cursor_turns_come_from_its_screen() {
        let mut a = test_agent(Kind::Cursor);
        a.set_state(State::Idle, false);
        a.asked = true;
        let mut start = hook("cursor", "SessionStart", None, None);
        start.session = Some("conv-1".into());
        a.on_hook(&start, false);
        assert_eq!((a.state, a.session.as_deref()), (State::Idle, Some("conv-1")));
        assert!(!a.hooked, "its hooks don't report turns");
        // It thinks and streams: working. Its title names the conversation.
        a.title = Some("Pseudo Terminal Explainer".into());
        redraw(&mut a);
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "working"));
        // Its approval prompt, as cursor-agent 2026.11.11 draws it: blocked.
        show(&a, "Run this command?\n Not in allowlist: rm\n  → Run (once) (y)\n    Skip & tell the agent what to do instead (esc or n)\n");
        a.on_output();
        a.last_output = ago(Duration::from_secs(1));
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "needs your approval"));
        // Approved: the command runs and Cursor redraws its timer.
        show(&a, "\x1b[2J\x1b[H$ rm -rf build 2s\n");
        redraw(&mut a);
        a.heuristics(false);
        assert_eq!(a.state, State::Working);
        a.on_hook(&hook("cursor", "ToolDone", Some("Shell"), Some("rm -rf build")), false);
        assert_eq!((a.state, a.tools_used, a.activity.as_str()), (State::Working, 1, "Shell rm -rf build ✓"));
        // It replies and goes quiet: done.
        a.work_started = Some(ago(Duration::from_secs(10)));
        a.last_output = ago(Duration::from_secs(2));
        a.heuristics(false);
        assert_eq!(a.state, State::Done);
        // A tool report that lands after the turn ended doesn't reopen it.
        assert_eq!(a.on_hook(&hook("cursor", "ToolDone", Some("Read"), Some("a.rs")), false), None);
        assert!(a.state == State::Done && a.activity.starts_with("finished after"), "{:?} {}", a.state, a.activity);
        a.shutdown();
    }

    #[test]
    fn claude_mid_turn_prose_is_not_a_prompt() {
        let mut a = test_agent(Kind::Claude);
        a.on_hook(&claude("UserPromptSubmit", None, None), false);
        show(&a, "Would you like to see Run (once)? Skip (esc or n)\n");
        a.on_output();
        a.last_output = ago(Duration::from_secs(1));
        a.heuristics(false);
        assert_eq!(a.state, State::Working, "only Grok and Cursor read the screen mid-turn");
        a.shutdown();
    }

    #[test]
    fn opencode_events_drive_the_state() {
        let mut a = test_agent(Kind::OpenCode);
        a.on_hook(&hook("opencode", "SessionStart", None, None), false);
        assert!(a.hooked && !a.reported);
        a.on_hook(&hook("opencode", "UserPromptSubmit", None, Some("hi")), false);
        assert_eq!(a.state, State::Working);
        a.on_hook(&hook("opencode", "PermissionRequest", Some("bash"), Some("rm *")), false);
        assert_eq!(a.state, State::Blocked);
        a.on_hook(&hook("opencode", "Answered", None, None), false);
        assert_eq!(a.state, State::Working);
        a.on_hook(&hook("opencode", "Question", None, Some("Which db?")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Which db?"));
        a.on_hook(&hook("opencode", "Busy", None, None), false);
        assert_eq!(a.state, State::Blocked, "busy doesn't clear a question");
        a.on_hook(&hook("opencode", "Answered", None, None), false);
        a.on_hook(&hook("opencode", "Interrupt", None, None), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Idle, "interrupted"));
        a.on_hook(&hook("opencode", "Busy", None, None), false);
        assert_eq!(a.state, State::Working, "a queued message starts a turn");
        a.on_hook(&hook("opencode", "Stop", None, None), false);
        assert_eq!(a.state, State::Done);
        a.shutdown();
    }

    #[test]
    fn opencode_waits_for_its_own_events() {
        let mut a = test_agent(Kind::OpenCode);
        a.on_hook(&hook("opencode", "UserPromptSubmit", None, Some("hi")), false);
        a.last_output = ago(Duration::from_secs(10));
        a.heuristics(false);
        assert_eq!(a.state, State::Working, "a long quiet tool isn't an interrupt");
        a.on_hook(&hook("opencode", "Question", None, Some("Which db?")), false);
        for _ in 0..8 {
            a.on_output();
        }
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked, "redrawing while it asks");
        a.shutdown();
    }

    #[test]
    fn opencode_idle_at_start_is_not_done() {
        let mut a = test_agent(Kind::OpenCode);
        a.on_hook(&hook("opencode", "SessionStart", None, None), false);
        assert_eq!(a.on_hook(&hook("opencode", "Stop", None, None), false), None);
        assert_eq!(a.state, State::Idle, "no turn yet: no Done, no chime");
        a.on_hook(&hook("opencode", "UserPromptSubmit", None, Some("hi")), false);
        a.on_hook(&hook("opencode", "Stop", None, None), false);
        assert_eq!(a.state, State::Done);
        a.shutdown();
    }

    #[test]
    fn opencode_prompt_on_screen_blocks_mid_turn() {
        let mut a = test_agent(Kind::OpenCode);
        a.on_hook(&hook("opencode", "UserPromptSubmit", None, Some("hi")), false);
        show(&a, "△ Permission required\n  $ rm -rf dist\n  Allow once   Allow always   Reject\n");
        for _ in 0..8 {
            a.on_output();
        }
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked, "even while it redraws, and before permission.asked");
        a.on_hook(&hook("opencode", "Answered", None, None), false);
        assert_eq!(a.state, State::Working);
        a.shutdown();
    }

    #[test]
    fn cursor_prose_is_not_a_prompt() {
        let mut a = test_agent(Kind::Cursor);
        a.set_state(State::Idle, false);
        a.asked = true;
        redraw(&mut a);
        a.heuristics(false);
        assert_eq!(a.state, State::Working);
        show(&a, "Run this command?\n → Run (once) (y)\n   Skip (esc or n)\n");
        a.on_output();
        a.heuristics(false);
        assert_eq!(a.state, State::Working, "not while the screen is still drawing");
        a.last_output = ago(Duration::from_millis(700));
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked);
        // Approved: the prompt goes, its question to you stays on screen.
        show(&a, "\x1b[2J\x1b[HDone. Would you like to add tests too? (y/n)\n");
        redraw(&mut a);
        a.heuristics(false);
        assert_eq!(a.state, State::Working, "the prompt went");
        // The turn ends on that question: done, not waiting on you.
        a.work_started = Some(ago(Duration::from_secs(10)));
        a.last_output = ago(Duration::from_secs(2));
        assert!(a.screen_needs_input(), "the generic patterns would see a question");
        a.heuristics(false);
        assert_eq!(a.state, State::Done);
        // And it stays done while it sits there.
        a.last_output = ago(Duration::from_secs(5));
        a.state_since = ago(Duration::from_secs(3));
        a.heuristics(false);
        assert_eq!(a.state, State::Done);
        a.shutdown();
    }

    #[test]
    fn grok_stays_done_through_its_redraw_after_a_turn() {
        let mut a = test_agent(Kind::Grok);
        a.set_state(State::Idle, false);
        a.on_title("⠋ - Responding - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Working);
        a.work_started = Some(ago(Duration::from_secs(10)));
        a.on_title("grok".into());
        a.last_output = ago(Duration::from_secs(1));
        a.heuristics(false);
        assert_eq!(a.state, State::Done);
        // Grok names the conversation and redraws: its title still says idle.
        a.on_title("Count lines in fruit.txt - grok".into());
        redraw(&mut a);
        a.heuristics(false);
        assert!(a.state == State::Done && a.activity.starts_with("finished after"), "{:?} {}", a.state, a.activity);
        // A real turn shows its spinner.
        a.on_title("⠙ - Thinking - Count lines in fruit.txt - grok".into());
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "Thinking"));
        a.shutdown();
    }

    #[test]
    fn grok_title_signals() {
        assert_eq!(grok_title(None, false), None);
        assert_eq!(grok_title(Some("vim"), false), None);
        assert_eq!(grok_title(Some("grok"), false), Some(TitleSignal::Idle));
        assert_eq!(grok_title(Some("fix login - grok"), false), Some(TitleSignal::Idle));
        assert_eq!(grok_title(Some("⠋ - Running: cargo test - fix login - grok"), false), Some(TitleSignal::Busy(Some("Running: cargo test".into()))));
        assert_eq!(grok_title(Some("⠙ - grok"), false), Some(TitleSignal::Busy(None)));
        assert_eq!(grok_title(Some("⚠ Action Required - ⠙ - Waiting - grok"), false), Some(TitleSignal::Attention));
        // The label blinked off, but was there a moment ago.
        assert_eq!(grok_title(Some("⠙ - Waiting - grok"), true), Some(TitleSignal::Attention));
    }

    #[test]
    fn grok_follows_its_title() {
        let mut a = test_agent(Kind::Grok);
        a.set_state(State::Idle, false);
        a.on_title("⠋ - Thinking - grok".into());
        a.heuristics(false);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "Thinking"));
        assert!(a.turn_seen);
        a.on_title("⚠ Action Required - ⠙ - Waiting - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked);
        // Blinking off while unfocused: still waiting.
        a.on_title("⠹ - Waiting - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked);
        // Answered: the label stays gone.
        a.state_since = ago(Duration::from_secs(3));
        a.title_attention = Some(ago(Duration::from_secs(2)));
        a.heuristics(false);
        assert_eq!(a.state, State::Working);
        a.work_started = Some(ago(Duration::from_secs(10)));
        a.on_title("grok".into());
        a.last_output = ago(Duration::from_secs(1));
        a.heuristics(false);
        assert_eq!(a.state, State::Done);
        a.shutdown();
    }

    #[test]
    fn grok_with_user_hooks_takes_only_approvals_from_the_title() {
        let mut a = test_agent(Kind::Grok);
        a.set_state(State::Idle, false);
        a.on_hook(&hook("grok", "UserPromptSubmit", None, Some("go")), false);
        assert!(a.hooked && a.state == State::Working);
        a.on_title("⚠ Action Required - ⠙ - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked);
        a.state_since = ago(Duration::from_secs(3));
        a.title_attention = Some(ago(Duration::from_secs(2)));
        a.on_title("⠙ - Running: ls - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Working);
        a.on_hook(&hook("grok", "Stop", None, None), false);
        assert_eq!(a.state, State::Done);
        a.shutdown();
    }

    #[test]
    fn grok_answered_stays_answered_while_the_label_fades() {
        // With the user's hooks: approved, the tool runs and the turn ends,
        // all within the blink window. Never back to "needs you".
        let mut a = test_agent(Kind::Grok);
        a.set_state(State::Idle, false);
        a.on_hook(&hook("grok", "UserPromptSubmit", None, Some("go")), false);
        a.on_title("⚠ Action Required - ⠙ - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked);
        std::thread::sleep(Duration::from_millis(5));
        a.on_hook(&hook("grok", "PostToolUse", Some("edit"), Some("src/main.rs")), false);
        assert_eq!(a.state, State::Working);
        a.on_title("⠹ - Editing - grok".into());
        assert_eq!(a.heuristics(false), None, "the label a moment ago was the answered one");
        assert_eq!(a.state, State::Working);
        a.on_hook(&hook("grok", "Stop", None, None), false);
        a.heuristics(false);
        assert_eq!(a.state, State::Done, "the Done stays");

        // Without hooks: the turn ends right after the answer.
        let mut b = test_agent(Kind::Grok);
        b.set_state(State::Idle, false);
        b.on_title("⠋ - Thinking - grok".into());
        b.heuristics(false);
        b.work_started = Some(ago(Duration::from_secs(10)));
        b.on_title("⚠ Action Required - ⠙ - grok".into());
        b.heuristics(false);
        assert_eq!(b.state, State::Blocked);
        b.on_title("fix login - grok".into());
        b.heuristics(false);
        assert_eq!(b.state, State::Blocked, "the label blinks: still waiting");
        b.state_since = ago(Duration::from_secs(3));
        b.title_attention = Some(ago(Duration::from_secs(2)));
        b.last_output = ago(Duration::from_secs(1));
        b.heuristics(false);
        assert_eq!(b.state, State::Done);
        assert!(b.activity.starts_with("finished after"), "{}", b.activity);
        a.shutdown();
        b.shutdown();
    }

    #[test]
    fn grok_label_seen_between_ticks_still_blocks() {
        let mut a = test_agent(Kind::Grok);
        a.set_state(State::Idle, false);
        a.on_title("⠋ - Thinking - grok".into());
        a.heuristics(false);
        std::thread::sleep(Duration::from_millis(5));
        // On and off again before the next tick.
        a.on_title("⚠ Action Required - ⠙ - grok".into());
        a.on_title("⠙ - Waiting - grok".into());
        a.heuristics(false);
        assert_eq!(a.state, State::Blocked);
        a.shutdown();
    }

    #[test]
    fn grok_question_mid_turn_reads_from_the_screen() {
        for title in ["⠙ - Waiting - grok", "fix login - grok"] {
            let mut a = test_agent(Kind::Grok);
            a.set_state(State::Idle, false);
            a.on_title("⠋ - Thinking - grok".into());
            a.heuristics(false);
            a.work_started = Some(ago(Duration::from_secs(10)));
            assert_eq!(a.state, State::Working);
            // Its question UI, while the title spins on: never quiet.
            a.on_title(title.into());
            show(&a, "Which database?\n (○) Postgres\n (○) SQLite\nTab:next answer  Enter:submit  x:dismiss\n");
            redraw(&mut a);
            a.heuristics(false);
            assert_eq!(a.state, State::Blocked, "{title}: {}", a.activity);
            redraw(&mut a);
            a.heuristics(false);
            assert_eq!(a.state, State::Blocked, "{title}: stays while the question is up");
            // Answered: the UI goes and the title takes over.
            show(&a, "\x1b[2J\x1b[HUsing Postgres.\n");
            a.on_title("⠋ - Thinking - grok".into());
            redraw(&mut a);
            a.heuristics(false);
            assert_eq!(a.state, State::Working, "{title}");
            a.shutdown();
        }
    }

    #[test]
    fn an_exited_agent_keeps_the_folder_it_was_last_in() {
        let mut a = test_agent(Kind::Shell);
        let real = |p: &Path| std::fs::canonicalize(p).unwrap();
        let seen = a.live_cwd();
        a.set_state(State::Exited, false);
        // Its pid could be anyone's now: say, ours.
        a.pid = std::process::id();
        assert_eq!(real(&a.live_cwd()), real(&seen));
        a.shutdown();
    }

    #[test]
    fn grok_permission_hook_shows_its_message() {
        let mut a = test_agent(Kind::Grok);
        a.set_state(State::Idle, false);
        a.on_hook(&hook("grok", "UserPromptSubmit", None, Some("go")), false);
        a.on_hook(&hook("grok", "PermissionRequest", Some("permission"), Some("Grok needs your permission")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Grok needs your permission"));
        let mut b = test_agent(Kind::Grok);
        b.set_state(State::Idle, false);
        b.on_hook(&hook("grok", "UserPromptSubmit", None, Some("go")), false);
        b.on_hook(&hook("grok", "PermissionRequest", Some("permission"), None), false);
        assert_eq!((b.state, b.activity.as_str()), (State::Blocked, "approve permission"));
        a.shutdown();
        b.shutdown();
    }

    #[test]
    fn copilot_has_its_own_letter() {
        assert_eq!(Kind::Copilot.shortcut(), Some('p'));
        assert_eq!(Kind::from_shortcut('p'), Some(Kind::Copilot));
        assert_eq!(Kind::from_label("copilot"), Some(Kind::Copilot));
        assert!(Kind::Copilot.takes_prompts() && Kind::Copilot.resumes() && !Kind::Copilot.prompts_only_on_screen());
    }

    #[test]
    fn copilot_argv_loads_the_plugin_and_names_the_conversation() {
        let mut s = spec(Kind::Copilot);
        s.prompt = Some("fix it".into());
        let id = "0b5e2c1a-7f3d-4c2e-9a1b-2d3e4f5a6b7c";
        assert_eq!(
            command_for(&s, &ints(), Path::new("/bin/trix"), Some(id)),
            ("copilot".into(), vec!["--plugin-dir".into(), "/run/copilot-plugin-1".into(), format!("--session-id={id}"), "--interactive=fix it".into()])
        );
        // Restored: the same flag resumes it. A prompt starting with `-` stays a value.
        s.resume = Some("abc".into());
        s.command = Some("--model gpt-5".into());
        s.prompt = Some("- go on".into());
        assert_eq!(command_for(&s, &Integrations::default(), Path::new("/bin/trix"), None).1, ["--session-id=abc", "--model", "gpt-5", "--interactive=- go on"]);
        // Nothing to name, nothing to load: just copilot.
        assert!(command_for(&spec(Kind::Copilot), &Integrations::default(), Path::new("/bin/trix"), None).1.is_empty());
    }

    #[test]
    fn copilot_leaves_the_conversation_to_flags_that_pick_one() {
        assert!(!copilot_picks_session(None));
        assert!(!copilot_picks_session(Some("--model gpt-5 -s")));
        for f in ["--continue", "-r", "--resume", "--resume=x", "--session-id abc", "--session-id=abc", "--connect"] {
            assert!(copilot_picks_session(Some(f)), "{f}");
        }
    }

    #[test]
    fn grok_and_copilot_get_a_new_conversation_id() {
        for k in [Kind::Grok, Kind::Copilot] {
            let mut s = spec(k);
            assert!(new_conversation(&s).is_some(), "{k:?}");
            s.command = Some("--resume=x".into());
            assert_eq!(new_conversation(&s), None, "{k:?}: the flags pick one");
            s.command = None;
            s.resume = Some("x".into());
            assert_eq!(new_conversation(&s), None, "{k:?}: resumed");
        }
        for k in [Kind::Claude, Kind::Codex, Kind::OpenCode, Kind::Cursor, Kind::Shell, Kind::Custom] {
            assert_eq!(new_conversation(&spec(k)), None, "{k:?}");
        }
    }

    fn copilot(event: &str, tool: Option<&str>, detail: Option<&str>) -> HookMsg {
        hook("copilot", event, tool, detail)
    }

    #[test]
    fn copilot_turn_from_its_hooks() {
        let mut a = test_agent(Kind::Copilot);
        a.set_state(State::Idle, false);
        // A turn's end before any turn means nothing.
        assert_eq!(a.on_hook(&copilot("Stop", None, None), false), None);
        assert_eq!(a.state, State::Idle);
        a.on_hook(&copilot("SessionStart", None, None), false);
        assert!(a.hooked);
        a.on_hook(&copilot("UserPromptSubmit", None, Some("fix the login")), false);
        assert_eq!((a.state, a.activity.as_str(), a.last_prompt.as_deref()), (State::Working, "thinking", Some("fix the login")));
        a.on_hook(&copilot("PreToolUse", Some("Bash"), Some("cargo test")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Working, "Bash cargo test"));
        // Its permission prompt: a notification naming no tool.
        a.on_hook(&copilot("PermissionRequest", None, Some("Copilot needs your permission")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "approve Bash cargo test"));
        a.on_hook(&copilot("PostToolUse", Some("Bash"), None), false);
        assert_eq!(a.state, State::Working);
        // No tool waiting: Copilot's own words.
        a.on_hook(&copilot("PermissionRequest", None, Some("Copilot needs your permission")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Copilot needs your permission"));
        a.on_hook(&copilot("PostToolUse", Some("Read"), None), false);
        a.on_hook(&copilot("Stop", None, None), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Done, "finished · 1 tool"));
        a.shutdown();
    }

    #[test]
    fn copilot_ask_user_needs_you() {
        let mut a = test_agent(Kind::Copilot);
        a.set_state(State::Idle, false);
        a.on_hook(&copilot("UserPromptSubmit", None, Some("set it up")), false);
        a.on_hook(&copilot("PreToolUse", Some("AskUserQuestion"), Some("Which database?")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Which database?"));
        // Its dialog's notification keeps the question.
        a.on_hook(&copilot("Question", None, Some("Copilot has a question")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Which database?"));
        a.on_hook(&copilot("PostToolUse", Some("AskUserQuestion"), None), false);
        assert_eq!(a.state, State::Working);
        // A dialog no tool announced still needs you.
        a.on_hook(&copilot("Question", None, Some("Sign in to the MCP server")), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "Sign in to the MCP server"));
        a.shutdown();
    }

    #[test]
    fn copilot_permission_with_nothing_to_say() {
        let mut a = test_agent(Kind::Copilot);
        a.set_state(State::Idle, false);
        a.on_hook(&copilot("UserPromptSubmit", None, None), false);
        a.on_hook(&copilot("PermissionRequest", None, None), false);
        assert_eq!((a.state, a.activity.as_str()), (State::Blocked, "needs your approval"));
        // A finished turn forgets the tool it last ran.
        a.on_hook(&copilot("PreToolUse", Some("Bash"), Some("ls")), false);
        a.on_hook(&copilot("Stop", None, None), false);
        a.on_hook(&copilot("UserPromptSubmit", None, None), false);
        a.on_hook(&copilot("PermissionRequest", None, Some("needs permission")), false);
        assert_eq!(a.activity, "needs permission");
        a.shutdown();
    }
}
