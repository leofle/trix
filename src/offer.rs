//! Offering to clean up a worktree as the last agent in it closes: the
//! close dialog's extra line and `r`, the toast an exited agent's close
//! leaves, and `trix close --clean`. Built on `worktree::users`,
//! `cleanup::assess` and `cleanup::remove`, the path the review's X and
//! `trix worktrees` take: nothing here removes anything another way.
//! Quit, `trix kill`, detach and crashes never get here.

use std::path::{Path, PathBuf};

/// What a closing agent's worktree came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Clean, and its branch (none: a detached HEAD with nothing of its
    /// own) is in `into`: the folder and the branch can go.
    Merged { branch: Option<String>, into: String },
    /// Clean, but `branch` has commits `base` hasn't: the folder can go,
    /// the branch stays.
    Unmerged { branch: String, ahead: u32, base: String },
    /// It stays, and why: work in it, or someone else in it.
    Stays(String),
}

/// A worktree looked at, as its last agent closes.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub tree: PathBuf,
    pub main: PathBuf,
    pub verdict: Verdict,
}

impl Found {
    pub fn name(&self) -> String {
        self.tree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn removable(&self) -> bool {
        !matches!(self.verdict, Verdict::Stays(_))
    }

    pub fn merged(&self) -> bool {
        matches!(self.verdict, Verdict::Merged { .. })
    }
}

/// How `a` and its users come out. `users`: everyone but the agents
/// closing, from `worktree::users`.
fn verdict(a: &crate::cleanup::Assessment, users: &[String]) -> Verdict {
    if !users.is_empty() {
        return Verdict::Stays(format!("it's also used by {}", users.join("; ")));
    }
    if let Some(why) = a.keeps() {
        return Verdict::Stays(why);
    }
    let Some(base) = a.bases.first() else { return Verdict::Stays("the repository has no main or master to tell if it's merged".into()) };
    match a.branches.first() {
        _ if a.detached => Verdict::Merged { branch: None, into: base.clone() },
        Some(b) => match &b.state {
            crate::cleanup::State::Merged { into, .. } => Verdict::Merged { branch: Some(b.name.clone()), into: into.clone() },
            crate::cleanup::State::Unmerged { ahead } => Verdict::Unmerged { branch: b.name.clone(), ahead: *ahead, base: base.clone() },
        },
        None => Verdict::Stays("it has no branch to tell if it's merged".into()),
    }
}

/// Look at `tree` now. Blocking: git and every session are asked. `users`
/// says who else uses it (`worktree::users_later`). `None`: it isn't a
/// worktree of a repository any more.
pub fn look(tree: &Path, users: impl FnOnce() -> Vec<String>) -> Option<Found> {
    let main = crate::app::main_checkout(tree)?;
    let users = users();
    let a = crate::cleanup::assess(tree).ok()?;
    Some(Found { tree: tree.to_path_buf(), main, verdict: verdict(&a, &users) })
}

/// The close dialog's line about one worktree.
pub fn line(f: &Found) -> String {
    match &f.verdict {
        Verdict::Merged { branch: Some(b), into } => format!("Its worktree is clean and merged into {into}: r also removes it and {b}."),
        Verdict::Merged { branch: None, into } => format!("Its worktree is clean, and its HEAD is in {into}: r also removes it."),
        Verdict::Unmerged { branch, ahead, base } => {
            let s = if *ahead == 1 { "" } else { "s" };
            format!("Its worktree is clean; {branch} has {ahead} commit{s} not in {base}: r also removes the folder, keeping {branch}.")
        }
        Verdict::Stays(why) => format!("Its worktree {} stays: {why}.", f.name()),
    }
}

/// The many-close dialog's line: how many of their worktrees `r` removes.
pub fn summary(found: &[Found]) -> Option<String> {
    let merged = found.iter().filter(|f| f.merged()).count();
    let unmerged = found.iter().filter(|f| matches!(f.verdict, Verdict::Unmerged { .. })).count();
    let them = |n: usize| if n == 1 { "1 of their worktrees is".to_string() } else { format!("{n} of their worktrees are") };
    match (merged, unmerged) {
        (0, 0) => None,
        (m, 0) => Some(format!("{} clean and merged: r also removes {}.", them(m), if m == 1 { "it" } else { "them" })),
        (0, u) => Some(format!("{} clean but not merged: r also removes the folder{}, keeping the branch{}.", them(u), if u == 1 { "" } else { "s" }, if u == 1 { "" } else { "es" })),
        (m, u) => Some(format!("{} clean and merged, {u} more clean but not merged: r also removes them (the unmerged ones' branches stay).", them(m))),
    }
}

/// The toast an exited agent's close leaves when its worktree could go.
pub fn kept_toast(f: &Found) -> Option<String> {
    let how = match &f.verdict {
        Verdict::Merged { .. } => "clean, merged".to_string(),
        Verdict::Unmerged { ahead, base, .. } => format!("clean, {ahead} commit{} not in {base}", if *ahead == 1 { "" } else { "s" }),
        Verdict::Stays(_) => return None,
    };
    let name = f.name();
    Some(format!("kept worktree {name} ({how}): alt+v X, or `trix worktrees rm {name}`"))
}

/// Remove `f`'s worktree, looking again first: who uses it (`users`), what
/// it holds. The folder, and its branches that are merged. Blocking.
pub fn remove(f: &Found, users: impl FnOnce() -> Vec<String>) -> Result<String, String> {
    let users = users();
    let a = crate::cleanup::assess(&f.tree)?;
    if let Verdict::Stays(why) = verdict(&a, &users) {
        return Err(format!("kept worktree {}: {why}", f.name()));
    }
    crate::cleanup::remove(&f.main, &f.tree, &a, false, None).map(|said| format!("{}: {said}", f.name()))
}
