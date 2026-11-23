//! What a worktree holds, before anything of it is removed: uncommitted
//! work, commits on no branch, and whether its branches' content is in the
//! repository's default branch. "Merged" is judged by content, not only by
//! ancestry, so a branch that was squash-merged (as this repository does)
//! counts: worktrunk's "integrated" checks, minus the trivial ones.
//!
//! Offline: no fetch, no gh. The base is the default branch and its
//! remote-tracking ref as of the last fetch, never the main checkout's
//! current branch, which may be some feature branch. Blocking: run it off
//! the workspace's thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How a branch's content came to be in the base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merged {
    /// The base has its tip.
    Ancestor,
    /// Merging it into the base adds nothing (`merge-tree`, git 2.38+).
    SameContent,
    /// One commit of the base makes exactly its changes: a squash merge or
    /// a cherry-pick. That commit.
    Squashed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Merged { into: String, how: Merged },
    /// Its commits the base doesn't have, by ancestry.
    Unmerged { ahead: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub sha: String,
    pub state: State,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Assessment {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    /// HEAD is on no branch.
    pub detached: bool,
    /// Commits only HEAD has: on no branch, remote or tag.
    pub orphans: u32,
    /// The branch checked out, then the `trix/<name>` the worktree was made
    /// on if that's another one and still there.
    pub branches: Vec<Branch>,
    /// What "merged" was measured against: the default branch, then its
    /// remote-tracking ref. Empty: the repository has none.
    pub bases: Vec<String>,
}

impl Assessment {
    pub fn uncommitted(&self) -> usize {
        self.staged + self.unstaged + self.untracked
    }

    /// Why the worktree must stay, if it must: work git can't give back.
    pub fn keeps(&self) -> Option<String> {
        if self.uncommitted() > 0 {
            let mut bits = Vec::new();
            for (n, what) in [(self.staged, "staged"), (self.unstaged, "changed"), (self.untracked, "untracked")] {
                if n > 0 {
                    bits.push(format!("{n} {what}"));
                }
            }
            return Some(format!("it has uncommitted work ({}): commit it first, or it'd be lost", bits.join(", ")));
        }
        if self.orphans > 0 {
            let s = if self.orphans == 1 { "" } else { "s" };
            return Some(format!("HEAD has {} commit{s} on no branch: make a branch first (`git branch <name>`), or they'd be lost", self.orphans));
        }
        None
    }
}

/// What this git can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// `git merge-tree --write-tree`, from git 2.38.
    pub merge_tree: bool,
}

impl Caps {
    pub fn detect() -> Caps {
        let v = git(Path::new("."), &["--version"]).unwrap_or_default();
        Caps { merge_tree: version_at_least(&v, (2, 38)) }
    }
}

/// `git version 2.34.1` (or `2.39.3 (Apple Git-146)`) is at least `want`.
fn version_at_least(text: &str, want: (u32, u32)) -> bool {
    let Some(v) = text.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit())) else { return false };
    let mut parts = v.split('.').map(|p| p.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= want,
        _ => false,
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("git failed").to_string())
    }
}

/// `git <args> | git patch-id --stable`, in `dir`.
fn patch_ids(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut make = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("git: {e}"))?;
    let diff = make.stdout.take().ok_or("git: no output")?;
    let out = Command::new("git").arg("-C").arg(dir).args(["patch-id", "--stable"]).stdin(diff).stderr(Stdio::null()).output().map_err(|e| format!("git: {e}"))?;
    let made = make.wait().map_err(|e| format!("git: {e}"))?;
    if !made.success() || !out.status.success() {
        return Err("git couldn't make a patch-id".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn exists(dir: &Path, rev: &str) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).is_ok()
}

/// The repository's default branch: origin/HEAD's local name, else main or
/// master. Then its remote-tracking ref, if there is one.
pub fn bases(dir: &Path) -> Vec<String> {
    let remote = git(dir, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]).ok().filter(|r| !r.is_empty());
    let named = remote.as_deref().and_then(|r| r.strip_prefix("origin/")).map(String::from);
    let local = named.into_iter().chain(["main".to_string(), "master".to_string()]).find(|b| exists(dir, &format!("refs/heads/{b}")));
    let mut out = Vec::new();
    if let Some(b) = &local {
        out.push(b.clone());
    }
    let tracking = remote.or_else(|| local.as_ref().map(|b| format!("origin/{b}"))).filter(|r| exists(dir, &format!("refs/remotes/{r}")));
    out.extend(tracking);
    out
}

/// How `branch` stands against `base`.
fn against(dir: &Path, branch: &str, base: &str, caps: Caps) -> Result<Option<Merged>, String> {
    if git(dir, &["merge-base", "--is-ancestor", branch, base]).is_ok() {
        return Ok(Some(Merged::Ancestor));
    }
    if caps.merge_tree {
        // Exit 1 is a conflict: not the same content, and not an error.
        if let Ok(out) = git(dir, &["merge-tree", "--write-tree", base, branch])
            && out.lines().next() == Some(git(dir, &["rev-parse", &format!("{base}^{{tree}}")])?.as_str())
        {
            return Ok(Some(Merged::SameContent));
        }
    }
    let Ok(mb) = git(dir, &["merge-base", base, branch]) else { return Ok(None) };
    let ours = patch_ids(dir, &["diff", "--no-color", "--no-ext-diff", &mb, branch])?;
    let Some(ours) = ours.split_whitespace().next() else { return Ok(None) };
    // Bounded: a branch from long ago against years of the base.
    let theirs = patch_ids(dir, &["log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "--max-count=5000", &format!("{mb}..{base}")])?;
    Ok(theirs.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some(ours)).then(|| w.next().map(String::from)).flatten()
    }).map(Merged::Squashed))
}

fn branch_state(dir: &Path, name: &str, bases: &[String], caps: Caps) -> Result<Branch, String> {
    let sha = git(dir, &["rev-parse", &format!("refs/heads/{name}")])?;
    for base in bases {
        if let Some(how) = against(dir, &sha, base, caps)? {
            return Ok(Branch { name: name.into(), sha, state: State::Merged { into: base.clone(), how } });
        }
    }
    let ahead = bases.first().and_then(|b| git(dir, &["rev-list", "--count", &format!("{b}..{sha}")]).ok()).and_then(|n| n.parse().ok()).unwrap_or(0);
    Ok(Branch { name: name.into(), sha, state: State::Unmerged { ahead } })
}

/// `git status --porcelain`'s counts: staged, changed, untracked.
fn counts(status: &str) -> (usize, usize, usize) {
    let (mut staged, mut unstaged, mut untracked) = (0, 0, 0);
    for l in status.lines().filter(|l| l.len() >= 2) {
        let b = l.as_bytes();
        if &b[..2] == b"??" {
            untracked += 1;
            continue;
        }
        staged += usize::from(b[0] != b' ');
        unstaged += usize::from(b[1] != b' ');
    }
    (staged, unstaged, untracked)
}

/// The `trix/<name>` a worktree under `.trix/worktrees/<name>` was made on.
fn made_on(top: &Path) -> Option<String> {
    crate::worktree::managed_root(top)?.file_name().map(|n| format!("trix/{}", n.to_string_lossy()))
}

pub fn assess(path: &Path) -> Result<Assessment, String> {
    assess_with(path, Caps::detect())
}

pub fn assess_with(path: &Path, caps: Caps) -> Result<Assessment, String> {
    let top = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"]).map_err(|_| "not a git repository".to_string())?);
    let (staged, unstaged, untracked) = counts(&git(&top, &["status", "--porcelain", "--ignore-submodules=none"])?);
    let current = git(&top, &["branch", "--show-current"]).ok().filter(|b| !b.is_empty());
    let detached = current.is_none();
    let orphans = if detached { git(&top, &["rev-list", "--count", "HEAD", "--not", "--branches", "--remotes", "--tags"]).ok().and_then(|n| n.parse().ok()).unwrap_or(0) } else { 0 };
    let bases = bases(&top);
    let mut names: Vec<String> = current.into_iter().collect();
    if let Some(m) = made_on(&top).filter(|m| !names.contains(m) && exists(&top, &format!("refs/heads/{m}"))) {
        names.push(m);
    }
    let mut branches = Vec::new();
    // A branch isn't measured against itself, or against one it's the base of.
    for name in names.iter().filter(|n| !bases.iter().any(|b| b == *n)) {
        branches.push(branch_state(&top, name, &bases, caps)?);
    }
    Ok(Assessment { staged, unstaged, untracked, detached, orphans, branches, bases })
}

/// What a deletion is said as: how it was merged.
pub fn why(state: &State) -> String {
    match state {
        State::Merged { into, how: Merged::Ancestor } => format!("merged into {into}"),
        State::Merged { into, how: Merged::SameContent } => format!("its changes are all in {into}"),
        State::Merged { into, how: Merged::Squashed(c) } => format!("squash-merged into {into} as {}", &c[..c.len().min(7)]),
        State::Unmerged { ahead } => format!("{ahead} commit{} not in the default branch", if *ahead == 1 { "" } else { "s" }),
    }
}

/// Where deleted branches' tips are logged.
fn log_file() -> PathBuf {
    crate::worktree::state_dir().join("removed.log")
}

/// Delete a merged branch, after keeping its tip under
/// `refs/trix/removed/<branch>/<unix time>` (so its commits stay reachable
/// whatever gc does) and in `removed.log`. `branch -D`: `-d` refuses a
/// squash-merged branch, and the content check said it's in. What was done.
pub fn delete_merged(main: &Path, b: &Branch) -> Result<String, String> {
    let State::Merged { .. } = &b.state else { return Err(format!("{} isn't merged", b.name)) };
    // It moved since it was looked at: what's there now wasn't judged.
    let now_at = git(main, &["rev-parse", &format!("refs/heads/{}", b.name)])?;
    if now_at != b.sha {
        return Err(format!("{} moved since it was checked", b.name));
    }
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
    let backup = format!("refs/trix/removed/{}/{time}", b.name);
    git(main, &["update-ref", &backup, &b.sha, ""]).map_err(|e| format!("couldn't keep {}'s tip, so it stays: {e}", b.name))?;
    let why = why(&b.state);
    let entry = serde_json::json!({ "time": time, "repo": main.display().to_string(), "branch": b.name, "sha": b.sha, "why": why });
    let logged = std::fs::create_dir_all(crate::worktree::state_dir()).and_then(|()| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(log_file())?;
        writeln!(f, "{entry}")
    });
    if let Err(e) = logged {
        eprintln!("trix: couldn't log the removal of {}: {e}", b.name);
    }
    git(main, &["branch", "-D", &b.name])?;
    Ok(format!("{} ({why}; `git branch {} {}` brings it back)", b.name, b.name, &b.sha[..b.sha.len().min(12)]))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repo on main with `a.txt`, and a trix worktree on trix/w.
    fn repo(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("trix-cleanup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        sh(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "1\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "first"]);
        let wt = root.join(".trix/worktrees/w");
        sh(&root, &["worktree", "add", "-q", "-b", "trix/w", &wt.display().to_string()]);
        (root, wt)
    }

    fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-q", "-m", msg]);
    }

    const ALL: Caps = Caps { merge_tree: true };
    const OLD_GIT: Caps = Caps { merge_tree: false };

    fn state(wt: &Path, caps: Caps) -> State {
        let a = assess_with(wt, caps).unwrap();
        assert_eq!(a.branches[0].name, "trix/w");
        a.branches[0].state.clone()
    }

    /// The worktree's two commits, squashed onto main as one.
    fn squash(root: &Path) {
        sh(root, &["merge", "-q", "--squash", "trix/w"]);
        sh(root, &["commit", "-q", "-m", "Squashed (#1)"]);
    }

    fn two_commits(wt: &Path) {
        commit(wt, "a.txt", "1\nTWO\n3\n4\n5\n6\n7\n8\n", "two");
        commit(wt, "b.txt", "new\n", "b");
    }

    #[test]
    fn a_plain_merge_is_merged_by_ancestry() {
        let (root, wt) = repo("plain");
        two_commits(&wt);
        assert_eq!(state(&wt, ALL), State::Unmerged { ahead: 2 });
        sh(&root, &["merge", "-q", "--no-ff", "-m", "merge", "trix/w"]);
        assert_eq!(state(&wt, ALL), State::Merged { into: "main".into(), how: Merged::Ancestor });
    }

    #[test]
    fn a_squash_merge_is_merged() {
        let (root, wt) = repo("squash");
        two_commits(&wt);
        squash(&root);
        assert_eq!(state(&wt, ALL), State::Merged { into: "main".into(), how: Merged::SameContent });
        let squashed = sh(&root, &["rev-parse", "HEAD"]);
        assert_eq!(state(&wt, OLD_GIT), State::Merged { into: "main".into(), how: Merged::Squashed(squashed) }, "without merge-tree, the patch-id finds it");
    }

    #[test]
    fn a_squash_merge_is_found_after_main_moved_on_in_the_same_lines() {
        let (root, wt) = repo("moved-on");
        two_commits(&wt);
        squash(&root);
        let squashed = sh(&root, &["rev-parse", "HEAD"]);
        commit(&root, "a.txt", "1\nzwei\n3\n4\n5\n6\n7\n8\n", "later");
        let out = Command::new("git").arg("-C").arg(&root).args(["merge-tree", "--write-tree", "main", "trix/w"]).output().unwrap();
        assert!(!out.status.success(), "merge-tree sees a conflict");
        assert_eq!(state(&wt, ALL), State::Merged { into: "main".into(), how: Merged::Squashed(squashed) });
    }

    #[test]
    fn a_cherry_pick_is_merged() {
        let (root, wt) = repo("pick");
        commit(&wt, "a.txt", "1\n2\n3\nFOUR\n5\n6\n7\n8\n", "four");
        commit(&root, "c.txt", "main's own\n", "other");
        sh(&root, &["cherry-pick", "trix/w"]);
        let picked = sh(&root, &["rev-parse", "HEAD"]);
        assert_eq!(state(&wt, OLD_GIT), State::Merged { into: "main".into(), how: Merged::Squashed(picked) });
        assert!(matches!(state(&wt, ALL), State::Merged { .. }));
    }

    #[test]
    fn a_partial_squash_is_unmerged() {
        let (root, wt) = repo("partial");
        two_commits(&wt);
        // Only the first of its two commits reaches main.
        commit(&root, "a.txt", "1\nTWO\n3\n4\n5\n6\n7\n8\n", "part of it");
        assert_eq!(state(&wt, ALL), State::Unmerged { ahead: 2 });
        assert_eq!(state(&wt, OLD_GIT), State::Unmerged { ahead: 2 });
    }

    #[test]
    fn a_squash_edited_before_merging_is_unmerged() {
        let (root, wt) = repo("edited");
        two_commits(&wt);
        sh(&root, &["merge", "-q", "--squash", "trix/w"]);
        std::fs::write(root.join("a.txt"), "1\nTwo, edited\n3\n4\n5\n6\n7\n8\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "Squashed, edited"]);
        assert_eq!(state(&wt, ALL), State::Unmerged { ahead: 2 });
        assert_eq!(state(&wt, OLD_GIT), State::Unmerged { ahead: 2 });
    }

    #[test]
    fn the_remote_tracking_default_branch_counts_too() {
        let (root, wt) = repo("tracking");
        two_commits(&wt);
        // Squashed on GitHub and fetched, but main not pulled.
        let bare = root.join("origin.git");
        sh(&root, &["init", "-q", "--bare", &bare.display().to_string()]);
        sh(&root, &["remote", "add", "origin", &bare.display().to_string()]);
        sh(&root, &["push", "-q", "origin", "main"]);
        let side = root.join("side");
        sh(&root, &["worktree", "add", "-q", "--detach", &side.display().to_string(), "main"]);
        sh(&side, &["merge", "-q", "--squash", "trix/w"]);
        sh(&side, &["commit", "-q", "-m", "Squashed on the server"]);
        sh(&side, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
        sh(&root, &["fetch", "-q", "origin"]);
        sh(&root, &["remote", "set-head", "origin", "main"]);
        assert_eq!(bases(&wt), ["main", "origin/main"]);
        assert!(matches!(state(&wt, ALL), State::Merged { into, .. } if into == "origin/main"));
    }

    #[test]
    fn uncommitted_work_and_orphan_commits_keep_the_worktree() {
        let (_root, wt) = repo("keeps");
        std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
        std::fs::write(wt.join("new.txt"), "untracked\n").unwrap();
        std::fs::write(wt.join("staged.txt"), "staged\n").unwrap();
        sh(&wt, &["add", "staged.txt"]);
        let a = assess_with(&wt, ALL).unwrap();
        assert_eq!((a.staged, a.unstaged, a.untracked), (1, 1, 1));
        assert!(a.keeps().unwrap().contains("1 staged, 1 changed, 1 untracked"));
        sh(&wt, &["checkout", "-q", "--", "a.txt"]);
        sh(&wt, &["rm", "-q", "--cached", "staged.txt"]);
        std::fs::remove_file(wt.join("staged.txt")).unwrap();
        std::fs::remove_file(wt.join("new.txt")).unwrap();
        // Detached, with two commits no branch has.
        sh(&wt, &["checkout", "-q", "--detach"]);
        commit(&wt, "a.txt", "orphan\n", "one");
        commit(&wt, "a.txt", "orphan 2\n", "two");
        let a = assess_with(&wt, ALL).unwrap();
        assert!(a.detached && a.orphans == 2, "{a:?}");
        assert!(a.keeps().unwrap().contains("2 commits on no branch"));
        assert_eq!(a.branches.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["trix/w"], "the branch it was made on is still looked at");
        // On a branch's commit, detached isn't a reason to keep it.
        sh(&wt, &["checkout", "-q", "--detach", "trix/w"]);
        assert_eq!(assess_with(&wt, ALL).unwrap().keeps(), None);
    }

    #[test]
    fn the_branch_it_was_made_on_is_looked_at_too() {
        let (root, wt) = repo("made-on");
        two_commits(&wt);
        squash(&root);
        sh(&wt, &["switch", "-q", "-c", "feature"]);
        commit(&wt, "f.txt", "more\n", "more");
        let a = assess_with(&wt, ALL).unwrap();
        let got: Vec<(&str, bool)> = a.branches.iter().map(|b| (b.name.as_str(), matches!(b.state, State::Merged { .. }))).collect();
        assert_eq!(got, [("feature", false), ("trix/w", true)]);
    }

    #[test]
    fn git_versions_are_read() {
        assert!(!version_at_least("git version 2.34.1", (2, 38)));
        assert!(version_at_least("git version 2.38.0", (2, 38)));
        assert!(version_at_least("git version 2.39.3 (Apple Git-146)", (2, 38)));
        assert!(version_at_least("git version 3.0", (2, 38)));
        assert!(!version_at_least("garbage", (2, 38)));
    }

    #[test]
    fn a_deleted_branch_is_kept_under_a_hidden_ref_and_logged() {
        let (root, wt) = repo("deleted");
        two_commits(&wt);
        squash(&root);
        let b = assess_with(&wt, ALL).unwrap().branches.remove(0);
        sh(&root, &["worktree", "remove", &wt.display().to_string()]);
        let said = delete_merged(&root, &b).unwrap();
        assert!(said.starts_with("trix/w (its changes are all in main; `git branch trix/w "), "{said}");
        assert!(sh(&root, &["branch", "--list", "trix/w"]).is_empty(), "gone");
        let refs = sh(&root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/trix/removed/"]);
        assert!(refs.starts_with("refs/trix/removed/trix/w/") && refs.ends_with(&b.sha), "{refs}");
        let log = std::fs::read_to_string(log_file()).unwrap();
        let entry: serde_json::Value = log.lines().rev().filter_map(|l| serde_json::from_str(l).ok()).find(|e: &serde_json::Value| e["repo"] == root.display().to_string()).expect("logged");
        assert_eq!((entry["branch"].as_str(), entry["sha"].as_str()), (Some("trix/w"), Some(b.sha.as_str())));
        assert!(entry["time"].as_u64().is_some() && entry["why"].as_str().unwrap().contains("main"));
        // And it comes back the way it says.
        sh(&root, &["branch", "trix/w", &b.sha]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unmerged_or_moved_branch_is_never_deleted() {
        let (root, wt) = repo("not-deleted");
        two_commits(&wt);
        let b = assess_with(&wt, ALL).unwrap().branches.remove(0);
        assert!(delete_merged(&root, &b).is_err());
        squash(&root);
        let b = assess_with(&wt, ALL).unwrap().branches.remove(0);
        commit(&wt, "late.txt", "after the check\n", "late");
        assert!(delete_merged(&root, &b).unwrap_err().contains("moved"));
        assert!(!sh(&root, &["branch", "--list", "trix/w"]).is_empty());
    }
}
