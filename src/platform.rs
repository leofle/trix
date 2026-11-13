//! What differs between Linux and macOS, in one place: process liveness and
//! working folders, the terminal's foreground job, and the desktop
//! (notifications, sound, clipboard). Everything else in trix is plain Unix.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Is there a process with this pid? (Ours or not: `EPERM` still means it
/// exists.)
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    if pid <= 0 {
        return false;
    }
    let ok = unsafe { libc::kill(pid, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Open files the server asks for at most. macOS refuses more than 10240
/// (`OPEN_MAX`) however high the hard limit is, and that is about 2000 agents.
const FD_WANTED: libc::rlim_t = 10240;

/// The soft open-file limit to ask for: `FD_WANTED`, kept within the hard
/// limit, and never lower than what is set now.
fn fd_target(soft: libc::rlim_t, hard: libc::rlim_t) -> libc::rlim_t {
    FD_WANTED.min(hard).max(soft)
}

/// Raise the soft limit on open files. Each agent needs about five (the
/// terminal, its pipes, a socket and the log), and a shell inherits the
/// usual macOS default of 256, which runs out at around 50 agents. Returns
/// the limit now in force, or `None` if it can't be read.
pub fn raise_fd_limit() -> Option<libc::rlim_t> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return None;
    }
    let want = fd_target(lim.rlim_cur, lim.rlim_max);
    if want > lim.rlim_cur {
        let raised = libc::rlimit { rlim_cur: want, rlim_max: lim.rlim_max };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            return Some(want);
        }
    }
    Some(lim.rlim_cur)
}

/// The process group in the foreground of the terminal behind `pty` (the
/// master side), or `None` if it can't be told.
pub fn foreground_group(pty: &impl AsRawFd) -> Option<i32> {
    let g = unsafe { libc::tcgetpgrp(pty.as_raw_fd()) };
    (g > 0).then_some(g)
}

/// A process's current working folder.
#[cfg(target_os = "linux")]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let path: Vec<u8> = info.pvi_cdir.vip_path.as_flattened().iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(path)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// A process's parent.
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid …`, and the name may hold anything, ")" too.
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some(info.pbi_ppid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

/// Is `pid` still a child of this process? Once a child is gone and
/// reaped, its pid may go to anything else: this is what makes signalling
/// it safe.
fn ours(pid: u32) -> bool {
    pid != 0 && parent_of(pid) == Some(std::process::id())
}

/// Signal `pid` and, when it leads its own process group (an agent does:
/// its terminal's session), everything in that group.
fn signal(pid: u32, sig: libc::c_int) {
    let Ok(p) = libc::pid_t::try_from(pid) else { return };
    unsafe {
        if libc::getpgid(p) == p {
            libc::kill(-p, sig);
        } else {
            libc::kill(p, sig);
        }
    }
}

/// See that `pids`, our children that were just hung up on, stop: a program
/// that ignores the hang-up (OpenCode 2 does) gets SIGTERM after `grace`,
/// and SIGKILL after another. Blocks until they're gone or killed, at most
/// about twice `grace`.
pub fn stop(pids: &[u32], grace: std::time::Duration) {
    let gone_by = |deadline: std::time::Instant| loop {
        if !pids.iter().any(|&p| ours(p)) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    for sig in [libc::SIGTERM, libc::SIGKILL] {
        if gone_by(std::time::Instant::now() + grace) {
            return;
        }
        for &p in pids.iter().filter(|&&p| ours(p)) {
            signal(p, sig);
        }
    }
}

/// Wait for a short-lived helper off the main thread, so it can't linger as
/// a zombie.
pub fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn quiet(cmd: &mut Command) -> &mut Command {
    cmd.stdout(Stdio::null()).stderr(Stdio::null())
}

/// Run `cmd` with `input` on its stdin, in the background.
fn feed(cmd: &mut Command, input: Vec<u8>) {
    let Ok(mut child) = quiet(cmd).stdin(Stdio::piped()).spawn() else { return };
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    reap(child);
}

/// A desktop notification with trix's icon. `urgent` ones should stay until
/// seen.
pub fn notify(title: &str, body: &str, urgent: bool) {
    // Tests notify too: never on the desktop of the person running them.
    if cfg!(test) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = urgent;
        let (title, body) = (title.to_string(), body.to_string());
        // From trix.app, which may need making first, so off the main thread.
        std::thread::spawn(move || {
            if !crate::notifier::post(&title, &body) {
                osascript_notify(&title, &body);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        const ICON: &[u8] = include_bytes!("../assets/trix.png");
        // notify-send takes the icon as a file: keep one next to the sockets.
        let icon = crate::hooks::runtime_dir().join("trix.png");
        if std::fs::metadata(&icon).map(|m| m.len()).ok() != Some(ICON.len() as u64) {
            let _ = std::fs::write(&icon, ICON);
        }
        let mut cmd = Command::new("notify-send");
        cmd.args(["-a", "trix", "-u", if urgent { "critical" } else { "normal" }]);
        if icon.is_file() {
            cmd.arg("-i").arg(&icon);
        }
        if let Ok(c) = quiet(cmd.args([title, body])).spawn() {
            reap(c);
        }
    }
}

/// A notification from Script Editor, when trix.app can't post it.
#[cfg(target_os = "macos")]
fn osascript_notify(title: &str, body: &str) {
    // Title and body go in as arguments, never into the script's text.
    let script = "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run";
    if let Ok(mut c) = quiet(Command::new("osascript").args(["-e", script, title, body])).spawn() {
        let _ = c.wait();
    }
}

/// Play a WAV file's bytes once, in the background.
pub fn play(wav: &'static [u8]) {
    if cfg!(target_os = "macos") {
        // afplay only reads files: keep one copy next to the sockets.
        std::thread::spawn(move || {
            let path = crate::hooks::runtime_dir().join("chime.wav");
            if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(wav.len() as u64) && std::fs::write(&path, wav).is_err() {
                return;
            }
            if let Ok(mut c) = quiet(Command::new("afplay").arg(&path)).spawn() {
                let _ = c.wait();
            }
        });
        return;
    }
    feed(Command::new("pw-play").args(["--media-role", "Notification", "-"]), wav.to_vec());
}

/// Play a sound file once, in the background: anything `pw-play` (wav, flac,
/// ogg) or `afplay` (also mp3, m4a) can play.
pub fn play_file(path: &std::path::Path) {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("afplay")
    } else {
        let mut c = Command::new("pw-play");
        c.args(["--media-role", "Notification"]);
        c
    };
    if let Ok(c) = quiet(cmd.arg(path).stdin(Stdio::null())).spawn() {
        reap(c);
    }
}

/// Run a user's command (an argv, no shell) with `input` on its stdin and
/// `env` added, in the background. Only starting it can fail here.
pub fn run(argv: &[String], env: &[(&str, String)], input: Vec<u8>) -> std::io::Result<()> {
    let (prog, args) = argv.split_first().ok_or_else(|| std::io::Error::other("empty command"))?;
    let mut cmd = Command::new(prog);
    cmd.args(args).envs(env.iter().map(|(k, v)| (*k, v.as_str())));
    if let Some(home) = std::env::var_os("HOME") {
        cmd.current_dir(home);
    }
    let mut child = quiet(&mut cmd).stdin(Stdio::piped()).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    reap(child);
    Ok(())
}

/// Put text on the system clipboard, for terminals that ignore OSC 52.
pub fn copy(text: &str) {
    // Tests copy too: never over what the person running them copied.
    if cfg!(test) {
        return;
    }
    if cfg!(target_os = "macos") {
        feed(&mut Command::new("pbcopy"), text.as_bytes().to_vec());
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        feed(&mut Command::new("wl-copy"), text.as_bytes().to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness() {
        assert!(alive(std::process::id()));
        assert!(alive(1), "init exists, even though it isn't ours");
        assert!(!alive(0), "0 would mean our own process group");
        assert!(!alive(u32::MAX));
        let mut c = Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        assert!(!alive(pid), "reaped");
    }

    #[test]
    fn own_working_folder() {
        assert_eq!(cwd_of(std::process::id()), std::env::current_dir().ok());
    }

    #[test]
    fn fd_limit_target() {
        assert_eq!(fd_target(256, libc::RLIM_INFINITY), FD_WANTED, "the macOS default is raised");
        assert_eq!(fd_target(256, 1024), 1024, "never past the hard limit");
        assert_eq!(fd_target(1_048_576, libc::RLIM_INFINITY), 1_048_576, "never lowered");
        assert_eq!(fd_target(256, 256), 256);
    }

    #[test]
    fn raising_the_fd_limit_sticks() {
        let mut before = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut before) };
        let now = raise_fd_limit().expect("readable");
        assert!(now >= before.rlim_cur);
        let mut after = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut after) };
        assert_eq!(after.rlim_cur, now);
    }

    #[test]
    fn only_our_own_children_are_ours() {
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        assert!(ours(child.id()));
        assert!(!ours(std::process::id()), "ourselves");
        assert!(!ours(1), "launchd / init");
        assert!(!ours(0));
        let _ = child.kill();
        let _ = child.wait();
        assert!(!ours(child.id()), "reaped: its pid may be anyone's now");
    }
}
