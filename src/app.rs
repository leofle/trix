use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent::{Agent, Kind, Spawn, State};
use crate::hooks::HookMsg;
use crate::input;
use crate::search::{self, Hit};
use crate::session::{Out, SessionEvent};
use crate::state::{self, Saved, SavedAgent};
use crate::theme::{Theme, ThemeWatcher};

pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    FocusGained,
    FocusLost,
    Output(u32),
    PtyWrite(u32, String),
    PtyWriteFn(u32, Arc<dyn Fn(WindowSize) -> String + Send + Sync>),
    Title(u32, String),
    Bell(u32),
    Exit(u32, Option<i32>),
    Clipboard(String),
    Hook(HookMsg),
    Git(u32, Option<String>, Option<(u32, u32, u32)>),
    /// Attach / detach / control traffic; handled by the server loop.
    Session(SessionEvent),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Focus,
    Grid,
}

#[derive(Default, Clone)]
pub struct TextInput {
    pub text: String,
    pub cursor: usize, // in chars
}

impl TextInput {
    pub fn new(s: &str) -> Self {
        TextInput { text: s.into(), cursor: s.chars().count() }
    }
    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(self.text.len())
    }
    pub fn insert(&mut self, s: &str) {
        let b = self.byte(self.cursor);
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(b, &clean);
        self.cursor += clean.chars().count();
    }
    /// Returns true when the key was consumed.
    pub fn key(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.text.chars().count();
        match k.code {
            KeyCode::Char('u') if ctrl => {
                let b = self.byte(self.cursor);
                self.text.drain(..b);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                let (a, b) = (self.byte(i), self.byte(self.cursor));
                self.text.drain(a..b);
                self.cursor = i;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char(c) if !ctrl => self.insert(&c.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                let b = self.byte(self.cursor - 1);
                self.text.remove(b);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let b = self.byte(self.cursor);
                self.text.remove(b);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }
}

pub struct NewForm {
    pub kind: usize,
    pub field: usize,
    pub name: TextInput,
    pub name_touched: bool,
    pub dir: TextInput,
    pub prompt: TextInput,
    pub worktree: bool,
    pub args: TextInput,
    pub error: Option<String>,
}

pub const NEW_FIELDS: [&str; 6] = ["Agent", "Folder", "Name", "Prompt", "Worktree", "Args"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BroadcastTarget {
    All,
    Waiting,
}

/// The alt+f search box.
#[derive(Default)]
pub struct Search {
    pub input: TextInput,
    pub hits: Vec<Hit>,
    pub sel: usize,
    /// How many agents have at least one hit.
    pub agents: usize,
    /// Some agent had more hits than are kept.
    pub capped: bool,
}

pub enum Modal {
    New(NewForm),
    Search(Search),
    Broadcast(TextInput, BroadcastTarget),
    Rename(u32, TextInput),
    ConfirmClose(u32),
    ConfirmQuit,
    /// A fresh server found a saved workspace: bring it back?
    Restore,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Good,
    Bad,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub at: Instant,
}

/// Screen regions from the last frame, for mouse hit-testing.
#[derive(Default)]
pub struct HitMap {
    pub cards: Vec<(Rect, u32)>,
    pub panes: Vec<(Rect, u32)>,
    pub new_button: Option<Rect>,
}

struct Drag {
    agent: u32,
    origin: Rect,
    moved: bool,
}

pub struct App {
    pub agents: Vec<Agent>,
    specs: HashMap<u32, Spawn>,
    pub focus: usize,
    pub view: View,
    pub sidebar: bool,
    /// alt+m: a narrow sidebar, one line per agent.
    pub sidebar_mini: bool,
    pub modal: Option<Modal>,
    pub theme: Theme,
    theme_watch: ThemeWatcher,
    pub themed_by_omarchy: bool,
    tx: Sender<AppEvent>,
    socket: PathBuf,
    claude_settings: Option<PathBuf>,
    exe: PathBuf,
    pub term_focused: bool,
    next_id: u32,
    pub toast: Option<Toast>,
    pub hits: HitMap,
    pub quit: bool,
    /// alt+d: let the client go, keep everything running.
    pub detach: bool,
    pub dirty: bool,
    pub started: Instant,
    pub start_dir: PathBuf,
    drag: Option<Drag>,
    git_targets: Arc<Mutex<Vec<(u32, PathBuf)>>>,
    pub last_title: String,
    pub pane_size: (u16, u16),
    /// Read-only terminals watching (`trix view`).
    pub viewers: usize,
    /// Told the human once that Codex isn't sending its hooks.
    codex_hint: bool,
    /// The attached client's terminal (title, bell, clipboard passthrough).
    out: Out,
    /// Saved workspace awaiting the human's restore / start-fresh answer.
    /// Kept in every save until then, so detaching first loses nothing.
    pub pending_restore: Option<Saved>,
    last_saved: Option<Saved>,
    last_completion_sound: Option<Instant>,
    /// Restored custom commands, shown exited until the human starts them.
    parked: HashSet<u32>,
    /// `trix wait`s still waiting.
    pub waiters: Vec<crate::control::Waiter>,
}

impl App {
    pub fn new(tx: Sender<AppEvent>, socket: PathBuf, claude_settings: Option<PathBuf>, exe: PathBuf, out: Out) -> App {
        let mut theme_watch = ThemeWatcher::new();
        let polled = theme_watch.poll();
        let themed = polled.is_some();
        let git_targets = Arc::new(Mutex::new(Vec::new()));
        spawn_git_watcher(git_targets.clone(), tx.clone());
        let prefs = state::load_prefs();
        App {
            agents: Vec::new(),
            specs: HashMap::new(),
            focus: 0,
            view: View::Focus,
            sidebar: true,
            sidebar_mini: prefs.sidebar_mini,
            modal: None,
            theme: polled.unwrap_or_default(),
            theme_watch,
            themed_by_omarchy: themed,
            tx,
            socket,
            claude_settings,
            exe,
            term_focused: true,
            next_id: 1,
            toast: None,
            hits: HitMap::default(),
            quit: false,
            detach: false,
            dirty: true,
            started: Instant::now(),
            start_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            drag: None,
            git_targets,
            last_title: String::new(),
            pane_size: (80, 24),
            viewers: 0,
            codex_hint: false,
            out,
            pending_restore: state::load(),
            last_saved: None,
            last_completion_sound: None,
            parked: HashSet::new(),
            waiters: Vec::new(),
        }
    }

    /// A terminal attached: it starts from a blank screen and its own folder.
    pub fn on_attach(&mut self, cwd: PathBuf) {
        if cwd.is_dir() {
            self.start_dir = cwd;
        }
        self.term_focused = true;
        if let Some(a) = self.agents.get_mut(self.focus) {
            a.unseen = false;
        }
        self.last_title.clear();
        self.update_title();
        if self.pending_restore.is_some() && self.modal.is_none() {
            self.modal = Some(Modal::Restore);
        }
        self.dirty = true;
    }

    // ───────────────────────────── restore ─────────────────────────────

    /// What a new server would need to rebuild this workspace.
    fn snapshot(&self) -> Saved {
        let mut agents: Vec<SavedAgent> = self
            .agents
            .iter()
            .filter(|a| a.state != State::Exited || self.parked.contains(&a.id))
            .filter_map(|a| {
                let spec = self.specs.get(&a.id)?;
                Some(SavedAgent {
                    kind: a.kind.label().into(),
                    name: a.name.clone(),
                    // A shell may have cd'd since; agents stay where they started.
                    cwd: if a.kind == Kind::Shell { a.live_cwd() } else { spec.cwd.clone() },
                    command: spec.command.clone(),
                    session: if matches!(a.kind, Kind::Claude | Kind::Codex) { a.session.clone() } else { None },
                })
            })
            .collect();
        let focus = self.agents.get(self.focus).and_then(|f| agents.iter().position(|s| s.name == f.name)).unwrap_or(0);
        if let Some(p) = &self.pending_restore {
            agents.extend(p.agents.iter().cloned());
        }
        Saved::new(focus, agents)
    }

    /// Save the workspace if it changed since last time.
    pub fn persist(&mut self) {
        let snap = self.snapshot();
        if self.last_saved.as_ref() == Some(&snap) {
            return;
        }
        match state::store(&snap) {
            Ok(()) => self.last_saved = Some(snap),
            Err(e) => eprintln!("trix: could not save workspace to {}: {e}", state::path().display()),
        }
    }

    /// Relaunch the saved agents: Claude and Codex back in their conversation,
    /// shells in their folder. Custom commands could do anything when re-run,
    /// so they come back stopped and wait for an Enter.
    fn restore_saved(&mut self) {
        let Some(saved) = self.pending_restore.take() else { return };
        let (mut restored, mut resumed, mut skipped) = (0, 0, Vec::new());
        let mut focus = None;
        for (i, s) in saved.agents.into_iter().enumerate() {
            let Some(kind) = Kind::from_label(&s.kind) else { continue };
            if !s.cwd.is_dir() {
                skipped.push(s.name);
                continue;
            }
            let resume = s.session.clone().filter(|_| matches!(kind, Kind::Claude | Kind::Codex));
            let spec = Spawn {
                kind,
                name: self.unique_name(&s.name),
                cwd: s.cwd,
                command: s.command,
                prompt: None,
                branch_note: Some(if resume.is_some() { "resuming conversation" } else { "restored" }.into()),
                resume,
            };
            let res = if kind == Kind::Custom {
                let cmd = spec.command.clone().unwrap_or_default();
                let note = shell_words::quote(&format!("  was running: {cmd}")).into_owned();
                let shown = Spawn {
                    command: Some(format!("printf '%s\\n' '' '  restored from your last session' {note} '  press enter to start it again'")),
                    ..clone_spec(&spec)
                };
                self.spawn_as(shown, spec).map(|id| {
                    self.parked.insert(id);
                })
            } else {
                if spec.resume.is_some() {
                    resumed += 1;
                }
                self.spawn(spec)
            };
            match res {
                Ok(()) => {
                    restored += 1;
                    if i == saved.focus || focus.is_none() {
                        focus = Some(self.agents.len() - 1);
                    }
                }
                Err(e) => self.toast(format!("could not restore: {e}"), ToastKind::Bad),
            }
        }
        if let Some(f) = focus {
            self.set_focus(f);
        }
        let mut msg = format!("restored {restored} agent{}", if restored == 1 { "" } else { "s" });
        if resumed > 0 {
            msg.push_str(&format!(" · {resumed} conversation{} resumed", if resumed == 1 { "" } else { "s" }));
        }
        if !skipped.is_empty() {
            msg.push_str(&format!(" · skipped {} (folder gone)", skipped.join(", ")));
        }
        self.toast(msg, if skipped.is_empty() { ToastKind::Good } else { ToastKind::Info });
    }

    /// Nobody is looking any more: every state change becomes a notification.
    pub fn on_detach(&mut self) {
        self.term_focused = false;
        self.drag = None;
        self.modal = None;
    }

    /// Plain-text status for `trix ls`.
    pub fn listing(&self) -> String {
        let count = |s: State| self.agents.iter().filter(|a| a.state == s).count();
        let session = if crate::session::is_default() { String::new() } else { format!("{} · ", crate::session::name()) };
        let mut out = format!(
            "trix · {session}{} agent{} · {} need{} you · {} working · {} done\n",
            self.agents.len(),
            if self.agents.len() == 1 { "" } else { "s" },
            count(State::Blocked),
            if count(State::Blocked) == 1 { "s" } else { "" },
            count(State::Working),
            count(State::Done),
        );
        if self.viewers > 0 {
            out.insert_str(out.len() - 1, &format!(" · {} watching", self.viewers));
        }
        let w = self.agents.iter().map(|a| a.name.chars().count()).max().unwrap_or(0);
        for (i, a) in self.agents.iter().enumerate() {
            let since = crate::ui::fmt_dur(a.state_since.elapsed());
            let mark = if a.unseen { "•" } else { " " };
            let name = format!("{:<w$}", a.name);
            out.push_str(&format!(
                "{:>2} {mark} {name}  {:<6}  {:<9} {:>6}  {}  {}\n",
                i + 1,
                a.kind.label(),
                a.state.label(),
                since,
                crate::hooks::one_line(&a.activity, 50),
                tilde(&a.cwd),
            ));
        }
        out
    }

    pub fn focused(&self) -> Option<&Agent> {
        self.agents.get(self.focus)
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.agents.iter().position(|a| a.id == id)
    }

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast { text: text.into(), kind, at: Instant::now() });
        self.dirty = true;
    }

    pub fn animating(&self) -> bool {
        self.agents.iter().any(|a| matches!(a.state, State::Working | State::Blocked | State::Starting))
            || self.toast.as_ref().is_some_and(|t| t.at.elapsed() < Duration::from_secs(4))
    }

    // ───────────────────────────── spawning ─────────────────────────────

    pub fn open_new(&mut self) {
        let dir = self
            .focused()
            .map(|a| a.live_cwd())
            .unwrap_or_else(|| self.start_dir.clone());
        let mut f = NewForm {
            kind: 0,
            field: 0,
            name: TextInput::default(),
            name_touched: false,
            dir: TextInput::new(&tilde(&dir)),
            prompt: TextInput::default(),
            worktree: false,
            args: TextInput::default(),
            error: None,
        };
        f.name = TextInput::new(&self.suggest_name(&dir));
        self.modal = Some(Modal::New(f));
    }

    fn suggest_name(&self, dir: &Path) -> String {
        let base = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "agent".into());
        self.unique_name(&slug(&base))
    }

    pub fn unique_name(&self, base: &str) -> String {
        if !self.agents.iter().any(|a| a.name == base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}-{n}")).find(|n| !self.agents.iter().any(|a| &a.name == n)).unwrap()
    }

    pub fn spawn(&mut self, spec: Spawn) -> anyhow::Result<()> {
        self.spawn_as(clone_spec(&spec), spec).map(|_| ())
    }

    /// Start an agent without taking the human's focus off what they're
    /// looking at (`trix spawn`).
    pub fn spawn_quiet(&mut self, spec: Spawn) -> anyhow::Result<u32> {
        let id = self.start(clone_spec(&spec), spec)?;
        if self.agents.len() == 1 {
            self.set_focus(0);
        }
        self.dirty = true;
        Ok(id)
    }

    /// Run `run`, but remember `keep` as what Enter-to-restart launches.
    fn spawn_as(&mut self, run: Spawn, keep: Spawn) -> anyhow::Result<u32> {
        let id = self.start(run, keep)?;
        self.set_focus(self.agents.len() - 1);
        Ok(id)
    }

    fn start(&mut self, run: Spawn, keep: Spawn) -> anyhow::Result<u32> {
        let id = self.next_id;
        self.next_id += 1;
        let agent = Agent::spawn(id, run, self.pane_size, self.tx.clone(), &self.socket, self.claude_settings.as_deref(), &self.exe)?;
        self.specs.insert(id, keep);
        self.agents.push(agent);
        self.sync_git_targets();
        Ok(id)
    }

    fn launch_form(&mut self) {
        let Some(Modal::New(f)) = &mut self.modal else { return };
        let kind = Kind::ALL[f.kind];
        let mut cwd = expand(&f.dir.text);
        if !cwd.is_dir() {
            f.error = Some(format!("folder not found: {}", f.dir.text));
            return;
        }
        if kind == Kind::Custom && f.args.text.trim().is_empty() {
            f.error = Some("custom agents need a command".into());
            f.field = 5;
            return;
        }
        let name = if f.name.text.trim().is_empty() { "agent".to_string() } else { f.name.text.trim().to_string() };
        let mut note = None;
        if f.worktree {
            match make_worktree(&cwd, &slug(&name)) {
                Ok((path, branch, reused)) => {
                    if let Some(other) = self.agents.iter().find(|a| a.cwd == path) {
                        f.error = Some(format!("{branch} is already {}'s worktree — pick another name", other.name));
                        f.field = 2;
                        return;
                    }
                    cwd = path;
                    note = Some(format!("{} worktree on {branch}", if reused { "reusing" } else { "new" }));
                }
                Err(e) => {
                    f.error = Some(e.to_string());
                    return;
                }
            }
        }
        let spec = Spawn {
            kind,
            name,
            cwd,
            command: Some(f.args.text.clone()).filter(|s| !s.trim().is_empty()),
            prompt: Some(f.prompt.text.clone()).filter(|s| !s.trim().is_empty()),
            branch_note: note,
            resume: None,
        };
        self.modal = None;
        if let Err(e) = self.spawn(spec) {
            self.toast(format!("could not start: {e}"), ToastKind::Bad);
        }
    }

    fn restart(&mut self, idx: usize) {
        let old_id = self.agents[idx].id;
        let Some(mut spec) = self.specs.remove(&old_id) else { return };
        spec.prompt = None;
        spec.resume = None;
        self.parked.remove(&old_id);
        // A fresh id, so late hook / exit events from the old process can't
        // land on the new one.
        let id = self.next_id;
        self.next_id += 1;
        let size = self.agents[idx].size;
        match Agent::spawn(id, clone_spec(&spec), size, self.tx.clone(), &self.socket, self.claude_settings.as_deref(), &self.exe) {
            Ok(a) => {
                self.agents[idx].shutdown();
                self.agents[idx] = a;
                self.toast(format!("restarted {}", spec.name), ToastKind::Good);
                self.specs.insert(id, spec);
                self.sync_git_targets();
            }
            Err(e) => {
                self.specs.insert(old_id, spec);
                self.toast(format!("restart failed: {e}"), ToastKind::Bad);
            }
        }
    }

    pub fn close(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            let a = self.agents.remove(i);
            a.shutdown();
            self.specs.remove(&id);
            self.parked.remove(&id);
            if self.focus >= self.agents.len() {
                self.focus = self.agents.len().saturating_sub(1);
            }
            self.toast(format!("closed {}", a.name), ToastKind::Info);
            self.sync_git_targets();
        }
    }

    fn sync_git_targets(&self) {
        let list = self.agents.iter().map(|a| (a.id, a.live_cwd())).collect();
        *self.git_targets.lock().unwrap() = list;
    }

    pub fn set_focus(&mut self, idx: usize) {
        if idx >= self.agents.len() {
            return;
        }
        let prev = self.focus;
        if prev != idx {
            if let Some(a) = self.agents.get(prev) {
                if a.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                    a.write(&b"\x1b[O"[..]);
                }
            }
            let a = &self.agents[idx];
            if a.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                a.write(&b"\x1b[I"[..]);
            }
        }
        self.focus = idx;
        self.agents[idx].unseen = false;
        self.dirty = true;
    }

    /// Jump to the agent that most needs the human: blocked first, then
    /// unseen finished work, oldest first.
    pub fn next_attention(&mut self) {
        let mut best: Option<(u8, Instant, usize)> = None;
        for (i, a) in self.agents.iter().enumerate() {
            if i == self.focus && !a.unseen {
                continue;
            }
            let rank = match a.state {
                State::Blocked => 0,
                State::Done if a.unseen => 1,
                State::Exited if a.unseen => 2,
                _ => continue,
            };
            let cand = (rank, a.state_since, i);
            if best.is_none_or(|b| (cand.0, cand.1) < (b.0, b.1)) {
                best = Some(cand);
            }
        }
        match best {
            Some((_, _, i)) => self.set_focus(i),
            None => self.toast("all clear — nobody needs you", ToastKind::Good),
        }
    }

    // ───────────────────────────── events ─────────────────────────────

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(k) => {
                if k.kind != KeyEventKind::Release {
                    self.on_key(k);
                    self.dirty = true;
                }
            }
            AppEvent::Mouse(m) => self.on_mouse(m),
            AppEvent::Paste(s) => self.on_paste(s),
            AppEvent::Resize => self.dirty = true,
            AppEvent::FocusGained => {
                self.term_focused = true;
                if let Some(a) = self.agents.get_mut(self.focus) {
                    a.unseen = false;
                }
                self.dirty = true;
            }
            AppEvent::FocusLost => self.term_focused = false,
            AppEvent::Output(id) => {
                if let Some(i) = self.index_of(id) {
                    self.agents[i].on_output();
                    // Only redraw for output we can actually see.
                    if self.view == View::Grid || i == self.focus {
                        self.dirty = true;
                    }
                }
            }
            AppEvent::PtyWrite(id, s) => {
                if let Some(i) = self.index_of(id) {
                    self.agents[i].write(s.into_bytes());
                }
            }
            AppEvent::PtyWriteFn(id, f) => {
                if let Some(i) = self.index_of(id) {
                    let (c, l) = self.agents[i].size;
                    let ws = WindowSize { num_cols: c, num_lines: l, cell_width: 9, cell_height: 18 };
                    self.agents[i].write(f(ws).into_bytes());
                }
            }
            AppEvent::Title(id, t) => {
                if let Some(i) = self.index_of(id) {
                    self.agents[i].title = Some(t);
                    self.dirty = true;
                }
            }
            AppEvent::Bell(id) => {
                if let Some(i) = self.index_of(id) {
                    if i != self.focus || !self.term_focused {
                        self.agents[i].unseen = true;
                        self.dirty = true;
                    }
                }
            }
            AppEvent::Exit(id, code) => {
                if let Some(i) = self.index_of(id) {
                    let focused = i == self.focus && self.term_focused;
                    let a = &mut self.agents[i];
                    a.exit_code = code;
                    a.activity = match code {
                        Some(0) | None => "exited".into(),
                        Some(c) => format!("exited with code {c}"),
                    };
                    a.set_state(State::Exited, focused);
                    a.unseen = !focused;
                    if self.parked.contains(&id) {
                        // Not news: it was restored stopped on purpose.
                        a.activity = "restored · enter to start".into();
                        a.unseen = false;
                    }
                    self.dirty = true;
                }
            }
            AppEvent::Clipboard(s) => copy_to_clipboard(&self.out, &s),
            AppEvent::Hook(msg) => {
                if let Some(i) = self.index_of(msg.agent) {
                    let focused = i == self.focus && self.term_focused;
                    if let Some(prev) = self.agents[i].on_hook(&msg, focused) {
                        self.transitioned(i, prev);
                    }
                    // A finished turn through `notify` but never a lifecycle
                    // hook: Codex has them, they just aren't trusted yet.
                    if msg.event == "agent-turn-complete" && !self.agents[i].hooked && !self.codex_hint {
                        self.codex_hint = true;
                        self.toast("for live Codex detail, trust trix's hooks once in Codex: /hooks", ToastKind::Info);
                    }
                    self.dirty = true;
                }
            }
            AppEvent::Session(_) => {}
            AppEvent::Git(id, branch, diff) => {
                if let Some(i) = self.index_of(id) {
                    self.agents[i].branch = branch;
                    self.agents[i].diff = diff;
                    self.dirty = true;
                }
            }
        }
    }

    /// Timer: heuristics, theme reload, toast expiry, terminal title.
    pub fn tick(&mut self) {
        for i in 0..self.agents.len() {
            let focused = i == self.focus && self.term_focused;
            if let Some(prev) = self.agents[i].heuristics(focused) {
                self.transitioned(i, prev);
                self.dirty = true;
            }
        }
        if self.toast.as_ref().is_some_and(|t| t.at.elapsed() > Duration::from_secs(4)) {
            self.toast = None;
            self.dirty = true;
        }
        self.update_title();
    }

    pub fn slow_tick(&mut self) {
        if let Some(t) = self.theme_watch.poll() {
            self.theme = t;
            self.themed_by_omarchy = true;
            self.dirty = true;
        }
        self.sync_git_targets();
        self.persist();
        self.dirty = true; // clocks & elapsed timers
    }

    fn transitioned(&mut self, i: usize, prev: State) {
        let a = &self.agents[i];
        let looking = i == self.focus && self.term_focused;
        let (title, body, urgent) = match a.state {
            State::Blocked => (format!("{} needs you", a.name), a.activity.clone(), true),
            State::Done if prev == State::Working => {
                (format!("{} finished", a.name), a.activity.clone(), false)
            }
            _ => return,
        };
        let title = if crate::session::is_default() { title } else { format!("{title} · {}", crate::session::name()) };
        if !looking {
            desktop_notify(&title, &body, urgent);
            if a.state == State::Done && std::env::var_os("TRIX_NO_SOUND").is_none() {
                let now = Instant::now();
                if completion_sound_due(self.last_completion_sound, now) {
                    self.last_completion_sound = Some(now);
                    play_completion_sound();
                }
            }
            if !self.term_focused {
                // Bell → compositor urgency hint on the terminal window.
                self.out.send_all(b"\x07");
            }
        }
    }

    fn update_title(&mut self) {
        let blocked = self.agents.iter().filter(|a| a.state == State::Blocked).count();
        let working = self.agents.iter().filter(|a| a.state == State::Working).count();
        let base = if crate::session::is_default() { "trix".to_string() } else { format!("trix · {}", crate::session::name()) };
        let title = match (blocked, working) {
            (0, 0) => base,
            (0, w) => format!("{base} · {w} working"),
            (b, _) => format!("{base} · {b} need{} you", if b == 1 { "s" } else { "" }),
        };
        if title != self.last_title {
            self.out.send_all(format!("\x1b]0;{title}\x07").as_bytes());
            self.last_title = title;
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.modal.is_some() {
            self.modal_key(k);
            return;
        }
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if alt && !ctrl {
            let handled = match k.code {
                KeyCode::Char('n') => {
                    self.open_new();
                    true
                }
                KeyCode::Char(c @ '1'..='9') => {
                    self.set_focus(c as usize - '1' as usize);
                    true
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if !self.agents.is_empty() {
                        self.set_focus((self.focus + 1) % self.agents.len());
                    }
                    true
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !self.agents.is_empty() {
                        self.set_focus((self.focus + self.agents.len() - 1) % self.agents.len());
                    }
                    true
                }
                KeyCode::Char('a') => {
                    self.next_attention();
                    true
                }
                KeyCode::Char('g') => {
                    self.view = if self.view == View::Grid { View::Focus } else { View::Grid };
                    true
                }
                KeyCode::Char('s') => {
                    self.sidebar = !self.sidebar;
                    true
                }
                KeyCode::Char('m') => {
                    // From hidden, alt+m brings the sidebar back small.
                    self.sidebar_mini = !self.sidebar || !self.sidebar_mini;
                    self.sidebar = true;
                    if let Err(e) = state::store_prefs(&state::Prefs { sidebar_mini: self.sidebar_mini }) {
                        self.toast(format!("couldn't remember the sidebar size: {e}"), ToastKind::Bad);
                    }
                    true
                }
                KeyCode::Char('f') => {
                    if self.agents.is_empty() {
                        self.toast("no agents to search yet", ToastKind::Info);
                    } else {
                        self.modal = Some(Modal::Search(Search::default()));
                    }
                    true
                }
                KeyCode::Char('b') => {
                    if self.agents.is_empty() {
                        self.toast("no agents to talk to yet", ToastKind::Info);
                    } else {
                        self.modal = Some(Modal::Broadcast(TextInput::default(), BroadcastTarget::All));
                    }
                    true
                }
                KeyCode::Char('r') => {
                    if let Some(a) = self.focused() {
                        self.modal = Some(Modal::Rename(a.id, TextInput::new(&a.name)));
                    }
                    true
                }
                KeyCode::Char('w') => {
                    if let Some(a) = self.focused() {
                        if a.state == State::Exited {
                            let id = a.id;
                            self.close(id);
                        } else {
                            self.modal = Some(Modal::ConfirmClose(a.id));
                        }
                    }
                    true
                }
                KeyCode::Char('d') => {
                    self.detach = true;
                    true
                }
                KeyCode::Char('/') | KeyCode::Char('?') => {
                    self.modal = Some(Modal::Help);
                    true
                }
                KeyCode::Char('q') => {
                    if self.agents.iter().any(|a| a.state != State::Exited) {
                        self.modal = Some(Modal::ConfirmQuit);
                    } else {
                        self.quit = true;
                    }
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }

        let Some(a) = self.agents.get_mut(self.focus) else {
            if k.code == KeyCode::Enter || k.code == KeyCode::Char('n') {
                self.open_new();
            }
            return;
        };

        // Scrollback.
        if k.modifiers.contains(KeyModifiers::SHIFT) && matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) {
            let mut t = a.term.lock();
            if !t.mode().contains(TermMode::ALT_SCREEN) {
                t.scroll_display(if k.code == KeyCode::PageUp { Scroll::PageUp } else { Scroll::PageDown });
                return;
            }
        }

        if a.state == State::Exited {
            if k.code == KeyCode::Enter {
                let i = self.focus;
                self.restart(i);
            }
            return;
        }

        if a.state == State::Blocked && matches!(k.code, KeyCode::Esc | KeyCode::Enter) {
            a.dismissed_at = Some(Instant::now());
        }
        let bytes = {
            let mut t = a.term.lock();
            t.scroll_display(Scroll::Bottom);
            t.selection = None;
            input::encode_key(&k, *t.mode())
        };
        a.user_input(bytes);
    }

    fn on_paste(&mut self, s: String) {
        if let Some(m) = &mut self.modal {
            match m {
                Modal::New(f) => {
                    let first = s.lines().next().unwrap_or("");
                    match f.field {
                        1 => f.dir.insert(first),
                        2 => f.name.insert(first),
                        3 => f.prompt.insert(&s.replace('\n', " ")),
                        5 => f.args.insert(first),
                        _ => {}
                    }
                }
                Modal::Broadcast(t, _) | Modal::Rename(_, t) => t.insert(&s.replace('\n', " ")),
                Modal::Search(q) => {
                    q.input.insert(s.lines().next().unwrap_or(""));
                    self.refresh_search();
                }
                _ => {}
            }
            self.dirty = true;
            return;
        }
        if let Some(a) = self.agents.get_mut(self.focus) {
            let bytes = input::encode_paste(&s, *a.term.lock().mode());
            a.user_input(bytes);
        }
    }

    fn modal_key(&mut self, k: KeyEvent) {
        let Some(modal) = &mut self.modal else { return };
        if k.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        match modal {
            Modal::Help => self.modal = None,
            Modal::Search(s) => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let last = s.hits.len().saturating_sub(1);
                match k.code {
                    KeyCode::Up | KeyCode::BackTab => s.sel = s.sel.saturating_sub(1),
                    KeyCode::Char('p') if ctrl => s.sel = s.sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Tab => s.sel = (s.sel + 1).min(last),
                    KeyCode::Char('n') if ctrl => s.sel = (s.sel + 1).min(last),
                    KeyCode::PageUp => s.sel = s.sel.saturating_sub(10),
                    KeyCode::PageDown => s.sel = (s.sel + 10).min(last),
                    KeyCode::Enter => {
                        if let Some(hit) = s.hits.get(s.sel).cloned() {
                            let query = s.input.text.clone();
                            self.modal = None;
                            self.jump_to(&hit, &query);
                        }
                    }
                    _ => {
                        let before = s.input.text.clone();
                        s.input.key(&k);
                        if s.input.text != before {
                            self.refresh_search();
                        }
                    }
                }
            }
            Modal::Restore => match k.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    self.restore_saved();
                }
                KeyCode::Char('n') => {
                    self.modal = None;
                    self.pending_restore = None;
                    self.toast("starting fresh", ToastKind::Info);
                }
                _ => {}
            },
            Modal::ConfirmQuit => match k.code {
                KeyCode::Char('y') | KeyCode::Enter => self.quit = true,
                _ => self.modal = None,
            },
            Modal::ConfirmClose(id) => {
                let id = *id;
                self.modal = None;
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Enter) {
                    self.close(id);
                }
            }
            Modal::Rename(id, t) => {
                if k.code == KeyCode::Enter {
                    let (id, name) = (*id, t.text.trim().to_string());
                    if let Some(i) = self.index_of(id) {
                        if !name.is_empty() {
                            self.agents[i].name = name.clone();
                            if let Some(s) = self.specs.get_mut(&id) {
                                s.name = name;
                            }
                        }
                    }
                    self.modal = None;
                } else {
                    t.key(&k);
                }
            }
            Modal::Broadcast(t, target) => match k.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    *target = if *target == BroadcastTarget::All { BroadcastTarget::Waiting } else { BroadcastTarget::All };
                }
                KeyCode::Enter => {
                    let (text, target) = (t.text.clone(), *target);
                    self.modal = None;
                    self.broadcast(&text, target);
                }
                _ => {
                    t.key(&k);
                }
            },
            Modal::New(f) => {
                f.error = None;
                let n = NEW_FIELDS.len();
                match k.code {
                    KeyCode::Enter => {
                        self.launch_form();
                        return;
                    }
                    KeyCode::Tab | KeyCode::Down => f.field = (f.field + 1) % n,
                    KeyCode::BackTab | KeyCode::Up => f.field = (f.field + n - 1) % n,
                    _ => {}
                }
                match f.field {
                    0 => match k.code {
                        KeyCode::Left | KeyCode::Char('h') => f.kind = (f.kind + Kind::ALL.len() - 1) % Kind::ALL.len(),
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => f.kind = (f.kind + 1) % Kind::ALL.len(),
                        KeyCode::Char('c') => f.kind = 0,
                        KeyCode::Char('x') => f.kind = 1,
                        KeyCode::Char('s') => f.kind = 2,
                        _ => {}
                    },
                    1 => {
                        if k.code == KeyCode::Tab {
                        } else if f.dir.key(&k) && !f.name_touched {
                            let suggested = {
                                let d = expand(&f.dir.text);
                                let base = d.file_name().map(|s| slug(&s.to_string_lossy())).unwrap_or_default();
                                base
                            };
                            if !suggested.is_empty() {
                                f.name = TextInput::new(&suggested);
                            }
                        }
                    }
                    2 => {
                        if f.name.key(&k) {
                            f.name_touched = true;
                        }
                    }
                    3 => {
                        f.prompt.key(&k);
                    }
                    4 => {
                        if matches!(k.code, KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right | KeyCode::Char('y') | KeyCode::Char('n')) {
                            f.worktree = match k.code {
                                KeyCode::Char('y') => true,
                                KeyCode::Char('n') => false,
                                _ => !f.worktree,
                            };
                        }
                    }
                    5 => {
                        f.args.key(&k);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Re-run the open search over every agent's scrollback.
    fn refresh_search(&mut self) {
        let Some(Modal::Search(s)) = &self.modal else { return };
        let query = s.input.text.clone();
        let mut hits = Vec::new();
        let (mut agents, mut capped) = (0, false);
        for a in &self.agents {
            let found = search::search_term(&a.term.lock(), a.id, &query, search::PER_AGENT);
            agents += usize::from(!found.is_empty());
            capped |= found.len() >= search::PER_AGENT;
            hits.extend(found);
        }
        if let Some(Modal::Search(s)) = &mut self.modal {
            s.hits = hits;
            s.sel = 0;
            s.agents = agents;
            s.capped = capped;
        }
        self.dirty = true;
    }

    /// Focus the hit's agent, scroll the line into view and select the match.
    fn jump_to(&mut self, hit: &Hit, query: &str) {
        let Some(idx) = self.index_of(hit.agent) else {
            self.toast("that agent is gone", ToastKind::Info);
            return;
        };
        self.set_focus(idx);
        self.view = View::Focus;
        let found = {
            let mut t = self.agents[idx].term.lock();
            match search::relocate(&t, hit, query) {
                Some(now) => {
                    let (screen, history) = (t.grid().screen_lines(), t.grid().history_size());
                    let want = search::offset_for(now.start.line.0, screen, history) as i32;
                    let delta = want - t.grid().display_offset() as i32;
                    t.scroll_display(Scroll::Delta(delta));
                    let mut sel = Selection::new(SelectionType::Simple, now.start, Side::Left);
                    sel.update(now.end, Side::Right);
                    t.selection = Some(sel);
                    true
                }
                None => false,
            }
        };
        if !found {
            self.toast("that line has scrolled out of the scrollback", ToastKind::Info);
        }
        self.dirty = true;
    }

    fn broadcast(&mut self, text: &str, target: BroadcastTarget) {
        if text.trim().is_empty() {
            return;
        }
        let mut n = 0;
        for a in self.agents.iter_mut() {
            if !broadcast_reaches(a, target) {
                continue;
            }
            let mode = *a.term.lock().mode();
            a.user_input(input::encode_paste(text, mode));
            // Submit a beat later so TUIs see the paste and Enter as separate events.
            a.write_later(&b"\r"[..], Duration::from_millis(80));
            n += 1;
        }
        self.toast(format!("sent to {n} agent{}", if n == 1 { "" } else { "s" }), ToastKind::Good);
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        if self.modal.is_some() {
            return;
        }
        let (x, y) = (m.column, m.row);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(r) = self.hits.new_button {
                    if inside(&r) {
                        self.open_new();
                        self.dirty = true;
                        return;
                    }
                }
                if let Some(&(_, id)) = self.hits.cards.iter().find(|(r, _)| inside(r)) {
                    if let Some(i) = self.index_of(id) {
                        self.set_focus(i);
                    }
                    return;
                }
                if let Some(&(r, id)) = self.hits.panes.iter().find(|(r, _)| inside(r)) {
                    if let Some(i) = self.index_of(id) {
                        if i != self.focus {
                            self.set_focus(i);
                        }
                        let a = &self.agents[i];
                        let mut t = a.term.lock();
                        let p = viewport_point(&t, r, x, y);
                        t.selection = Some(Selection::new(SelectionType::Simple, p, Side::Left));
                        self.drag = Some(Drag { agent: id, origin: r, moved: false });
                        self.dirty = true;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(d) = &mut self.drag {
                    if let Some(i) = self.agents.iter().position(|a| a.id == d.agent) {
                        let mut t = self.agents[i].term.lock();
                        let p = viewport_point(&t, d.origin, x, y);
                        if let Some(sel) = &mut t.selection {
                            sel.update(p, Side::Right);
                        }
                        d.moved = true;
                        self.dirty = true;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(d) = self.drag.take() {
                    if let Some(i) = self.index_of(d.agent) {
                        let mut t = self.agents[i].term.lock();
                        if d.moved {
                            if let Some(text) = t.selection_to_string().filter(|s| !s.trim().is_empty()) {
                                drop(t);
                                copy_to_clipboard(&self.out, &text);
                                let lines = text.lines().count();
                                self.toast(format!("copied {} line{}", lines, if lines == 1 { "" } else { "s" }), ToastKind::Good);
                                return;
                            }
                        } else {
                            t.selection = None;
                        }
                    }
                    self.dirty = true;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(m.kind, MouseEventKind::ScrollUp);
                if let Some(&(_, id)) = self.hits.panes.iter().find(|(r, _)| inside(r)) {
                    if let Some(i) = self.index_of(id) {
                        let a = &self.agents[i];
                        let mode = *a.term.lock().mode();
                        if mode.contains(TermMode::ALT_SCREEN) {
                            let seq: &[u8] = match (up, mode.contains(TermMode::APP_CURSOR)) {
                                (true, true) => b"\x1bOA\x1bOA\x1bOA",
                                (true, false) => b"\x1b[A\x1b[A\x1b[A",
                                (false, true) => b"\x1bOB\x1bOB\x1bOB",
                                (false, false) => b"\x1b[B\x1b[B\x1b[B",
                            };
                            a.write(seq.to_vec());
                        } else {
                            a.term.lock().scroll_display(Scroll::Delta(if up { 3 } else { -3 }));
                        }
                        self.dirty = true;
                    }
                } else if let Some(_) = self.hits.cards.iter().find(|(r, _)| inside(r)) {
                    if !self.agents.is_empty() {
                        let n = self.agents.len();
                        let next = if up { (self.focus + n - 1) % n } else { (self.focus + 1) % n };
                        self.set_focus(next);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn shutdown_all(&mut self) {
        for a in &self.agents {
            a.shutdown();
        }
    }
}

/// Whether a broadcast should be typed into this agent. Never shells (the
/// prompt would run as a command), and never an agent sitting on a prompt:
/// the trailing Enter would answer it — e.g. approve a pending `rm -rf`.
/// Starting agents may be on a "trust this folder?" dialog, so skip those too.
pub fn broadcast_reaches(a: &Agent, target: BroadcastTarget) -> bool {
    if !a.kind.is_agent() {
        return false;
    }
    match target {
        BroadcastTarget::All => matches!(a.state, State::Idle | State::Working | State::Done),
        BroadcastTarget::Waiting => matches!(a.state, State::Idle | State::Done),
    }
}

fn viewport_point<T>(t: &alacritty_terminal::term::Term<T>, r: Rect, x: u16, y: u16) -> Point {
    let col = x.saturating_sub(r.x).min(r.width.saturating_sub(1)) as usize;
    let row = y.saturating_sub(r.y).min(r.height.saturating_sub(1)) as i32;
    let offset = t.grid().display_offset() as i32;
    Point::new(Line(row - offset), Column(col))
}

fn clone_spec(s: &Spawn) -> Spawn {
    Spawn {
        kind: s.kind,
        name: s.name.clone(),
        cwd: s.cwd.clone(),
        command: s.command.clone(),
        prompt: s.prompt.clone(),
        branch_note: s.branch_note.clone(),
        resume: s.resume.clone(),
    }
}

pub fn tilde(p: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let s = p.display().to_string();
    let home = home.trim_end_matches('/');
    match s.strip_prefix(home) {
        // Only a whole path component: `/home/al` must not turn `/home/alex` into `~ex`.
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => s,
    }
}

/// `~` and `~/…` are home; `~user` is left alone rather than guessed at.
pub fn expand(s: &str) -> PathBuf {
    let s = s.trim();
    match s.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(format!("{}{}", std::env::var("HOME").unwrap_or_default(), rest))
        }
        _ => PathBuf::from(s),
    }
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "agent".into() } else { out }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Create `<repo>/.trix/worktrees/<name>` on branch `trix/<name>`. The flag
/// is true when an existing worktree (e.g. from an earlier session) was reused.
pub fn make_worktree(dir: &Path, name: &str) -> anyhow::Result<(PathBuf, String, bool)> {
    let top = git(dir, &["rev-parse", "--show-toplevel"]).ok_or_else(|| anyhow::anyhow!("not a git repository"))?;
    let top = PathBuf::from(top);
    let common = git(&top, &["rev-parse", "--git-common-dir"]).unwrap_or_else(|| ".git".into());
    let common = if Path::new(&common).is_absolute() { PathBuf::from(common) } else { top.join(common) };
    // Anchor at the main checkout even when launched from inside a worktree
    // (the form defaults to the focused agent's folder), so worktrees never
    // nest inside each other.
    let root = match common.file_name() {
        Some(n) if n == ".git" => common.parent().map(Path::to_path_buf).unwrap_or(top),
        _ => top,
    };
    let exclude = common.join("info/exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !existing.lines().any(|l| l.trim() == ".trix/") {
        let _ = std::fs::create_dir_all(common.join("info"));
        let _ = std::fs::write(&exclude, format!("{existing}{}.trix/\n", if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" }));
    }
    let path = root.join(".trix/worktrees").join(name);
    let branch = format!("trix/{name}");
    if path.exists() {
        return Ok((path, branch, true));
    }
    let has_branch = git(&root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_some();
    let p = path.display().to_string();
    let args: Vec<&str> = if has_branch {
        vec!["worktree", "add", &p, &branch]
    } else {
        vec!["worktree", "add", "-b", &branch, &p]
    };
    let out = Command::new("git").arg("-C").arg(&root).args(&args).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("worktree: {}", err.lines().last().unwrap_or("failed"));
    }
    Ok((path, branch, false))
}

fn spawn_git_watcher(targets: Arc<Mutex<Vec<(u32, PathBuf)>>>, tx: Sender<AppEvent>) {
    std::thread::Builder::new()
        .name("trix-git".into())
        .spawn(move || loop {
            let list = targets.lock().unwrap().clone();
            for (id, dir) in list {
                let branch = git(&dir, &["branch", "--show-current"])
                    .filter(|b| !b.is_empty())
                    .or_else(|| git(&dir, &["rev-parse", "--short", "HEAD"]));
                let diff = branch.as_ref().and_then(|_| {
                    let s = git(&dir, &["diff", "HEAD", "--shortstat"])?;
                    Some(parse_shortstat(&s))
                });
                if tx.send(AppEvent::Git(id, branch, diff)).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        })
        .ok();
}

fn parse_shortstat(s: &str) -> (u32, u32, u32) {
    let mut r = (0, 0, 0);
    for part in s.split(',') {
        let part = part.trim();
        let n: u32 = part.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if part.contains("file") {
            r.0 = n;
        } else if part.contains("insertion") {
            r.1 = n;
        } else if part.contains("deletion") {
            r.2 = n;
        }
    }
    r
}

/// A burst of completions gets one chime, without delaying the workspace.
fn completion_sound_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(2))
}

fn play_completion_sound() {
    // Embed the approved chime so installed binaries need no asset directory.
    const CHIME: &[u8] = include_bytes!("../assets/task-complete.wav");
    crate::platform::play(CHIME);
}

fn desktop_notify(title: &str, body: &str, urgent: bool) {
    if std::env::var_os("TRIX_NO_NOTIFY").is_some() {
        return;
    }
    crate::platform::notify(title, body, urgent);
}

pub fn copy_to_clipboard(out: &Out, text: &str) {
    // OSC 52 reaches the attached terminal (works over SSH too)…
    let b64 = base64(text.as_bytes());
    out.send(format!("\x1b]52;c;{b64}\x07").as_bytes());
    // …and the system clipboard covers terminals that ignore it.
    crate::platform::copy(text);
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_chime_groups_a_burst_without_extending_the_cooldown() {
        let first = Instant::now();
        assert!(completion_sound_due(None, first));
        assert!(!completion_sound_due(Some(first), first));
        assert!(!completion_sound_due(Some(first), first + Duration::from_millis(1999)));
        assert!(completion_sound_due(Some(first), first + Duration::from_secs(2)));
    }

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }
    fn press(inp: &mut TextInput, code: KeyCode) -> bool {
        inp.key(&key(code, KeyModifiers::NONE))
    }
    fn ctrl(inp: &mut TextInput, c: char) -> bool {
        inp.key(&key(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn text_input_typing_and_cursor() {
        let mut t = TextInput::new("héllo");
        assert_eq!(t.cursor, 5);
        press(&mut t, KeyCode::Left);
        press(&mut t, KeyCode::Left);
        press(&mut t, KeyCode::Char('X'));
        assert_eq!((t.text.as_str(), t.cursor), ("hélXlo", 4));
        press(&mut t, KeyCode::Home);
        press(&mut t, KeyCode::Delete);
        assert_eq!(t.text, "élXlo");
        press(&mut t, KeyCode::Right);
        press(&mut t, KeyCode::Backspace);
        assert_eq!((t.text.as_str(), t.cursor), ("lXlo", 0));
        assert!(!press(&mut t, KeyCode::Backspace), "nothing to delete");
        press(&mut t, KeyCode::End);
        assert!(!press(&mut t, KeyCode::Delete), "nothing after the cursor");
        press(&mut t, KeyCode::Right);
        assert_eq!(t.cursor, 4, "cursor stays in bounds");
        assert!(!press(&mut t, KeyCode::F(2)), "unhandled keys are not consumed");
    }

    #[test]
    fn text_input_ctrl_editing() {
        let mut t = TextInput::new("git commit  -m");
        ctrl(&mut t, 'w');
        assert_eq!(t.text, "git commit  ");
        ctrl(&mut t, 'w');
        assert_eq!(t.text, "git ");
        ctrl(&mut t, 'a');
        assert_eq!(t.cursor, 0);
        ctrl(&mut t, 'e');
        assert_eq!(t.cursor, 4);
        t.cursor = 2;
        ctrl(&mut t, 'u');
        assert_eq!((t.text.as_str(), t.cursor), ("t ", 0));
        assert!(!ctrl(&mut t, 'x'), "ctrl+letter doesn't type the letter");
        assert_eq!(t.text, "t ");
    }

    #[test]
    fn pasted_control_chars_are_dropped() {
        let mut t = TextInput::new("");
        t.insert("a\nb\tc\x1b[2J");
        assert_eq!((t.text.as_str(), t.cursor), ("abc[2J", 6));
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("My Feature!!"), "my-feature");
        assert_eq!(slug("fix/login  bug"), "fix-login-bug");
        assert_eq!(slug("v1.2_rc"), "v1.2_rc");
        assert_eq!(slug("  "), "agent");
        assert_eq!(slug("Ärger"), "rger");
    }

    #[test]
    fn home_paths() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(tilde(Path::new(&home)), "~");
        assert_eq!(tilde(&Path::new(&home).join("code/x")), "~/code/x");
        assert_eq!(tilde(Path::new(&format!("{home}2/x"))), format!("{home}2/x"), "a sibling of home is not home");
        assert_eq!(tilde(Path::new("/etc")), "/etc");
        assert_eq!(expand("~"), PathBuf::from(&home));
        assert_eq!(expand(" ~/code "), Path::new(&home).join("code"));
        assert_eq!(expand("~bob/x"), PathBuf::from("~bob/x"));
        assert_eq!(expand("/abs"), PathBuf::from("/abs"));
    }

    #[test]
    fn git_shortstat() {
        assert_eq!(parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)"), (3, 10, 2));
        assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+)"), (1, 1, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 4 deletions(-)"), (1, 0, 4));
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[test]
    fn base64_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(i.as_bytes()), o);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn broadcast_never_answers_a_prompt() {
        use crate::agent::test_agent;
        let mut a = test_agent(Kind::Claude);
        let cases = [
            (State::Starting, false, false),
            (State::Idle, true, true),
            (State::Working, true, false),
            (State::Blocked, false, false),
            (State::Done, true, true),
            (State::Exited, false, false),
        ];
        for (s, all, waiting) in cases {
            a.set_state(s, true);
            assert_eq!(broadcast_reaches(&a, BroadcastTarget::All), all, "{s:?} all");
            assert_eq!(broadcast_reaches(&a, BroadcastTarget::Waiting), waiting, "{s:?} waiting");
        }
        let mut sh = test_agent(Kind::Shell);
        sh.set_state(State::Idle, true);
        assert!(!broadcast_reaches(&sh, BroadcastTarget::All), "shells are skipped");
    }
}
