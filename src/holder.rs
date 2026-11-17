//! One agent's terminal, held by a small process of its own: `trix holder`.
//!
//! The server used to own every agent's PTY, so anything that took it down
//! (a panic, which release builds turn into an abort, or a `kill -9`) hung up
//! on every agent at once. Now each agent runs under a holder that owns
//! exactly one PTY and the program in it, keeps the last stretch of its
//! output (`Ring`), and serves both over a Unix socket in the private runtime
//! folder. The server keeps the emulator and the state machine. When it goes,
//! the holders stay, and the next server finds them (`discover`), attaches,
//! and rebuilds each screen from what its holder kept.
//!
//! The wire: frames of a kind byte, a little-endian u32 length, then the
//! payload. Into the holder: `INPUT` (bytes for the program), `RESIZE` (cols
//! and rows, two LE u16s), `META` (the server's notes about the agent, kept
//! as they are and handed back on attach), `KILL` (stop the program, then the
//! holder). Out of it, once per attach: `HELLO` (JSON `Hello`), then `OUTPUT`
//! with everything buffered. After that `OUTPUT` as the program writes, and
//! `EXIT` (JSON, its exit code or null) when it ends. One server at a time:
//! a new connection takes over from the last.
//!
//! Sockets are named `holder.<session hash>.<agent id>.<holder pid>.sock`
//! (`hooks::short_hash`: a session name of any length fits in a socket
//! path), so a new server knows whose they are, and which are stale, without
//! connecting to them. The greeting names the session in full.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use alacritty_terminal::tty;

use crate::modes::Tracker;

pub const INPUT: u8 = 1;
pub const RESIZE: u8 = 2;
pub const META: u8 = 3;
pub const KILL: u8 = 4;
pub const HELLO: u8 = 10;
pub const OUTPUT: u8 = 11;
pub const EXIT: u8 = 12;

/// Bigger than any replay or paste; anything larger is a broken peer.
const MAX_FRAME: usize = 16 << 20;
/// How much recent output a holder keeps for the next server to replay:
/// several screens of a busy TUI, thousands of lines of a shell.
pub const RING_BYTES: usize = 4 << 20;
/// A program that ended is kept (with its last screen) for a server to
/// collect, but not forever when none comes.
const LINGER: Duration = Duration::from_secs(60 * 60);
/// A server that stops reading for this long is let go, rather than the
/// program being stalled behind it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Trimming the oldest output starts the replay at a line, if one starts
/// this close to the cut.
const LINE_SEARCH: usize = 4096;

// ───────────────────────────── framing ─────────────────────────────

/// One frame, ready to write.
pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(payload.len() + 5);
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    w.write_all(&frame(kind, payload))?;
    w.flush()
}

/// Read one whole frame, blocking.
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

/// Frames out of a byte stream that arrives in pieces: what a reader with a
/// timeout needs, since a timeout in the middle of `read_frame` would lose
/// the part already read.
#[derive(Default)]
pub struct Frames {
    buf: Vec<u8>,
}

impl Frames {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame, if one has arrived.
    pub fn next_frame(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let kind = self.buf[0];
        let payload = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        Ok(Some((kind, payload)))
    }
}

pub fn resize_payload(cols: u16, rows: u16) -> [u8; 4] {
    let (c, r) = (cols.to_le_bytes(), rows.to_le_bytes());
    [c[0], c[1], r[0], r[1]]
}

pub fn parse_resize(p: &[u8]) -> Option<(u16, u16)> {
    let [c0, c1, r0, r1] = <[u8; 4]>::try_from(p).ok()?;
    let (cols, rows) = (u16::from_le_bytes([c0, c1]), u16::from_le_bytes([r0, r1]));
    (cols > 0 && rows > 0).then_some((cols, rows))
}

// ───────────────────────────── the buffer ─────────────────────────────

/// The newest `cap` bytes of output, and the terminal modes in force where
/// they start (`modes::Tracker`), so a replay can open by restoring them.
/// When the oldest go, the cut moves on to the next line start if there's
/// one nearby, never stops inside an escape sequence, and on the alternate
/// screen moves on to where the program next cleared it, if that's near: a
/// full-screen program's replay then starts with a whole screen, not the
/// tail of an update to one.
pub struct Ring {
    buf: VecDeque<u8>,
    cap: usize,
    /// Everything that fell off the front, followed.
    base: Tracker,
    /// Everything, followed: the modes in force now.
    now: Tracker,
    dropped: u64,
}

/// How far past the cut a clear of the alternate screen is looked for.
const CLEAR_SEARCH: usize = 256 << 10;
/// How far the cut moves on to get out of an escape sequence or a string.
const GROUND_SEARCH: usize = 64 << 10;

impl Ring {
    pub fn new(cap: usize) -> Ring {
        Ring { buf: VecDeque::new(), cap, base: Tracker::default(), now: Tracker::default(), dropped: 0 }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.now.feed(bytes);
        let over = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        if over == 0 {
            self.buf.extend(bytes);
            return;
        }
        // The oldest go first, then whatever of `bytes` doesn't fit either.
        let old = over.min(self.buf.len());
        self.take(old);
        let skip = over - old;
        self.base.feed(&bytes[..skip]);
        self.dropped += skip as u64;
        self.buf.extend(&bytes[skip..]);
        self.settle();
    }

    /// Move the cut on to a better place to start a replay.
    fn settle(&mut self) {
        let near = self.buf.len().min(LINE_SEARCH);
        if let Some(nl) = self.buf.range(..near).position(|&b| b == b'\n') {
            self.take(nl + 1);
        }
        let mut n = 0;
        while !self.base.ground() && !self.buf.is_empty() && n < GROUND_SEARCH {
            self.take(1);
            n += 1;
        }
        if self.base.ground()
            && self.base.modes().alt.is_some()
            && let Some(at) = find(self.buf.range(..self.buf.len().min(CLEAR_SEARCH)), b"\x1b[2J")
            && at > 0
        {
            self.take(at);
        }
    }

    /// Drop the oldest `n` bytes, following them.
    fn take(&mut self, n: usize) {
        let gone: Vec<u8> = self.buf.drain(..n).collect();
        self.base.feed(&gone);
        self.dropped += n as u64;
    }

    /// What a new emulator should be given: the modes in force where the
    /// kept output starts, then that output.
    pub fn replay(&self) -> Vec<u8> {
        let mut out = self.base.modes().preamble();
        let (a, b) = self.buf.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        out
    }

    /// The modes in force now.
    pub fn modes(&self) -> &crate::modes::Modes {
        self.now.modes()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    #[cfg(test)]
    pub fn bytes(&self) -> Vec<u8> {
        let (a, b) = self.buf.as_slices();
        [a, b].concat()
    }
}

/// Where `pat` first starts in `hay`.
fn find<'a>(hay: impl Iterator<Item = &'a u8>, pat: &[u8]) -> Option<usize> {
    let mut window: VecDeque<u8> = VecDeque::with_capacity(pat.len());
    for (i, &b) in hay.enumerate() {
        if window.len() == pat.len() {
            window.pop_front();
        }
        window.push_back(b);
        if window.len() == pat.len() && window.iter().eq(pat.iter()) {
            return Some(i + 1 - pat.len());
        }
    }
    None
}

// ───────────────────────────── what's said ─────────────────────────────

/// What the server asks a holder to run (on its stdin, as JSON).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Spec {
    pub session: String,
    pub id: u32,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    /// The server's notes (`agent::Meta`), as they start.
    pub meta: String,
}

/// The holder's one line on stdout: up, or why not.
#[derive(Serialize, Deserialize, Debug)]
enum Ready {
    Up { pid: u32 },
    Failed { error: String },
}

/// What a holder says first to every server that attaches.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Hello {
    /// The program's pid.
    pub pid: u32,
    /// The holder's own.
    pub holder: u32,
    /// The size the PTY has now, which the replay was last drawn at.
    pub cols: u16,
    pub rows: u16,
    pub meta: String,
    /// Whose it is: the session in full (its socket has only a hash), and
    /// the agent's id.
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub id: u32,
    /// How many bytes of output were dropped from the front of the replay.
    #[serde(default)]
    pub dropped: u64,
    /// The program has ended (and `code` is how).
    #[serde(default)]
    pub exited: bool,
    #[serde(default)]
    pub code: Option<i32>,
}

// ───────────────────────────── where ─────────────────────────────

fn socket_name(session: &str, id: u32, pid: u32) -> String {
    format!("holder.{}.{id}.{pid}.sock", crate::hooks::short_hash(session))
}

pub fn socket_path(session: &str, id: u32, pid: u32) -> PathBuf {
    crate::hooks::socket_in(&socket_name(session, id, pid))
}

/// (session hash, agent id, holder pid) from a socket's file name.
fn parse_name(name: &str) -> Option<(&str, u32, u32)> {
    let rest = name.strip_prefix("holder.")?.strip_suffix(".sock")?;
    let mut parts = rest.split('.');
    let (hash, id, pid) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || hash.len() != 12 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((hash, id.parse().ok()?, pid.parse().ok()?))
}

/// Holders of `session` whose process is alive, by agent id: (id, socket).
/// Sockets of holders that are gone, any session's, are removed on the way.
pub fn discover(session: &str) -> Vec<(u32, PathBuf)> {
    discover_in(&crate::hooks::socket_dirs(), Some(session))
}

/// Every session's live holders.
pub fn discover_all() -> Vec<(u32, PathBuf)> {
    discover_in(&crate::hooks::socket_dirs(), None)
}

fn discover_in(dirs: &[PathBuf], session: Option<&str>) -> Vec<(u32, PathBuf)> {
    let want = session.map(crate::hooks::short_hash);
    let mut found = Vec::new();
    for e in dirs.iter().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten()) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((hash, id, pid)) = parse_name(&name) else { continue };
        if !crate::platform::alive(pid) {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        if want.as_deref().is_none_or(|w| w == hash) {
            found.push((id, e.path()));
        }
    }
    found.sort();
    found
}

/// Stop the holders that no server has: what a server that died hard left
/// running. `None`: every session's. How many were stopped; returns once
/// they have (or after about twice `STOP_GRACE`).
pub fn kill_orphans(session: Option<&str>) -> usize {
    let mut holders = Vec::new();
    for (_, path) in discover_in(&crate::hooks::socket_dirs(), session) {
        // Attached first: a holder greets whoever connects, and one that
        // hung up before hearing it is dropped unread, KILL and all.
        match attach(&path) {
            Ok(mut att) => {
                let _ = att.stream.set_write_timeout(Some(Duration::from_secs(2)));
                if write_frame(&mut att.stream, KILL, &[]).is_ok() {
                    holders.push((att.hello.holder, att.stream));
                }
            }
            // Alive by its pid, but nobody listens: the pid is someone else's.
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
    let pids: Vec<u32> = holders.iter().map(|(p, _)| *p).collect();
    wait_gone(&pids, crate::agent::STOP_GRACE * 2 + Duration::from_secs(1));
    pids.len()
}

/// Wait until none of `pids` is alive, or `limit` passes.
pub fn wait_gone(pids: &[u32], limit: Duration) {
    let deadline = Instant::now() + limit;
    while pids.iter().any(|&p| crate::platform::alive(p)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ───────────────────────────── server side ─────────────────────────────

/// Why a holder didn't come up.
#[derive(Debug)]
pub enum LaunchError {
    /// The holder is fine, the program isn't: what starting it said.
    Program(String),
    /// No holder: run the program some other way.
    Holder(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::Program(e) | LaunchError::Holder(e) => f.write_str(e),
        }
    }
}

/// Start `exe holder` for `spec`, and wait until it's serving. The socket it
/// serves on, and its pid.
pub fn launch(exe: &Path, spec: &Spec) -> Result<(PathBuf, u32), LaunchError> {
    let bad = |e: &dyn std::fmt::Display| LaunchError::Holder(e.to_string());
    let json = serde_json::to_vec(spec).map_err(|e| bad(&e))?;
    let mut child = Command::new(exe)
        .arg("holder")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| bad(&e))?;
    let holder = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    // Reaped whenever it ends, so it never lingers as a zombie that
    // `platform::alive` would still count.
    crate::platform::reap(child);
    let (mut stdin, stdout) = stdin.zip(stdout).ok_or_else(|| bad(&"no pipe to the holder"))?;
    stdin.write_all(&json).map_err(|e| bad(&e))?;
    drop(stdin);
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("trix-holder-up".into()).spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).map_err(|_| bad(&"the holder did not start"))?;
    match serde_json::from_str::<Ready>(line.trim()) {
        Ok(Ready::Up { .. }) => Ok((socket_path(&spec.session, spec.id, holder), holder)),
        Ok(Ready::Failed { error }) => Err(LaunchError::Program(error)),
        Err(_) => Err(bad(&"the holder did not start")),
    }
}

/// A server's line to one holder, just attached.
pub struct Attached {
    pub hello: Hello,
    /// The output it kept, oldest first.
    pub replay: Vec<u8>,
    pub stream: UnixStream,
}

/// Connect to the holder at `path` and take over from whoever had it.
pub fn attach(path: &Path) -> io::Result<Attached> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let (kind, hello) = read_frame(&mut stream)?;
    if kind != HELLO {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a trix holder"));
    }
    let hello: Hello = serde_json::from_slice(&hello).map_err(io::Error::other)?;
    let (kind, replay) = read_frame(&mut stream)?;
    if kind != OUTPUT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a trix holder"));
    }
    stream.set_read_timeout(None)?;
    Ok(Attached { hello, replay, stream })
}

/// What the server sends a holder.
pub enum Req {
    Input(Cow<'static, [u8]>),
    Resize(u16, u16),
    Meta(String),
    Kill,
}

/// A thread that writes `Req`s to the holder in order, so the server's own
/// loop never waits on a socket. It ends after `Kill`, at the first failed
/// write, or when every sender is gone.
pub fn writer(mut stream: UnixStream) -> Sender<Req> {
    let (tx, rx) = mpsc::channel::<Req>();
    let _ = std::thread::Builder::new().name("trix-held-write".into()).spawn(move || {
        for req in rx {
            let res = match req {
                Req::Input(b) => write_frame(&mut stream, INPUT, &b),
                Req::Resize(c, r) => write_frame(&mut stream, RESIZE, &resize_payload(c, r)),
                Req::Meta(m) => write_frame(&mut stream, META, m.as_bytes()),
                Req::Kill => {
                    let _ = write_frame(&mut stream, KILL, &[]);
                    break;
                }
            };
            if res.is_err() {
                break;
            }
        }
    });
    tx
}

// ───────────────────────────── the holder ─────────────────────────────

/// `trix holder`: read a `Spec` on stdin, start its program on a new PTY,
/// say so on stdout, then serve it until told to stop (or until it ended
/// and no server came for it for an hour). The exit status of the process.
pub fn main() -> i32 {
    // Its own session: whatever happens to the server's group, or to a
    // terminal it once had, doesn't reach this one.
    unsafe { libc::setsid() };
    let say = |r: &Ready| {
        if let Ok(s) = serde_json::to_string(r) {
            let mut out = io::stdout();
            let _ = writeln!(out, "{s}");
            let _ = out.flush();
        }
    };
    let mut input = Vec::new();
    let spec = io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_slice::<Spec>(&input).map_err(|e| e.to_string()))
        .and_then(|s| crate::session::check_name(&s.session).map(|()| s));
    let spec = match spec {
        Ok(s) => s,
        Err(e) => {
            say(&Ready::Failed { error: format!("trix holder: {e}") });
            return 2;
        }
    };
    let path = socket_path(&spec.session, spec.id, std::process::id());
    let held = match Held::start(&spec, &path) {
        Ok(h) => h,
        Err(e) => {
            say(&Ready::Failed { error: e.to_string() });
            return 1;
        }
    };
    say(&Ready::Up { pid: held.pid });
    // Let go of the server's pipes; stderr stays, it's the server's log.
    if let Ok(null) = File::options().read(true).write(true).open("/dev/null") {
        unsafe {
            libc::dup2(null.as_raw_fd(), 0);
            libc::dup2(null.as_raw_fd(), 1);
        }
    }
    held.serve();
    0
}

struct Shared {
    session: String,
    id: u32,
    ring: Ring,
    /// The attached server, numbered so a reader that ended can tell
    /// whether it's still the current one.
    client: Option<(u64, UnixStream)>,
    attaches: u64,
    meta: String,
    size: (u16, u16),
    /// The program ended, with this code.
    exit: Option<Option<i32>>,
    /// When the last server went away.
    alone_since: Instant,
}

impl Shared {
    /// Send to the attached server; one that can't take it is let go.
    fn tell(&mut self, kind: u8, payload: &[u8]) {
        if let Some((_, c)) = &mut self.client
            && write_frame(c, kind, payload).is_err()
        {
            self.client = None;
            self.alone_since = Instant::now();
        }
    }
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Done {
    Killed,
    Exited,
}

/// A running holder: its program, and the threads serving it.
pub struct Held {
    pub pid: u32,
    path: PathBuf,
    shared: Arc<Mutex<Shared>>,
    done: mpsc::Receiver<Done>,
    stopping: Arc<AtomicBool>,
}

impl Held {
    /// Bind `path`, start the program, and serve it from threads of this
    /// process. Tests run holders this way, inside the test process.
    pub fn start(spec: &Spec, path: &Path) -> io::Result<Held> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let started = Self::spawn(spec, listener, path);
        if started.is_err() {
            let _ = std::fs::remove_file(path);
        }
        started
    }

    fn spawn(spec: &Spec, listener: UnixListener, path: &Path) -> io::Result<Held> {
        let (cols, rows) = (spec.cols.max(2), spec.rows.max(1));
        let opts = tty::Options {
            shell: Some(tty::Shell::new(spec.program.clone(), spec.args.clone())),
            working_directory: Some(spec.cwd.clone()),
            drain_on_exit: true,
            env: spec.env.clone(),
        };
        let pty = tty::new(&opts, crate::agent::window_size((cols, rows)), u64::from(spec.id))?;
        let pid = pty.child().id();
        let master = pty.file().try_clone();
        // The PTY stays open for as long as the holder runs. Dropping it
        // would hang up on the program and wait for it, so it never is: the
        // holder ends by exiting, and stops the program itself (`stop`).
        std::mem::forget(pty);
        let master = master?;
        // The emulator's PTY reads without blocking; the holder's threads
        // want to block.
        unsafe {
            let fd = master.as_raw_fd();
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) & !libc::O_NONBLOCK);
        }
        let shared = Arc::new(Mutex::new(Shared {
            session: spec.session.clone(),
            id: spec.id,
            ring: Ring::new(RING_BYTES),
            client: None,
            attaches: 0,
            meta: spec.meta.clone(),
            size: (cols, rows),
            exit: None,
            alone_since: Instant::now(),
        }));
        let (done_tx, done) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));

        // The program's output: kept, and passed on as it comes.
        let (drained_tx, drained) = mpsc::channel::<()>();
        let mut reader = master.try_clone()?;
        let sh = shared.clone();
        std::thread::Builder::new().name("trix-holder-read".into()).spawn(move || {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut s = lock(&sh);
                        s.ring.push(&buf[..n]);
                        s.tell(OUTPUT, &buf[..n]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    // EIO: nothing has the terminal open any more.
                    Err(_) => break,
                }
            }
            let _ = drained_tx.send(());
        })?;

        // Its end: the exit code, after the last of its output.
        let sh = shared.clone();
        let tx = done_tx.clone();
        std::thread::Builder::new().name("trix-holder-wait".into()).spawn(move || {
            let code = wait_for(pid);
            let _ = drained.recv_timeout(Duration::from_secs(1));
            let mut s = lock(&sh);
            s.exit = Some(code);
            s.tell(EXIT, &serde_json::to_vec(&code).unwrap_or_default());
            drop(s);
            let _ = tx.send(Done::Exited);
        })?;

        // Servers attaching.
        let sh = shared.clone();
        let stop = stopping.clone();
        std::thread::Builder::new().name("trix-holder-accept".into()).spawn(move || {
            for conn in listener.incoming() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(conn) = conn else {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                if let Some((n, from)) = welcome(&sh, conn, pid) {
                    let Ok(input) = master.try_clone() else { continue };
                    if n > 1 {
                        nudge(&sh, &master);
                    }
                    let (sh, tx) = (sh.clone(), done_tx.clone());
                    let _ = std::thread::Builder::new().name("trix-holder-client".into()).spawn(move || serve_client(n, from, input, pid, &sh, &tx));
                }
            }
        })?;

        Ok(Held { pid, path: path.to_path_buf(), shared, done, stopping })
    }

    /// Serve until told to stop, or until the program ended and no server
    /// came for its last screen for `LINGER`. Then stop serving.
    pub fn serve(self) {
        loop {
            match self.done.recv_timeout(Duration::from_secs(5)) {
                Ok(Done::Killed) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Done::Exited) | Err(RecvTimeoutError::Timeout) => {
                    let s = lock(&self.shared);
                    if s.exit.is_some() && s.client.is_none() && s.alone_since.elapsed() >= LINGER {
                        break;
                    }
                }
            }
        }
        self.finish();
    }

    /// Stop accepting, and take the socket away.
    fn finish(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        // Wake the accepting thread so it sees that.
        drop(UnixStream::connect(&self.path));
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A server just connected: take it over from the last, and tell it where
/// things are (`HELLO`, then the replay). Its number and the stream to read
/// it from, unless it went away meanwhile.
fn welcome(sh: &Mutex<Shared>, mut conn: UnixStream, pid: u32) -> Option<(u64, UnixStream)> {
    let _ = conn.set_write_timeout(Some(WRITE_TIMEOUT));
    let from = conn.try_clone().ok()?;
    let mut s = lock(sh);
    if let Some((_, old)) = s.client.take() {
        let _ = old.shutdown(std::net::Shutdown::Both);
    }
    let hello = Hello {
        pid,
        holder: std::process::id(),
        cols: s.size.0,
        rows: s.size.1,
        meta: s.meta.clone(),
        session: s.session.clone(),
        id: s.id,
        dropped: s.ring.dropped(),
        exited: s.exit.is_some(),
        code: s.exit.flatten(),
    };
    let hello = serde_json::to_vec(&hello).ok()?;
    if write_frame(&mut conn, HELLO, &hello).is_err() || write_frame(&mut conn, OUTPUT, &s.ring.replay()).is_err() {
        s.alone_since = Instant::now();
        return None;
    }
    s.attaches += 1;
    let n = s.attaches;
    s.client = Some((n, conn));
    Some((n, from))
}

/// A server took over from another: a full-screen program redraws all of
/// its screen for it, when told the terminal changed size. One row less for
/// a moment, then its size again (whatever it is by then: the new server
/// may have resized it meanwhile). Shells, on the main screen, are left be.
fn nudge(sh: &Arc<Mutex<Shared>>, master: &File) {
    let (cols, rows) = {
        let s = lock(sh);
        if s.exit.is_some() || s.ring.modes().alt.is_none() {
            return;
        }
        s.size
    };
    let Ok(pty) = master.try_clone() else { return };
    let sh = sh.clone();
    let _ = std::thread::Builder::new().name("trix-holder-nudge".into()).spawn(move || {
        set_size(&pty, cols, if rows > 1 { rows - 1 } else { rows + 1 });
        std::thread::sleep(Duration::from_millis(150));
        let (cols, rows) = lock(&sh).size;
        set_size(&pty, cols, rows);
    });
}

/// Do what server number `n` asks, until it goes.
fn serve_client(n: u64, mut from: UnixStream, mut input: File, pid: u32, sh: &Mutex<Shared>, done: &Sender<Done>) {
    loop {
        match read_frame(&mut from) {
            Ok((INPUT, bytes)) => {
                let _ = input.write_all(&bytes);
            }
            Ok((RESIZE, p)) => {
                if let Some((cols, rows)) = parse_resize(&p) {
                    set_size(&input, cols, rows);
                    lock(sh).size = (cols, rows);
                }
            }
            Ok((META, p)) => lock(sh).meta = String::from_utf8_lossy(&p).into_owned(),
            Ok((KILL, _)) => {
                stop(pid, sh);
                let _ = done.send(Done::Killed);
                return;
            }
            // From a newer trix, maybe: nothing to do with it.
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let mut s = lock(sh);
    if s.client.as_ref().is_some_and(|(c, _)| *c == n) {
        s.client = None;
        s.alone_since = Instant::now();
    }
}

/// Hang up on the program, as closing its terminal would, and see that it
/// stops (`platform::stop`: SIGTERM, then SIGKILL, for one that won't).
fn stop(pid: u32, sh: &Mutex<Shared>) {
    if lock(sh).exit.is_some() {
        return;
    }
    if let Ok(p) = libc::pid_t::try_from(pid) {
        // It leads its own process group (its terminal's session).
        unsafe {
            libc::kill(-p, libc::SIGHUP);
            libc::kill(p, libc::SIGHUP);
        }
    }
    crate::platform::stop(&[pid], crate::agent::STOP_GRACE);
}

fn set_size(pty: &File, cols: u16, rows: u16) {
    let ws = crate::agent::window_size((cols, rows));
    let win = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols.saturating_mul(ws.cell_width),
        ws_ypixel: rows.saturating_mul(ws.cell_height),
    };
    unsafe { libc::ioctl(pty.as_raw_fd(), libc::TIOCSWINSZ, &raw const win) };
}

/// Wait for our child `pid` to end: its exit code, or `None` for a signal.
fn wait_for(pid: u32) -> Option<i32> {
    let p = libc::pid_t::try_from(pid).ok()?;
    let mut status = 0;
    loop {
        let r = unsafe { libc::waitpid(p, &mut status, 0) };
        if r == p {
            break;
        }
        if r == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return None;
    }
    libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, INPUT, b"ls\r").unwrap();
        write_frame(&mut wire, RESIZE, &resize_payload(120, 40)).unwrap();
        write_frame(&mut wire, KILL, &[]).unwrap();
        let mut r = &wire[..];
        assert_eq!(read_frame(&mut r).unwrap(), (INPUT, b"ls\r".to_vec()));
        let (k, p) = read_frame(&mut r).unwrap();
        assert_eq!((k, parse_resize(&p)), (RESIZE, Some((120, 40))));
        assert_eq!(read_frame(&mut r).unwrap(), (KILL, vec![]));
        assert_eq!(read_frame(&mut r).unwrap_err().kind(), io::ErrorKind::UnexpectedEof, "nothing left");
    }

    #[test]
    fn a_frame_cut_short_is_an_error() {
        let wire = frame(OUTPUT, b"hello");
        assert!(read_frame(&mut &wire[..wire.len() - 1]).is_err());
        assert!(read_frame(&mut &wire[..3]).is_err(), "half a header");
    }

    #[test]
    fn huge_frames_are_refused_before_reading_them() {
        let mut head = vec![OUTPUT];
        head.extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        assert_eq!(read_frame(&mut &head[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut f = Frames::default();
        f.feed(&head);
        assert!(f.next_frame().is_err());
    }

    #[test]
    fn frames_arrive_in_pieces() {
        let mut wire = frame(OUTPUT, b"first");
        wire.extend(frame(EXIT, b"0"));
        wire.extend(frame(OUTPUT, b""));
        let mut f = Frames::default();
        let mut got = Vec::new();
        // One byte at a time: every split of a header and a payload.
        for b in &wire {
            f.feed(std::slice::from_ref(b));
            while let Some(fr) = f.next_frame().unwrap() {
                got.push(fr);
            }
        }
        assert_eq!(got, vec![(OUTPUT, b"first".to_vec()), (EXIT, b"0".to_vec()), (OUTPUT, vec![])]);
        // And all at once.
        f.feed(&wire);
        assert_eq!(f.next_frame().unwrap(), Some((OUTPUT, b"first".to_vec())));
        assert_eq!(f.next_frame().unwrap(), Some((EXIT, b"0".to_vec())));
        assert_eq!(f.next_frame().unwrap(), Some((OUTPUT, vec![])));
        assert_eq!(f.next_frame().unwrap(), None);
    }

    #[test]
    fn odd_resizes_are_ignored() {
        assert_eq!(parse_resize(&resize_payload(0, 10)), None);
        assert_eq!(parse_resize(&resize_payload(10, 0)), None);
        assert_eq!(parse_resize(&[1, 2, 3]), None);
        assert_eq!(parse_resize(&resize_payload(u16::MAX, 1)), Some((u16::MAX, 1)));
    }

    #[test]
    fn the_ring_keeps_the_newest_bytes() {
        let mut r = Ring::new(10);
        r.push(b"abc");
        r.push(b"def");
        assert_eq!(r.bytes(), b"abcdef");
        r.push(b"ghijkl");
        assert_eq!(r.bytes(), b"cdefghijkl", "the oldest went");
        assert_eq!(r.bytes().len(), 10);
        r.push(b"0123456789ABCDEF");
        assert_eq!(r.bytes(), b"6789ABCDEF", "a push bigger than it all");
        let mut none = Ring::new(0);
        none.push(b"anything");
        assert!(none.bytes().is_empty());
    }

    #[test]
    fn the_ring_starts_at_a_line_after_trimming() {
        let mut r = Ring::new(16);
        r.push(b"line one\nline two\nthree");
        // 23 bytes into 16: the cut lands in "line one", then moves past
        // the end of that line.
        assert_eq!(r.bytes(), b"line two\nthree", "not \"e\\nline two\\nthree\"");
        assert!(r.bytes().len() <= 16);
        // Nothing trimmed, nothing skipped.
        let mut r = Ring::new(64);
        r.push(b"partial line, kept whole");
        assert_eq!(r.bytes(), b"partial line, kept whole");
        // No newline near the cut: keep the bytes as they are.
        let mut r = Ring::new(8);
        r.push(b"abcdefghijkl");
        assert_eq!(r.bytes(), b"efghijkl");
    }

    #[test]
    fn socket_names_round_trip() {
        let h = |s: &str| crate::hooks::short_hash(s);
        let (d, w) = (h("default"), h("work-2_x"));
        assert_eq!(parse_name(&socket_name("default", 3, 4242)), Some((d.as_str(), 3, 4242)));
        assert_eq!(parse_name(&socket_name("work-2_x", 17, 1)), Some((w.as_str(), 17, 1)));
        assert_ne!(d, w);
        assert_eq!(parse_name(&format!("holder.{d}.3.sock")), None, "no pid");
        assert_eq!(parse_name(&format!("holder.{d}.3.4.5.sock")), None);
        assert_eq!(parse_name("holder.default.3.4.sock"), None, "a name, not a hash: from before");
        assert_eq!(parse_name("session.sock"), None);
        assert_eq!(parse_name("trix-12.sock"), None);
    }

    #[test]
    fn a_session_name_of_any_length_fits_a_socket() {
        let long = "s".repeat(200);
        let name = socket_name(&long, u32::MAX, u32::MAX);
        assert!(name.len() <= crate::hooks::MAX_SOCKET_NAME, "{} bytes: {name}", name.len());
        // Under a runtime folder as long as macOS's: still bindable.
        let dir = PathBuf::from(format!("/var/folders/zz/{}/T/trix-501", "x".repeat(28)));
        let path = dir.join(&name);
        assert!(path.as_os_str().len() < 104, "{}", path.display());
        assert_eq!(parse_name(&name).map(|(_, id, pid)| (id, pid)), Some((u32::MAX, u32::MAX)));
    }

    fn tmp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("trix-test-holder-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stale_sockets_are_swept_and_live_ones_found() {
        let (dir, short) = (tmp_dir(), tmp_dir());
        let mut dead = Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        let me = std::process::id();
        let long = "l".repeat(200);
        for name in [socket_name("default", 2, me), socket_name("default", 1, me), socket_name("work", 5, me), socket_name("default", 9, dead_pid), "unrelated.sock".into()] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        // The short folder too, where sockets go that the runtime folder is too long for.
        std::fs::write(short.join(socket_name("default", 3, me)), "").unwrap();
        std::fs::write(short.join(socket_name(&long, 4, me)), "").unwrap();
        let dirs = [dir.clone(), short.clone()];
        let ids: Vec<u32> = discover_in(&dirs, Some("default")).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, [1, 2, 3], "this session's, by id, in either folder");
        assert!(!dir.join(socket_name("default", 9, dead_pid)).exists(), "a dead holder's socket is removed");
        assert!(dir.join("unrelated.sock").exists());
        let ids: Vec<u32> = discover_in(&dirs, Some(&long)).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, [4], "a long session's");
        assert_eq!(discover_in(&dirs, None).len(), 5, "every session's");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(short);
    }

    /// A fresh emulator, fed `bytes`.
    fn emulate(bytes: &[u8], cols: usize, lines: usize) -> alacritty_terminal::Term<alacritty_terminal::event::VoidListener> {
        use alacritty_terminal::term::{Config, Term};
        let size = crate::agent::TermSize { cols, lines };
        let mut term = Term::new(Config::default(), &size, alacritty_terminal::event::VoidListener);
        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    fn screen(term: &alacritty_terminal::Term<alacritty_terminal::event::VoidListener>) -> String {
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Line};
        let grid = term.grid();
        (0..term.screen_lines() as i32)
            .map(|l| (0..term.columns()).map(|c| grid[Line(l)][Column(c)].c).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_replay_restores_the_alternate_screen_after_its_setup_fell_off() {
        use alacritty_terminal::term::TermMode;
        let mut r = Ring::new(4096);
        // A shell's history, then a full-screen program sets up...
        r.push(b"$ ls\nfile-a file-b\n$ top\n");
        r.push(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?25l\x1b[?1000h\x1b[?1006h");
        // ...and repaints, over and over, much more than is kept.
        for i in 0..400 {
            r.push(format!("\x1b[H\x1b[2Jframe {i}\r\nload {}\r\n", i * 3).as_bytes());
        }
        assert!(r.dropped() > 0);
        assert!(!r.bytes().windows(8).any(|w| w == b"\x1b[?1049h"), "the setup is gone from the buffer");
        assert!(r.bytes().starts_with(b"\x1b[H\x1b[2J") || r.bytes().starts_with(b"\x1b[2J"), "it starts at a whole screen: {:?}", String::from_utf8_lossy(&r.bytes()[..20]));
        let term = emulate(&r.replay(), 40, 10);
        let mode = *term.mode();
        assert!(mode.contains(TermMode::ALT_SCREEN), "on the alternate screen");
        assert!(mode.contains(TermMode::APP_CURSOR) && mode.contains(TermMode::APP_KEYPAD));
        assert!(!mode.contains(TermMode::SHOW_CURSOR), "cursor still hidden");
        assert!(mode.contains(TermMode::MOUSE_REPORT_CLICK) && mode.contains(TermMode::SGR_MOUSE));
        let shown = screen(&term);
        assert!(shown.starts_with("frame 399\nload 1197"), "{shown:?}");
        assert!(!shown.contains("file-a"), "no shell history on its screen");

        // Without the preamble, the same bytes land on the main screen.
        let bare = emulate(&r.bytes(), 40, 10);
        assert!(!bare.mode().contains(TermMode::ALT_SCREEN));

        // Once it quits, the main screen is back, and so are the modes.
        r.push(b"\x1b[?1049l\x1b[?1l\x1b>\x1b[?25h\x1b[?1000l\x1b[?1006l$ ");
        let term = emulate(&r.replay(), 40, 10);
        assert!(!term.mode().contains(TermMode::ALT_SCREEN) && term.mode().contains(TermMode::SHOW_CURSOR));
        assert!(!term.mode().contains(TermMode::APP_CURSOR));
    }

    #[test]
    fn the_cut_never_lands_inside_a_sequence() {
        let mut r = Ring::new(64);
        // Long titles: strings that would straddle any cut.
        for i in 0..50 {
            r.push(format!("\x1b]0;a long window title number {i}\x07x").as_bytes());
        }
        let kept = r.bytes();
        assert!(kept.starts_with(b"\x1b]") || kept.starts_with(b"x"), "{:?}", String::from_utf8_lossy(&kept));
        // Split mode sequences are followed whole across pushes and cuts.
        let mut r = Ring::new(32);
        r.push(b"\x1b[?10");
        r.push(b"49h");
        for _ in 0..20 {
            r.push(b"0123456789");
        }
        assert_eq!(r.modes().alt, Some(1049));
        assert!(r.replay().starts_with(b"\x1b[?1049h"));
    }

    pub(crate) fn spec(program: &str, args: &[&str]) -> Spec {
        Spec {
            session: "test".into(),
            id: 1,
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: std::env::temp_dir(),
            env: HashMap::new(),
            cols: 80,
            rows: 24,
            meta: r#"{"name":"t"}"#.into(),
        }
    }

    /// Read frames until `want` says stop, or 5 seconds pass.
    fn read_until(s: &mut UnixStream, mut want: impl FnMut(u8, &[u8]) -> bool) -> bool {
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut frames = Frames::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline {
            match s.read(&mut buf) {
                Ok(0) => return false,
                Ok(n) => frames.feed(&buf[..n]),
                Err(_) => {}
            }
            while let Some((k, p)) = frames.next_frame().unwrap() {
                if want(k, &p) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn a_holder_replays_to_the_next_server_and_survives_the_last() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        let held = Held::start(&spec("/bin/sh", &["-c", "echo started; while read l; do echo \"got $l\"; done"]), &path).unwrap();
        let pid = held.pid;
        let shared = held.shared.clone();
        let serving = std::thread::spawn(move || held.serve());

        let mut first = attach(&path).unwrap();
        assert_eq!(first.hello.pid, pid);
        assert_eq!((first.hello.cols, first.hello.rows), (80, 24));
        assert_eq!(first.hello.meta, r#"{"name":"t"}"#);
        assert_eq!((first.hello.session.as_str(), first.hello.id, first.hello.dropped), ("test", 1, 0));
        assert!(!first.hello.exited);
        let mut seen = String::from_utf8_lossy(&first.replay).into_owned();
        assert!(seen.contains("started") || read_until(&mut first.stream, |k, p| {
            if k == OUTPUT {
                seen.push_str(&String::from_utf8_lossy(p));
            }
            seen.contains("started")
        }));
        write_frame(&mut first.stream, INPUT, b"one\n").unwrap();
        write_frame(&mut first.stream, META, b"{\"name\":\"renamed\"}").unwrap();
        write_frame(&mut first.stream, RESIZE, &resize_payload(100, 30)).unwrap();
        assert!(read_until(&mut first.stream, |k, p| k == OUTPUT && String::from_utf8_lossy(p).contains("got one")));

        // The server goes, the way a crash goes: no goodbye.
        drop(first);
        assert!(crate::platform::alive(pid), "the program outlives its server");

        let mut second = attach(&path).unwrap();
        let replay = String::from_utf8_lossy(&second.replay).into_owned();
        assert!(replay.contains("started") && replay.contains("got one"), "replayed: {replay:?}");
        assert_eq!(second.hello.meta, "{\"name\":\"renamed\"}");
        assert_eq!((second.hello.cols, second.hello.rows), (100, 30));
        write_frame(&mut second.stream, INPUT, b"two\n").unwrap();
        assert!(read_until(&mut second.stream, |k, p| k == OUTPUT && String::from_utf8_lossy(p).contains("got two")));

        // A third takes over from the second, which is let go.
        let third = attach(&path).unwrap();
        let mut gone = [0u8; 1];
        second.stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(second.stream.read(&mut gone), Ok(0) | Err(_)), "the old server is hung up on");

        let tx = writer(third.stream.try_clone().unwrap());
        tx.send(Req::Kill).unwrap();
        serving.join().unwrap();
        assert!(!crate::platform::alive(pid), "killed means the program too");
        assert!(lock(&shared).exit.is_some() || !crate::platform::alive(pid));
        assert!(!path.exists(), "the socket goes with it");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_full_screen_program_is_told_to_redraw_for_a_new_server() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        let script = "trap 'echo winch $(stty size)' WINCH; printf '\\033[?1049hready\\n'; while :; do sleep 0.05; done";
        let held = Held::start(&spec("bash", &["--norc", "--noprofile", "-c", script]), &path).unwrap();
        let serving = std::thread::spawn(move || held.serve());
        let mut first = attach(&path).unwrap();
        let mut seen = String::from_utf8_lossy(&first.replay).into_owned();
        assert!(seen.contains("ready") || read_until(&mut first.stream, |_, p| {
            seen.push_str(&String::from_utf8_lossy(p));
            seen.contains("ready")
        }));
        assert!(!seen.contains("winch"), "the first server isn't a takeover");
        let mut second = attach(&path).unwrap();
        let mut told = String::new();
        assert!(
            read_until(&mut second.stream, |k, p| {
                if k == OUTPUT {
                    told.push_str(&String::from_utf8_lossy(p));
                }
                told.contains("winch 24 80")
            }),
            "the size wiggled, so it redraws, and is back where it was: {told:?}"
        );
        assert!(told.contains("winch 23 80"), "{told:?}");
        write_frame(&mut second.stream, KILL, &[]).unwrap();
        serving.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_ended_program_is_kept_for_the_next_server() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        let held = Held::start(&spec("/bin/sh", &["-c", "echo bye; exit 3"]), &path).unwrap();
        let serving = std::thread::spawn(move || held.serve());
        let deadline = Instant::now() + Duration::from_secs(5);
        let att = loop {
            let att = attach(&path).unwrap();
            if att.hello.exited || Instant::now() > deadline {
                break att;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(att.hello.exited);
        assert_eq!(att.hello.code, Some(3));
        assert!(String::from_utf8_lossy(&att.replay).contains("bye"));
        let tx = writer(att.stream);
        tx.send(Req::Kill).unwrap();
        serving.join().unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_program_that_cant_start_leaves_no_socket() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        assert!(Held::start(&spec("/no/such/program", &[]), &path).is_err());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_attached_server_hears_the_exit() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        let held = Held::start(&spec("/bin/sh", &["-c", "read l; exit 7"]), &path).unwrap();
        let serving = std::thread::spawn(move || held.serve());
        let mut att = attach(&path).unwrap();
        write_frame(&mut att.stream, INPUT, b"\n").unwrap();
        let mut code = None;
        assert!(read_until(&mut att.stream, |k, p| {
            if k == EXIT {
                code = serde_json::from_slice::<Option<i32>>(p).ok().flatten();
            }
            k == EXIT
        }));
        assert_eq!(code, Some(7));
        write_frame(&mut att.stream, KILL, &[]).unwrap();
        serving.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
