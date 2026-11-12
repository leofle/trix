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
use crate::config::Action;
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
    /// An agent's context use changed (`None`: a fresh conversation, nothing yet).
    Context(u32, Option<crate::usage::Context>),
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
    Menu(Menu),
}

/// One agent's menu: right-click it, or the menu key for the focused one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Menu {
    pub agent: u32,
    /// The cell it was opened from (a click); `None` centres it.
    pub at: Option<(u16, u16)>,
    /// The item highlighted, by what it is: the list can change under it
    /// (Restart comes when the agent exits).
    pub sel: MenuItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Rename,
    Restart,
    CopyPath,
    Close,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Rename => "Rename",
            MenuItem::Restart => "Restart",
            MenuItem::CopyPath => "Copy folder path",
            MenuItem::Close => "Close",
        }
    }

    /// The letter that picks it, underlined in its label.
    pub fn letter(self) -> char {
        match self {
            MenuItem::Rename => 'r',
            MenuItem::Restart => 's',
            MenuItem::CopyPath => 'f',
            MenuItem::Close => 'c',
        }
    }

    /// The workspace key that does the same, shown beside it.
    pub fn action(self) -> Option<Action> {
        match self {
            MenuItem::Rename => Some(Action::Rename),
            MenuItem::Close => Some(Action::Close),
            MenuItem::Restart | MenuItem::CopyPath => None,
        }
    }
}

/// Two clicks on one agent's card or title within this are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

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
    /// Each pane's title row: its top border, where its name is.
    pub titles: Vec<(Rect, u32)>,
    pub new_button: Option<Rect>,
    /// An open menu: all of it, and the row of each item.
    pub menu: Option<Rect>,
    pub menu_items: Vec<(Rect, MenuItem)>,
}

struct Drag {
    agent: u32,
    origin: Rect,
    moved: bool,
}

pub struct App {
    /// A search the typing outran, and when it's due (see `search_changed`).
    pub search_due: Option<Instant>,
    /// The times on screen at the last `slow_tick` (`ui::times_shown`).
    times_shown: String,
    /// The last workspace that failed to save, and when.
    save_failed: Option<(Saved, Instant)>,
    last_search: Option<Instant>,
    pub agents: Vec<Agent>,
    specs: HashMap<u32, Spawn>,
    pub focus: usize,
    pub view: View,
    pub sidebar: bool,
    /// alt+m: a narrow sidebar, one line per agent.
    pub sidebar_mini: bool,
    /// alt+c: one-line cards, so more fit. Otherwise cards keep their size
    /// and the list scrolls.
    pub sidebar_compact: bool,
    /// The first card the sidebar shows; it follows the focus.
    pub sidebar_scroll: usize,
    pub modal: Option<Modal>,
    pub theme: Theme,
    theme_watch: ThemeWatcher,
    pub themed_by_omarchy: bool,
    tx: Sender<AppEvent>,
    socket: PathBuf,
    integrations: crate::hooks::Integrations,
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
    git_targets: Arc<Mutex<GitTargets>>,
    usage_targets: crate::usage::Targets,
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
    /// `trix events` subscribers, and what they last heard about each agent.
    pub subscribers: Vec<crate::events::Subscriber>,
    pub published: HashMap<u32, crate::events::Published>,
    /// `~/.config/trix/config.toml`, and the write it was read from.
    pub config: crate::config::Config,
    config_stamp: Option<crate::config::Stamp>,
    /// The file had a mistake when it was last read.
    config_failed: bool,
    last_needs_you_sound: Option<Instant>,
    /// The last left click on a card or title: when, which agent, which row.
    last_click: Option<(Instant, u32, u16)>,
}

impl App {
    pub fn new(tx: Sender<AppEvent>, socket: PathBuf, integrations: crate::hooks::Integrations, exe: PathBuf, out: Out) -> App {
        let mut theme_watch = ThemeWatcher::new();
        let polled = theme_watch.poll();
        let themed = polled.is_some();
        let git_targets = Arc::new(Mutex::new(GitTargets::default()));
        spawn_git_watcher(git_targets.clone(), tx.clone());
        let usage_targets = crate::usage::Targets::default();
        crate::usage::spawn_watcher(usage_targets.clone(), tx.clone());
        let prefs = state::load_prefs();
        // Tests never read the developer's own config.
        let (loaded, config_stamp) = if cfg!(test) { (Ok(Default::default()), None) } else { (crate::config::load(), crate::config::stamp()) };
        let (config, config_error) = match loaded {
            Ok(c) => (c, None),
            Err(e) => (crate::config::Config::default(), Some(e)),
        };
        let mut app = App {
            agents: Vec::new(),
            specs: HashMap::new(),
            focus: 0,
            view: View::Focus,
            sidebar: true,
            sidebar_mini: prefs.sidebar_mini,
            sidebar_compact: prefs.sidebar_compact,
            sidebar_scroll: 0,
            modal: None,
            theme: polled.unwrap_or_default(),
            theme_watch,
            themed_by_omarchy: themed,
            tx,
            socket,
            integrations,
            exe,
            term_focused: true,
            next_id: 1,
            toast: None,
            hits: HitMap::default(),
            quit: false,
            detach: false,
            dirty: true,
            search_due: None,
            last_search: None,
            times_shown: String::new(),
            save_failed: None,
            started: Instant::now(),
            start_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            drag: None,
            git_targets,
            usage_targets,
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
            subscribers: Vec::new(),
            published: HashMap::new(),
            config,
            config_stamp,
            config_failed: config_error.is_some(),
            last_needs_you_sound: None,
            last_click: None,
        };
        if let Some(e) = config_error {
            app.toast(format!("config: {e} — using the defaults (`trix config` checks it)"), ToastKind::Bad);
        }
        app
    }

    /// The config file changed: use it, or say why not and keep the old one.
    fn reload_config(&mut self) {
        let stamp = crate::config::stamp();
        if stamp == self.config_stamp || cfg!(test) {
            return;
        }
        self.config_stamp = stamp;
        self.apply_config(crate::config::load());
    }

    fn apply_config(&mut self, loaded: Result<crate::config::Config, String>) {
        let failed = std::mem::replace(&mut self.config_failed, loaded.is_err());
        match loaded {
            // A fix that lands back on the settings in use still deserves a word.
            Ok(c) if c == self.config => {
                if failed {
                    self.toast("config OK again", ToastKind::Good);
                }
            }
            Ok(c) => {
                self.config = c;
                self.toast("config reloaded", ToastKind::Good);
            }
            Err(e) => self.toast(format!("config: {e} — kept the previous settings"), ToastKind::Bad),
        }
        self.dirty = true;
    }

    /// The key for an action, as the help shows it. Unbound: "—".
    pub fn key(&self, a: crate::config::Action) -> String {
        self.config.keys.label(a).unwrap_or_else(|| "—".into())
    }

    /// A terminal attached: it starts from a blank screen and its own folder.
    pub fn on_attach(&mut self, cwd: PathBuf) {
        if cwd.is_dir() {
            self.start_dir = cwd;
        }
        // Another session may have changed them since this one started.
        let prefs = state::load_prefs();
        self.sidebar_mini = prefs.sidebar_mini;
        self.sidebar_compact = prefs.sidebar_compact;
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
        let mut agents = self.open_agents();
        let focus = self.agents.get(self.focus).and_then(|f| agents.iter().position(|s| s.name == f.name)).unwrap_or(0);
        if let Some(p) = &self.pending_restore {
            agents.extend(p.agents.iter().cloned());
        }
        Saved::new(focus, agents)
    }

    /// The agents open now, as they'd be started again (saved workspace,
    /// `trix layout save`).
    pub fn open_agents(&self) -> Vec<SavedAgent> {
        self
            .agents
            .iter()
            .filter(|a| a.state != State::Exited || self.parked.contains(&a.id))
            .filter_map(|a| {
                let spec = self.specs.get(&a.id)?;
                // Claude names its conversation at start, but writes it only
                // with the first message: until then `--resume` finds nothing.
                let written = a.transcript.as_ref().is_none_or(|p| p.exists());
                Some(SavedAgent {
                    kind: a.kind.label().into(),
                    name: a.name.clone(),
                    // A shell may have cd'd since; agents stay where they started.
                    cwd: if a.kind == Kind::Shell { a.live_cwd() } else { spec.cwd.clone() },
                    command: spec.command.clone(),
                    // An id trix made up for Grok names no conversation until
                    // a turn; one it resumed does from the start.
                    session: if a.kind.resumes() && (!a.session_named_by_trix || a.turn_seen) && written { a.session.clone() } else { None },
                })
            })
            .collect()
    }

    /// Save the workspace if it changed since last time. A save that fails
    /// (disk full, read-only folder) is retried when the workspace changes or
    /// a minute later, and logged once, not every second.
    pub fn persist(&mut self) {
        let snap = self.snapshot();
        if self.last_saved.as_ref() == Some(&snap) {
            return;
        }
        if self.save_failed.as_ref().is_some_and(|(s, at)| *s == snap && at.elapsed() < Duration::from_secs(60)) {
            return;
        }
        match state::store(&snap) {
            Ok(()) => {
                if self.save_failed.take().is_some() {
                    eprintln!("trix: saving the workspace works again");
                }
                self.last_saved = Some(snap);
            }
            Err(e) => {
                if self.save_failed.is_none() {
                    eprintln!("trix: could not save workspace to {}: {e} (retrying quietly)", state::path().display());
                }
                self.save_failed = Some((snap, Instant::now()));
            }
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
            let resume = s.session.clone().filter(|_| kind.resumes());
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
                "{:>2} {mark} {name}  {:<8}  {:<9} {:>6}  {}  {}\n",
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

    /// What an agent's menu offers, in order. Nothing once it's gone.
    pub fn menu_items(&self, id: u32) -> Vec<MenuItem> {
        let Some(a) = self.agents.iter().find(|a| a.id == id) else { return Vec::new() };
        let mut items = vec![MenuItem::Rename];
        if a.state == State::Exited {
            items.push(MenuItem::Restart);
        }
        items.extend([MenuItem::CopyPath, MenuItem::Close]);
        items
    }

    pub fn open_menu(&mut self, id: u32, at: Option<(u16, u16)>) {
        let Some(i) = self.index_of(id) else { return };
        // On an exited agent, restarting it is what you most likely came for.
        let sel = if self.agents[i].state == State::Exited { MenuItem::Restart } else { MenuItem::Rename };
        self.modal = Some(Modal::Menu(Menu { agent: id, at, sel }));
        self.dirty = true;
    }

    /// The menu key: the focused agent's menu, by its name on screen.
    fn open_focused_menu(&mut self) {
        let Some(id) = self.focused().map(|a| a.id) else { return };
        let on = |hits: &[(Rect, u32)]| hits.iter().find(|(_, i)| *i == id).map(|(r, _)| (r.x + 2, r.y + 1));
        let at = on(&self.hits.titles).or_else(|| on(&self.hits.cards));
        self.open_menu(id, at);
    }

    fn run_menu(&mut self, id: u32, item: MenuItem) {
        self.modal = None;
        self.dirty = true;
        let Some(i) = self.index_of(id) else { return };
        match item {
            MenuItem::Rename => self.start_rename(id),
            MenuItem::Restart => self.restart(i),
            MenuItem::CopyPath => {
                // Where it is now: a shell may have moved since it started.
                let cwd = self.agents[i].live_cwd();
                copy_to_clipboard(&self.out, &cwd.display().to_string());
                self.toast(format!("copied {}", tilde(&cwd)), ToastKind::Good);
            }
            MenuItem::Close => self.ask_close(id),
        }
    }

    fn start_rename(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            self.modal = Some(Modal::Rename(id, TextInput::new(&self.agents[i].name)));
            self.dirty = true;
        }
    }

    /// Close an exited agent; ask first about one that still runs.
    fn ask_close(&mut self, id: u32) {
        match self.index_of(id).map(|i| self.agents[i].state) {
            Some(State::Exited) => self.close(id),
            Some(_) => self.modal = Some(Modal::ConfirmClose(id)),
            None => {}
        }
    }

    fn menu_key(&mut self, m: Menu, k: KeyEvent) {
        let items = self.menu_items(m.agent);
        if items.is_empty() {
            self.modal = None;
            return;
        }
        let last = items.len() - 1;
        let at = items.iter().position(|&i| i == m.sel).unwrap_or(0);
        // An item's own key works here too, and the menu key closes it.
        if let Some((action, _)) = self.config.keys.lookup(&k) {
            if action == Action::Menu {
                self.modal = None;
            } else if let Some(&item) = items.iter().find(|i| i.action() == Some(action)) {
                self.run_menu(m.agent, item);
            }
            return;
        }
        // Letters only as themselves: ctrl+c is "go away", not Close.
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let sel = match k.code {
            KeyCode::Esc => {
                self.modal = None;
                return;
            }
            KeyCode::Char('c' | 'g') if ctrl => {
                self.modal = None;
                return;
            }
            KeyCode::Up | KeyCode::BackTab => at.checked_sub(1).unwrap_or(last),
            KeyCode::Char('k') if plain => at.checked_sub(1).unwrap_or(last),
            KeyCode::Down | KeyCode::Tab => if at >= last { 0 } else { at + 1 },
            KeyCode::Char('j') if plain => if at >= last { 0 } else { at + 1 },
            KeyCode::Home => 0,
            KeyCode::End => last,
            KeyCode::Enter => {
                self.run_menu(m.agent, items[at]);
                return;
            }
            KeyCode::Char(' ') if plain => {
                self.run_menu(m.agent, items[at]);
                return;
            }
            KeyCode::Char(c) if plain => {
                if let Some(&item) = items.iter().find(|i| i.letter() == c.to_ascii_lowercase()) {
                    self.run_menu(m.agent, item);
                }
                return;
            }
            _ => return,
        };
        self.modal = Some(Modal::Menu(Menu { sel: items[sel], ..m }));
    }

    /// The mouse while a menu is open: pick, point, or click away.
    fn menu_mouse(&mut self, m: Menu, e: MouseEvent) {
        let (x, y) = (e.column, e.row);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let under = self.hits.menu_items.iter().position(|(r, _)| inside(r));
        match e.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(i) = under {
                    let item = self.hits.menu_items[i].1;
                    self.run_menu(m.agent, item);
                } else if !self.hits.menu.is_some_and(|r| inside(&r)) {
                    self.modal = None;
                    self.dirty = true;
                }
            }
            // Right-click elsewhere: that agent's menu there, or none.
            MouseEventKind::Down(MouseButton::Right) if !self.hits.menu.is_some_and(|r| inside(&r)) => {
                self.modal = None;
                self.dirty = true;
                self.on_mouse(e);
            }
            MouseEventKind::Moved => {
                if let Some(sel) = under.map(|i| self.hits.menu_items[i].1).filter(|&i| i != m.sel) {
                    self.modal = Some(Modal::Menu(Menu { sel, ..m }));
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast { text: text.into(), kind, at: Instant::now() });
        self.dirty = true;
    }

    /// When the picture next changes on its own: the next spinner step (every
    /// 90 ms), the next blink of a "needs you" glyph (every 540 ms), a toast
    /// fading. `None` when nothing moves. Frames are numbered from `started`
    /// (see `ui::frame_no`), so these are exact.
    pub fn next_frame_change(&self) -> Option<Instant> {
        const STEP: u128 = 90;
        let ms = self.started.elapsed().as_millis();
        let at = |n: u128| self.started + Duration::from_millis((n + 1) as u64);
        let spin = self.agents.iter().any(|a| matches!(a.state, State::Working | State::Starting));
        let blink = self.agents.iter().any(|a| a.state == State::Blocked);
        let mut next: Option<Instant> = None;
        let mut take = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        if spin {
            take(at((ms / STEP + 1) * STEP));
        } else if blink {
            take(at((ms / (6 * STEP) + 1) * 6 * STEP));
        }
        if let Some(t) = &self.toast {
            let fade = t.at + Duration::from_millis(3200);
            if Instant::now() < fade {
                take(fade + Duration::from_millis(1));
            }
        }
        next
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
        unique_in(&self.agents, base)
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
        let agent = Agent::spawn(id, run, self.pane_size, self.tx.clone(), &self.socket, &self.integrations, &self.exe)?;
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
        if kind == Kind::Custom && !f.prompt.text.trim().is_empty() {
            f.error = Some("a custom command takes no prompt: put it in Args, or send it once it runs".into());
            f.field = 3;
            return;
        }
        let typed = f.name.text.trim().to_string();
        // A name you typed must be free; a suggested one is made unique.
        let name = if f.name_touched && !typed.is_empty() {
            if only_digits(&typed) {
                f.error = Some("a name can't be only digits: it would read as an agent number".into());
                f.field = 2;
                return;
            }
            if self.agents.iter().any(|a| a.name == typed) {
                f.error = Some(format!("there is already an agent named '{typed}'"));
                f.field = 2;
                return;
            }
            typed
        } else {
            let base = if typed.is_empty() { "agent".to_string() } else { typed };
            unique_in(&self.agents, &base)
        };
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
        match Agent::spawn(id, clone_spec(&spec), size, self.tx.clone(), &self.socket, &self.integrations, &self.exe) {
            Ok(a) => {
                self.agents[idx].shutdown();
                self.agents[idx] = a;
                crate::events::restarted(self, old_id, id);
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
            // A menu or dialog about it has nothing left to act on.
            if matches!(self.modal, Some(Modal::Menu(Menu { agent, .. }) | Modal::Rename(agent, _) | Modal::ConfirmClose(agent)) if agent == id) {
                self.modal = None;
            }
            self.specs.remove(&id);
            self.parked.remove(&id);
            let was_focused = i == self.focus;
            if i < self.focus {
                // Everything after it moved up one: stay on the same agent.
                self.focus -= 1;
            } else if self.focus >= self.agents.len() {
                self.focus = self.agents.len().saturating_sub(1);
            }
            // The next agent slid into the focused slot: it is the one being
            // looked at now, so it is seen, and may want to know it has focus.
            if was_focused && let Some(next) = self.agents.get_mut(self.focus) {
                next.unseen = false;
                if next.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                    next.write(&b"\x1b[I"[..]);
                }
                self.dirty = true;
            }
            self.toast(format!("closed {}", a.name), ToastKind::Info);
            self.sync_git_targets();
        }
    }

    fn sync_git_targets(&self) {
        // An exited agent's branch and diff can't change under it.
        let agents = self.agents.iter().filter(|a| a.state != State::Exited).map(|a| (a.id, a.live_cwd())).collect();
        let watched = self.out.attached();
        let mut t = self.git_targets.lock().unwrap();
        t.agents = agents;
        t.watched = watched;
        drop(t);
        // Nor its context: it keeps the last reading.
        let usage = self
            .agents
            .iter()
            .filter(|a| a.state != State::Exited)
            .map(|a| crate::usage::Target { id: a.id, kind: a.kind, transcript: a.transcript.clone(), session: a.session.clone() })
            .collect();
        *self.usage_targets.lock().unwrap() = usage;
    }

    pub fn set_focus(&mut self, idx: usize) {
        if idx >= self.agents.len() {
            return;
        }
        let prev = self.focus;
        if prev != idx {
            if let Some(a) = self.agents.get(prev)
                && a.term.lock().mode().contains(TermMode::FOCUS_IN_OUT)
            {
                a.write(&b"\x1b[O"[..]);
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
                    self.agents[i].on_title(t);
                    self.dirty = true;
                }
            }
            AppEvent::Bell(id) => {
                if let Some(i) = self.index_of(id)
                    && (i != self.focus || !self.term_focused)
                {
                    self.agents[i].unseen = true;
                    self.dirty = true;
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
            AppEvent::Context(id, ctx) => {
                if let Some(i) = self.index_of(id) {
                    self.agents[i].context = ctx;
                    self.dirty = true;
                }
            }
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
        self.reload_config();
        // The clock and the timers: redraw when what they show changed.
        let shown = crate::ui::times_shown(self);
        if shown != self.times_shown {
            self.times_shown = shown;
            self.dirty = true;
        }
    }

    fn transitioned(&mut self, i: usize, prev: State) {
        let a = &self.agents[i];
        let looking = i == self.focus && self.term_focused;
        let needs_you = a.state == State::Blocked;
        let (title, body, urgent) = match a.state {
            State::Blocked => (format!("{} needs you", a.name), a.activity.clone(), true),
            State::Done if prev == State::Working => {
                (format!("{} finished", a.name), a.activity.clone(), false)
            }
            _ => return,
        };
        if looking {
            return;
        }
        let title = if crate::session::is_default() { title } else { format!("{title} · {}", crate::session::name()) };
        let n = &self.config.notify;
        let wanted = if needs_you { n.needs_you } else { n.done };
        if wanted && n.desktop {
            desktop_notify(&title, &body, urgent);
        }
        let command = n.command.clone().filter(|_| wanted);
        let bell = wanted && n.bell && !self.term_focused;
        let sound = &self.config.sound;
        let tone = if needs_you { sound.needs_you.clone() } else { sound.done.clone() };
        let tone = tone.filter_muted(sound.muted(a.kind.label(), &a.name));
        let event = command.as_ref().map(|_| crate::events::agent_event("state", a, i + 1, Some(prev)).to_string());
        let agent = a.name.clone();

        if let (Some(argv), Some(event)) = (command, event) {
            let env = [
                ("TRIX_NOTIFY_EVENT", if needs_you { "needs-you" } else { "done" }.to_string()),
                ("TRIX_NOTIFY_AGENT", agent),
                ("TRIX_NOTIFY_TITLE", title),
                ("TRIX_NOTIFY_BODY", body),
                ("TRIX_NOTIFY_SESSION", crate::session::name().to_string()),
            ];
            if let Err(e) = crate::platform::run(&argv, &env, event.into_bytes()) {
                self.toast(format!("notify.command {}: {e}", argv[0]), ToastKind::Bad);
            }
        }
        if tone != crate::config::Tone::Off && std::env::var_os("TRIX_NO_SOUND").is_none() {
            // A burst of them makes one sound, needs-you and done apart.
            let last = if needs_you { &mut self.last_needs_you_sound } else { &mut self.last_completion_sound };
            let now = Instant::now();
            if completion_sound_due(*last, now) {
                *last = Some(now);
                match tone {
                    crate::config::Tone::File(p) => crate::platform::play_file(&p),
                    _ => play_completion_sound(),
                }
            }
        }
        if bell {
            // Bell → compositor urgency hint on the terminal window.
            self.out.send_all(b"\x07");
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
        if let Some((action, n)) = self.config.keys.lookup(&k) {
            match action {
                Action::New => self.open_new(),
                Action::Jump => self.set_focus(n),
                Action::Next => {
                    if !self.agents.is_empty() {
                        self.set_focus((self.focus + 1) % self.agents.len());
                    }
                }
                Action::Prev => {
                    if !self.agents.is_empty() {
                        self.set_focus((self.focus + self.agents.len() - 1) % self.agents.len());
                    }
                }
                Action::Attention => self.next_attention(),
                Action::Grid => self.view = if self.view == View::Grid { View::Focus } else { View::Grid },
                Action::Sidebar => self.sidebar = !self.sidebar,
                Action::MiniSidebar => {
                    // From hidden, it brings the sidebar back small.
                    self.sidebar_mini = !self.sidebar || !self.sidebar_mini;
                    self.sidebar = true;
                    // Load first: change only this, whatever else prefs.json holds.
                    let mut prefs = state::load_prefs();
                    prefs.sidebar_mini = self.sidebar_mini;
                    if let Err(e) = state::store_prefs(&prefs) {
                        self.toast(format!("couldn't remember the sidebar size: {e}"), ToastKind::Bad);
                    }
                }
                Action::Compact => {
                    self.sidebar_compact = !self.sidebar_compact;
                    self.sidebar = true;
                    self.toast(if self.sidebar_compact { "sidebar: compact cards" } else { "sidebar: full cards, scrolling" }, ToastKind::Info);
                    let mut prefs = state::load_prefs();
                    prefs.sidebar_compact = self.sidebar_compact;
                    if let Err(e) = state::store_prefs(&prefs) {
                        self.toast(format!("couldn't remember that: {e}"), ToastKind::Bad);
                    }
                }
                Action::Search => {
                    if self.agents.is_empty() {
                        self.toast("no agents to search yet", ToastKind::Info);
                    } else {
                        self.modal = Some(Modal::Search(Search::default()));
                    }
                }
                Action::Broadcast => {
                    if self.agents.is_empty() {
                        self.toast("no agents to talk to yet", ToastKind::Info);
                    } else {
                        self.modal = Some(Modal::Broadcast(TextInput::default(), BroadcastTarget::All));
                    }
                }
                Action::Rename => {
                    if let Some(id) = self.focused().map(|a| a.id) {
                        self.start_rename(id);
                    }
                }
                Action::Close => {
                    if let Some(id) = self.focused().map(|a| a.id) {
                        self.ask_close(id);
                    }
                }
                Action::Menu => self.open_focused_menu(),
                Action::Detach => self.detach = true,
                Action::Help => self.modal = Some(Modal::Help),
                Action::Quit => {
                    if self.agents.iter().any(|a| a.state != State::Exited) {
                        self.modal = Some(Modal::ConfirmQuit);
                    } else {
                        self.quit = true;
                    }
                }
            }
            return;
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
                    self.search_changed();
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
        // Jump to a hit for what's typed now, not for what was typed last scan.
        if k.code == KeyCode::Enter && self.search_due.is_some() && matches!(self.modal, Some(Modal::Search(_))) {
            self.refresh_search();
        }
        if let Some(Modal::Menu(m)) = self.modal {
            self.menu_key(m, k);
            return;
        }
        let Some(modal) = &mut self.modal else { return };
        if k.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        match modal {
            // Handled above.
            Modal::Menu(_) => {}
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
                            self.search_changed();
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
                    if only_digits(&name) {
                        self.toast("a name can't be only digits: it would read as an agent number", ToastKind::Bad);
                        return;
                    }
                    if self.agents.iter().any(|a| a.id != id && a.name == name) {
                        // Keep the box open: two agents with one name can't be told apart.
                        self.toast(format!("there is already an agent named '{name}'"), ToastKind::Bad);
                        return;
                    }
                    if let Some(i) = self.index_of(id)
                        && !name.is_empty()
                    {
                        self.agents[i].name = name.clone();
                        if let Some(s) = self.specs.get_mut(&id) {
                            s.name = name;
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
                        KeyCode::Char(c) => {
                            if let Some(i) = Kind::from_shortcut(c).and_then(|k| Kind::ALL.iter().position(|a| *a == k)) {
                                f.kind = i;
                            }
                        }
                        _ => {}
                    },
                    1 => {
                        if k.code == KeyCode::Tab {
                        } else if f.dir.key(&k) && !f.name_touched {
                            let suggested = {
                                let d = expand(&f.dir.text);
                                d.file_name().map(|s| slug(&s.to_string_lossy())).unwrap_or_default()
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
    /// The query changed. A search reads every agent's whole scrollback
    /// (tens of ms with a few full ones), so while typing it runs at most
    /// every `SEARCH_EVERY`, and once more when the typing stops.
    fn search_changed(&mut self) {
        const SEARCH_EVERY: Duration = Duration::from_millis(150);
        match self.last_search {
            Some(t) if t.elapsed() < SEARCH_EVERY => self.search_due = Some(t + SEARCH_EVERY),
            _ => self.refresh_search(),
        }
    }

    /// Run the search `search_changed` put off, once it's due.
    pub fn run_due_search(&mut self) {
        if self.search_due.is_some_and(|t| Instant::now() >= t) {
            self.refresh_search();
        }
    }

    fn refresh_search(&mut self) {
        self.search_due = None;
        let Some(Modal::Search(s)) = &self.modal else { return };
        self.last_search = Some(Instant::now());
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
            a.submit(input::encode_paste(text, mode));
            n += 1;
        }
        self.toast(format!("sent to {n} agent{}", if n == 1 { "" } else { "s" }), ToastKind::Good);
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        if let Some(Modal::Menu(menu)) = self.modal {
            self.menu_mouse(menu, m);
            return;
        }
        if self.modal.is_some() {
            return;
        }
        let (x, y) = (m.column, m.row);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        // An agent's name: its card in the sidebar, or its pane's title.
        let named = self.hits.cards.iter().chain(&self.hits.titles).find(|(r, _)| inside(r)).map(|&(_, id)| id);
        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(id) = named.or_else(|| self.hits.panes.iter().find(|(r, _)| inside(r)).map(|&(_, id)| id)) {
                    self.open_menu(id, Some((x, y)));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(r) = self.hits.new_button
                    && inside(&r)
                {
                    self.open_new();
                    self.dirty = true;
                    return;
                }
                if let Some(id) = named {
                    let now = Instant::now();
                    let double = self.last_click.is_some_and(|(at, prev, row)| prev == id && row == y && now.duration_since(at) < DOUBLE_CLICK);
                    self.last_click = if double { None } else { Some((now, id, y)) };
                    if double {
                        self.start_rename(id);
                    } else if let Some(i) = self.index_of(id) {
                        self.set_focus(i);
                    }
                    return;
                }
                if let Some(&(r, id)) = self.hits.panes.iter().find(|(r, _)| inside(r))
                    && let Some(i) = self.index_of(id)
                {
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
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(d) = &mut self.drag
                    && let Some(i) = self.agents.iter().position(|a| a.id == d.agent)
                {
                    let mut t = self.agents[i].term.lock();
                    let p = viewport_point(&t, d.origin, x, y);
                    if let Some(sel) = &mut t.selection {
                        sel.update(p, Side::Right);
                    }
                    d.moved = true;
                    self.dirty = true;
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
                } else if self.hits.cards.iter().any(|(r, _)| inside(r)) && !self.agents.is_empty() {
                    let n = self.agents.len();
                    let next = if up { (self.focus + n - 1) % n } else { (self.focus + 1) % n };
                    self.set_focus(next);
                }
            }
            _ => {}
        }
    }

    /// Stop every agent, and wait until each has. The server exits right
    /// after, and a program that ignores the hang-up (OpenCode 2) would
    /// outlive it.
    pub fn shutdown_all(&mut self) {
        let pids: Vec<u32> = self.agents.iter().filter(|a| a.hang_up()).map(|a| a.pid).collect();
        crate::platform::stop(&pids, crate::agent::STOP_GRACE);
    }
}

/// Whether a broadcast should be typed into this agent. Only Claude and Codex:
/// never a shell or a custom command (the prompt would run as a command), and
/// never an agent sitting on a prompt (by its state, or by what its screen shows):
/// the trailing Enter would answer it — e.g. approve a pending `rm -rf`.
/// Starting agents may be on a "trust this folder?" dialog, so skip those too.
/// A name of only digits would be taken for an agent's number (`trix close 2`).
pub fn only_digits(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_digit())
}

pub fn broadcast_reaches(a: &Agent, target: BroadcastTarget) -> bool {
    if !a.kind.takes_prompts() {
        return false;
    }
    let by_state = match target {
        BroadcastTarget::All => matches!(a.state, State::Idle | State::Working | State::Done),
        BroadcastTarget::Waiting => matches!(a.state, State::Idle | State::Done),
    };
    // The state can lag behind the screen: a permission dialog is up before
    // its hook arrives. The trailing Enter would answer it, so look too.
    by_state && !a.screen_needs_input()
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

/// `base`, or `base-2`, `base-3`… whichever no agent has yet.
fn unique_in(agents: &[Agent], base: &str) -> String {
    if !agents.iter().any(|a| a.name == base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|n| !agents.iter().any(|a| &a.name == n)).unwrap()
}

/// A slug that is also a safe folder and branch name: no `.` or `..`, no
/// leading or trailing dot, no `..` inside, no `.lock` ending.
fn worktree_name(slug: &str) -> String {
    let mut out = String::new();
    for c in slug.chars() {
        if !(c == '.' && out.ends_with('.')) {
            out.push(c);
        }
    }
    let mut out = out.trim_matches('.').to_string();
    while let Some(s) = out.strip_suffix(".lock") {
        out = s.trim_end_matches('.').to_string();
    }
    if out.is_empty() { "agent".into() } else { out }
}

/// Create `<repo>/.trix/worktrees/<name>` on branch `trix/<name>`. The flag
/// is true when an existing worktree (e.g. from an earlier session) was reused.
pub fn make_worktree(dir: &Path, name: &str) -> anyhow::Result<(PathBuf, String, bool)> {
    let name = &worktree_name(name);
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

/// A folder's branch, and its diff against HEAD (files, insertions, deletions).
type GitInfo = (Option<String>, Option<(u32, u32, u32)>);

/// What the git watcher looks at: each live agent's folder, and whether
/// anyone is attached to see the result.
#[derive(Default)]
struct GitTargets {
    agents: Vec<(u32, PathBuf)>,
    watched: bool,
}

/// Branch and diff of every agent's folder, refreshed every few seconds while
/// someone is attached and less often while not (`trix ls --json` still reads
/// them). git runs once per folder however many agents share it, and only a
/// change is sent on.
fn spawn_git_watcher(targets: Arc<Mutex<GitTargets>>, tx: Sender<AppEvent>) {
    const WATCHED: Duration = Duration::from_secs(3);
    const UNWATCHED: Duration = Duration::from_secs(20);
    std::thread::Builder::new()
        .name("trix-git".into())
        .spawn(move || {
            let mut known: HashMap<u32, GitInfo> = HashMap::new();
            let mut last_run: Option<Instant> = None;
            loop {
                let (agents, watched) = {
                    let t = targets.lock().unwrap();
                    (t.agents.clone(), t.watched)
                };
                let every = if watched { WATCHED } else { UNWATCHED };
                // Look again every second, so attaching speeds it up at once.
                if last_run.is_some_and(|t| t.elapsed() < every) && agents.iter().all(|(id, _)| known.contains_key(id)) {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                last_run = Some(Instant::now());
                let mut by_dir: HashMap<&PathBuf, GitInfo> = HashMap::new();
                for (id, dir) in &agents {
                    let now = by_dir
                        .entry(dir)
                        .or_insert_with(|| {
                            let branch = git(dir, &["branch", "--show-current"])
                                .filter(|b| !b.is_empty())
                                .or_else(|| git(dir, &["rev-parse", "--short", "HEAD"]));
                            let diff = branch.as_ref().and_then(|_| Some(parse_shortstat(&git(dir, &["diff", "HEAD", "--shortstat"])?)));
                            (branch, diff)
                        })
                        .clone();
                    if known.get(id) != Some(&now) {
                        known.insert(*id, now.clone());
                        if tx.send(AppEvent::Git(*id, now.0, now.1)).is_err() {
                            return;
                        }
                    }
                }
                known.retain(|id, _| agents.iter().any(|(a, _)| a == id));
            }
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

    fn test_app() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), Default::default(), "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        app
    }

    #[test]
    fn config_reloads_say_what_happened() {
        let mut app = test_app();
        let read = |text: &str| crate::config::parse(text, std::path::Path::new("/"));
        let toast = |app: &mut App| app.toast.take().map(|t| t.text);
        app.apply_config(read("[keys]\nnew = \"alt+t\"\n"));
        assert_eq!(toast(&mut app).as_deref(), Some("config reloaded"));
        app.apply_config(read("[keys]\nnew = \"t\"\n"));
        let t = toast(&mut app).unwrap_or_default();
        assert!(t.contains("alone would catch") && t.ends_with("kept the previous settings"), "{t}");
        assert_eq!(app.key(crate::config::Action::New), "alt+t");
        // Fixed, back to the settings in use: say so all the same.
        app.apply_config(read("[keys]\nnew = \"alt+t\"\n"));
        assert_eq!(toast(&mut app).as_deref(), Some("config OK again"));
        // Saved again, unchanged: nothing to say.
        app.apply_config(read("[keys]\nnew = \"alt+t\"\n"));
        assert_eq!(toast(&mut app), None);
    }

    /// Two agents, `a` (id 1, focused) and `b` (id 2), with sidebar cards
    /// and pane titles where a frame would have put them.
    fn menu_app() -> App {
        let mut app = test_app();
        for (id, name) in [(1, "a"), (2, "b")] {
            let mut a = crate::agent::test_agent(Kind::Claude);
            a.id = id;
            a.name = name.into();
            a.set_state(State::Idle, false);
            app.agents.push(a);
        }
        app.hits.cards = vec![(Rect::new(0, 2, 20, 3), 1), (Rect::new(0, 5, 20, 3), 2)];
        app.hits.titles = vec![(Rect::new(22, 1, 60, 1), 1)];
        app
    }
    fn click(app: &mut App, button: MouseButton, x: u16, y: u16) {
        app.on_mouse(MouseEvent { kind: MouseEventKind::Down(button), column: x, row: y, modifiers: KeyModifiers::NONE });
    }
    fn hit(app: &mut App, code: KeyCode, m: KeyModifiers) {
        app.on_key(KeyEvent::new(code, m));
    }
    fn menu_of(app: &App) -> Option<Menu> {
        match app.modal {
            Some(Modal::Menu(m)) => Some(m),
            _ => None,
        }
    }

    #[test]
    fn right_click_opens_that_agents_menu() {
        let mut app = menu_app();
        click(&mut app, MouseButton::Right, 3, 6);
        assert_eq!(menu_of(&app), Some(Menu { agent: 2, at: Some((3, 6)), sel: MenuItem::Rename }));
        assert_eq!(app.focus, 0, "a menu doesn't move the focus");
        assert_eq!(app.menu_items(2), [MenuItem::Rename, MenuItem::CopyPath, MenuItem::Close]);
        hit(&mut app, KeyCode::Down, KeyModifiers::NONE);
        hit(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Close));
        hit(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Rename), "it wraps");
        hit(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Close));
        hit(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(matches!(&app.modal, Some(Modal::Rename(2, t)) if t.text == "b"), "its letter");
        // Right-click on a pane title, and on the pane itself.
        app.modal = None;
        click(&mut app, MouseButton::Right, 40, 1);
        assert_eq!(menu_of(&app).map(|m| m.agent), Some(1));
        app.modal = None;
        app.hits.panes = vec![(Rect::new(23, 2, 58, 20), 1)];
        click(&mut app, MouseButton::Right, 40, 10);
        assert_eq!(menu_of(&app).map(|m| m.agent), Some(1));
        app.modal = None;
        click(&mut app, MouseButton::Right, 90, 30);
        assert!(app.modal.is_none(), "nothing there, no menu");
    }

    #[test]
    fn the_menu_does_what_it_offers() {
        let mut app = menu_app();
        let alt = KeyModifiers::ALT;
        // The menu key opens the focused agent's, by its pane's title.
        hit(&mut app, KeyCode::Char('o'), alt);
        assert_eq!(menu_of(&app), Some(Menu { agent: 1, at: Some((24, 2)), sel: MenuItem::Rename }));
        hit(&mut app, KeyCode::Char('o'), alt);
        assert!(app.modal.is_none(), "and closes it");
        // An item's own key works inside it.
        hit(&mut app, KeyCode::Char('o'), alt);
        hit(&mut app, KeyCode::Char('r'), alt);
        assert!(matches!(app.modal, Some(Modal::Rename(1, _))));
        app.modal = None;
        // Copy its folder.
        app.open_menu(1, None);
        hit(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(app.modal.is_none());
        assert!(app.toast.as_ref().is_some_and(|t| t.text.starts_with("copied ")), "{:?}", app.toast.as_ref().map(|t| &t.text));
        // Close asks first while it runs.
        app.open_menu(2, None);
        hit(&mut app, KeyCode::End, KeyModifiers::NONE);
        hit(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.modal, Some(Modal::ConfirmClose(2))));
        app.modal = None;
        // An exited agent can be restarted, and closes without asking.
        app.agents[1].set_state(State::Exited, false);
        assert_eq!(app.menu_items(2), [MenuItem::Rename, MenuItem::Restart, MenuItem::CopyPath, MenuItem::Close]);
        app.open_menu(2, None);
        hit(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(app.agents.len(), 1);
        assert!(app.modal.is_none());
        // Esc, and keys that mean nothing here.
        app.open_menu(1, None);
        hit(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(menu_of(&app).is_some());
        hit(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.modal.is_none());
    }

    #[test]
    fn the_menu_follows_the_mouse() {
        let mut app = menu_app();
        click(&mut app, MouseButton::Right, 3, 3);
        // Where the frame drew it.
        app.hits.menu = Some(Rect::new(3, 3, 24, 5));
        app.hits.menu_items = vec![(Rect::new(4, 4, 22, 1), MenuItem::Rename), (Rect::new(4, 5, 22, 1), MenuItem::CopyPath), (Rect::new(4, 6, 22, 1), MenuItem::Close)];
        app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: 10, row: 6, modifiers: KeyModifiers::NONE });
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Close), "pointing picks");
        click(&mut app, MouseButton::Left, 3, 3);
        assert!(menu_of(&app).is_some(), "its border is part of it");
        click(&mut app, MouseButton::Left, 10, 4);
        assert!(matches!(app.modal, Some(Modal::Rename(1, _))), "clicking an item runs it");
        // Clicking elsewhere closes it; right-clicking elsewhere moves it.
        app.open_menu(1, Some((3, 3)));
        click(&mut app, MouseButton::Right, 10, 6);
        assert_eq!(menu_of(&app).map(|m| m.agent), Some(1), "inside the menu, a right-click does nothing");
        click(&mut app, MouseButton::Right, 1, 6);
        assert_eq!(menu_of(&app).map(|m| (m.agent, m.at)), Some((2, Some((1, 6)))));
        click(&mut app, MouseButton::Left, 70, 20);
        assert!(app.modal.is_none());
        assert_eq!(app.focus, 0, "the click that closed it did nothing else");
    }

    #[test]
    fn double_click_on_a_name_renames() {
        let mut app = menu_app();
        click(&mut app, MouseButton::Left, 3, 6);
        assert_eq!((app.focus, app.modal.is_none()), (1, true), "one click focuses");
        click(&mut app, MouseButton::Left, 4, 6);
        assert!(matches!(&app.modal, Some(Modal::Rename(2, t)) if t.text == "b"));
        app.modal = None;
        // Too slow, or on another agent, is two clicks.
        app.last_click = Some((Instant::now().checked_sub(Duration::from_secs(1)).unwrap(), 2, 6));
        click(&mut app, MouseButton::Left, 3, 6);
        assert!(app.modal.is_none());
        click(&mut app, MouseButton::Left, 3, 3);
        assert!(app.modal.is_none());
        assert_eq!(app.focus, 0);
        // A pane's title too.
        click(&mut app, MouseButton::Left, 40, 1);
        click(&mut app, MouseButton::Left, 41, 1);
        assert!(matches!(app.modal, Some(Modal::Rename(1, _))));
    }

    #[test]
    fn ctrl_and_alt_letters_dont_pick_menu_items() {
        let mut app = menu_app();
        app.agents[1].set_state(State::Exited, false);
        for (code, m) in [('c', KeyModifiers::CONTROL), ('c', KeyModifiers::ALT), ('s', KeyModifiers::CONTROL), ('f', KeyModifiers::CONTROL), ('k', KeyModifiers::ALT)] {
            app.open_menu(2, None);
            hit(&mut app, KeyCode::Char(code), m);
            assert_eq!(app.agents.len(), 2, "{m:?}+{code} closed it");
            assert!(!matches!(app.modal, Some(Modal::Rename(..) | Modal::ConfirmClose(_))), "{m:?}+{code} opened a dialog");
        }
        // ctrl+c is "go away".
        app.open_menu(2, None);
        hit(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.modal.is_none());
        // Shift is still the letter.
        app.open_menu(1, None);
        hit(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert!(matches!(app.modal, Some(Modal::Rename(1, _))));
    }

    #[test]
    fn an_exited_agents_menu_starts_on_restart() {
        let mut app = menu_app();
        app.agents[1].set_state(State::Exited, false);
        app.open_menu(2, None);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Restart));
        app.open_menu(1, None);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Rename));
    }

    #[test]
    fn the_highlight_stays_on_its_item_when_the_list_changes() {
        let mut app = menu_app();
        app.open_menu(2, None);
        hit(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::Close));
        // It exits while the menu is open: Restart comes in above Close.
        app.agents[1].set_state(State::Exited, false);
        hit(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(menu_of(&app).map(|m| m.sel), Some(MenuItem::CopyPath), "one up from Close");
        hit(&mut app, KeyCode::Down, KeyModifiers::NONE);
        hit(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.agents.len(), 1, "Enter ran Close, not what took its place");
    }

    #[test]
    fn closing_an_agent_takes_its_menu_with_it() {
        let mut app = menu_app();
        app.open_menu(2, None);
        app.close(2);
        assert!(app.modal.is_none());
        app.open_menu(1, None);
        let other = app.agents.iter().find(|a| a.id == 1).map(|a| a.id);
        assert_eq!(other, Some(1));
        app.open_menu(7, None);
        assert_eq!(menu_of(&app).map(|m| m.agent), Some(1), "no menu for an agent that isn't there");
    }

    #[test]
    fn closing_an_agent_before_the_focused_one_keeps_the_focus() {
        let mut app = test_app();
        for (id, name) in [(1, "a"), (2, "b"), (3, "c")] {
            let mut a = crate::agent::test_agent(Kind::Claude);
            a.id = id;
            a.name = name.into();
            app.agents.push(a);
        }
        app.focus = 1;
        let a = app.agents[0].id;
        app.close(a);
        assert_eq!(app.agents[app.focus].name, "b", "still on b");
        let c = app.agents[1].id;
        app.close(c);
        assert_eq!(app.agents[app.focus].name, "b");
        let b = app.agents[0].id;
        app.close(b);
        assert_eq!(app.focus, 0);
    }

    #[test]
    fn quick_submissions_stay_separate() {
        let mut app = test_app();
        // Echo off: what's on screen is only what cat received, line by line.
        // With echo on, how the terminal's echoes interleave with cat's
        // output is up to the scheduler, and CI runners differ. `ready` says
        // echo is off: a login shell can take a while to get there.
        let spec = Spawn { kind: Kind::Custom, name: "cat".into(), cwd: std::env::temp_dir(), command: Some("stty -echo; echo ready; exec cat".into()), prompt: None, branch_note: None, resume: None };
        let id = app.spawn_quiet(spec).unwrap();
        let i = app.index_of(id).unwrap();
        // What's on screen after `ready`, once it shows.
        let after_ready = |app: &App| -> Option<Vec<String>> {
            let tail = crate::search::tail(&app.agents[i].term.lock(), 20);
            let lines: Vec<String> = tail.iter().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect();
            let at = lines.iter().position(|l| l == "ready")?;
            Some(lines[at + 1..].to_vec())
        };
        let wait_for = |app: &App, done: &dyn Fn(&[String]) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !after_ready(app).is_some_and(|l| done(&l)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for(&app, &|_| true);
        assert!(after_ready(&app).is_some(), "the shell never turned echo off");
        app.agents[i].submit(b"one".to_vec());
        app.agents[i].submit(b"two".to_vec());
        wait_for(&app, &|l| l.len() >= 2);
        // A moment more, so an extra line would show.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(after_ready(&app).unwrap_or_default(), ["one", "two"], "not 'onetwo' and an empty Enter");
        app.agents[i].shutdown();
    }

    #[test]
    fn kind_letters_pick_their_kind() {
        let mut app = test_app();
        for (c, kind) in [('c', Kind::Claude), ('x', Kind::Codex), ('g', Kind::Grok), ('o', Kind::OpenCode), ('u', Kind::Cursor), ('s', Kind::Shell)] {
            app.open_new();
            app.modal_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            let Some(Modal::New(f)) = &app.modal else { panic!("the form stays open") };
            assert_eq!(Kind::ALL[f.kind], kind, "{c}");
            app.modal = None;
        }
        // Letters are unique, and none is a kind-row navigation key.
        let letters: Vec<char> = Kind::ALL.iter().filter_map(|k| k.shortcut()).collect();
        assert_eq!(letters.len(), letters.iter().collect::<HashSet<_>>().len());
        assert!(!letters.iter().any(|c| matches!(c, 'h' | 'l' | ' ')));
    }

    #[test]
    fn a_grok_session_is_saved_once_it_names_a_conversation() {
        let mut app = test_app();
        let mut a = crate::agent::test_agent(Kind::Grok);
        a.id = 3;
        // Restored with --resume: the conversation exists before any new turn.
        a.session = Some("0b5e2c1a-7f3d-4c2e-9a1b-2d3e4f5a6b7c".into());
        assert!(!a.session_named_by_trix && !a.turn_seen);
        app.specs.insert(3, Spawn { kind: Kind::Grok, name: "g".into(), cwd: std::env::temp_dir(), command: None, prompt: None, branch_note: None, resume: None });
        app.agents.push(a);
        assert_eq!(app.open_agents()[0].session.as_deref(), Some("0b5e2c1a-7f3d-4c2e-9a1b-2d3e4f5a6b7c"), "restored, then saved again before you type");
        // An id trix made up for a new conversation: nothing to resume until a turn.
        app.agents[0].session_named_by_trix = true;
        assert_eq!(app.open_agents()[0].session, None, "no turn yet: nothing to resume");
        app.agents[0].turn_seen = true;
        assert_eq!(app.open_agents()[0].session.as_deref(), Some("0b5e2c1a-7f3d-4c2e-9a1b-2d3e4f5a6b7c"));
        app.agents[0].shutdown();
    }

    #[test]
    fn a_claude_session_is_saved_once_its_conversation_is_written() {
        let mut app = test_app();
        let mut a = crate::agent::test_agent(Kind::Claude);
        a.id = 4;
        let log = std::env::temp_dir().join(format!("trix-transcript-test-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&log);
        // SessionStart names the conversation and its log, before any message.
        a.session = Some("1edd8d96-d5b4-47b7-98a2-3b9a0daa56b2".into());
        a.transcript = Some(log.clone());
        app.specs.insert(4, Spawn { kind: Kind::Claude, name: "c".into(), cwd: std::env::temp_dir(), command: None, prompt: None, branch_note: None, resume: None });
        app.agents.push(a);
        assert_eq!(app.open_agents()[0].session, None, "no message yet: --resume would find nothing");
        std::fs::write(&log, "{}\n").unwrap();
        assert_eq!(app.open_agents()[0].session.as_deref(), Some("1edd8d96-d5b4-47b7-98a2-3b9a0daa56b2"));
        // No log reported at all (another agent): the id alone decides.
        app.agents[0].transcript = None;
        let _ = std::fs::remove_file(&log);
        assert!(app.open_agents()[0].session.is_some());
        app.agents[0].shutdown();
    }

    #[test]
    fn names_from_the_form_and_rename_stay_unique() {
        let mut app = test_app();
        let mut a = crate::agent::test_agent(Kind::Claude);
        a.id = 7;
        a.name = "web".into();
        app.agents.push(a);
        let tmp = std::env::temp_dir();
        let form = |name: &str, touched: bool| {
            let mut f = NewForm { kind: Kind::ALL.iter().position(|k| *k == Kind::Custom).unwrap(), field: 0, name: TextInput::new(name), name_touched: touched, dir: TextInput::new(&tmp.display().to_string()), prompt: TextInput::default(), worktree: false, args: TextInput::new("exec sleep 60"), error: None };
            f.args = TextInput::new("exec sleep 60");
            Modal::New(f)
        };
        app.modal = Some(form("web", true));
        app.launch_form();
        let Some(Modal::New(f)) = &app.modal else { panic!("the form stays open") };
        assert_eq!(f.error.as_deref(), Some("there is already an agent named 'web'"));

        app.modal = Some(form("web", false));
        app.launch_form();
        assert!(app.modal.is_none());
        assert_eq!(app.agents.last().unwrap().name, "web-2", "a suggested name is made unique");

        let mut f = form("job", true);
        if let Modal::New(f) = &mut f {
            f.prompt = TextInput::new("do it");
        }
        app.modal = Some(f);
        app.launch_form();
        let Some(Modal::New(f)) = &app.modal else { panic!("the form stays open") };
        assert!(f.error.as_deref().unwrap().contains("takes no prompt"));

        let other = app.agents.last().unwrap().id;
        app.modal = Some(Modal::Rename(other, TextInput::new("web")));
        app.modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::Rename(..))), "rename to a taken name is refused");
        assert_eq!(app.agents.last().unwrap().name, "web-2");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn digit_names_are_refused() {
        assert!(only_digits("1") && only_digits("42"));
        assert!(!only_digits("") && !only_digits("web2") && !only_digits("2web") && !only_digits("-1"));
    }

    #[test]
    fn closing_the_focused_agent_marks_the_next_one_seen() {
        let mut app = test_app();
        for id in 1..=3 {
            let mut a = crate::agent::test_agent(Kind::Claude);
            a.id = id;
            a.set_state(State::Idle, false);
            app.agents.push(a);
        }
        let (first, second, third) = (app.agents[0].id, app.agents[1].id, app.agents[2].id);
        app.agents[1].unseen = true;
        app.agents[2].unseen = true;
        app.focus = 0;
        app.close(first);
        assert_eq!(app.agents[app.focus].id, second, "the next agent is focused");
        assert!(!app.agents[app.focus].unseen, "and is no longer unseen");
        assert!(app.agents.iter().find(|a| a.id == third).unwrap().unseen, "others keep their flag");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn fast_typing_searches_once_more_when_it_stops() {
        let mut app = test_app();
        app.agents.push(crate::agent::test_agent(Kind::Claude));
        app.modal = Some(Modal::Search(Search::default()));
        for c in "error".chars() {
            app.modal_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let first = app.last_search.expect("the first key searches at once");
        assert!(app.search_due.is_some(), "the rest wait for a pause");
        app.run_due_search();
        assert_eq!(app.last_search, Some(first), "not due yet");
        std::thread::sleep(Duration::from_millis(160));
        app.run_due_search();
        assert!(app.search_due.is_none() && app.last_search != Some(first), "ran once typing stopped");

        // Enter jumps with the hits for what's typed, even mid-wait.
        app.modal_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(app.search_due.is_some());
        app.modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search_due.is_none());
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn frames_are_scheduled_for_when_the_picture_changes() {
        let mut app = test_app();
        app.toast = None;
        let mut a = crate::agent::test_agent(Kind::Claude);
        a.set_state(State::Idle, false);
        app.agents.push(a);
        assert_eq!(app.next_frame_change(), None, "nothing moves");
        let until = |app: &App| app.next_frame_change().unwrap().saturating_duration_since(Instant::now());
        app.agents[0].set_state(State::Blocked, false);
        let wait = until(&app);
        assert!(wait <= Duration::from_millis(541), "next blink: {wait:?}");
        // Over a second of blinking, only the blinks are drawn.
        let mut frames = 0;
        let end = Instant::now() + Duration::from_millis(1100);
        while Instant::now() < end {
            std::thread::sleep(until(&app));
            frames += 1;
        }
        assert!((2..=3).contains(&frames), "{frames} frames for a blink every 540 ms");
        app.agents[0].set_state(State::Working, false);
        assert!(until(&app) <= Duration::from_millis(91), "spinner steps every 90 ms");
        app.agents[0].set_state(State::Done, false);
        app.toast("hi", ToastKind::Info);
        let wait = until(&app);
        assert!(wait > Duration::from_millis(3000) && wait <= Duration::from_millis(3201), "the toast fades: {wait:?}");
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn worktree_names_are_safe_folders_and_branches() {
        assert_eq!(worktree_name(".."), "agent");
        assert_eq!(worktree_name("."), "agent");
        assert_eq!(worktree_name("a..b"), "a.b");
        assert_eq!(worktree_name(".hidden."), "hidden");
        assert_eq!(worktree_name("fix.lock"), "fix");
        assert_eq!(worktree_name("v1.2-api"), "v1.2-api");
    }

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
        let mut custom = test_agent(Kind::Custom);
        custom.set_state(State::Idle, true);
        assert!(!broadcast_reaches(&custom, BroadcastTarget::All), "a custom command may be bash or ssh: skipped");
        assert!(!broadcast_reaches(&custom, BroadcastTarget::Waiting));
    }
}
