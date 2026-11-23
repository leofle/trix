//! `trix worktrees`: trix's worktrees (those under `.trix/worktrees/`),
//! what each holds and who uses it, and removing what holds no work.
//!
//! Built on what's there, with no record of its own: `git worktree list`,
//! `worktree::users` (every session's agents, restore offers, saved
//! workspaces and left-running agents), trix's locks, and
//! `cleanup::assess` / `cleanup::remove`, which the review's X uses too.
//! It shows its plan and asks, no by default; without a terminal it's a dry
//! run unless `--yes`. Each removal looks again right before it's done.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cleanup::{Assessment, Caps, State};
use crate::worktree::Lock;

/// Exit codes, for scripts and agents.
pub const DONE: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;
/// Nothing was done: a dry run, or the answer was no.
pub const NOT_DONE: i32 = 3;

const DAY: Duration = Duration::from_secs(86400);
/// How long a deleted branch's tip is kept under `refs/trix/removed/`.
const KEEP_TIPS: Duration = Duration::from_secs(30 * 86400);

#[derive(Debug, PartialEq)]
enum Cmd {
    List,
    Prune,
    Rm(String),
}

#[derive(Debug, PartialEq)]
struct Opts {
    all: bool,
    json: bool,
    yes: bool,
    folders: bool,
    keep_branch: bool,
    min_age: Duration,
    /// `rm`: asked for by name, so its age doesn't matter and an unmerged
    /// branch only keeps the branch, as X does.
    rm: bool,
}

fn parse(args: &[String]) -> Result<(Cmd, Opts), String> {
    let mut o = Opts { all: false, json: false, yes: false, folders: false, keep_branch: false, min_age: DAY, rm: false };
    let mut cmd = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--all" | "-a" => o.all = true,
            "--json" => o.json = true,
            "--yes" | "-y" => o.yes = true,
            "--folders" => o.folders = true,
            "--keep-branch" => o.keep_branch = true,
            "--min-age" => {
                let v = it.next().ok_or("--min-age needs a time, like 1d or 12h")?;
                o.min_age = if v == "0" { Duration::ZERO } else { crate::idle::parse_after(v).map_err(|e| format!("--min-age: {e}"))?.unwrap_or(Duration::ZERO) };
            }
            "prune" | "rm" if cmd.is_none() => cmd = Some(a.clone()),
            name if cmd.as_deref() == Some("rm") && !name.starts_with('-') => cmd = Some(format!("rm {name}")),
            other => return Err(format!("worktrees: unknown argument '{other}'")),
        }
    }
    let cmd = match cmd.as_deref() {
        None => Cmd::List,
        Some("prune") => Cmd::Prune,
        Some("rm") => return Err("worktrees rm needs the worktree's name".into()),
        Some(rm) => Cmd::Rm(rm.trim_start_matches("rm ").to_string()),
    };
    o.rm = matches!(cmd, Cmd::Rm(_));
    let only = |flag: bool, name: &str, ok: bool| if flag && !ok { Err(format!("{name} doesn't go with this")) } else { Ok(()) };
    only(o.folders, "--folders", cmd == Cmd::Prune)?;
    only(o.keep_branch, "--keep-branch", matches!(cmd, Cmd::Rm(_)))?;
    only(o.yes, "--yes", cmd != Cmd::List)?;
    Ok((cmd, o))
}

// ───────────────────────────── what's there ─────────────────────────────

/// One entry of `git worktree list --porcelain`.
#[derive(Clone, Debug, Default, PartialEq)]
struct Entry {
    path: PathBuf,
    /// Its HEAD's commit.
    head: String,
    branch: Option<String>,
    prunable: bool,
}

fn entries(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.push(Entry { path: PathBuf::from(p), ..Entry::default() });
        } else if let Some(e) = out.last_mut() {
            if let Some(h) = line.strip_prefix("HEAD ") {
                e.head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                e.branch = Some(b.to_string());
            } else if line == "prunable" || line.starts_with("prunable ") {
                e.prunable = true;
            }
        }
    }
    out
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    crate::review::git(dir, args)
}

fn listing(main: &Path) -> String {
    git(main, &["-c", "core.quotePath=false", "worktree", "list", "--porcelain"]).unwrap_or_default()
}

/// A worktree of trix's, as it stands, and what prune would do with it.
#[derive(Debug, Serialize)]
pub struct Tree {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub missing: bool,
    /// "none", "trix", or someone else's reason.
    pub lock: String,
    pub users: Vec<String>,
    pub uncommitted: usize,
    pub orphans: u32,
    pub last_used_seconds: Option<u64>,
    pub state: String,
    pub plan: Plan,
    #[serde(skip)]
    lock_kind: Option<Lock>,
    #[serde(skip)]
    a: Assessment,
    #[serde(skip)]
    age: Option<Duration>,
    /// Ignored files that go with the folder.
    pub ignored: Vec<String>,
}

/// What prune (or rm) does with a worktree.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Plan {
    /// Kept. `why`: when what it is doesn't say it already.
    Keep { why: Option<String> },
    /// Remove the folder, then the branches that are merged.
    Remove,
    /// Remove the folder only: its branch isn't merged (`--folders`).
    RemoveFolder,
    /// The folder is gone: drop git's entry for it.
    DropEntry,
}

impl Plan {
    pub fn acts(&self) -> bool {
        !matches!(self, Plan::Keep { .. })
    }
}

/// A `trix/*` branch no worktree has checked out.
#[derive(Debug, Serialize)]
pub struct Loose {
    pub name: String,
    pub state: String,
    pub prune: bool,
    #[serde(skip)]
    a: Assessment,
}

/// One repository's worktrees, branches and kept tips.
#[derive(Debug, Serialize)]
pub struct Repo {
    pub repo: PathBuf,
    pub worktrees: Vec<Tree>,
    pub branches: Vec<Loose>,
    /// Deleted branches' tips under `refs/trix/removed/`, and the ones
    /// older than 30 days that prune drops.
    pub kept_tips: usize,
    pub old_tips: Vec<String>,
}

impl Repo {
    fn acts(&self) -> bool {
        self.worktrees.iter().any(|t| t.plan.acts()) || self.branches.iter().any(|b| b.prune) || !self.old_tips.is_empty()
    }
}

/// `--min-age`, the way you'd type it: whole days as days.
fn as_given(d: Duration) -> String {
    match d.as_secs() {
        0 => "0".into(),
        s if s % 86400 == 0 => format!("{}d", s / 86400),
        _ => crate::idle::coarse(d),
    }
}

fn since(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or_default()
}

fn s(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What a worktree is, in a few words.
fn state_of(t: &Tree) -> String {
    if let Some(Lock::Other(r)) = &t.lock_kind {
        return format!("locked by someone else ({r})");
    }
    if !t.users.is_empty() {
        return format!("in use: {}", t.users.join("; "));
    }
    if t.missing {
        return if t.a.orphans > 0 { format!("folder missing, {} on no branch", s(t.a.orphans as usize, "commit", "commits")) } else { "folder missing".into() };
    }
    if t.uncommitted > 0 {
        return s(t.uncommitted, "uncommitted file", "uncommitted files");
    }
    if t.orphans > 0 {
        return format!("{} on no branch", s(t.orphans as usize, "commit", "commits"));
    }
    match t.a.branches.first() {
        _ if t.a.detached => "clean, detached".into(),
        Some(b) => format!("clean, {}", crate::cleanup::why(&b.state)),
        None => "clean".into(),
    }
}

/// What to do with `t`.
fn decide(t: &Tree, o: &Opts) -> Plan {
    let rm = o.rm;
    let keep = |why: Option<String>| Plan::Keep { why };
    if matches!(t.lock_kind, Some(Lock::Other(_))) || !t.users.is_empty() {
        return keep(None);
    }
    if t.missing && t.a.orphans > 0 {
        let n = s(t.a.orphans as usize, "commit", "commits");
        return keep(Some(format!("its HEAD has {n} on no branch, and dropping the entry would lose them: `git branch <name> {}` keeps them", t.head)));
    }
    if t.missing {
        return if t.lock_kind == Some(Lock::Trix) || rm { Plan::DropEntry } else { keep(Some("not locked by trix, so not trix's to drop: `git worktree prune` does".into())) };
    }
    if t.a.keeps().is_some() {
        return keep(None);
    }
    if !rm && t.age.is_some_and(|a| a < o.min_age) {
        return keep(Some(format!("used {} ago, under --min-age {}", crate::idle::coarse(t.age.unwrap_or_default()), as_given(o.min_age))));
    }
    let merged = t.a.detached || t.a.branches.first().is_some_and(|b| matches!(b.state, State::Merged { .. }));
    if merged && !t.a.bases.is_empty() {
        return Plan::Remove;
    }
    if rm || o.folders {
        return Plan::RemoveFolder;
    }
    if t.a.bases.is_empty() {
        return keep(Some("the repository has no main or master to tell if it's merged".into()));
    }
    keep(Some("--folders removes the folder and keeps the branch".into()))
}

/// The worktree in `e` of the repository at `main`, looked at now.
fn tree(main: &Path, e: &Entry, text: &str, seen: &crate::worktree::Seen, caps: Caps, now: SystemTime, o: &Opts) -> Tree {
    let missing = e.prunable || !e.path.is_dir();
    let lock_kind = crate::worktree::lock_in(text, &e.path);
    // Before git looks inside: a status could touch the index.
    let age = crate::cleanup::last_used(main, &e.path).map(|t| since(now, t));
    let a = if missing {
        match e.branch.as_deref() {
            Some(b) => crate::cleanup::assess_branch(main, b, caps).ok().unwrap_or_default(),
            // Its HEAD, in the entry about to go, may be all that holds
            // these commits: once it's gone, gc takes them.
            None => {
                let orphans = git(main, &["rev-list", "--count", &e.head, "--not", "--branches", "--remotes", "--tags"]).ok().and_then(|n| n.parse().ok());
                // Can't tell (a HEAD git no longer has): as if there were.
                Assessment { detached: true, orphans: orphans.unwrap_or(1), ..Assessment::default() }
            }
        }
    } else {
        crate::cleanup::assess_with(&e.path, caps).unwrap_or_default()
    };
    let mut t = Tree {
        name: e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        path: e.path.clone(),
        branch: e.branch.clone(),
        head: e.head.clone(),
        missing,
        lock: match &lock_kind {
            Lock::None => "none".into(),
            Lock::Trix => "trix".into(),
            Lock::Other(r) => r.clone(),
        },
        users: seen.users(&e.path),
        uncommitted: a.uncommitted(),
        orphans: a.orphans,
        last_used_seconds: age.map(|a| a.as_secs()),
        state: String::new(),
        plan: Plan::Keep { why: None },
        lock_kind: Some(lock_kind),
        a,
        age,
        ignored: Vec::new(),
    };
    t.state = state_of(&t);
    t.plan = decide(&t, o);
    if !t.missing && matches!(t.plan, Plan::Remove | Plan::RemoveFolder) {
        t.ignored = crate::cleanup::ignored(&t.path);
    }
    t
}

fn trees(main: &Path, seen: &crate::worktree::Seen, caps: Caps, now: SystemTime, o: &Opts) -> Vec<Tree> {
    let text = listing(main);
    entries(&text).iter().filter(|e| crate::worktree::managed_root(&e.path).as_deref() == Some(e.path.as_path())).map(|e| tree(main, e, &text, seen, caps, now, o)).collect()
}

/// When a branch was last moved: its reflog, else its tip's commit time.
fn branch_age(main: &Path, name: &str, now: SystemTime) -> Option<Duration> {
    let common = git(main, &["rev-parse", "--git-common-dir"]).ok().map(|d| main.join(d))?;
    if let Ok(t) = std::fs::metadata(common.join("logs/refs/heads").join(name)).and_then(|m| m.modified()) {
        return Some(since(now, t));
    }
    let secs: u64 = git(main, &["log", "-1", "--format=%ct", &format!("refs/heads/{name}")]).ok()?.parse().ok()?;
    Some(since(now, SystemTime::UNIX_EPOCH + Duration::from_secs(secs)))
}

fn loose(main: &Path, checked_out: &[String], caps: Caps, now: SystemTime, o: &Opts) -> Vec<Loose> {
    let names = git(main, &["for-each-ref", "--format=%(refname:short)", "refs/heads/trix/"]).unwrap_or_default();
    names
        .lines()
        .filter(|n| !n.is_empty() && !checked_out.iter().any(|c| c == n))
        .filter_map(|n| {
            let a = crate::cleanup::assess_branch(main, n, caps).ok()?;
            let b = a.branches.first()?;
            let merged = matches!(b.state, State::Merged { .. });
            let young = branch_age(main, n, now).is_some_and(|age| age < o.min_age);
            let mut state = crate::cleanup::why(&b.state);
            if merged && young {
                state.push_str(&format!(", moved under --min-age {}", as_given(o.min_age)));
            }
            Some(Loose { name: n.to_string(), state, prune: merged && !young, a })
        })
        .collect()
}

/// (count, the refs older than 30 days) under `refs/trix/removed/`.
fn tips(main: &Path, now: SystemTime) -> (usize, Vec<String>) {
    let refs = git(main, &["for-each-ref", "--format=%(refname)", "refs/trix/removed/"]).unwrap_or_default();
    let all: Vec<&str> = refs.lines().filter(|l| !l.is_empty()).collect();
    let old = all
        .iter()
        .filter(|r| r.rsplit('/').next().and_then(|t| t.parse::<u64>().ok()).is_some_and(|t| since(now, SystemTime::UNIX_EPOCH + Duration::from_secs(t)) > KEEP_TIPS))
        .map(|r| r.to_string())
        .collect();
    (all.len(), old)
}

fn repo(main: &Path, seen: &crate::worktree::Seen, caps: Caps, now: SystemTime, o: &Opts) -> Repo {
    let worktrees = trees(main, seen, caps, now, o);
    let checked_out: Vec<String> = entries(&listing(main)).into_iter().filter_map(|e| e.branch).collect();
    let branches = loose(main, &checked_out, caps, now, o);
    let (kept_tips, old_tips) = tips(main, now);
    Repo { repo: main.to_path_buf(), worktrees, branches, kept_tips, old_tips }
}

/// The repositories to look in: this folder's, or with `--all` every one
/// trix knows of: where sessions' agents are or would come back, the ports
/// ledger, and the folders you've started agents in.
fn repos(all: bool, cwd: &Path, seen: &crate::worktree::Seen) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.to_path_buf()];
    if all {
        dirs.extend(seen.dirs());
        dirs.extend(crate::worktree::ledger_dirs());
        dirs.extend(crate::state::load_prefs().recent_dirs.into_iter().map(PathBuf::from));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs.iter().filter(|d| d.is_dir()) {
        if let Some(m) = crate::app::main_checkout(d).and_then(|m| m.canonicalize().ok())
            && !out.contains(&m)
        {
            out.push(m);
        }
    }
    out
}

// ───────────────────────────── saying it ─────────────────────────────

fn branches_going(t: &Tree, o: &Opts) -> Vec<String> {
    if o.keep_branch {
        return Vec::new();
    }
    t.a.branches.iter().filter(|b| matches!(b.state, State::Merged { .. })).map(|b| b.name.clone()).collect()
}

/// What happens to `t`, as said after its state.
fn action(t: &Tree, o: &Opts, verb: &str) -> String {
    let and = |names: Vec<String>| if names.is_empty() { String::new() } else { format!(" and {}", names.join(", ")) };
    let ignored = if t.ignored.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = t.ignored.iter().take(4).map(String::as_str).collect();
        let more = if t.ignored.len() > 4 { format!(" and {} more", t.ignored.len() - 4) } else { String::new() };
        format!(" (with its ignored {}{more})", shown.join(", "))
    };
    match &t.plan {
        Plan::Keep { why: None } => "kept".into(),
        Plan::Keep { why: Some(w) } => format!("kept: {w}"),
        Plan::Remove => format!("{verb} removes it{}{ignored}", and(branches_going(t, o))),
        Plan::RemoveFolder => {
            let kept: Vec<String> = t.a.branches.iter().filter(|b| o.keep_branch || !matches!(b.state, State::Merged { .. })).map(|b| b.name.clone()).collect();
            format!("{verb} removes the folder{}{ignored}, keeping {}", and(branches_going(t, o)), if kept.is_empty() { "its branch".into() } else { kept.join(", ") })
        }
        Plan::DropEntry => format!("{verb} drops git's entry for it{}", and(branches_going(t, o))),
    }
}

fn say(r: &Repo, o: &Opts, verb: &str) -> String {
    let going = r.worktrees.iter().filter(|t| t.plan.acts()).count();
    let mut out = format!("{} · {} · {} can go\n", crate::app::tilde(&r.repo), s(r.worktrees.len(), "worktree", "worktrees"), going);
    let w = r.worktrees.iter().map(|t| t.name.chars().count()).max().unwrap_or(0);
    let bw = r.worktrees.iter().map(|t| t.branch.as_deref().unwrap_or("(detached)").chars().count()).max().unwrap_or(0);
    for t in &r.worktrees {
        let used = t.age.map(crate::idle::coarse).unwrap_or_else(|| "?".into());
        let lock = match &t.lock_kind {
            Some(Lock::Trix) => " · locked",
            _ => "",
        };
        out.push_str(&format!(
            "  {:<w$}  {:<bw$}  {:>4}  {}{lock} → {}\n",
            crate::hooks::printable(&t.name),
            crate::hooks::printable(t.branch.as_deref().unwrap_or("(detached)")),
            used,
            crate::hooks::printable(&t.state),
            action(t, o, verb)
        ));
    }
    if !r.branches.is_empty() {
        let each: Vec<String> = r.branches.iter().map(|b| format!("{} ({}){}", b.name, b.state, if b.prune { format!(" → {verb} deletes it") } else { String::new() })).collect();
        out.push_str(&format!("  branches with no worktree: {}\n", each.join(" · ")));
    }
    if r.kept_tips > 0 {
        out.push_str(&format!("  {} of deleted branches under refs/trix/removed/", s(r.kept_tips, "tip", "tips")));
        if !r.old_tips.is_empty() {
            out.push_str(&format!(", {} older than 30 days → {verb} drops them", r.old_tips.len()));
        }
        out.push('\n');
    }
    out
}

// ───────────────────────────── doing it ─────────────────────────────

/// Do `r`'s plan, looking again right before each step: who uses it, what
/// it holds. What was done and what wasn't, and whether all went well.
fn act(r: &Repo, o: &Opts, caps: Caps) -> (Vec<String>, bool) {
    let mut said = Vec::new();
    let mut ok = true;
    let main = &r.repo;
    for t in r.worktrees.iter().filter(|t| t.plan.acts()) {
        // Looked at again now: an agent may have come back to it meanwhile.
        let text = listing(main);
        let Some(e) = entries(&text).into_iter().find(|e| e.path == t.path) else {
            said.push(format!("{}: already gone", t.name));
            continue;
        };
        let again = tree(main, &e, &text, &crate::worktree::seen(), caps, SystemTime::now(), o);
        if again.plan != t.plan {
            said.push(format!("{}: left alone, it changed since the plan: {}", t.name, again.state));
            continue;
        }
        match crate::cleanup::remove(main, &again.path, &again.a, o.keep_branch, None) {
            Ok(done) => said.push(format!("{}: {done}", t.name)),
            Err(e) => {
                ok = false;
                said.push(format!("{}: not removed: {e}", t.name));
            }
        }
    }
    for b in r.branches.iter().filter(|b| b.prune) {
        let Some(branch) = b.a.branches.first() else { continue };
        let checked_out = entries(&listing(main)).iter().any(|e| e.branch.as_deref() == Some(b.name.as_str()));
        if checked_out {
            said.push(format!("{}: left alone, a worktree has it checked out now", b.name));
            continue;
        }
        match crate::cleanup::delete_merged(main, branch) {
            Ok(done) => said.push(format!("deleted {done}")),
            Err(e) => {
                ok = false;
                said.push(format!("{}: not deleted: {e}", b.name));
            }
        }
    }
    for tip in &r.old_tips {
        match git(main, &["update-ref", "-d", tip]) {
            Ok(_) => said.push(format!("dropped {tip}")),
            Err(e) => {
                ok = false;
                said.push(format!("{tip}: not dropped: {e}"));
            }
        }
    }
    (said, ok)
}

/// Where `trix worktrees` talks: the terminal, or a test.
trait Io {
    /// What it says, as it goes: the plan comes before the question.
    fn say(&mut self, text: &str);
    fn err(&mut self, text: &str);
    /// Go ahead? `None`: there's no terminal to ask.
    fn ask(&mut self) -> Option<bool>;
}

struct Terminal;

impl Io for Terminal {
    fn say(&mut self, text: &str) {
        print!("{text}");
    }
    fn err(&mut self, text: &str) {
        eprint!("{text}");
    }
    fn ask(&mut self) -> Option<bool> {
        use std::io::{IsTerminal, Write};
        if !std::io::stdin().is_terminal() {
            return None;
        }
        let _ = std::io::stdout().flush();
        eprint!("Go ahead? [y/N] ");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        Some(matches!(line.trim(), "y" | "Y" | "yes"))
    }
}

#[derive(Serialize)]
struct JsonOut<'a> {
    repos: &'a [Repo],
    /// Prune and rm: whether it was done; false for a dry run or a no.
    #[serde(skip_serializing_if = "Option::is_none")]
    done: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    said: Vec<String>,
}

const DRY_RUN: &str = "dry run: nothing was removed (no terminal to ask; --yes does it)";

/// Show `repos`' plan, ask, and do it. Its exit code.
fn go(repos: &[Repo], o: &Opts, caps: Caps, io: &mut dyn Io) -> i32 {
    let json = |done: Option<bool>, said: Vec<String>| serde_json::to_string_pretty(&JsonOut { repos, done, said }).unwrap_or_default() + "\n";
    if !o.json {
        let verb = if o.rm { "rm" } else { "prune" };
        io.say(&repos.iter().map(|r| say(r, o, verb)).collect::<Vec<_>>().join("\n"));
    }
    if !repos.iter().any(Repo::acts) {
        if o.json {
            io.say(&json(Some(true), vec!["nothing to prune".into()]));
        } else {
            io.say("nothing to prune\n");
        }
        return DONE;
    }
    // A script's answer can't be typed: --json acts only with --yes.
    let answer = if o.yes { Some(true) } else if o.json { None } else { io.ask() };
    match answer {
        None => {
            io.say(&if o.json { json(Some(false), vec![DRY_RUN.into()]) } else { format!("{DRY_RUN}\n") });
            NOT_DONE
        }
        Some(false) => {
            io.say(&if o.json { json(Some(false), Vec::new()) } else { "nothing was removed\n".into() });
            NOT_DONE
        }
        Some(true) => {
            let mut said = Vec::new();
            let mut ok = true;
            for r in repos.iter().filter(|r| r.acts()) {
                let (s, fine) = act(r, o, caps);
                said.extend(s);
                ok &= fine;
            }
            io.say(&if o.json { json(Some(true), said) } else { said.iter().map(|l| format!("{l}\n")).collect() });
            if ok { DONE } else { FAILED }
        }
    }
}

/// `trix worktrees …` in `cwd`. Its exit code.
fn run_with(args: &[String], cwd: &Path, caps: Caps, io: &mut dyn Io) -> i32 {
    let (cmd, o) = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            io.err(&format!("trix: {e}\n"));
            return USAGE;
        }
    };
    let seen = crate::worktree::seen();
    let now = SystemTime::now();
    let found = repos(o.all && !matches!(cmd, Cmd::Rm(_)), cwd, &seen);
    if found.is_empty() {
        io.err(&format!("trix: {} isn't in a git repository{}\n", crate::app::tilde(cwd), if o.all { ", and no other repository trix knows of was found" } else { ": try --all" }));
        return FAILED;
    }
    match cmd {
        Cmd::List => {
            let all: Vec<Repo> = found.iter().map(|m| repo(m, &seen, caps, now, &o)).collect();
            if o.json {
                io.say(&(serde_json::to_string_pretty(&JsonOut { repos: &all, done: None, said: Vec::new() }).unwrap_or_default() + "\n"));
            } else {
                io.say(&all.iter().map(|r| say(r, &o, "prune")).collect::<Vec<_>>().join("\n"));
            }
            DONE
        }
        Cmd::Prune => {
            let all: Vec<Repo> = found.iter().map(|m| repo(m, &seen, caps, now, &o)).collect();
            go(&all, &o, caps, io)
        }
        Cmd::Rm(name) => {
            let main = &found[0];
            let text = listing(main);
            let Some(e) = entries(&text).into_iter().find(|e| crate::worktree::managed_root(&e.path).as_deref() == Some(e.path.as_path()) && (e.path.file_name().is_some_and(|n| n == name.as_str()) || e.path == Path::new(&name))) else {
                io.err(&format!("trix: no worktree of trix's called {name} in {}: `trix worktrees` lists them\n", crate::app::tilde(main)));
                return FAILED;
            };
            let t = tree(main, &e, &text, &seen, caps, now, &o);
            if !t.plan.acts() {
                let why = match &t.plan {
                    Plan::Keep { why: Some(w) } => w.clone(),
                    _ => t.a.keeps().unwrap_or_else(|| t.state.clone()),
                };
                io.err(&format!("trix: kept {name}: {why}\n"));
                return FAILED;
            }
            let one = [Repo { repo: main.clone(), worktrees: vec![t], branches: Vec::new(), kept_tips: 0, old_tips: Vec::new() }];
            go(&one, &o, caps, io)
        }
    }
}

/// `trix worktrees …` from the command line. Its exit code.
pub fn run(args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    run_with(args, &cwd, Caps::detect(), &mut Terminal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::tests::sh;

    const CAPS: Caps = Caps { merge_tree: true };

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// A repo on main, with trix worktrees made as trix makes them: locked.
    fn repo(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("trix-worktrees-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        sh(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "first"]);
        root
    }

    fn add(root: &Path, name: &str) -> PathBuf {
        let wt = root.join(".trix/worktrees").join(name);
        sh(root, &["worktree", "add", "-q", "-b", &format!("trix/{name}"), &wt.display().to_string()]);
        sh(root, &["worktree", "lock", "--reason", crate::worktree::LOCK_REASON, &wt.display().to_string()]);
        wt
    }

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-q", "-m", file]);
    }

    /// `name` with a commit squash-merged into main.
    fn squashed(root: &Path, name: &str) -> PathBuf {
        let wt = add(root, name);
        commit(&wt, &format!("{name}.txt"), "work\n");
        sh(root, &["merge", "-q", "--squash", &format!("trix/{name}")]);
        sh(root, &["commit", "-q", "-m", &format!("Squash {name}")]);
        wt
    }

    /// Everything said, both streams, and the answer to give.
    struct Said<F: FnMut() -> Option<bool>> {
        out: String,
        answer: F,
    }

    impl<F: FnMut() -> Option<bool>> Io for Said<F> {
        fn say(&mut self, text: &str) {
            self.out.push_str(text);
        }
        fn err(&mut self, text: &str) {
            self.out.push_str(text);
        }
        fn ask(&mut self) -> Option<bool> {
            (self.answer)()
        }
    }

    fn run_asking(root: &Path, a: &[&str], answer: impl FnMut() -> Option<bool>) -> (String, i32) {
        let mut io = Said { out: String::new(), answer };
        let code = run_with(&args(a), root, CAPS, &mut io);
        (io.out, code)
    }

    /// Run it with no age limit unless the args set one, answering `answer`.
    fn run_in(root: &Path, a: &[&str], answer: Option<bool>) -> (String, i32) {
        let mut a: Vec<&str> = a.to_vec();
        if !a.contains(&"--min-age") {
            a.extend(["--min-age", "0"]);
        }
        run_asking(root, &a, move || answer)
    }

    fn has_branch(root: &Path, b: &str) -> bool {
        !sh(root, &["branch", "--list", b]).is_empty()
    }

    #[test]
    fn the_arguments_are_read() {
        assert_eq!(parse(&args(&[])).unwrap().0, Cmd::List);
        assert_eq!(parse(&args(&["prune", "--folders", "--yes"])).unwrap().0, Cmd::Prune);
        assert_eq!(parse(&args(&["rm", "w", "--keep-branch"])).unwrap().0, Cmd::Rm("w".into()));
        assert_eq!(parse(&args(&["prune", "--min-age", "12h"])).unwrap().1.min_age, Duration::from_secs(12 * 3600));
        assert_eq!(parse(&args(&[])).unwrap().1.min_age, DAY, "a day by default");
        assert!(parse(&args(&["rm"])).is_err());
        assert!(parse(&args(&["--folders"])).is_err());
        assert!(parse(&args(&["prune", "--keep-branch"])).is_err());
        assert!(parse(&args(&["--yes"])).is_err());
        assert!(parse(&args(&["prune", "--min-age", "soon"])).is_err());
    }

    #[test]
    fn porcelain_entries_are_read() {
        let e = entries("worktree /r\nHEAD 1\nbranch refs/heads/main\n\nworktree /r/.trix/worktrees/a\nHEAD 2\nbranch refs/heads/trix/a\nlocked trix: x\nprunable gitdir file points to non-existent location\n\nworktree /r/.trix/worktrees/b\nHEAD 3\ndetached\n");
        assert_eq!(e.len(), 3);
        assert_eq!((e[1].branch.as_deref(), e[1].prunable), (Some("trix/a"), true));
        assert_eq!((e[2].branch.as_deref(), e[2].prunable), (None, false));
    }

    #[test]
    fn prune_removes_a_clean_merged_worktree_and_its_branch() {
        crate::worktree::pretend_nobody();
        let root = repo("merged");
        let wt = squashed(&root, "done");
        std::fs::write(wt.join(".env"), "A=1\n").unwrap();
        let (list, code) = run_in(&root, &[], None);
        assert_eq!(code, DONE);
        assert!(list.contains("1 worktree · 1 can go") && list.contains("done  trix/done") && list.contains("· locked → prune removes it and trix/done (with its ignored .env)"), "{list}");
        let (out, code) = run_in(&root, &["prune", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        assert!(out.contains("\ndone: removed the worktree and trix/done ("), "{out}");
        assert!(!wt.exists() && !has_branch(&root, "trix/done"));
        assert!(sh(&root, &["for-each-ref", "refs/trix/removed/"]).contains("refs/trix/removed/trix/done/"), "its tip is kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_drops_a_trix_locked_entry_whose_folder_is_gone() {
        crate::worktree::pretend_nobody();
        let root = repo("missing");
        let merged = squashed(&root, "gone");
        let unmerged = add(&root, "lost");
        commit(&unmerged, "lost.txt", "unmerged\n");
        std::fs::remove_dir_all(&merged).unwrap();
        std::fs::remove_dir_all(&unmerged).unwrap();
        // Someone else's entry with its folder gone is theirs.
        let theirs = root.join(".trix/worktrees/theirs");
        sh(&root, &["worktree", "add", "-q", "-b", "trix/theirs", &theirs.display().to_string()]);
        std::fs::remove_dir_all(&theirs).unwrap();
        let (list, _) = run_in(&root, &[], None);
        assert!(list.contains("folder missing · locked → prune drops git's entry for it and trix/gone"), "{list}");
        assert!(list.lines().any(|l| l.trim_start().starts_with("lost ") && l.ends_with("→ prune drops git's entry for it")), "unmerged: the entry goes, the branch stays: {list}");
        assert!(list.contains("kept: not locked by trix"), "{list}");
        let (out, code) = run_in(&root, &["prune", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        let left = sh(&root, &["worktree", "list", "--porcelain"]);
        assert!(!left.contains("worktrees/gone") && !left.contains("worktrees/lost"), "{left}");
        assert!(left.contains("worktrees/theirs"), "not trix's to drop");
        assert!(!has_branch(&root, "trix/gone"), "merged: deleted");
        assert!(has_branch(&root, "trix/lost"), "unmerged: kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_detached_entry_with_commits_on_no_branch_is_kept() {
        crate::worktree::pretend_nobody();
        let root = repo("missing-detached");
        let wt = add(&root, "adrift");
        sh(&wt, &["checkout", "-q", "--detach"]);
        commit(&wt, "only-here.txt", "on no branch\n");
        let sha = sh(&wt, &["rev-parse", "HEAD"]);
        // And one with nothing of its own: that entry can go.
        let empty = add(&root, "empty");
        sh(&empty, &["checkout", "-q", "--detach"]);
        std::fs::remove_dir_all(&wt).unwrap();
        std::fs::remove_dir_all(&empty).unwrap();
        let (list, _) = run_in(&root, &[], None);
        let line = list.lines().find(|l| l.trim_start().starts_with("adrift ")).unwrap_or_else(|| panic!("{list}"));
        assert!(line.contains("folder missing, 1 commit on no branch · locked → kept: its HEAD has 1 commit on no branch") && line.contains(&format!("`git branch <name> {sha}` keeps them")), "{line}");
        let (out, code) = run_in(&root, &["prune", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        let (out, code) = run_in(&root, &["rm", "adrift", "--yes"], None);
        assert_eq!(code, FAILED, "{out}");
        assert!(out.starts_with("trix: kept adrift: its HEAD has 1 commit on no branch"), "{out}");
        let left = sh(&root, &["worktree", "list", "--porcelain"]);
        assert!(left.contains("worktrees/adrift") && !left.contains("worktrees/empty"), "{left}");
        // Still reachable from the entry kept: gc keeps it.
        sh(&root, &["gc", "-q", "--prune=now"]);
        assert_eq!(sh(&root, &["cat-file", "-t", &sha]), "commit");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_deletes_merged_branches_with_no_worktree_and_old_tips() {
        crate::worktree::pretend_nobody();
        let root = repo("loose");
        let wt = squashed(&root, "old");
        sh(&root, &["worktree", "unlock", &wt.display().to_string()]);
        sh(&root, &["worktree", "remove", &wt.display().to_string()]);
        sh(&root, &["branch", "trix/unmerged", "main"]);
        sh(&root, &["switch", "-q", "trix/unmerged"]);
        commit(&root, "u.txt", "u\n");
        sh(&root, &["switch", "-q", "main"]);
        let head = sh(&root, &["rev-parse", "HEAD"]);
        let old = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() - 31 * 86400;
        let recent = old + 30 * 86400;
        sh(&root, &["update-ref", &format!("refs/trix/removed/trix/x/{old}"), &head]);
        sh(&root, &["update-ref", &format!("refs/trix/removed/trix/y/{recent}"), &head]);
        let (list, _) = run_in(&root, &[], None);
        assert!(list.contains("branches with no worktree: trix/old (") && list.contains("→ prune deletes it · trix/unmerged (1 commit not in the default branch)"), "{list}");
        assert!(list.contains("2 tips of deleted branches under refs/trix/removed/, 1 older than 30 days → prune drops them"), "{list}");
        let (out, code) = run_in(&root, &["prune", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        assert!(!has_branch(&root, "trix/old") && has_branch(&root, "trix/unmerged"));
        let tips = sh(&root, &["for-each-ref", "--format=%(refname)", "refs/trix/removed/"]);
        assert!(!tips.contains(&format!("trix/x/{old}")) && tips.contains(&format!("trix/y/{recent}")), "{tips}");
        assert!(tips.contains("refs/trix/removed/trix/old/"), "the branch just deleted keeps its tip");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folders_removes_an_unmerged_worktree_and_keeps_its_branch() {
        crate::worktree::pretend_nobody();
        let root = repo("folders");
        let wt = add(&root, "wip");
        commit(&wt, "wip.txt", "not merged\n");
        let (list, _) = run_in(&root, &["prune"], None);
        assert!(list.contains("kept: --folders removes the folder and keeps the branch") && list.contains("nothing to prune"), "{list}");
        let (list, _) = run_in(&root, &["prune", "--folders"], None);
        assert!(list.contains("prune removes the folder, keeping trix/wip"), "{list}");
        let (out, code) = run_in(&root, &["prune", "--folders", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        assert!(out.contains("kept trix/wip (1 commit not in main)"), "{out}");
        assert!(!wt.exists() && has_branch(&root, "trix/wip"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_holds_work_or_is_used_is_never_pruned() {
        let root = repo("refusals");
        let used = squashed(&root, "used");
        let dirty = squashed(&root, "dirty");
        std::fs::write(dirty.join("a.txt"), "changed\n").unwrap();
        let orphan = squashed(&root, "orphan");
        sh(&orphan, &["checkout", "-q", "--detach"]);
        commit(&orphan, "o.txt", "on no branch\n");
        let foreign = squashed(&root, "foreign");
        sh(&root, &["worktree", "unlock", &foreign.display().to_string()]);
        sh(&root, &["worktree", "lock", "--reason", "my backup", &foreign.display().to_string()]);
        let young = squashed(&root, "young");
        crate::worktree::pretend_users("sidebar", "trix-2", &used.join("src"));
        let (list, _) = run_asking(&root, &["prune"], || panic!("nothing to ask"));
        let line = |n: &str| list.lines().find(|l| l.trim_start().starts_with(&format!("{n} "))).unwrap_or_else(|| panic!("{n}: {list}")).to_string();
        assert!(line("used").contains("in use: trix-2 in session sidebar · locked → kept"), "{list}");
        assert!(line("dirty").contains("1 uncommitted file · locked → kept"), "{list}");
        assert!(line("orphan").contains("1 commit on no branch · locked → kept"), "{list}");
        assert!(line("foreign").contains("locked by someone else (my backup) → kept"), "{list}");
        assert!(line("young").contains("kept: used ") && line("young").contains("under --min-age 1d"), "{list}");
        assert!(list.ends_with("nothing to prune\n"), "{list}");
        for (n, why) in [("used", "trix-2 in session sidebar"), ("dirty", "uncommitted"), ("orphan", "on no branch"), ("foreign", "my backup")] {
            let (out, code) = run_in(&root, &["rm", n, "--yes"], None);
            assert_eq!(code, FAILED, "{out}");
            assert!(out.starts_with(&format!("trix: kept {n}: ")) && out.contains(why), "{out}");
        }
        crate::worktree::pretend_nobody();
        for wt in [&used, &dirty, &orphan, &foreign, &young] {
            assert!(wt.exists());
        }
        assert_eq!(sh(&root, &["worktree", "list", "--porcelain"]).matches("locked my backup").count(), 1, "their lock stays");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn without_a_terminal_it_is_a_dry_run_unless_yes() {
        crate::worktree::pretend_nobody();
        let root = repo("dry");
        let wt = squashed(&root, "done");
        let (out, code) = run_in(&root, &["prune"], None);
        assert_eq!(code, NOT_DONE);
        assert!(out.contains("dry run: nothing was removed"), "{out}");
        assert!(wt.exists());
        let (out, code) = run_in(&root, &["prune"], Some(false));
        assert!(out.ends_with("\nnothing was removed\n") && code == NOT_DONE, "no is the default: {out}");
        let (out, code) = run_in(&root, &["rm", "done"], None);
        assert_eq!(code, NOT_DONE, "{out}");
        assert!(wt.exists());
        let (out, code) = run_in(&root, &["prune", "--json"], Some(true));
        assert_eq!(code, NOT_DONE, "--json never asks: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["done"], false);
        assert_eq!(v["repos"][0]["worktrees"][0]["plan"]["action"], "remove");
        assert!(wt.exists());
        let (out, code) = run_in(&root, &["rm", "done"], Some(true));
        assert_eq!(code, DONE, "{out}");
        assert!(!wt.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rm_removes_one_and_can_keep_its_branch() {
        crate::worktree::pretend_nobody();
        let root = repo("rm");
        let wt = squashed(&root, "one");
        let wip = add(&root, "wip");
        commit(&wip, "w.txt", "unmerged\n");
        let (out, code) = run_in(&root, &["rm", "one", "--keep-branch", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        assert!(out.contains("kept trix/one (as asked)"), "{out}");
        assert!(!wt.exists() && has_branch(&root, "trix/one"));
        // An unmerged one goes as X takes it: the folder, not the branch.
        let (out, code) = run_in(&root, &["rm", "wip", "--yes"], None);
        assert_eq!(code, DONE, "{out}");
        assert!(!wip.exists() && has_branch(&root, "trix/wip"));
        let (out, code) = run_in(&root, &["rm", "nope", "--yes"], None);
        assert_eq!(code, FAILED);
        assert!(out.contains("no worktree of trix's called nope"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_worktree_used_again_after_the_plan_is_left_alone() {
        crate::worktree::pretend_nobody();
        let root = repo("again");
        let wt = squashed(&root, "back");
        let back = wt.clone();
        let answer = move || {
            // While you read the plan, an agent comes back to it.
            crate::worktree::pretend_users("default", "back", &back);
            Some(true)
        };
        let (out, code) = run_asking(&root, &["prune", "--min-age", "0"], answer);
        crate::worktree::pretend_nobody();
        assert_eq!(code, DONE);
        assert!(out.contains("back: left alone, it changed since the plan: in use: back in session default"), "{out}");
        assert!(wt.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_listing_is_json_for_scripts() {
        crate::worktree::pretend_nobody();
        let root = repo("json");
        squashed(&root, "done");
        let (out, code) = run_in(&root, &["--json"], None);
        assert_eq!(code, DONE);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let t = &v["repos"][0]["worktrees"][0];
        assert_eq!((t["name"].as_str(), t["branch"].as_str(), t["lock"].as_str(), t["missing"].as_bool()), (Some("done"), Some("trix/done"), Some("trix"), Some(false)));
        assert!(t["state"].as_str().unwrap().starts_with("clean, "), "{t}");
        assert!(t["last_used_seconds"].as_u64().is_some());
        assert!(v.get("done").is_none(), "a listing does nothing");
        let _ = std::fs::remove_dir_all(&root);
    }
}
