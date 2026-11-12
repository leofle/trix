//! Layouts: a named set of tabs to open again on demand.
//!
//! `trix layout save work` writes the agents open now (kind, name, folder,
//! extra args, and the conversation each is in) to
//! `~/.config/trix/layouts/work.toml`. `trix layout open work` starts them
//! again, with fresh conversations unless `--resume` asks for the saved ones.
//! The files are plain TOML, meant to be read and edited by hand or by an
//! agent. Unlike the saved workspace (crash and restart recovery, one per
//! session), a layout stays until you remove it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::agent::{Kind, Spawn};
use crate::app::{self, App, ToastKind};
use crate::control::{self, Reply, Request};
use crate::session::{self, ClientMsg};
use crate::state::SavedAgent;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LayoutAgent {
    pub kind: String,
    pub name: String,
    /// `~/…` when under your home folder, so a layout reads well and moves
    /// between machines.
    pub cwd: String,
    /// Extra arguments; for a custom agent, its command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// The conversation `--resume` picks up (Claude and Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct LayoutFile {
    #[serde(default, rename = "agent")]
    agents: Vec<LayoutAgent>,
}

impl From<&SavedAgent> for LayoutAgent {
    fn from(s: &SavedAgent) -> Self {
        LayoutAgent { kind: s.kind.clone(), name: s.name.clone(), cwd: app::tilde(&s.cwd), args: s.command.clone(), session: s.session.clone() }
    }
}

// ───────────────────────────── files ─────────────────────────────

pub fn dir() -> PathBuf {
    crate::state::xdg_dir("XDG_CONFIG_HOME", ".config").join("trix/layouts")
}

fn file_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

/// Layout names end up as file names: the same rules as session names.
fn check_name(name: &str) -> Result<(), String> {
    session::check_name(name).map_err(|_| format!("bad layout name '{name}': use up to 32 letters, digits, - or _, starting with a letter or digit"))
}

pub fn to_toml(name: &str, agents: &[LayoutAgent]) -> String {
    let body = toml::to_string(&LayoutFile { agents: agents.to_vec() }).unwrap_or_default();
    format!(
        "# trix layout '{name}' · `trix layout open {name}` opens these tabs again\n\
         # kind: claude, codex, shell or custom · args: extra arguments (for custom, the command)\n\
         # cwd: the folder; a relative one starts from your home folder\n\
         # session: the conversation `--resume` picks up; delete the line to always start fresh\n\n{body}"
    )
}

pub fn from_toml(text: &str) -> Result<Vec<LayoutAgent>, String> {
    let mut agents = toml::from_str::<LayoutFile>(text).map(|f| f.agents).map_err(|e| e.message().to_string())?;
    for i in 0..agents.len() {
        let name = agents[i].name.trim().to_string();
        if name.is_empty() {
            return Err(format!("agent {} has an empty name", i + 1));
        }
        if agents[..i].iter().any(|a| a.name == name) {
            return Err(format!("two agents are named '{name}'"));
        }
        agents[i].name = name;
    }
    Ok(agents)
}

fn load_from(dir: &Path, name: &str) -> Result<Vec<LayoutAgent>, String> {
    let path = file_in(dir, name);
    let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("no layout '{name}' (`trix layout ls` lists them)"),
        _ => format!("can't read {}: {e}", app::tilde(&path)),
    })?;
    from_toml(&text).map_err(|e| format!("{}: {e}", app::tilde(&path)))
}

fn store_in(dir: &Path, name: &str, agents: &[LayoutAgent]) -> std::io::Result<PathBuf> {
    let path = file_in(dir, name);
    crate::state::write_atomic(&path, to_toml(name, agents).as_bytes())?;
    Ok(path)
}

/// Every layout in `dir`, by name, with what's in it (or why it can't be read).
fn list_in(dir: &Path) -> Vec<(String, Result<Vec<LayoutAgent>, String>)> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
        .filter(|n| check_name(n).is_ok())
        .collect();
    names.sort();
    names.into_iter().map(|n| (n.clone(), load_from(dir, &n))).collect()
}

// ───────────────────────────── client side ─────────────────────────────

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Save { name: String, force: bool },
    Open { name: String, resume: bool },
    Ls,
    Rm { name: String },
}

pub const USAGE: &str = "usage: trix layout save <name> [--force] | open <name> [--resume] | ls | rm <name>";

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let (cmd, rest) = args.split_first().ok_or(USAGE)?;
    let flags: Vec<&str> = rest.iter().filter(|a| a.starts_with('-')).map(String::as_str).collect();
    let pos: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    let allowed: &[&str] = match cmd.as_str() {
        "save" => &["--force", "-f"],
        "open" => &["--resume", "-r"],
        "ls" | "list" | "rm" | "remove" => &[],
        other => return Err(format!("unknown layout command '{other}'\n{USAGE}")),
    };
    if let Some(f) = flags.iter().find(|f| !allowed.contains(f)) {
        return Err(format!("layout {cmd}: unknown option '{f}'"));
    }
    let has = |names: &[&str]| flags.iter().any(|f| names.contains(f));
    let name = || -> Result<String, String> {
        match pos.as_slice() {
            [n] => check_name(n).map(|_| n.to_string()),
            [] => Err(format!("layout {cmd} needs a layout name")),
            _ => Err(format!("layout {cmd} takes one layout name")),
        }
    };
    Ok(match cmd.as_str() {
        "save" => Cmd::Save { name: name()?, force: has(&["--force", "-f"]) },
        "open" => Cmd::Open { name: name()?, resume: has(&["--resume", "-r"]) },
        "rm" | "remove" => Cmd::Rm { name: name()? },
        _ if !pos.is_empty() => return Err("layout ls takes no arguments".into()),
        _ => Cmd::Ls,
    })
}

fn ask(req: Request) -> Result<Reply, String> {
    session::control(&ClientMsg::Control { protocol: control::CONTROL, from: control::caller(), req }, None)
}

/// Run a layout command, printing what happened. `Ok(true)`: attach to the
/// workspace now (an `open` from a terminal outside trix).
pub fn run(cmd: Cmd) -> Result<bool, String> {
    let dir = dir();
    match cmd {
        Cmd::Ls => {
            let all = list_in(&dir);
            if all.is_empty() {
                println!("no layouts yet · `trix layout save <name>` saves the tabs you have open");
                return Ok(false);
            }
            let w = all.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
            for (name, agents) in all {
                match agents {
                    Ok(a) => {
                        let names: Vec<&str> = a.iter().map(|a| a.name.as_str()).collect();
                        let n = a.len();
                        println!("{name:<w$}  {n} agent{}  {}", if n == 1 { " " } else { "s" }, names.join(", "));
                    }
                    Err(e) => println!("{name:<w$}  unreadable: {e}"),
                }
            }
            Ok(false)
        }
        Cmd::Rm { name } => {
            let path = file_in(&dir, &name);
            std::fs::remove_file(&path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => format!("no layout '{name}'"),
                _ => format!("can't remove {}: {e}", app::tilde(&path)),
            })?;
            println!("removed layout {name}");
            Ok(false)
        }
        Cmd::Save { name, force } => {
            let path = file_in(&dir, &name);
            if path.exists() && !force {
                return Err(format!("layout '{name}' exists — --force to replace it"));
            }
            let reply = reply_or_err(ask(Request::Layout)?)?;
            let saved: Vec<SavedAgent> = serde_json::from_str(&reply).map_err(|e| format!("bad reply from the workspace: {e}"))?;
            if saved.is_empty() {
                return Err("nothing open to save".into());
            }
            let agents: Vec<LayoutAgent> = saved.iter().map(LayoutAgent::from).collect();
            let path = store_in(&dir, &name, &agents).map_err(|e| format!("can't write {}: {e}", app::tilde(&path)))?;
            let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
            println!("saved layout {name}: {} ({})", names.join(", "), app::tilde(&path));
            Ok(false)
        }
        Cmd::Open { name, resume } => {
            let agents = load_from(&dir, &name)?;
            if agents.is_empty() {
                return Err(format!("layout '{name}' has no agents"));
            }
            // From a pane, never attach: that would nest trix. Only a pane of
            // the session this targets can't start it, since it's in it.
            let in_pane = std::env::var_os("TRIX").is_some();
            let own_session = in_pane && std::env::var("TRIX_SESSION").is_ok_and(|s| s == session::name());
            if session::connect().is_none() {
                if own_session {
                    return Err("this pane's workspace isn't running".into());
                }
                session::start_server().map_err(|e| format!("starting the workspace: {e}"))?;
            }
            let reply = ask(Request::Open { name, agents, resume })?;
            print!("{}", reply.out);
            if !reply.err.is_empty() {
                eprintln!("trix: {}", reply.err);
            }
            let hint = || match (in_pane, own_session) {
                (false, _) => eprintln!("trix: `{}` attaches to the workspace", session::command()),
                (true, false) => eprintln!("trix: session '{}' · `{}` from outside trix attaches to it", session::name(), session::command()),
                (true, true) => {}
            };
            if reply.code != 0 {
                // Attaching would cover what failed and why with the TUI.
                hint();
                std::process::exit(reply.code);
            }
            if in_pane {
                hint();
            }
            Ok(!in_pane)
        }
    }
}

fn reply_or_err(r: Reply) -> Result<String, String> {
    if r.code == 0 { Ok(r.out) } else { Err(r.err) }
}

// ───────────────────────────── server side ─────────────────────────────

/// What starting this layout entry runs, or why it can't.
pub fn spec_for(a: &LayoutAgent, resume: bool) -> Result<Spawn, String> {
    let kind = Kind::from_label(&a.kind).ok_or_else(|| format!("unknown kind '{}'", a.kind))?;
    let cwd = home_relative(&a.cwd);
    if !cwd.is_dir() {
        return Err(format!("folder not found: {}", a.cwd));
    }
    let command = a.args.clone().filter(|s| !s.trim().is_empty());
    if kind == Kind::Custom && command.is_none() {
        return Err("custom agent without a command (args)".into());
    }
    let resume = a.session.clone().filter(|_| resume && matches!(kind, Kind::Claude | Kind::Codex));
    Ok(Spawn { kind, name: a.name.clone(), cwd, command, prompt: None, branch_note: None, resume })
}

/// A layout's folder: `~` expanded, and a relative one taken from the home
/// folder, not from wherever the workspace happened to be started.
fn home_relative(cwd: &str) -> PathBuf {
    let p = app::expand(cwd);
    if p.is_absolute() {
        return p;
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(p)
}

/// `trix layout open`: start every entry that isn't open already.
pub fn open(app: &mut App, from: Option<u32>, name: &str, agents: Vec<LayoutAgent>, resume: bool) -> Reply {
    let by = control::caller_name(app, from);
    let w = agents.iter().map(|a| a.name.chars().count()).max().unwrap_or(0);
    let (mut out, mut opened, mut failed) = (String::new(), 0, 0);
    for a in &agents {
        let (verb, rest) = if app.agents.iter().any(|x| x.name == a.name) {
            ("skipped", "already open".to_string())
        } else {
            match spec_for(a, resume).and_then(|spec| {
                let resumed = spec.resume.is_some();
                app.spawn_quiet(spec).map(|id| (id, resumed)).map_err(|e| e.to_string())
            }) {
                Ok((id, resumed)) => {
                    opened += 1;
                    if let Some(x) = app.agents.iter_mut().find(|x| x.id == id) {
                        x.managed_by = by.clone();
                    }
                    ("opened", format!("{}{}", a.cwd, if resumed { " · resuming its conversation" } else { "" }))
                }
                Err(e) => {
                    failed += 1;
                    ("failed", e)
                }
            }
        };
        out.push_str(&format!("{verb:<9}{:<w$}  {rest}\n", a.name));
    }
    drop_from_restore(app, &agents);
    if opened > 0 {
        app.toast(format!("opened layout {name} · {opened} agent{}", if opened == 1 { "" } else { "s" }), ToastKind::Good);
    }
    let mut r = Reply::ok(out);
    if failed > 0 {
        r.code = 1;
        r.err = format!("{failed} of layout {name}'s agents couldn't start");
    }
    r
}

/// A workspace that `layout open` started still offers to restore the last
/// session. Its entries for tabs this layout now has open would come back a
/// second time (`web-2`, maybe in the same conversation), so the layout takes
/// them over and the offer keeps only the rest.
fn drop_from_restore(app: &mut App, agents: &[LayoutAgent]) {
    let Some(p) = &mut app.pending_restore else { return };
    let covered = |s: &SavedAgent| agents.iter().any(|a| a.name == s.name) && app.agents.iter().any(|x| x.name == s.name);
    let focused = p.agents.get(p.focus).filter(|s| !covered(s)).map(|s| s.name.clone());
    p.agents.retain(|s| !covered(s));
    p.focus = focused.and_then(|f| p.agents.iter().position(|s| s.name == f)).unwrap_or(0);
    if p.agents.is_empty() {
        app.pending_restore = None;
        if matches!(app.modal, Some(app::Modal::Restore)) {
            app.modal = None;
        }
    }
    app.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("trix-test-layouts-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        d
    }
    fn entry(kind: &str, name: &str, cwd: &str) -> LayoutAgent {
        LayoutAgent { kind: kind.into(), name: name.into(), cwd: cwd.into(), args: None, session: None }
    }
    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse(&args(&["save", "work"])), Ok(Cmd::Save { name: "work".into(), force: false }));
        assert_eq!(parse(&args(&["save", "-f", "work"])), Ok(Cmd::Save { name: "work".into(), force: true }));
        assert_eq!(parse(&args(&["open", "work", "--resume"])), Ok(Cmd::Open { name: "work".into(), resume: true }));
        assert_eq!(parse(&args(&["open", "work"])), Ok(Cmd::Open { name: "work".into(), resume: false }));
        assert_eq!(parse(&args(&["ls"])), Ok(Cmd::Ls));
        assert_eq!(parse(&args(&["rm", "work"])), Ok(Cmd::Rm { name: "work".into() }));
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["open"])).unwrap_err().contains("needs a layout name"));
        assert!(parse(&args(&["open", "a", "b"])).is_err());
        assert!(parse(&args(&["open", "../etc"])).unwrap_err().contains("bad layout name"), "names are file names");
        assert!(parse(&args(&["save", "w", "--resume"])).unwrap_err().contains("unknown option"));
        assert!(parse(&args(&["ls", "x"])).is_err());
        assert!(parse(&args(&["load", "w"])).unwrap_err().contains("unknown layout command"));
    }

    #[test]
    fn toml_round_trip_is_readable() {
        let mut api = entry("claude", "api", "~/code/api");
        api.session = Some("4f1c-abc".into());
        let mut dev = entry("custom", "dev", "~/code/web");
        dev.args = Some("npm run dev".into());
        let agents = vec![api, entry("shell", "infra", "/srv/infra"), dev];
        let text = to_toml("work", &agents);
        assert!(text.starts_with("# trix layout 'work'"));
        assert!(text.contains("[[agent]]\nkind = \"claude\"\nname = \"api\"\ncwd = \"~/code/api\"\nsession = \"4f1c-abc\""), "{text}");
        assert!(!text.contains("args = \"\"") && !text.contains("session = \"\""), "absent fields stay out:\n{text}");
        assert_eq!(from_toml(&text), Ok(agents));
    }

    #[test]
    fn hand_written_files() {
        let a = from_toml("[[agent]]\nkind = \"shell\"\nname = \"s\"\ncwd = \"/tmp\"\n").unwrap();
        assert_eq!(a, [entry("shell", "s", "/tmp")]);
        assert_eq!(from_toml(""), Ok(vec![]));
        assert!(from_toml("[[agent]]\nkind = \"shell\"\n").unwrap_err().contains("name"), "says what's missing");
        assert!(from_toml("not toml at all [").is_err());
        let a = from_toml("[[agent]]\nkind = \"shell\"\nname = \" api \"\ncwd = \"/tmp\"\n").unwrap();
        assert_eq!(a[0].name, "api", "names are trimmed");
        assert!(from_toml("[[agent]]\nkind = \"shell\"\nname = \"  \"\ncwd = \"/tmp\"\n").unwrap_err().contains("agent 1 has an empty name"));
        let twice = "[[agent]]\nkind = \"shell\"\nname = \"a\"\ncwd = \"/tmp\"\n\n[[agent]]\nkind = \"shell\"\nname = \"a \"\ncwd = \"/tmp\"\n";
        assert!(from_toml(twice).unwrap_err().contains("two agents are named 'a'"));
    }

    #[test]
    fn files_save_list_and_load() {
        let dir = tmp();
        assert!(list_in(&dir).is_empty(), "no folder yet");
        let path = store_in(&dir, "work", &[entry("shell", "s", "/tmp")]).unwrap();
        assert_eq!(path, dir.join("work.toml"));
        store_in(&dir, "home", &[entry("shell", "a", "/tmp"), entry("shell", "b", "/tmp")]).unwrap();
        std::fs::write(dir.join("broken.toml"), "[[agent]]").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a layout").unwrap();
        let all = list_in(&dir);
        let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["broken", "home", "work"]);
        assert!(all[0].1.is_err());
        assert_eq!(all[1].1.as_ref().unwrap().len(), 2);
        assert!(load_from(&dir, "nope").unwrap_err().contains("no layout 'nope'"));
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 4, "atomic writes leave no temp files: {names:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn specs_start_fresh_unless_resuming() {
        let mut a = entry("claude", "api", "/tmp");
        a.session = Some("abc".into());
        assert_eq!(spec_for(&a, false).unwrap().resume, None, "fresh by default");
        assert_eq!(spec_for(&a, true).unwrap().resume.as_deref(), Some("abc"));
        let mut sh = entry("shell", "s", "/tmp");
        sh.session = Some("abc".into());
        assert_eq!(spec_for(&sh, true).unwrap().resume, None, "only Claude and Codex resume");
        assert!(spec_for(&entry("claude", "x", "/no/such/dir"), false).unwrap_err().contains("folder not found"));
        assert!(spec_for(&entry("bard", "x", "/tmp"), false).unwrap_err().contains("unknown kind"));
        assert!(spec_for(&entry("custom", "x", "/tmp"), false).unwrap_err().contains("command"));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(spec_for(&entry("shell", "h", "~"), false).unwrap().cwd, PathBuf::from(&home), "~ expands");
        assert_eq!(home_relative("Projects/x"), PathBuf::from(&home).join("Projects/x"), "relative means from home");
        assert_eq!(home_relative("/srv/x"), PathBuf::from("/srv/x"));
    }

    #[test]
    fn opening_skips_whats_open_and_reports_failures() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), None, "/bin/true".into(), crate::session::Out::default());
        app.pending_restore = None;
        let tmp = std::env::temp_dir().display().to_string();
        let job = |name: &str| {
            let mut e = entry("custom", name, &tmp);
            e.args = Some("exec sleep 60".into());
            e
        };
        let first = open(&mut app, None, "work", vec![job("a"), job("b")], false);
        assert_eq!(first.code, 0);
        assert_eq!(app.agents.len(), 2);
        let again = open(&mut app, None, "work", vec![job("a"), job("c"), entry("shell", "gone", "/no/such/dir")], false);
        assert_eq!(app.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert!(again.out.contains("skipped  a") && again.out.contains("already open"), "{}", again.out);
        assert!(again.out.contains("opened   c"), "{}", again.out);
        assert!(again.out.contains("failed   gone") && again.out.contains("folder not found"), "{}", again.out);
        assert_eq!(again.code, 1);
        for a in &app.agents {
            a.shutdown();
        }
    }

    #[test]
    fn opening_takes_over_matching_restore_entries() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(tx, "/nonexistent.sock".into(), None, "/bin/true".into(), crate::session::Out::default());
        let tmp = std::env::temp_dir();
        let saved = |name: &str| SavedAgent { kind: "claude".into(), name: name.into(), cwd: tmp.clone(), command: None, session: Some(format!("s-{name}")) };
        app.pending_restore = Some(crate::state::Saved::new(2, vec![saved("web"), saved("api"), saved("docs")]));
        app.modal = Some(app::Modal::Restore);
        let mut job = entry("custom", "web", &tmp.display().to_string());
        job.args = Some("exec sleep 60".into());
        let gone = entry("shell", "api", "/no/such/dir");
        assert_eq!(open(&mut app, None, "work", vec![job, gone], false).code, 1);
        let p = app.pending_restore.as_ref().expect("api and docs are still on offer");
        assert_eq!(p.agents.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["api", "docs"], "only web is open now");
        assert_eq!(p.agents[p.focus].name, "docs", "focus follows the entry");

        let mut docs = entry("custom", "docs", &tmp.display().to_string());
        docs.args = Some("exec sleep 60".into());
        let mut api = docs.clone();
        api.name = "api".into();
        open(&mut app, None, "rest", vec![docs, api], false);
        assert!(app.pending_restore.is_none() && app.modal.is_none(), "nothing left to restore, so no prompt");
        for a in &app.agents {
            a.shutdown();
        }
    }
}
