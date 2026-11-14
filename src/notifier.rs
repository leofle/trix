//! macOS notifications with trix's icon. macOS shows the icon of the app
//! that posts a notification, and `osascript`'s come from Script Editor. So
//! trix keeps a small app of its own: `trix.app` in
//! `~/Library/Application Support/trix`, a copy of this binary with an
//! Info.plist and the icon. Each notification runs that copy, which posts it
//! through UserNotifications and exits. macOS asks once whether trix may
//! notify you.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, SystemTime};

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotificationRequest,
    UNNotificationSettings, UNUserNotificationCenter,
};

const BUNDLE_ID: &str = "io.github.leofle.trix";
const ICON: &[u8] = include_bytes!("../assets/trix.icns");

/// What the app's process exits with.
const POSTED: i32 = 0;
/// You turned trix's notifications off: nothing else should notify instead.
const NOT_ALLOWED: i32 = 1;
/// Anything else: the caller falls back to `osascript`.
const FAILED: i32 = 2;

/// How long the first notification waits for your answer to macOS's
/// "allow notifications?" before it's dropped.
const ASK_TIMEOUT: Duration = Duration::from_secs(120);

/// Is this process the copy inside trix.app?
pub fn is_app() -> bool {
    std::env::current_exe().is_ok_and(|p| p.ends_with("trix.app/Contents/MacOS/trix"))
}

/// Post a notification from trix.app, setting the app up first if this
/// binary hasn't yet. Blocks until it's posted (or, the first time, until
/// you answer macOS's question), so call it off the main thread. False means
/// it couldn't be posted this way, and the caller should notify another way.
pub fn post(title: &str, body: &str) -> bool {
    static APP: OnceLock<Option<PathBuf>> = OnceLock::new();
    let app = APP.get_or_init(|| {
        install()
            .inspect_err(|e| eprintln!("trix: notifications come from Script Editor: could not make trix.app: {e}"))
            .ok()
    });
    let Some(exe) = app else { return false };
    let status = Command::new(exe)
        .args([title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    status.is_ok_and(|s| s.code() != Some(FAILED))
}

/// trix.app's folder.
fn app_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library/Application Support/trix/trix.app"))
}

/// Make trix.app from this binary, unless it already is, and return its
/// executable.
fn install() -> std::io::Result<PathBuf> {
    let app = app_dir().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let me = std::fs::read(std::env::current_exe()?)?;
    let plist = info_plist(&stamp(&me));
    let exe = app.join("Contents/MacOS/trix");
    if is_current(&app, &plist) {
        register(&app);
        return Ok(exe);
    }
    let parent = app.parent().expect("trix.app has a parent");
    let tmp = parent.join(format!(".build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let built = build(&tmp.join("trix.app"), &plist, &me).and_then(|()| {
        let _ = std::fs::remove_dir_all(&app);
        std::fs::rename(tmp.join("trix.app"), &app)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    // Another trix may have put the same app in place first.
    if built.is_err() && !is_current(&app, &plist) {
        built?;
    }
    register(&app);
    Ok(exe)
}

/// Tell Launch Services about the app. macOS only lets an app it knows
/// about notify, and it may never find one in Application Support by
/// itself: then every notification falls back to Script Editor's.
fn register(app: &Path) {
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    let _ = Command::new(LSREGISTER)
        .arg("-f")
        .arg(app)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Is the app at `app` this binary's, untouched?
fn is_current(app: &Path, plist: &str) -> bool {
    app.join("Contents/MacOS/trix").is_file()
        && std::fs::read(app.join("Contents/Info.plist")).is_ok_and(|p| p == plist.as_bytes())
}

fn build(app: &Path, plist: &str, exe: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let contents = app.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::create_dir_all(contents.join("Resources"))?;
    std::fs::write(contents.join("Info.plist"), plist)?;
    std::fs::write(contents.join("Resources/trix.icns"), ICON)?;
    let bin = contents.join("MacOS/trix");
    std::fs::write(&bin, exe)?;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
    // Sign the whole app, so the Info.plist and icon are sealed into its
    // signature: macOS only takes notifications from an app that checks out.
    let signed = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !signed.success() {
        return Err(std::io::Error::other("codesign failed"));
    }
    Ok(())
}

/// Which trix binary an app was made from, so a new build remakes it.
fn stamp(exe: &[u8]) -> String {
    use std::hash::Hasher;
    let mut h = std::hash::DefaultHasher::new();
    h.write(exe);
    format!("{:016x}", h.finish())
}

fn info_plist(stamp: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>trix</string>
	<key>CFBundleIconFile</key>
	<string>trix</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleName</key>
	<string>trix</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSUIElement</key>
	<true/>
	<key>TrixBuild</key>
	<string>{stamp}</string>
</dict>
</plist>
"#
    )
}

/// Run as trix.app: post `[title, body]` and exit with what happened. macOS
/// also opens the app, with no arguments, when you click a notification:
/// there's nothing to do then.
pub fn run(args: &[String]) -> i32 {
    let [title, body] = args else { return POSTED };
    let center = UNUserNotificationCenter::currentNotificationCenter();

    // Asks you the first time, and answers at once after that.
    let (tx, rx) = mpsc::channel();
    let answer = RcBlock::new(move |granted: Bool, _: *mut NSError| {
        let _ = tx.send(granted.as_bool());
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &answer);
    if rx.recv_timeout(ASK_TIMEOUT) != Ok(true) {
        return if denied(&center) { NOT_ALLOWED } else { FAILED };
    }

    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    // Unique, so a new notification never replaces an older one.
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let id = NSString::from_str(&format!("trix-{}-{nanos}", std::process::id()));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    let (tx, rx) = mpsc::channel();
    let added = RcBlock::new(move |err: *mut NSError| {
        let _ = tx.send(err.is_null());
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&added));
    if rx.recv_timeout(Duration::from_secs(10)) == Ok(true) { POSTED } else { FAILED }
}

/// Did you say no to trix's notifications (then, or later in System
/// Settings)?
fn denied(center: &UNUserNotificationCenter) -> bool {
    let (tx, rx) = mpsc::channel();
    let got = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        let _ = tx.send(status);
    });
    center.getNotificationSettingsWithCompletionHandler(&got);
    rx.recv_timeout(Duration::from_secs(5)) == Ok(UNAuthorizationStatus::Denied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_build_remakes_the_app() {
        assert_eq!(stamp(b"trix"), stamp(b"trix"));
        assert_ne!(stamp(b"trix"), stamp(b"trix 2"));
        let dir = std::env::temp_dir().join(format!("trix-notifier-test-{}", std::process::id()));
        let app = dir.join("trix.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/trix"), b"").unwrap();
        let plist = info_plist(&stamp(b"trix"));
        std::fs::write(app.join("Contents/Info.plist"), &plist).unwrap();
        assert!(is_current(&app, &plist));
        assert!(!is_current(&app, &info_plist(&stamp(b"trix 2"))), "another build");
        std::fs::remove_file(app.join("Contents/MacOS/trix")).unwrap();
        assert!(!is_current(&app, &plist), "no executable");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_plist_names_the_app_and_its_icon() {
        let p = info_plist("abc");
        assert!(p.contains("<string>io.github.leofle.trix</string>"));
        assert!(p.contains("<key>CFBundleIconFile</key>\n\t<string>trix</string>"));
        assert!(p.contains("<key>LSUIElement</key>\n\t<true/>"), "no Dock icon while it posts");
        assert!(p.contains("<key>TrixBuild</key>\n\t<string>abc</string>"));
        assert!(ICON.starts_with(b"icns"));
    }

    #[test]
    fn only_the_copy_in_the_app_is_the_app() {
        assert!(!is_app(), "the test binary");
        assert_eq!(run(&[]), POSTED, "opened by a click: nothing to do");
        assert_eq!(run(&["one".into()]), POSTED);
    }
}
