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

/// A desktop notification. `urgent` ones should stay until seen.
pub fn notify(title: &str, body: &str, urgent: bool) {
    if cfg!(target_os = "macos") {
        // Title and body go in as arguments, never into the script's text.
        let script = "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run";
        if let Ok(c) = quiet(Command::new("osascript").args(["-e", script, title, body])).spawn() {
            reap(c);
        }
        return;
    }
    let urgency = if urgent { "critical" } else { "normal" };
    if let Ok(c) = quiet(Command::new("notify-send").args(["-a", "trix", "-u", urgency, title, body])).spawn() {
        reap(c);
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

/// Put text on the system clipboard, for terminals that ignore OSC 52.
pub fn copy(text: &str) {
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
}
