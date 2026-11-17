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
//! a new connection takes over from the last, which is told `BYE` first, so
//! it knows it was replaced rather than dropped. A server that stops reading
//! is hung up on without one, and may attach again.
//!
//! A holder that can't write to its server within `WRITE_TIMEOUT` hangs up
//! on it; the server attaches again (`agent::read_holder`). Input for the
//! program goes through a queue and a thread of its own, so a program that
//! stopped reading never keeps a `KILL` from being heard.
//!
//! `Hello` carries `PROTOCOL`, since holders outlive upgrades: a server
//! attaches to holders that speak its version or an older one, and leaves
//! newer ones be. That a holder greets with `HELLO` first, and stops on
//! `KILL`, never changes.
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
pub const BYE: u8 = 13;

/// What holders and servers speak. Bumped when either side would misread
/// the other; `Hello` from a holder before versions says 0, and is read as 1
/// (nothing changed but the field).
pub const PROTOCOL: u32 = 1;

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
/// Input the program hasn't read yet, at most: a program that stopped
/// reading loses keystrokes past this, rather than stalling the holder.
const INPUT_BACKLOG: usize = 1 << 20;
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
    /// Where clears of the screen start, as offsets into all the output
    /// ever pushed, oldest first: found once, as bytes come in, so a cut on
    /// the alternate screen needn't search for one. At most one per
    /// `CLEAR_GAP` bytes, which bounds the list.
    clears: VecDeque<u64>,
    /// The last bytes pushed, for a clear split across two pushes.
    tail: [u8; 3],
}

/// How far past the cut a clear of the alternate screen is looked for.
const CLEAR_SEARCH: usize = 256 << 10;
/// How far the cut moves on to get out of an escape sequence or a string.
const GROUND_SEARCH: usize = 64 << 10;
/// Clears this close after the last one noted aren't noted: a cut may then
/// move on that much further than it had to.
const CLEAR_GAP: u64 = 4 << 10;
const CLEAR: &[u8; 4] = b"\x1b[2J";

impl Ring {
    pub fn new(cap: usize) -> Ring {
        Ring { buf: VecDeque::new(), cap, base: Tracker::default(), now: Tracker::default(), dropped: 0, clears: VecDeque::new(), tail: [0; 3] }
    }

    /// Offset of the first byte kept, into all the output ever pushed.
    fn start(&self) -> u64 {
        self.dropped
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.now.feed(bytes);
        self.note_clears(bytes);
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

    /// Note where in `bytes`, about to be pushed, the screen is cleared.
    fn note_clears(&mut self, bytes: &[u8]) {
        let at = self.dropped + self.buf.len() as u64;
        // `tail` then `bytes`, without copying them together.
        let tail = self.tail;
        let byte = |i: usize| if i < 3 { tail[i] } else { bytes[i - 3] };
        let n = bytes.len() + 3;
        let mut i = 0;
        while i + 4 <= n {
            if (0..4).all(|k| byte(i + k) == CLEAR[k]) {
                // Where it starts, as an offset (the tail's bytes are before `at`).
                let pos = (at + i as u64).saturating_sub(3);
                if self.clears.back().is_none_or(|&last| pos >= last + CLEAR_GAP) {
                    self.clears.push_back(pos);
                }
                i += 4;
            } else {
                i += 1;
            }
        }
        self.tail = [byte(n - 3), byte(n - 2), byte(n - 1)];
    }

    /// Move the cut on to a better place to start a replay.
    fn settle(&mut self) {
        let near = self.buf.len().min(LINE_SEARCH);
        if let Some(nl) = self.buf.range(..near).position(|&b| b == b'\n') {
            self.take(nl + 1);
        }
        if !self.base.ground() {
            // Byte by byte until out of the sequence: followed as they go,
            // dropped all at once.
            let mut n = 0;
            for &b in self.buf.iter().take(GROUND_SEARCH) {
                self.base.feed(&[b]);
                n += 1;
                if self.base.ground() {
                    break;
                }
            }
            self.buf.drain(..n);
            self.dropped += n as u64;
        }
        let start = self.start();
        while self.clears.front().is_some_and(|&c| c < start) {
            self.clears.pop_front();
        }
        if self.base.ground()
            && self.base.modes().alt.is_some()
            && let Some(&c) = self.clears.front()
            && c > start
            && c - start <= CLEAR_SEARCH as u64
        {
            self.take((c - start) as usize);
        }
    }

    /// Drop the oldest `n` bytes, following them.
    fn take(&mut self, n: usize) {
        let n = n.min(self.buf.len());
        let (a, b) = self.buf.as_slices();
        let in_a = n.min(a.len());
        self.base.feed(&a[..in_a]);
        self.base.feed(&b[..n - in_a]);
        self.buf.drain(..n);
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
    /// What it speaks (`PROTOCOL`); 0 from a holder before versions.
    #[serde(default)]
    pub version: u32,
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
    /// How long since the program last wrote anything, in milliseconds.
    #[serde(default)]
    pub quiet_ms: u64,
}

impl Hello {
    /// What it speaks, counting a holder from before versions as 1.
    pub fn protocol(&self) -> u32 {
        self.version.max(1)
    }

    /// Whether this trix can serve it: it speaks this version or an older
    /// one. A newer holder is left running for the trix that started it.
    pub fn compatible(&self) -> bool {
        self.protocol() <= PROTOCOL
    }
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
                // Stopped whether or not it hears it (`see_stopped`).
                let _ = write_frame(&mut att.stream, KILL, &[]);
                holders.push(((att.hello.holder, att.hello.pid), att.stream));
            }
            // Alive by its pid, but nobody listens: the pid is someone else's.
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
    let held: Vec<(u32, u32)> = holders.iter().map(|(p, _)| *p).collect();
    see_stopped(&held, crate::agent::HOLDER_GRACE);
    held.len()
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
    launch_within(exe, spec, Duration::from_secs(10))
}

/// `launch`, giving the holder `limit` to say it's up. One that doesn't is
/// stopped, so it can't start the program after the server gave up on it.
fn launch_within(exe: &Path, spec: &Spec, limit: Duration) -> Result<(PathBuf, u32), LaunchError> {
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
    let path = socket_path(&spec.session, spec.id, holder);
    let Ok(line) = rx.recv_timeout(limit) else {
        abandon(holder, &path);
        return Err(bad(&"the holder did not start in time"));
    };
    match serde_json::from_str::<Ready>(line.trim()) {
        Ok(Ready::Up { .. }) => Ok((path, holder)),
        Ok(Ready::Failed { error }) => Err(LaunchError::Program(error)),
        Err(_) => {
            abandon(holder, &path);
            Err(bad(&"the holder did not start"))
        }
    }
}

/// A holder the server gave up on: killed, and its socket removed. A
/// program it started by then is hung up on as its PTY closes.
pub fn abandon(holder: u32, path: &Path) {
    if holder == std::process::id() {
        return;
    }
    if let Ok(p) = libc::pid_t::try_from(holder) {
        unsafe { libc::kill(p, libc::SIGKILL) };
    }
    let _ = std::fs::remove_file(path);
}

/// Make sure holders told to stop (`KILL`) did, with their programs: those
/// still up after `limit` are made to, holder and program group both. Each
/// is (holder pid, program pid). The server mustn't leave one running that
/// couldn't take its `KILL`, or the next server would put the agent back.
pub fn see_stopped(held: &[(u32, u32)], limit: Duration) {
    let me = std::process::id();
    // A holder in this very process (tests run them so) is never waited on.
    let holders: Vec<u32> = held.iter().map(|&(h, _)| h).filter(|&h| h != me).collect();
    wait_gone(&holders, limit);
    let stuck: Vec<(u32, u32)> = held.iter().copied().filter(|&(h, p)| h != me && p != me && crate::platform::alive(h)).collect();
    if stuck.is_empty() {
        return;
    }
    eprintln!("trix: {} holder(s) did not stop when told; stopping them", stuck.len());
    let signal_all = |sig: libc::c_int| {
        for &(h, pid) in &stuck {
            for (p, group) in [(pid, true), (h, false)] {
                let Ok(p) = libc::pid_t::try_from(p) else { continue };
                if p <= 0 {
                    continue;
                }
                unsafe {
                    // The program leads its own group (its terminal's session).
                    if group && libc::getpgid(p) == p {
                        libc::kill(-p, sig);
                    }
                    libc::kill(p, sig);
                }
            }
        }
    };
    let pids: Vec<u32> = stuck.iter().flat_map(|&(h, p)| [h, p]).collect();
    signal_all(libc::SIGTERM);
    wait_gone(&pids, crate::agent::STOP_GRACE);
    signal_all(libc::SIGKILL);
    wait_gone(&pids, Duration::from_secs(1));
}

/// A server's line to one holder, just attached.
pub struct Attached {
    pub hello: Hello,
    /// The output it kept, oldest first.
    pub replay: Vec<u8>,
    pub stream: UnixStream,
    /// Its socket, to attach again.
    pub path: PathBuf,
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
    Ok(Attached { hello, replay, stream, path: path.to_path_buf() })
}

/// What the server sends a holder.
pub enum Req {
    Input(Cow<'static, [u8]>),
    Resize(u16, u16),
    Meta(String),
    Kill,
    /// Attached again (the holder had let this server go): write here now.
    Stream(UnixStream),
}

/// A thread that writes `Req`s to the holder in order, so the server's own
/// loop never waits on a socket. After a failed write it keeps what comes
/// (input up to `HELD_BACK`) until a new `Stream`, and sends it there; it
/// ends after `Kill`, or when every sender is gone. `killed` says a `Kill`
/// was sent: the holder's going away isn't news then.
pub fn writer(stream: UnixStream, killed: Arc<AtomicBool>) -> Sender<Req> {
    const HELD_BACK: usize = 64 << 10;
    let (tx, rx) = mpsc::channel::<Req>();
    let _ = std::thread::Builder::new().name("trix-held-write".into()).spawn(move || {
        let send = |s: &mut UnixStream, req: &Req| match req {
            Req::Input(b) => write_frame(s, INPUT, b),
            Req::Resize(c, r) => write_frame(s, RESIZE, &resize_payload(*c, *r)),
            Req::Meta(m) => write_frame(s, META, m.as_bytes()),
            Req::Kill => write_frame(s, KILL, &[]),
            Req::Stream(_) => Ok(()),
        };
        let mut stream = Some(stream);
        // What couldn't be sent, for the next stream.
        let mut waiting: Vec<Req> = Vec::new();
        let mut waiting_input = 0;
        for req in rx {
            if let Req::Stream(mut s) = req {
                let ok = waiting.drain(..).all(|r| send(&mut s, &r).is_ok());
                waiting_input = 0;
                stream = ok.then_some(s);
                continue;
            }
            let kill = matches!(req, Req::Kill);
            if kill {
                killed.store(true, Ordering::SeqCst);
            }
            if let Some(s) = &mut stream {
                if send(s, &req).is_ok() {
                    if kill {
                        break;
                    }
                    continue;
                }
                stream = None;
            }
            if kill {
                break;
            }
            match &req {
                Req::Input(b) if waiting_input + b.len() > HELD_BACK => continue,
                Req::Input(b) => waiting_input += b.len(),
                _ => {}
            }
            waiting.push(req);
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
    /// When the program last wrote anything.
    last_output: Instant,
    /// How long a server may keep a write waiting before it's let go.
    write_timeout: Duration,
}

impl Shared {
    /// Send to the attached server; one that can't take it is let go, and
    /// hung up on, so it hears that (and may attach again) rather than
    /// waiting on a connection nobody serves.
    fn tell(&mut self, kind: u8, payload: &[u8]) {
        if let Some((_, c)) = &mut self.client
            && write_frame(c, kind, payload).is_err()
        {
            if let Some((_, c)) = self.client.take() {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
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
        Self::start_with(spec, path, WRITE_TIMEOUT)
    }

    /// `start`, letting a server that stops reading go after `write_timeout`.
    pub(crate) fn start_with(spec: &Spec, path: &Path, write_timeout: Duration) -> io::Result<Held> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let started = Self::spawn(spec, listener, path, write_timeout);
        if started.is_err() {
            let _ = std::fs::remove_file(path);
        }
        started
    }

    fn spawn(spec: &Spec, listener: UnixListener, path: &Path, write_timeout: Duration) -> io::Result<Held> {
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
            last_output: Instant::now(),
            write_timeout,
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
                        s.last_output = Instant::now();
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

        // Input for the program, written on a thread of its own: a program
        // that stopped reading blocks that one, never the server's commands
        // (a `KILL` above all).
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let backlog = Arc::new(AtomicUsize::new(0));
        let mut input = master.try_clone()?;
        let pending = backlog.clone();
        std::thread::Builder::new().name("trix-holder-input".into()).spawn(move || {
            for bytes in input_rx {
                let _ = input.write_all(&bytes);
                pending.fetch_sub(bytes.len(), Ordering::Relaxed);
            }
        })?;
        let input = Input { tx: input_tx, backlog };

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
                    let Ok(pty) = master.try_clone() else { continue };
                    if n > 1 {
                        nudge(&sh, &master);
                    }
                    let (sh, tx, input) = (sh.clone(), done_tx.clone(), input.clone());
                    let _ = std::thread::Builder::new().name("trix-holder-client".into()).spawn(move || serve_client(n, from, &input, &pty, pid, &sh, &tx));
                }
            }
        })?;

        Ok(Held { pid, path: path.to_path_buf(), shared, done, stopping })
    }

    /// How many times a server has attached, as it goes: for tests.
    #[cfg(test)]
    pub(crate) fn attaches(&self) -> impl Fn() -> u64 + use<> {
        let sh = self.shared.clone();
        move || lock(&sh).attaches
    }

    /// The server's notes it has, as they go: for tests.
    #[cfg(test)]
    pub(crate) fn notes(&self) -> impl Fn() -> String + use<> {
        let sh = self.shared.clone();
        move || lock(&sh).meta.clone()
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
    let mut s = lock(sh);
    let _ = conn.set_write_timeout(Some(s.write_timeout));
    let from = conn.try_clone().ok()?;
    if let Some((_, mut old)) = s.client.take() {
        // Told why, so it doesn't attach again and take it back.
        let _ = old.set_write_timeout(Some(Duration::from_millis(200)));
        let _ = write_frame(&mut old, BYE, &[]);
        let _ = old.shutdown(std::net::Shutdown::Both);
    }
    let hello = Hello {
        version: PROTOCOL,
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
        quiet_ms: u64::try_from(s.last_output.elapsed().as_millis()).unwrap_or(u64::MAX),
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

/// The queue to the thread writing the program's input, and how much is
/// waiting in it.
#[derive(Clone)]
struct Input {
    tx: Sender<Vec<u8>>,
    backlog: Arc<AtomicUsize>,
}

impl Input {
    /// Queue `bytes` for the program, unless it has stopped reading and
    /// `INPUT_BACKLOG` is waiting already. Whether they were queued.
    fn send(&self, bytes: Vec<u8>) -> bool {
        let n = bytes.len();
        if self.backlog.fetch_add(n, Ordering::Relaxed) + n > INPUT_BACKLOG {
            self.backlog.fetch_sub(n, Ordering::Relaxed);
            return false;
        }
        if self.tx.send(bytes).is_err() {
            self.backlog.fetch_sub(n, Ordering::Relaxed);
            return false;
        }
        true
    }
}

/// Do what server number `n` asks, until it goes.
fn serve_client(n: u64, mut from: UnixStream, input: &Input, pty: &File, pid: u32, sh: &Mutex<Shared>, done: &Sender<Done>) {
    let mut refused = false;
    loop {
        match read_frame(&mut from) {
            Ok((INPUT, bytes)) => {
                // Said once per server, not per keystroke.
                if !input.send(bytes) && !std::mem::replace(&mut refused, true) {
                    eprintln!("trix holder: the program isn't reading its input; dropping what's typed until it does");
                }
            }
            Ok((RESIZE, p)) => {
                if let Some((cols, rows)) = parse_resize(&p) {
                    set_size(pty, cols, rows);
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

    #[test]
    fn a_clear_is_found_once_even_split_across_pushes() {
        let mut r = Ring::new(1 << 20);
        r.push(b"\x1b[?1049hsetup");
        r.push(b"text\x1b[");
        r.push(b"2Jframe one");
        assert_eq!(r.clears, [17], "where the split clear starts");
        // Clears closer than CLEAR_GAP to the last noted aren't noted.
        r.push(b"\x1b[2Jframe two");
        assert_eq!(r.clears.len(), 1);
        r.push(&vec![b'x'; CLEAR_GAP as usize]);
        r.push(b"\x1b[2Jframe three");
        assert_eq!(r.clears.len(), 2);
    }

    #[test]
    fn trimming_on_the_alternate_screen_starts_at_a_clear_without_searching() {
        let mut r = Ring::new(64 << 10);
        r.push(b"\x1b[?1049h");
        let frame = |n: usize| {
            let mut f = format!("\x1b[H\x1b[2Jframe {n}\r\n").into_bytes();
            f.extend(std::iter::repeat_n(b'.', 6000));
            f
        };
        for n in 0..200 {
            r.push(&frame(n));
        }
        let kept = r.bytes();
        assert!(kept.starts_with(b"\x1b[2Jframe "), "{:?}", String::from_utf8_lossy(&kept[..40]));
        assert!(r.clears.len() <= 64 << 10 >> 12, "a short list: {}", r.clears.len());
        assert!(r.clears.iter().all(|&c| c >= r.start()), "none from before the cut");
        let replay = r.replay();
        assert!(replay.starts_with(b"\x1b[?1049h"), "the preamble puts the screen back");
    }

    #[test]
    fn holders_greet_with_their_version() {
        let hello = |version: u32| Hello { version, pid: 1, holder: 2, cols: 80, rows: 24, meta: String::new(), session: String::new(), id: 1, dropped: 0, exited: false, code: None, quiet_ms: 0 };
        assert!(hello(0).compatible() && hello(0).protocol() == 1, "from before versions");
        assert!(hello(PROTOCOL).compatible());
        assert!(!hello(PROTOCOL + 1).compatible(), "a newer holder is left be");
        // One from before versions reads as version 0.
        let old: Hello = serde_json::from_str(r#"{"pid":1,"holder":2,"cols":80,"rows":24,"meta":""}"#).unwrap();
        assert_eq!(old.version, 0);
    }

    /// Poll `f` until it holds, or `limit` passes. Whether it held.
    fn eventually(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if f() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_server_that_stops_reading_is_hung_up_on() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        let held = Held::start_with(&spec("/bin/sh", &["-c", "yes flood-flood-flood | head -c 20000000; while read l; do echo \"got $l\"; done"]), &path, Duration::from_millis(200)).unwrap();
        let (pid, shared) = (held.pid, held.shared.clone());
        let serving = std::thread::spawn(move || held.serve());
        // Attached, and then never read: it falls behind the flood.
        let mut stalled = attach(&path).unwrap();
        assert!(eventually(Duration::from_secs(10), || lock(&shared).client.is_none()), "let go");
        let (said, ended) = drain(&mut stalled.stream);
        assert!(ended, "and hung up on, so it hears it");
        assert!(!said.contains(&BYE), "not told it was replaced: it may attach again");
        assert!(crate::platform::alive(pid), "the program never noticed");
        // Attaching again works (once the flood is over, which this one
        // wouldn't keep up with either), and so does typing.
        assert!(eventually(Duration::from_secs(10), || lock(&shared).last_output.elapsed() > Duration::from_millis(300)), "the flood is over");
        let mut again = attach(&path).unwrap();
        assert!(!again.hello.exited);
        assert!(again.hello.dropped > 0, "it kept the newest of it");
        write_frame(&mut again.stream, INPUT, b"ping\n").unwrap();
        assert!(read_until(&mut again.stream, |k, p| k == OUTPUT && String::from_utf8_lossy(p).contains("got ping")));
        write_frame(&mut again.stream, KILL, &[]).unwrap();
        serving.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_kill_is_heard_while_the_program_ignores_its_input() {
        let dir = tmp_dir();
        let path = dir.join("h.sock");
        let held = Held::start(&spec("/bin/sh", &["-c", "exec sleep 30"]), &path).unwrap();
        let pid = held.pid;
        let (done_tx, done) = mpsc::channel();
        std::thread::spawn(move || {
            held.serve();
            let _ = done_tx.send(());
        });
        let mut att = attach(&path).unwrap();
        att.stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        // Far more than a PTY buffers, to a program that never reads it.
        let chunk = vec![b'k'; 64 << 10];
        for _ in 0..40 {
            write_frame(&mut att.stream, INPUT, &chunk).expect("the holder keeps reading its server");
        }
        write_frame(&mut att.stream, KILL, &[]).unwrap();
        assert!(done.recv_timeout(Duration::from_secs(10)).is_ok(), "the KILL was heard");
        assert!(eventually(Duration::from_secs(2), || !crate::platform::alive(pid)), "and the program stopped");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_holder_too_slow_to_start_is_stopped() {
        let dir = tmp_dir();
        let pidfile = dir.join("pid");
        // A "holder" that never says it's up.
        let exe = dir.join("slow-holder");
        std::fs::write(&exe, format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", pidfile.display())).unwrap();
        std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let res = launch_within(&exe, &spec("/bin/true", &[]), Duration::from_millis(500));
        assert!(matches!(res, Err(LaunchError::Holder(_))), "the server runs the program some other way");
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(eventually(Duration::from_secs(2), || pidfile.exists()), "it did start");
        let pid: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(eventually(Duration::from_secs(3), || !crate::platform::alive(pid)), "and was stopped, so it can't start the program too");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn holders_that_dont_stop_are_made_to() {
        use std::os::unix::process::CommandExt;
        // A holder that ignores everything polite, and its program in a
        // process group of its own, as a program in a PTY is.
        let holder = Command::new("/bin/sh").args(["-c", "trap '' TERM HUP; while :; do sleep 0.05; done"]).spawn().unwrap();
        let program = Command::new("/bin/sh").args(["-c", "trap '' TERM HUP; while :; do sleep 0.05; done"]).process_group(0).spawn().unwrap();
        let (h, p) = (holder.id(), program.id());
        crate::platform::reap(holder);
        crate::platform::reap(program);
        let started = Instant::now();
        see_stopped(&[(h, p)], Duration::from_millis(200));
        assert!(eventually(Duration::from_secs(2), || !crate::platform::alive(h) && !crate::platform::alive(p)), "both made to stop");
        assert!(started.elapsed() < crate::agent::STOP_GRACE + Duration::from_secs(4));
        // Its own process is never one of them (tests hold holders in it).
        see_stopped(&[(std::process::id(), std::process::id())], Duration::from_millis(10));
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

    /// Read `s` to its end (or 5 seconds' silence): the kinds of the frames
    /// it carried, and whether it ended.
    fn drain(s: &mut UnixStream) -> (Vec<u8>, bool) {
        // macOS refuses socket options on a socket the other end already
        // shut down (EINVAL), which is itself the hang-up.
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let mut frames = Frames::default();
        let mut kinds = Vec::new();
        let mut buf = [0u8; 64 << 10];
        let ended = loop {
            match s.read(&mut buf) {
                Ok(0) => break true,
                Ok(n) => frames.feed(&buf[..n]),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break false,
                Err(_) => break true,
            }
            while let Ok(Some((k, _))) = frames.next_frame() {
                if kinds.last() != Some(&k) {
                    kinds.push(k);
                }
            }
        };
        (kinds, ended)
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
        let (said, hung_up) = drain(&mut second.stream);
        assert_eq!(said, vec![BYE], "the old server is told why");
        assert!(hung_up, "and hung up on");

        let tx = writer(third.stream.try_clone().unwrap(), Arc::default());
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
        let tx = writer(att.stream, Arc::default());
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
