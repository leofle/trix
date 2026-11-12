//! Detach / attach.
//!
//! The workspace (agents, PTYs, emulators, state machines) lives in a
//! background `trix server` process. `trix` itself is a thin client: it puts
//! the terminal in raw mode, forwards input events to the server and copies the
//! rendered frames it gets back to stdout. Closing the terminal (or `alt+d`)
//! only ends the client; agents keep running, and their state changes still
//! reach you as desktop notifications.
//!
//! Besides the one controlling terminal, any number of read-only viewers can
//! attach (`trix view`). They get a copy of the controller's frames, cropped
//! or padded to their own size, and their keys never reach the workspace.
//!
//! Wire format, both directions: `[kind: u8][len: u32 le][payload]`.
//! Client → server payloads are JSON `ClientMsg`s. Server → client frames are
//! raw terminal bytes (`OUT`), a goodbye with a message to print (`BYE`), or a
//! plain-text reply (`TEXT`, for `trix ls`; a JSON `control::Reply` for the
//! agent-control commands).

use crossterm::event::Event;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::app::AppEvent;
use crate::hooks::runtime_dir;

pub const OUT: u8 = 1;
pub const BYE: u8 = 2;
pub const TEXT: u8 = 3;
pub const MSG: u8 = 10;

const MAX_FRAME: usize = 64 << 20;

/// Bumped whenever the wire format changes, so a freshly built client never
/// talks to an older server still running in the background.
pub const PROTOCOL: u32 = 1;

#[derive(Serialize, Deserialize)]
pub enum ClientMsg {
    /// Attach this terminal. `spawn` lists agent kinds to start in `cwd`.
    Hello { protocol: u32, cols: u16, rows: u16, cwd: PathBuf, spawn: Vec<String> },
    Event(Event),
    List,
    Kill,
    /// Watch read-only. A new variant rather than a Hello flag, so an older
    /// server rejects it instead of handing the viewer control.
    View { protocol: u32, cols: u16, rows: u16 },
    /// `trix send/read/wait/spawn/close`, `trix ls --json`. `from` is the
    /// pane asking, when it is one of this session's.
    Control { protocol: u32, from: Option<u32>, req: crate::control::Request },
}

/// Session-level events, delivered to the server's main loop.
pub enum SessionEvent {
    Attach { id: u64, stream: UnixStream, cols: u16, rows: u16, cwd: PathBuf, spawn: Vec<String> },
    Resize(u64, u16, u16),
    View { id: u64, stream: UnixStream, cols: u16, rows: u16 },
    ViewResize(u64, u16, u16),
    /// A viewer pressed a key that means "stop watching".
    Unview(u64),
    Detached(u64),
    List(UnixStream),
    Control { stream: UnixStream, from: Option<u32>, req: crate::control::Request },
    /// Held open until the server has stopped, so `trix kill` can wait for it.
    Kill(UnixStream),
}

// ───────────────────────────── naming ─────────────────────────────

/// The session everyone gets without `-s`. Its files keep their original,
/// unsuffixed names, so a server started by an older build is still found.
pub const DEFAULT: &str = "default";

static NAME: OnceLock<String> = OnceLock::new();

/// Pick the session this process works with. Set once, before any path below
/// is used; the server inherits it through `TRIX_SESSION`.
pub fn set_name(name: String) {
    let _ = NAME.set(name);
}

pub fn name() -> &'static str {
    NAME.get().map(String::as_str).unwrap_or(DEFAULT)
}

pub fn is_default() -> bool {
    name() == DEFAULT
}

/// Names end up in file names and on the command line: keep them boring.
pub fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Ok(()) } else { Err(format!("bad session name '{name}': use up to 32 letters, digits, - or _, starting with a letter or digit")) }
}

/// `trix`, or `trix -s work`: what to type to reach this session again.
pub fn command() -> String {
    if is_default() { "trix".into() } else { format!("trix -s {}", name()) }
}

/// `<stem>.<ext>` for the default session, `<stem>-<name>.<ext>` otherwise.
fn file_for(session: &str, stem: &str, ext: &str) -> PathBuf {
    if session == DEFAULT {
        runtime_dir().join(format!("{stem}.{ext}"))
    } else {
        runtime_dir().join(format!("{stem}-{session}.{ext}"))
    }
}

fn control_path_for(session: &str) -> PathBuf {
    file_for(session, "session", "sock")
}

pub fn control_path() -> PathBuf {
    control_path_for(name())
}

fn lock_path() -> PathBuf {
    file_for(name(), "session", "lock")
}

pub fn log_path() -> PathBuf {
    file_for(name(), "server", "log")
}

/// Sessions that have a control socket, default first. A socket can outlive a
/// server that died hard, so callers still have to connect to know.
pub fn candidates() -> Vec<String> {
    let files = std::fs::read_dir(runtime_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned());
    session_names(files)
}

/// Session names from runtime-dir file names, default first, then by name.
fn session_names(files: impl Iterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = files
        .filter_map(|file| {
            let stem = file.strip_suffix(".sock")?;
            if stem == "session" {
                return Some(DEFAULT.to_string());
            }
            let n = stem.strip_prefix("session-")?;
            check_name(n).is_ok().then(|| n.to_string())
        })
        .collect();
    names.sort_by(|a, b| (a != DEFAULT, a).cmp(&(b != DEFAULT, b)));
    names.dedup();
    names
}

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

fn send_msg(w: &mut impl Write, msg: &ClientMsg) -> io::Result<()> {
    let json = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, MSG, &json)
}

// ───────────────────────────── server side ─────────────────────────────

/// One client's socket, written by a thread of its own: a terminal that stops
/// reading (stalled SSH, a suspended terminal) never blocks the server, which
/// only queues frames. One that falls `MAX_BEHIND` bytes behind is dropped.
struct Peer {
    stream: UnixStream,
    tx: Sender<Job>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

enum Job {
    Frame(Vec<u8>),
    /// Everything queued so far is written: hang up.
    Close,
}

/// A few seconds of full-screen frames.
const MAX_BEHIND: usize = 8 << 20;

impl Peer {
    fn new(stream: UnixStream) -> Peer {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Ok(mut w) = stream.try_clone() {
            // Only this thread waits on it; a write that can't finish in this
            // long means the reader is gone for good.
            let _ = w.set_write_timeout(Some(Duration::from_secs(10)));
            let (queued, done) = (queued.clone(), done.clone());
            let _ = std::thread::Builder::new().name("trix-peer".into()).spawn(move || {
                for job in rx {
                    match job {
                        Job::Frame(b) => {
                            let ok = w.write_all(&b).and_then(|()| w.flush()).is_ok();
                            queued.fetch_sub(b.len(), Ordering::Relaxed);
                            if !ok {
                                break;
                            }
                        }
                        Job::Close => break,
                    }
                }
                // Its reader thread sees this and reports `Detached`.
                let _ = w.shutdown(std::net::Shutdown::Both);
                done.store(true, Ordering::Release);
            });
        } else {
            done.store(true, Ordering::Release);
        }
        Peer { stream, tx, queued, done }
    }

    /// Queue one frame. False when the client is gone or too far behind,
    /// and then it has been hung up on.
    fn send(&self, kind: u8, payload: &[u8]) -> bool {
        let len = payload.len() + 5;
        if self.done.load(Ordering::Acquire) || self.queued.load(Ordering::Relaxed) + len > MAX_BEHIND {
            self.hang_up();
            return false;
        }
        let mut buf = Vec::with_capacity(len);
        buf.push(kind);
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(payload);
        self.queued.fetch_add(len, Ordering::Relaxed);
        if self.tx.send(Job::Frame(buf)).is_err() {
            self.hang_up();
            return false;
        }
        true
    }

    /// Hang up once what's queued (a goodbye, say) has been written.
    fn close(&self) {
        if self.tx.send(Job::Close).is_err() {
            self.hang_up();
        }
    }

    fn hang_up(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// Wait until the writer is done (after `close`), but not past `until`.
    fn wait_done(&self, until: Instant) {
        while !self.done.load(Ordering::Acquire) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Link {
    client: Option<(u64, Peer)>,
    /// A controller `frame` dropped for being gone or stalled, whose
    /// `Detached` hasn't been handled yet.
    dropped: Option<u64>,
    buf: Vec<u8>,
    /// The controller's size, kept after it leaves: viewers keep seeing the
    /// layout it had, and agents keep their terminal size.
    size: (u16, u16),
    viewers: Vec<Viewer>,
}

struct Viewer {
    id: u64,
    peer: Peer,
    size: (u16, u16),
    /// What this viewer's screen shows now; `None` repaints from scratch.
    shown: Option<Buffer>,
}

/// The currently attached client, shared by the renderer and the app (for
/// title, bell and OSC 52 passthrough). Everything written while detached is
/// dropped.
#[derive(Clone)]
pub struct Out(Arc<Mutex<Link>>);

impl Default for Out {
    fn default() -> Self {
        Out(Arc::new(Mutex::new(Link { client: None, dropped: None, buf: Vec::new(), size: (80, 24), viewers: Vec::new() })))
    }
}

impl Link {
    fn frame(&mut self, kind: u8, payload: &[u8]) {
        if let Some((id, p)) = &self.client {
            if !p.send(kind, payload) {
                // Gone or stalled: dropped. Its reader thread sees the hang-up
                // and reports `Detached`.
                self.dropped = Some(*id);
                self.client = None;
            }
        }
    }

    fn bye(&mut self, message: &str) {
        if let Some((_, p)) = self.client.take() {
            p.send(BYE, message.as_bytes());
            p.close();
        }
    }

    /// Send to every viewer, dropping the ones that are gone or stalled.
    fn send_viewers(&mut self, kind: u8, payload: &[u8]) {
        self.viewers.retain(|v| v.peer.send(kind, payload));
    }
}

impl Out {
    fn link(&self) -> std::sync::MutexGuard<'_, Link> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Terminal bytes for the controller only (clipboard).
    pub fn send(&self, bytes: &[u8]) {
        self.link().frame(OUT, bytes);
    }

    /// Terminal bytes for everyone watching (title, bell).
    pub fn send_all(&self, bytes: &[u8]) {
        let mut l = self.link();
        l.frame(OUT, bytes);
        l.send_viewers(OUT, bytes);
    }

    /// Anyone to draw for: the controller or a viewer.
    pub fn attached(&self) -> bool {
        let l = self.link();
        l.client.is_some() || !l.viewers.is_empty()
    }

    /// A terminal is in control (keys, focus, "seen").
    pub fn controlled(&self) -> bool {
        self.link().client.is_some()
    }

    pub fn viewers(&self) -> usize {
        self.link().viewers.len()
    }

    pub fn add_viewer(&self, id: u64, stream: UnixStream, size: (u16, u16)) {
        self.link().viewers.push(Viewer { id, peer: Peer::new(stream), size: (size.0.max(1), size.1.max(1)), shown: None });
    }

    pub fn resize_viewer(&self, id: u64, cols: u16, rows: u16) {
        if let Some(v) = self.link().viewers.iter_mut().find(|v| v.id == id) {
            v.size = (cols.max(1), rows.max(1));
            v.shown = None;
        }
    }

    /// Say goodbye to one viewer. Returns true when it was one.
    pub fn bye_viewer(&self, id: u64, message: &str) -> bool {
        let mut l = self.link();
        let Some(i) = l.viewers.iter().position(|v| v.id == id) else { return false };
        let v = l.viewers.remove(i);
        v.peer.send(BYE, message.as_bytes());
        v.peer.close();
        true
    }

    /// Copy a finished frame to every viewer, as a diff against what each one
    /// shows already.
    pub fn mirror(&self, frame: &Buffer, badge: Style) {
        let mut l = self.link();
        if l.viewers.is_empty() {
            return;
        }
        l.viewers.retain_mut(|v| {
            let next = viewer_frame(frame, v.size, badge);
            let bytes = paint(v.shown.as_ref(), &next);
            if !v.peer.send(OUT, &bytes) {
                return false;
            }
            v.shown = Some(next);
            true
        });
    }

    /// Make `stream` the client. A previously attached one is told why and
    /// let go. Returns true when somebody was kicked.
    pub fn attach(&self, id: u64, stream: UnixStream, size: (u16, u16)) -> bool {
        let mut l = self.link();
        let kicked = l.client.is_some();
        if kicked {
            l.bye("[trix attached from another terminal · `trix view` watches without taking over]");
        }
        l.client = Some((id, Peer::new(stream)));
        l.dropped = None;
        l.size = (size.0.max(1), size.1.max(1));
        l.buf.clear();
        kicked
    }

    /// The controller is done (detach, or kicked).
    pub fn bye(&self, message: &str) {
        self.link().bye(message);
    }

    /// The workspace is going away: everyone is done.
    pub fn bye_all(&self, message: &str) {
        let mut l = self.link();
        let mut peers: Vec<Peer> = l.client.take().map(|(_, p)| p).into_iter().collect();
        peers.extend(l.viewers.drain(..).map(|v| v.peer));
        drop(l);
        for p in &peers {
            p.send(BYE, message.as_bytes());
            p.close();
        }
        // The server is about to exit: let the goodbyes go out first, but
        // don't wait long on a terminal that isn't reading.
        let until = Instant::now() + Duration::from_secs(1);
        for p in &peers {
            p.wait_done(until);
            p.hang_up();
        }
    }

    /// The client with this id went away on its own. Returns true when it
    /// was the controller.
    pub fn lost(&self, id: u64) -> bool {
        let mut l = self.link();
        l.viewers.retain(|v| v.id != id);
        if l.client.as_ref().is_some_and(|(c, _)| *c == id) {
            if let Some((_, p)) = l.client.take() {
                p.hang_up();
            }
            return true;
        }
        // Dropped earlier for stalling, and nobody has taken over since.
        if l.dropped == Some(id) {
            l.dropped = None;
            return l.client.is_none();
        }
        false
    }

    pub fn resize(&self, id: u64, cols: u16, rows: u16) -> bool {
        let mut l = self.link();
        if l.client.as_ref().is_some_and(|(c, _)| *c == id) {
            l.size = (cols.max(1), rows.max(1));
            return true;
        }
        false
    }

    fn size(&self) -> (u16, u16) {
        self.link().size
    }
}

const BADGE: &str = " ◉ view only · q leaves ";

/// The controller's frame as one viewer sees it: cropped or padded to the
/// viewer's size, with a badge so nobody mistakes it for the real thing.
fn viewer_frame(frame: &Buffer, (cols, rows): (u16, u16), badge: Style) -> Buffer {
    let mut out = Buffer::empty(Rect::new(0, 0, cols, rows));
    let src = frame.area;
    let (w, h) = (cols.min(src.width), rows.min(src.height));
    for y in 0..h {
        for x in 0..w {
            let cell = &frame[(src.x + x, src.y + y)];
            // Half of a wide character would spill past the crop.
            if x + 1 == w && w < src.width && cell.symbol().width() > 1 {
                continue;
            }
            out[(x, y)] = cell.clone();
        }
    }
    // At the right end of the controller's top bar, over its clock.
    let bw = BADGE.width() as u16;
    if w >= bw {
        out.set_string(w - bw, 0, BADGE, badge);
    }
    out
}

/// Escape codes that turn `shown` into `next` (everything, from a cleared
/// screen, when the viewer has nothing yet).
fn paint(shown: Option<&Buffer>, next: &Buffer) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut be = CrosstermBackend::new(&mut bytes);
    let blank;
    let from = match shown {
        Some(b) if b.area == next.area => b,
        _ => {
            let _ = be.clear();
            blank = Buffer::empty(next.area);
            &blank
        }
    };
    let _ = be.draw(from.diff(next).into_iter());
    let _ = be.hide_cursor();
    let _ = Backend::flush(&mut be);
    bytes
}

/// Keys a viewer presses to stop watching. Everything else is ignored.
fn leaves(k: &KeyEvent) -> bool {
    if k.kind == KeyEventKind::Release {
        return false;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    matches!(k.code, KeyCode::Char('q' | 'Q') | KeyCode::Esc)
        || (ctrl && matches!(k.code, KeyCode::Char('c' | 'd')))
        || (alt && k.code == KeyCode::Char('d'))
}

/// Buffers a frame's escape codes and ships them as one `OUT` on flush.
pub struct OutWriter(Out);

impl Write for OutWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.link().buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut l = self.0.link();
        let buf = std::mem::take(&mut l.buf);
        if !buf.is_empty() {
            l.frame(OUT, &buf);
        }
        l.buf = buf;
        l.buf.clear();
        Ok(())
    }
}

/// A crossterm backend whose "terminal" is the attached client: same escape
/// codes, but the size comes from the client and nothing queries a tty.
pub struct RemoteBackend {
    inner: CrosstermBackend<OutWriter>,
    out: Out,
}

impl RemoteBackend {
    pub fn new(out: Out) -> Self {
        RemoteBackend { inner: CrosstermBackend::new(OutWriter(out.clone())), out }
    }
}

impl Backend for RemoteBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(Position::ORIGIN)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
        self.inner.set_cursor_position(p)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, t: ClearType) -> io::Result<()> {
        self.inner.clear_region(t)
    }
    fn size(&self) -> io::Result<Size> {
        let (w, h) = self.out.size();
        Ok(Size::new(w, h))
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        let (w, h) = self.out.size();
        Ok(WindowSize { columns_rows: Size::new(w, h), pixels: Size::new(w * 9, h * 18) })
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

/// Only one server per user. The lock is held for the server's lifetime and
/// released by the kernel however it dies.
pub fn lock() -> io::Result<Option<File>> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(lock_path())?;
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok((r == 0).then_some(f))
}

/// Accept clients on the control socket.
pub fn listen(path: &Path, tx: Sender<AppEvent>) -> io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::thread::Builder::new().name("trix-control".into()).spawn(move || {
        for stream in listener.incoming() {
            // A failing accept (out of file descriptors, say) fails again at
            // once: back off instead of spinning a core.
            let Ok(stream) = stream else {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            let tx = tx.clone();
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let _ = std::thread::Builder::new().name("trix-client".into()).spawn(move || serve_client(id, stream, tx));
        }
    })?;
    Ok(())
}

fn serve_client(id: u64, mut s: UnixStream, tx: Sender<AppEvent>) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let first = match read_frame(&mut s) {
        Ok((MSG, p)) => serde_json::from_slice::<ClientMsg>(&p),
        _ => return,
    };
    let _ = s.set_read_timeout(None);
    let send = |e: SessionEvent| tx.send(AppEvent::Session(e)).is_ok();
    match first {
        Ok(ClientMsg::Hello { protocol, cols, rows, cwd, spawn }) => {
            let (cols, rows) = clamp_size(cols, rows);
            if protocol != PROTOCOL {
                let msg = "trix: the running workspace is from a different trix build — `trix kill` it (stops its agents) and start again";
                let _ = write_frame(&mut s, BYE, msg.as_bytes());
                return;
            }
            let Ok(w) = s.try_clone() else { return };
            if !send(SessionEvent::Attach { id, stream: w, cols, rows, cwd, spawn }) {
                return;
            }
            let mut r = BufReader::new(s);
            while let Ok((MSG, p)) = read_frame(&mut r) {
                let Ok(ClientMsg::Event(ev)) = serde_json::from_slice::<ClientMsg>(&p) else { continue };
                let ev = match ev {
                    Event::Key(k) => AppEvent::Key(k),
                    Event::Mouse(m) => AppEvent::Mouse(m),
                    Event::Paste(s) => AppEvent::Paste(s),
                    Event::Resize(c, r) => {
                        let (c, r) = clamp_size(c, r);
                        AppEvent::Session(SessionEvent::Resize(id, c, r))
                    }
                    Event::FocusGained => AppEvent::FocusGained,
                    Event::FocusLost => AppEvent::FocusLost,
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
            send(SessionEvent::Detached(id));
        }
        Ok(ClientMsg::View { protocol, cols, rows }) => {
            let (cols, rows) = clamp_size(cols, rows);
            if protocol != PROTOCOL {
                let msg = "trix: the running workspace is from a different trix build — `trix kill` it (stops its agents) and start again";
                let _ = write_frame(&mut s, BYE, msg.as_bytes());
                return;
            }
            let Ok(w) = s.try_clone() else { return };
            if !send(SessionEvent::View { id, stream: w, cols, rows }) {
                return;
            }
            let mut r = BufReader::new(s);
            while let Ok((MSG, p)) = read_frame(&mut r) {
                let ev = match serde_json::from_slice::<ClientMsg>(&p) {
                    Ok(ClientMsg::Event(Event::Key(k))) if leaves(&k) => SessionEvent::Unview(id),
                    Ok(ClientMsg::Event(Event::Resize(c, r))) => {
                        let (c, r) = clamp_size(c, r);
                        SessionEvent::ViewResize(id, c, r)
                    }
                    _ => continue,
                };
                if !send(ev) {
                    return;
                }
            }
            send(SessionEvent::Detached(id));
        }
        Ok(ClientMsg::List) => {
            send(SessionEvent::List(s));
        }
        Ok(ClientMsg::Kill) => {
            send(SessionEvent::Kill(s));
        }
        Ok(ClientMsg::Control { protocol, from, req }) => {
            if protocol != crate::control::CONTROL {
                let r = crate::control::Reply { code: 1, err: "the running workspace is from a different trix build — restart it to use this".into(), ..Default::default() };
                crate::control::reply(&mut s, &r);
                return;
            }
            send(SessionEvent::Control { stream: s, from, req });
        }
        _ => {}
    }
}

// ───────────────────────────── client side ─────────────────────────────

pub fn connect() -> Option<UnixStream> {
    UnixStream::connect(control_path()).ok()
}

fn connect_to(session: &str) -> Option<UnixStream> {
    UnixStream::connect(control_path_for(session)).ok()
}

/// Start a detached server and wait for it to accept connections.
pub fn start_server() -> anyhow::Result<UnixStream> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_path())?;
    let mut cmd = Command::new(exe);
    cmd.arg("server").env("TRIX_SESSION", name()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // New session: no controlling terminal, so closing this one can't SIGHUP it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = connect() {
            return Ok(s);
        }
        // A clean exit means another trix won the race to start the server and
        // ours stepped aside: keep trying to reach it until the deadline.
        if let Ok(Some(status)) = child.try_wait().map(|s| s.filter(|s| !s.success())) {
            let log = std::fs::read_to_string(log_path()).unwrap_or_default();
            anyhow::bail!("server exited ({status}){}{}", if log.trim().is_empty() { "" } else { ":\n" }, log.trim());
        }
        if Instant::now() > deadline {
            anyhow::bail!("server did not start (see {})", log_path().display());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Ask a session's server something that has a plain-text answer. `None`
/// when that session isn't running.
pub fn query(session: &str, msg: &ClientMsg) -> Option<String> {
    let mut s = connect_to(session)?;
    send_msg(&mut s, msg).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    match read_frame(&mut s) {
        Ok((TEXT, p)) => Some(String::from_utf8_lossy(&p).into_owned()),
        _ => Some(String::new()),
    }
}

/// The biggest terminal a client may claim. A client that says 65535 by 65535
/// would make the server allocate billions of cells.
const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 500;

fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS))
}

/// Send a control request to this session and wait for its reply: up to
/// `wait` seconds more than usual for `trix wait` (`Some(0)`: forever).
pub fn control(msg: &ClientMsg, wait: Option<u64>) -> Result<crate::control::Reply, String> {
    let Some(mut s) = connect() else {
        return Err(if is_default() { "no workspace running".into() } else { format!("no session '{}' running", name()) });
    };
    send_msg(&mut s, msg).map_err(|e| format!("talking to the workspace: {e}"))?;
    let timeout = match wait {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs.saturating_add(10))),
        None => Some(Duration::from_secs(15)),
    };
    let _ = s.set_read_timeout(timeout);
    match read_frame(&mut s) {
        Ok((TEXT, p)) => serde_json::from_slice(&p).map_err(|e| format!("bad reply from the workspace: {e}")),
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Err("the workspace didn't answer in time".into()),
        _ => Err(format!(
            "the running workspace is from an older trix that doesn't know this command — restart it (alt+q, then `{}`) to use it",
            command()
        )),
    }
}

/// Connect and send `msg`, leaving the replies to the caller (`trix events`).
pub fn open(msg: &ClientMsg) -> Result<UnixStream, String> {
    let Some(mut s) = connect() else {
        return Err(if is_default() { "no workspace running".into() } else { format!("no session '{}' running", name()) });
    };
    send_msg(&mut s, msg).map_err(|e| format!("talking to the workspace: {e}"))?;
    Ok(s)
}

pub fn send_once(session: &str, msg: &ClientMsg) -> bool {
    match connect_to(session) {
        Some(mut s) => {
            let ok = send_msg(&mut s, msg).is_ok();
            // Wait for the server to hang up, so `trix kill && trix` is safe.
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = s.read_to_end(&mut Vec::new());
            ok
        }
        None => false,
    }
}

/// How an attached client ended.
pub enum Ended {
    Bye(String),
    Lost,
    /// The server hung up without a word: a build that predates this kind
    /// of client.
    Refused,
}

/// How a terminal attaches.
pub enum Mode {
    /// In control, starting these agent kinds in the current folder.
    Control(Vec<String>),
    /// Read-only.
    View,
}

/// Run the client on an already-prepared terminal until the server says bye
/// or goes away.
pub fn run_client(mut s: UnixStream, cols: u16, rows: u16, mode: Mode) -> io::Result<Ended> {
    let hello = match mode {
        Mode::Control(spawn) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            ClientMsg::Hello { protocol: PROTOCOL, cols, rows, cwd, spawn }
        }
        Mode::View => ClientMsg::View { protocol: PROTOCOL, cols, rows },
    };
    send_msg(&mut s, &hello)?;
    let mut w = s.try_clone()?;
    std::thread::Builder::new().name("trix-input".into()).spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if send_msg(&mut w, &ClientMsg::Event(ev)).is_err() {
                break;
            }
        }
    })?;
    let mut r = BufReader::with_capacity(1 << 16, s);
    let mut out = io::stdout().lock();
    let mut heard = false;
    loop {
        let frame = read_frame(&mut r);
        if frame.is_err() && !heard {
            return Ok(Ended::Refused);
        }
        heard = true;
        match frame {
            Ok((OUT, p)) => {
                out.write_all(&p)?;
                out.flush()?;
            }
            Ok((BYE, p)) => return Ok(Ended::Bye(String::from_utf8_lossy(&p).into_owned())),
            Ok(_) => {}
            Err(_) => return Ok(Ended::Lost),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_viewer_that_never_reads_cannot_block_the_server() {
        let out = Out::default();
        let (server_end, _client_end) = UnixStream::pair().unwrap();
        out.add_viewer(1, server_end, (80, 24));
        let chunk = vec![b'x'; 64 << 10];
        let start = Instant::now();
        let mut sent = 0;
        while out.viewers() == 1 && sent < 1000 {
            out.send_all(&chunk);
            sent += 1;
        }
        assert_eq!(out.viewers(), 0, "dropped once too far behind");
        assert!(start.elapsed() < Duration::from_millis(500), "never waited on it: {:?}", start.elapsed());
        // The frame that overflowed is the last one counted, so the viewer held
        // everything up to MAX_BEHIND and was dropped on the next.
        assert!(sent * chunk.len() >= MAX_BEHIND, "not before it fell behind ({sent} frames)");
    }

    #[test]
    fn absurd_terminal_sizes_are_clamped() {
        assert_eq!(clamp_size(65535, 65535), (MAX_COLS, MAX_ROWS));
        assert_eq!(clamp_size(0, 0), (1, 1));
        assert_eq!(clamp_size(120, 40), (120, 40));
    }

    #[test]
    fn goodbyes_reach_a_reading_viewer() {
        let out = Out::default();
        let (server_end, mut client_end) = UnixStream::pair().unwrap();
        out.add_viewer(1, server_end, (80, 24));
        out.send_all(b"hello");
        out.bye_all("[trix quit]");
        assert_eq!(read_frame(&mut client_end).unwrap(), (OUT, b"hello".to_vec()));
        assert_eq!(read_frame(&mut client_end).unwrap(), (BYE, b"[trix quit]".to_vec()));
        assert!(read_frame(&mut client_end).is_err(), "hung up after");
    }
    use std::io::Cursor;

    #[test]
    fn session_names_are_checked() {
        for ok in ["work", "a", "9", "my-proj_2", &"x".repeat(32)] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "_x", ".x", "a/b", "a b", "a.b", "é", "../up", &"x".repeat(33)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_session_keeps_the_old_file_names() {
        let name = |p: PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name(file_for(DEFAULT, "session", "sock")), "session.sock");
        assert_eq!(name(file_for(DEFAULT, "server", "log")), "server.log");
        assert_eq!(name(file_for("work", "session", "sock")), "session-work.sock");
        assert_eq!(name(file_for("work", "server", "log")), "server-work.log");
    }

    #[test]
    fn running_sessions_from_socket_files() {
        let files = [
            "session-work.sock", "session.lock", "session-work.lock", "server-work.log", "session.sock",
            "trix-123.sock", "claude-settings-123.json", "session-a.sock", "session-.sock", "session-bad name.sock",
        ];
        let names = session_names(files.iter().map(|s| s.to_string()));
        assert_eq!(names, ["default", "a", "work"]);
        assert!(session_names(std::iter::empty()).is_empty());
        assert_eq!(session_names(["session-b.sock".to_string(), "session-a.sock".to_string()].into_iter()), ["a", "b"]);
    }

    #[test]
    fn frames_roundtrip_in_sequence() {
        let mut buf = Vec::new();
        write_frame(&mut buf, OUT, b"hello").unwrap();
        write_frame(&mut buf, BYE, b"").unwrap();
        assert_eq!(&buf[..5], &[OUT, 5, 0, 0, 0]);
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap(), (OUT, b"hello".to_vec()));
        assert_eq!(read_frame(&mut r).unwrap(), (BYE, Vec::new()));
        assert!(read_frame(&mut r).is_err(), "end of stream");
    }

    #[test]
    fn truncated_and_oversized_frames_fail() {
        let mut buf = Vec::new();
        write_frame(&mut buf, TEXT, b"abcdef").unwrap();
        buf.truncate(8);
        assert!(read_frame(&mut Cursor::new(buf)).is_err());

        let mut huge = vec![OUT];
        huge.extend_from_slice(&((MAX_FRAME as u32) + 1).to_le_bytes());
        let err = read_frame(&mut Cursor::new(huge)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    fn frame_of(lines: &[&str]) -> Buffer {
        let w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
        let mut b = Buffer::empty(Rect::new(0, 0, w, lines.len() as u16));
        for (y, l) in lines.iter().enumerate() {
            b.set_string(0, y as u16, l, Style::new());
        }
        b
    }
    fn row(b: &Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn viewer_frame_crops_pads_and_badges() {
        let f = frame_of(&["0123456789".repeat(4).as_str(), "second row", "third"]);
        // Smaller viewer: cropped, with the badge over the top right.
        let v = viewer_frame(&f, (30, 2), Style::new());
        assert_eq!(v.area, Rect::new(0, 0, 30, 2));
        assert_eq!(row(&v, 0), format!("012345{BADGE}"));
        assert_eq!(row(&v, 1), format!("{:<30}", "second row"));
        // Bigger viewer: padded with blanks, badge still on the frame's edge.
        let v = viewer_frame(&f, (50, 5), Style::new());
        assert_eq!(row(&v, 0), format!("{}{BADGE}{}", &"0123456789".repeat(4)[..16], " ".repeat(10)));
        assert_eq!(row(&v, 2), format!("{:<50}", "third"));
        assert_eq!(row(&v, 4), " ".repeat(50));
        // Too narrow for the badge: just the frame.
        let v = viewer_frame(&f, (8, 1), Style::new());
        assert_eq!(row(&v, 0), "01234567");
    }

    #[test]
    fn viewer_frame_never_cuts_a_wide_char_in_half() {
        let f = frame_of(&["ab日本"]);
        let v = viewer_frame(&f, (3, 1), Style::new());
        assert_eq!(v[(2, 0)].symbol(), " ", "the half that fits is left blank");
        assert_eq!(viewer_frame(&f, (4, 1), Style::new())[(2, 0)].symbol(), "日");
    }

    #[test]
    fn paint_from_scratch_then_diffs() {
        let a = frame_of(&["hello", "world"]);
        let first = String::from_utf8(paint(None, &a)).unwrap();
        assert!(first.contains("\x1b[2J"), "first paint clears the screen");
        assert!(first.contains("hello") && first.contains("world"));
        let same = String::from_utf8(paint(Some(&a), &a)).unwrap();
        assert!(!same.contains("hello") && !same.contains("world"), "no change, no text: {same:?}");
        let b = frame_of(&["hello", "wOrld"]);
        let delta = String::from_utf8(paint(Some(&a), &b)).unwrap();
        assert!(delta.contains('O') && !delta.contains("hello") && !delta.contains("\x1b[2J"));
        // A resized viewer starts over.
        let c = frame_of(&["hello!"]);
        assert!(String::from_utf8(paint(Some(&a), &c)).unwrap().contains("\x1b[2J"));
    }

    #[test]
    fn only_leave_keys_leave() {
        let k = |code, m| KeyEvent::new(code, m);
        for key in [
            k(KeyCode::Char('q'), KeyModifiers::NONE),
            k(KeyCode::Esc, KeyModifiers::NONE),
            k(KeyCode::Char('c'), KeyModifiers::CONTROL),
            k(KeyCode::Char('d'), KeyModifiers::ALT),
            // alt+q quits trix for the controller; for a viewer it only leaves.
            k(KeyCode::Char('q'), KeyModifiers::ALT),
        ] {
            assert!(leaves(&key), "{key:?}");
        }
        for key in [
            k(KeyCode::Enter, KeyModifiers::NONE),
            k(KeyCode::Char('y'), KeyModifiers::NONE),
            k(KeyCode::Char('a'), KeyModifiers::ALT),
            k(KeyCode::Char('n'), KeyModifiers::ALT),
        ] {
            assert!(!leaves(&key), "{key:?}");
        }
        let mut release = k(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!leaves(&release));
    }

    #[test]
    fn client_messages_roundtrip() {
        let mut buf = Vec::new();
        let hello = ClientMsg::Hello { protocol: PROTOCOL, cols: 120, rows: 40, cwd: "/tmp/x".into(), spawn: vec!["claude".into()] };
        send_msg(&mut buf, &hello).unwrap();
        send_msg(&mut buf, &ClientMsg::Event(Event::Paste("hi".into()))).unwrap();
        let mut r = Cursor::new(buf);
        let (kind, p) = read_frame(&mut r).unwrap();
        assert_eq!(kind, MSG);
        match serde_json::from_slice::<ClientMsg>(&p).unwrap() {
            ClientMsg::Hello { protocol, cols, rows, cwd, spawn } => {
                assert_eq!((protocol, cols, rows), (PROTOCOL, 120, 40));
                assert_eq!(cwd, PathBuf::from("/tmp/x"));
                assert_eq!(spawn, ["claude"]);
            }
            _ => panic!("expected Hello"),
        }
        let (_, p) = read_frame(&mut r).unwrap();
        assert!(matches!(serde_json::from_slice::<ClientMsg>(&p).unwrap(), ClientMsg::Event(Event::Paste(s)) if s == "hi"));
    }
}
