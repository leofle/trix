//! Detach / attach.
//!
//! The workspace (agents, PTYs, emulators, state machines) lives in a
//! background `trix server` process. `trix` itself is a thin client: it puts
//! the terminal in raw mode, forwards input events to the server and copies the
//! rendered frames it gets back to stdout. Closing the terminal (or `alt+d`)
//! only ends the client; agents keep running, and their state changes still
//! reach you as desktop notifications.
//!
//! Wire format, both directions: `[kind: u8][len: u32 le][payload]`.
//! Client → server payloads are JSON `ClientMsg`s. Server → client frames are
//! raw terminal bytes (`OUT`), a goodbye with a message to print (`BYE`), or a
//! plain-text reply (`TEXT`, for `trix ls`).

use crossterm::event::Event;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
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
}

/// Session-level events, delivered to the server's main loop.
pub enum SessionEvent {
    Attach { id: u64, stream: UnixStream, cols: u16, rows: u16, cwd: PathBuf, spawn: Vec<String> },
    Resize(u64, u16, u16),
    Detached(u64),
    List(UnixStream),
    Kill,
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
    let mut names: Vec<String> = std::fs::read_dir(runtime_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
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

struct Link {
    client: Option<(u64, UnixStream)>,
    buf: Vec<u8>,
    size: (u16, u16),
}

/// The currently attached client, shared by the renderer and the app (for
/// title, bell and OSC 52 passthrough). Everything written while detached is
/// dropped.
#[derive(Clone)]
pub struct Out(Arc<Mutex<Link>>);

impl Default for Out {
    fn default() -> Self {
        Out(Arc::new(Mutex::new(Link { client: None, buf: Vec::new(), size: (80, 24) })))
    }
}

impl Link {
    fn frame(&mut self, kind: u8, payload: &[u8]) {
        if let Some((_, s)) = &mut self.client {
            if write_frame(s, kind, payload).is_err() {
                // Gone or stalled: drop it. Its reader thread sees the
                // shutdown and reports `Detached`.
                let _ = s.shutdown(std::net::Shutdown::Both);
                self.client = None;
            }
        }
    }

    fn bye(&mut self, message: &str) {
        self.frame(BYE, message.as_bytes());
        if let Some((_, s)) = self.client.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Out {
    fn link(&self) -> std::sync::MutexGuard<'_, Link> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Terminal bytes outside the frame (title, bell, clipboard).
    pub fn send(&self, bytes: &[u8]) {
        self.link().frame(OUT, bytes);
    }

    pub fn attached(&self) -> bool {
        self.link().client.is_some()
    }

    /// Make `stream` the client. A previously attached one is told why and
    /// let go. Returns true when somebody was kicked.
    pub fn attach(&self, id: u64, stream: UnixStream, size: (u16, u16)) -> bool {
        let mut l = self.link();
        let kicked = l.client.is_some();
        if kicked {
            l.bye("[trix attached from another terminal]");
        }
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        l.client = Some((id, stream));
        l.size = (size.0.max(1), size.1.max(1));
        l.buf.clear();
        kicked
    }

    pub fn bye(&self, message: &str) {
        self.link().bye(message);
    }

    /// The client with this id went away on its own.
    pub fn lost(&self, id: u64) {
        let mut l = self.link();
        if l.client.as_ref().is_some_and(|(c, _)| *c == id) {
            l.client = None;
        }
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
        for stream in listener.incoming().flatten() {
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
                    Event::Resize(c, r) => AppEvent::Session(SessionEvent::Resize(id, c, r)),
                    Event::FocusGained => AppEvent::FocusGained,
                    Event::FocusLost => AppEvent::FocusLost,
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
            send(SessionEvent::Detached(id));
        }
        Ok(ClientMsg::List) => {
            send(SessionEvent::List(s));
        }
        Ok(ClientMsg::Kill) => {
            send(SessionEvent::Kill);
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
    let log = File::create(log_path())?;
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
        if let Ok(Some(status)) = child.try_wait() {
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
}

/// Run the client on an already-prepared terminal until the server says bye
/// or goes away.
pub fn run_client(mut s: UnixStream, cols: u16, rows: u16, spawn: Vec<String>) -> io::Result<Ended> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    send_msg(&mut s, &ClientMsg::Hello { protocol: PROTOCOL, cols, rows, cwd, spawn })?;
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
    loop {
        match read_frame(&mut r) {
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
