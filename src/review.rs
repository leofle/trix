//! The diff review (alt+v): one agent's changes, line by line, with notes
//! you leave on lines and send back to it as one prompt; and the git steps
//! that follow a review, run from trix: commit, push, open a PR, merge, and
//! clean up the worktree.
//!
//! Reading the diff never writes the index (see `app::DIFF_STAT`): plumbing
//! (`diff-index`) for tracked files, and untracked ones read off disk. The
//! git steps run in the background, one at a time per review, and report
//! back with `AppEvent::Review`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::app::TextInput;

/// What a line of the review is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A file's header: its path, and what happened to it.
    File,
    /// `@@ -a,b +c,d @@ …`
    Hunk,
    Context,
    Add,
    Del,
    /// Something about a file that isn't a line of it: binary, renamed,
    /// mode changed, too long to show.
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// The line itself, without its `+`/`-`/` ` (a header's text for the rest).
    pub text: String,
    /// Which file it belongs to, by number in `Review::files`.
    pub file: usize,
    /// Its number in the old file and in the new one, where it has one.
    pub old: Option<u32>,
    pub new: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: String,
    /// "new", "deleted", "renamed from x", "untracked"…; empty for a plain change.
    pub status: String,
    pub added: u32,
    pub removed: u32,
}

/// Parse `git diff`-style patch text (from `diff-index -p`) into files and
/// lines. Files with nothing to show (a stat-only change, which plumbing
/// reports with no hunks) are left out.
pub fn parse(text: &str) -> (Vec<File>, Vec<Line>) {
    let mut files: Vec<File> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    // The current file's lines, kept back until it proves to have something.
    let mut cur: Option<(File, Vec<Line>)> = None;
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    let flush = |cur: &mut Option<(File, Vec<Line>)>, files: &mut Vec<File>, lines: &mut Vec<Line>| {
        if let Some((f, ls)) = cur.take() {
            // The header alone is a stat-only change: nothing happened.
            if ls.len() > 1 || !f.status.is_empty() {
                let n = files.len();
                files.push(f);
                lines.extend(ls.into_iter().map(|l| Line { file: n, ..l }));
            }
        }
    };
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            flush(&mut cur, &mut files, &mut lines);
            in_hunk = false;
            let path = rest.rsplit_once(" b/").map(|(_, b)| b).unwrap_or(rest).to_string();
            let f = File { path: path.clone(), status: String::new(), added: 0, removed: 0 };
            cur = Some((f, vec![Line { kind: Kind::File, text: path, file: 0, old: None, new: None }]));
            continue;
        }
        let Some((f, ls)) = cur.as_mut() else { continue };
        let meta = |ls: &mut Vec<Line>, text: String| ls.push(Line { kind: Kind::Meta, text, file: 0, old: None, new: None });
        if raw.starts_with("@@") {
            in_hunk = true;
            // @@ -a[,b] +c[,d] @@
            let mut parts = raw.split_whitespace().skip(1);
            let start = |s: Option<&str>| s.and_then(|s| s[1..].split(',').next()?.parse::<u32>().ok()).unwrap_or(1);
            old = start(parts.next());
            new = start(parts.next());
            ls.push(Line { kind: Kind::Hunk, text: raw.to_string(), file: 0, old: Some(old), new: Some(new) });
            continue;
        }
        if in_hunk {
            match raw.as_bytes().first() {
                Some(b'+') => {
                    ls.push(Line { kind: Kind::Add, text: raw[1..].to_string(), file: 0, old: None, new: Some(new) });
                    new += 1;
                    f.added += 1;
                }
                Some(b'-') => {
                    ls.push(Line { kind: Kind::Del, text: raw[1..].to_string(), file: 0, old: Some(old), new: None });
                    old += 1;
                    f.removed += 1;
                }
                Some(b' ') | None => {
                    let text = raw.get(1..).unwrap_or("").to_string();
                    ls.push(Line { kind: Kind::Context, text, file: 0, old: Some(old), new: Some(new) });
                    old += 1;
                    new += 1;
                }
                // "\ No newline at end of file"
                _ => meta(ls, raw.trim_start_matches("\\ ").to_string()),
            }
            continue;
        }
        if raw.starts_with("new file mode") {
            f.status = "new".into();
        } else if raw.starts_with("deleted file mode") {
            f.status = "deleted".into();
        } else if let Some(from) = raw.strip_prefix("rename from ") {
            f.status = format!("renamed from {from}");
        } else if let Some(m) = raw.strip_prefix("new mode ") {
            if f.status.is_empty() {
                f.status = format!("mode {m}");
            }
        } else if raw.starts_with("Binary files") {
            if f.status.is_empty() {
                f.status = "binary".into();
            }
            meta(ls, "binary file".into());
        }
    }
    flush(&mut cur, &mut files, &mut lines);
    (files, lines)
}

/// The most lines of one untracked file shown, and the biggest one read.
const UNTRACKED_LINES: usize = 2000;
const UNTRACKED_BYTES: u64 = 512 * 1024;

/// An untracked file, as the diff of adding it.
fn untracked(dir: &Path, path: &str, n: usize) -> (File, Vec<Line>) {
    let mut f = File { path: path.to_string(), status: "untracked".into(), added: 0, removed: 0 };
    let mut ls = vec![Line { kind: Kind::File, text: path.to_string(), file: n, old: None, new: None }];
    let full = dir.join(path);
    let meta = |text: &str| Line { kind: Kind::Meta, text: text.into(), file: n, old: None, new: None };
    match std::fs::metadata(&full) {
        Ok(m) if m.len() > UNTRACKED_BYTES => ls.push(meta("too big to show")),
        Ok(m) if m.is_file() => {
            let bytes = std::fs::read(&full).unwrap_or_default();
            if bytes[..bytes.len().min(8000)].contains(&0) {
                ls.push(meta("binary file"));
            } else {
                let text = String::from_utf8_lossy(&bytes);
                let all: Vec<&str> = text.lines().collect();
                for (i, l) in all.iter().take(UNTRACKED_LINES).enumerate() {
                    ls.push(Line { kind: Kind::Add, text: l.to_string(), file: n, old: None, new: Some(i as u32 + 1) });
                }
                f.added = all.len() as u32;
                if all.len() > UNTRACKED_LINES {
                    ls.push(meta(&format!("{} more lines", all.len() - UNTRACKED_LINES)));
                }
            }
        }
        _ => ls.push(meta("not a regular file")),
    }
    (f, ls)
}

/// Run git in `dir`: its output, or what it said went wrong.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run(Command::new("git").arg("-C").arg(dir).args(args))
}

fn run(cmd: &mut Command) -> Result<String, String> {
    // Nothing may stop to ask: no terminal is there to answer.
    let out = cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        let err = text(&out.stderr);
        let err = if err.is_empty() { text(&out.stdout) } else { err };
        Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string())
    }
}

/// Where a folder sits in its repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The folder's own top level.
    pub top: PathBuf,
    /// The main checkout, which a worktree's branch merges into.
    pub main: PathBuf,
    pub branch: Option<String>,
    /// The main checkout's branch, when the folder is a worktree of it.
    pub base: Option<String>,
    /// The folder is a linked worktree, not the main checkout.
    pub worktree: bool,
    /// Commits on `branch` that `base` doesn't have.
    pub ahead: Option<u32>,
}

pub fn place(dir: &Path) -> Result<Place, String> {
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"]).map_err(|_| "not a git repository".to_string())?);
    // The first worktree listed is the main checkout.
    let list = git(&top, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let main = list.lines().find_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from).unwrap_or_else(|| top.clone());
    let same = |a: &Path, b: &Path| a.canonicalize().ok() == b.canonicalize().ok();
    let worktree = !same(&top, &main);
    let branch = git(&top, &["branch", "--show-current"]).ok().filter(|b| !b.is_empty());
    let base = if worktree { git(&main, &["branch", "--show-current"]).ok().filter(|b| !b.is_empty()) } else { None };
    let ahead = match (&base, &branch) {
        (Some(base), Some(_)) => git(&top, &["rev-list", "--count", &format!("{base}..HEAD")]).ok().and_then(|n| n.parse().ok()),
        _ => None,
    };
    Ok(Place { top, main, branch, base, worktree, ahead })
}

/// Everything uncommitted in `dir`, against HEAD, untracked files included.
pub fn load(dir: &Path) -> Result<(Vec<File>, Vec<Line>), String> {
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"]).map_err(|_| "not a git repository".to_string())?);
    // No HEAD yet (a fresh repository): only untracked files to show.
    let patch = git(&top, &["diff-index", "-p", "-M", "--no-color", "--no-ext-diff", "HEAD"]).unwrap_or_default();
    let (mut files, mut lines) = parse(&patch);
    let others = git(&top, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    for path in others.split('\0').filter(|p| !p.is_empty()) {
        let (f, ls) = untracked(&top, path, files.len());
        files.push(f);
        lines.extend(ls);
    }
    Ok((files, lines))
}

/// Where a note is left: the line it was on, found again after a reload by
/// its file, its text, and the number nearest the one it had.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub path: String,
    pub kind: Kind,
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub anchor: Anchor,
    /// The line it is on now.
    pub line: usize,
    pub body: String,
}

/// A git step a review can take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Stage everything and commit it with this message.
    Commit(String),
    Push,
    /// Push, then `gh pr create --fill`.
    Pr,
    /// Merge the branch into the main checkout's branch, there.
    Merge,
    /// `gh pr merge --merge` the branch's pull request.
    MergePr,
    /// Remove the worktree, and its branch if it's merged.
    Cleanup,
}

impl Op {
    pub fn doing(&self) -> &'static str {
        match self {
            Op::Commit(_) => "committing",
            Op::Push => "pushing",
            Op::Pr => "opening a pull request",
            Op::Merge => "merging",
            Op::MergePr => "merging the pull request",
            Op::Cleanup => "cleaning up",
        }
    }
}

/// What the review is doing with keys.
pub enum Mode {
    Browse,
    /// Writing a note on the line under the cursor.
    Note(TextInput),
    /// Writing the commit message.
    Commit(TextInput),
    /// A step waiting for a yes, and the button lit.
    Confirm(Op, usize),
}

pub struct Review {
    pub agent: u32,
    pub dir: PathBuf,
    pub place: Place,
    pub files: Vec<File>,
    pub lines: Vec<Line>,
    pub cursor: usize,
    /// The first row shown, kept by drawing so the view scrolls only when
    /// the cursor would leave it.
    pub top: std::cell::Cell<usize>,
    /// How many rows the last frame showed, for page up and down.
    pub page: std::cell::Cell<usize>,
    pub notes: Vec<Note>,
    pub mode: Mode,
    /// A git step under way.
    pub busy: Option<Op>,
    /// The last thing to say, and whether it went well.
    pub status: Option<(String, bool)>,
}

impl Review {
    pub fn open(agent: u32, dir: &Path) -> Result<Review, String> {
        let place = place(dir)?;
        let (files, lines) = load(dir)?;
        Ok(Review {
            agent,
            dir: dir.to_path_buf(),
            place,
            files,
            lines,
            cursor: 0,
            top: Default::default(),
            page: std::cell::Cell::new(20),
            notes: Vec::new(),
            mode: Mode::Browse,
            busy: None,
            status: None,
        })
    }

    /// Read the diff again, and put the notes back on their lines.
    pub fn reload(&mut self) -> Result<(), String> {
        self.place = place(&self.dir)?;
        let (files, lines) = load(&self.dir)?;
        self.set_diff(files, lines);
        Ok(())
    }

    /// A new diff: notes follow their lines, and go when theirs did.
    pub fn set_diff(&mut self, files: Vec<File>, lines: Vec<Line>) {
        let at = self.anchor(self.cursor);
        self.files = files;
        self.lines = lines;
        let mut notes = std::mem::take(&mut self.notes);
        notes.retain_mut(|n| match self.find(&n.anchor) {
            Some(i) => {
                n.line = i;
                true
            }
            None => false,
        });
        notes.sort_by_key(|n| n.line);
        self.notes = notes;
        self.cursor = at.and_then(|a| self.find(&a)).unwrap_or(0).min(self.lines.len().saturating_sub(1));
    }

    fn anchor(&self, i: usize) -> Option<Anchor> {
        let l = self.lines.get(i)?;
        Some(Anchor { path: self.files.get(l.file)?.path.clone(), kind: l.kind, text: l.text.clone(), old: l.old, new: l.new })
    }

    /// The line an anchor is on now: same file, kind and text, the nearest
    /// to where it was.
    fn find(&self, a: &Anchor) -> Option<usize> {
        let near = |l: &Line| {
            let d = |x: Option<u32>, y: Option<u32>| match (x, y) {
                (Some(x), Some(y)) => x.abs_diff(y),
                _ => 0,
            };
            d(l.new, a.new).max(d(l.old, a.old))
        };
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == a.kind && l.text == a.text && self.files.get(l.file).is_some_and(|f| f.path == a.path))
            .min_by_key(|(_, l)| near(l))
            .map(|(i, _)| i)
    }

    pub fn note_at(&self, line: usize) -> Option<&Note> {
        self.notes.iter().find(|n| n.line == line)
    }

    /// Leave a note on a line, or change the one there; an empty one
    /// takes it away.
    pub fn set_note(&mut self, line: usize, body: &str) {
        let body = body.trim();
        self.notes.retain(|n| n.line != line);
        if body.is_empty() {
            return;
        }
        if let Some(anchor) = self.anchor(line) {
            self.notes.push(Note { anchor, line, body: body.to_string() });
            self.notes.sort_by_key(|n| n.line);
        }
    }

    /// Move the cursor by `by` lines, within the diff.
    pub fn step(&mut self, by: isize) {
        let last = self.lines.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + by).clamp(0, last) as usize;
    }

    /// To the next file's header, or the previous one's.
    pub fn next_file(&mut self, forward: bool) {
        let heads = self.lines.iter().enumerate().filter(|(_, l)| l.kind == Kind::File).map(|(i, _)| i);
        let to = if forward { heads.into_iter().find(|&i| i > self.cursor) } else { heads.into_iter().rev().find(|&i| i < self.cursor) };
        if let Some(i) = to {
            self.cursor = i;
        }
    }

    /// To the next note, or the previous one, round at the ends.
    pub fn next_note(&mut self, forward: bool) {
        let at: Vec<usize> = self.notes.iter().map(|n| n.line).collect();
        let to = if forward {
            at.iter().find(|&&i| i > self.cursor).or(at.first())
        } else {
            at.iter().rev().find(|&&i| i < self.cursor).or(at.last())
        };
        if let Some(&i) = to {
            self.cursor = i;
        }
    }

    pub fn added(&self) -> u32 {
        self.files.iter().map(|f| f.added).sum()
    }

    pub fn removed(&self) -> u32 {
        self.files.iter().map(|f| f.removed).sum()
    }

    /// The notes, as one prompt for the agent.
    pub fn feedback(&self) -> String {
        let n = self.notes.len();
        let mut s = format!("Review notes on your uncommitted changes ({n} note{}). Please address each one:\n", if n == 1 { "" } else { "s" });
        for note in &self.notes {
            let a = &note.anchor;
            s.push('\n');
            match a.kind {
                Kind::File | Kind::Meta => s.push_str(&format!("{} (whole file)\n", a.path)),
                Kind::Hunk => s.push_str(&format!("{}:{} (this hunk: {})\n", a.path, a.new.unwrap_or(1), a.text)),
                Kind::Del => s.push_str(&format!("{}:{} (removed line, old numbering)\n  - {}\n", a.path, a.old.unwrap_or(0), a.text.trim_end())),
                Kind::Add => s.push_str(&format!("{}:{}\n  + {}\n", a.path, a.new.unwrap_or(0), a.text.trim_end())),
                Kind::Context => s.push_str(&format!("{}:{}\n    {}\n", a.path, a.new.unwrap_or(0), a.text.trim_end())),
            }
            for l in note.body.lines() {
                s.push_str(&format!("  → {l}\n"));
            }
        }
        s.trim_end().to_string()
    }

    /// Why a step can't be taken here, before asking about it.
    pub fn refusal(&self, op: &Op) -> Option<String> {
        let p = &self.place;
        let Some(branch) = &p.branch else { return Some("HEAD is detached: check out a branch first".into()) };
        match op {
            Op::Commit(_) => self.files.is_empty().then(|| "nothing to commit".into()),
            Op::Push | Op::Pr | Op::MergePr => None,
            Op::Merge | Op::Cleanup if !p.worktree => {
                Some(format!("{} is the main checkout, not a worktree: {branch} has no checkout here to merge into", crate::app::tilde(&p.top)))
            }
            Op::Merge => match &p.base {
                None => Some("the main checkout has no branch checked out to merge into".into()),
                Some(base) if base == branch => Some(format!("the main checkout is on {branch} itself")),
                Some(_) if !self.files.is_empty() => Some("there are uncommitted changes: commit them first, or they'd be left out".into()),
                Some(base) if p.ahead == Some(0) => Some(format!("{base} already has everything on {branch}")),
                Some(_) => None,
            },
            Op::Cleanup => (!self.files.is_empty()).then(|| "there are uncommitted changes: commit them first, or they'd be lost".into()),
        }
    }
}

/// What a git step came to: the text to show, and whether it went well.
pub type Outcome = Result<String, String>;

/// Take a step, in `dir` (the agent's folder). Blocking: run it off the
/// workspace's thread.
pub fn perform(op: &Op, dir: &Path) -> Outcome {
    let p = place(dir)?;
    let top = &p.top;
    let branch = p.branch.clone().ok_or("HEAD is detached")?;
    match op {
        Op::Commit(msg) => {
            if msg.trim().is_empty() {
                return Err("the commit needs a message".into());
            }
            git(top, &["add", "-A"])?;
            git(top, &["commit", "-q", "-m", msg.trim()])?;
            let head = git(top, &["log", "-1", "--format=%h %s"])?;
            Ok(format!("committed {head}"))
        }
        Op::Push => push(top, &branch),
        Op::Pr => {
            push(top, &branch)?;
            let url = run(Command::new("gh").current_dir(top).args(["pr", "create", "--fill", "--head", &branch]))
                .map_err(|e| if e.contains("No such file") { "gh isn't installed: https://cli.github.com".into() } else { e })?;
            Ok(format!("opened {}", url.lines().last().unwrap_or(&url)))
        }
        Op::MergePr => {
            run(Command::new("gh").current_dir(top).args(["pr", "merge", &branch, "--merge"]))
                .map_err(|e| if e.contains("No such file") { "gh isn't installed: https://cli.github.com".into() } else { e })?;
            Ok(format!("merged the pull request for {branch}"))
        }
        Op::Merge => {
            let base = p.base.clone().ok_or("the main checkout has no branch checked out")?;
            if !p.worktree {
                return Err("this folder is the main checkout, not a worktree".into());
            }
            // Never merge into a checkout someone is in the middle of.
            if !git(&p.main, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
                return Err(format!("{} has uncommitted changes on {base}: merge there by hand", crate::app::tilde(&p.main)));
            }
            let msg = format!("Merge branch '{branch}'");
            if let Err(e) = git(&p.main, &["merge", "--no-ff", "--no-edit", "-m", &msg, &branch]) {
                let _ = git(&p.main, &["merge", "--abort"]);
                return Err(format!("merge stopped, nothing changed: {e}"));
            }
            Ok(format!("merged {branch} into {base}"))
        }
        Op::Cleanup => {
            if !p.worktree {
                return Err("this folder is the main checkout, not a worktree".into());
            }
            // No --force: git keeps a worktree with changes in it.
            git(&p.main, &["worktree", "remove", &top.display().to_string()])?;
            match git(&p.main, &["branch", "-d", &branch]) {
                Ok(_) => Ok(format!("removed the worktree and {branch}")),
                Err(_) => Ok(format!("removed the worktree; kept {branch}, which isn't merged yet")),
            }
        }
    }
}

fn push(top: &Path, branch: &str) -> Outcome {
    let remotes = git(top, &["remote"])?;
    let remote = if remotes.lines().any(|r| r == "origin") { "origin" } else { remotes.lines().next().ok_or("no remote to push to")? };
    // Named both sides: a branch that tracks another (origin/main) still
    // goes to its own name.
    git(top, &["push", "-u", remote, &format!("{branch}:{branch}")])?;
    Ok(format!("pushed {branch} to {remote}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@ fn main() {
 one
-two
+TWO
+three
 four
diff --git a/stat-only.txt b/stat-only.txt
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
\\ No newline at end of file
diff --git a/old.rs b/moved.rs
similarity index 100%
rename from old.rs
rename to moved.rs
diff --git a/logo.png b/logo.png
index 4444444..5555555 100644
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn a_patch_parses_into_files_and_numbered_lines() {
        let (files, lines) = parse(PATCH);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        // The stat-only entry has nothing to show and is left out.
        assert_eq!(paths, ["src/a.rs", "new.txt", "moved.rs", "logo.png"]);
        assert_eq!((files[0].added, files[0].removed), (2, 1));
        assert_eq!(files[1].status, "new");
        assert_eq!(files[2].status, "renamed from old.rs");
        assert_eq!(files[3].status, "binary");
        let a: Vec<(Kind, &str, Option<u32>, Option<u32>)> =
            lines.iter().filter(|l| l.file == 0).map(|l| (l.kind, l.text.as_str(), l.old, l.new)).collect();
        assert_eq!(a, [
            (Kind::File, "src/a.rs", None, None),
            (Kind::Hunk, "@@ -1,3 +1,4 @@ fn main() {", Some(1), Some(1)),
            (Kind::Context, "one", Some(1), Some(1)),
            (Kind::Del, "two", Some(2), None),
            (Kind::Add, "TWO", None, Some(2)),
            (Kind::Add, "three", None, Some(3)),
            (Kind::Context, "four", Some(3), Some(4)),
        ]);
        let new: Vec<(Kind, &str)> = lines.iter().filter(|l| l.file == 1).map(|l| (l.kind, l.text.as_str())).collect();
        assert_eq!(new[2..], [(Kind::Add, "hello"), (Kind::Meta, "No newline at end of file")]);
        assert!(lines.iter().any(|l| l.file == 3 && l.kind == Kind::Meta && l.text == "binary file"));
    }

    fn review(patch: &str) -> Review {
        let (files, lines) = parse(patch);
        let place = Place { top: "/r".into(), main: "/r".into(), branch: Some("trix/x".into()), base: None, worktree: false, ahead: None };
        Review {
            agent: 1,
            dir: "/r".into(),
            place,
            files,
            lines,
            cursor: 0,
            top: Default::default(),
            page: std::cell::Cell::new(10),
            notes: Vec::new(),
            mode: Mode::Browse,
            busy: None,
            status: None,
        }
    }

    #[test]
    fn notes_become_one_prompt_naming_each_line() {
        let mut r = review(PATCH);
        r.set_note(4, "why upper case?");
        r.set_note(0, "split this file up");
        r.set_note(3, "  ");
        assert_eq!(r.notes.len(), 2, "an empty note is no note");
        let text = r.feedback();
        assert!(text.starts_with("Review notes on your uncommitted changes (2 notes)"), "{text}");
        assert!(text.contains("src/a.rs (whole file)\n  → split this file up"), "{text}");
        assert!(text.contains("src/a.rs:2\n  + TWO\n  → why upper case?"), "{text}");
        // The file note comes first: notes go in the diff's order.
        assert!(text.find("whole file").unwrap() < text.find("TWO").unwrap());
        // Writing over a note changes it; an empty one takes it away.
        r.set_note(4, "fine");
        assert_eq!(r.note_at(4).map(|n| n.body.as_str()), Some("fine"));
        r.set_note(4, "");
        assert!(r.note_at(4).is_none());
    }

    #[test]
    fn notes_follow_their_lines_through_a_reload_and_go_with_them() {
        let mut r = review(PATCH);
        r.set_note(5, "keep three");
        r.set_note(3, "two was right");
        r.cursor = 5;
        // Two lines land above both, and the deleted line comes back.
        let moved = PATCH.replace("@@ -1,3 +1,4 @@ fn main() {\n one\n-two\n", "@@ -1,3 +1,6 @@ fn main() {\n+zero\n+half\n one\n two\n");
        let (files, lines) = parse(&moved);
        r.set_diff(files, lines);
        assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
        let n = &r.notes[0];
        assert_eq!((r.lines[n.line].text.as_str(), n.body.as_str()), ("three", "keep three"));
        assert_eq!(r.cursor, n.line, "the cursor stays on its line too");
    }

    #[test]
    fn moving_by_files_and_notes() {
        let mut r = review(PATCH);
        r.next_file(true);
        assert_eq!(r.files[r.lines[r.cursor].file].path, "new.txt");
        r.next_file(false);
        assert_eq!(r.cursor, 0);
        r.step(-5);
        assert_eq!(r.cursor, 0);
        r.step(1000);
        assert_eq!(r.cursor, r.lines.len() - 1);
        r.set_note(2, "a");
        r.set_note(6, "b");
        r.cursor = 0;
        r.next_note(true);
        assert_eq!(r.cursor, 2);
        r.next_note(true);
        assert_eq!(r.cursor, 6);
        r.next_note(true);
        assert_eq!(r.cursor, 2, "round at the end");
        r.next_note(false);
        assert_eq!(r.cursor, 6, "and back round");
    }

    #[test]
    fn steps_that_cannot_work_say_why_first() {
        let mut r = review(PATCH);
        assert!(r.refusal(&Op::Merge).unwrap().contains("main checkout, not a worktree"));
        assert!(r.refusal(&Op::Cleanup).unwrap().contains("not a worktree"));
        assert_eq!(r.refusal(&Op::Commit("m".into())), None);
        r.place.worktree = true;
        r.place.base = Some("main".into());
        assert!(r.refusal(&Op::Merge).unwrap().contains("uncommitted"));
        assert!(r.refusal(&Op::Cleanup).unwrap().contains("lost"));
        r.set_diff(Vec::new(), Vec::new());
        assert_eq!(r.refusal(&Op::Commit("m".into())).as_deref(), Some("nothing to commit"));
        r.place.ahead = Some(0);
        assert!(r.refusal(&Op::Merge).unwrap().contains("already has everything"));
        r.place.ahead = Some(2);
        assert_eq!(r.refusal(&Op::Merge), None);
        assert_eq!(r.refusal(&Op::Cleanup), None);
        r.place.branch = None;
        assert!(r.refusal(&Op::Push).unwrap().contains("detached"));
    }

    /// A repository with a worktree on `trix/w`, a bare `origin`, and git
    /// that never asks anything; removed when dropped.
    pub(crate) struct Repo(PathBuf);

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    pub(crate) fn repo(tag: &str) -> (Repo, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("trix-test-review-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        std::fs::create_dir_all(&main).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
        ] {
            sh(&main, args);
        }
        std::fs::write(main.join("a.txt"), "one\ntwo\n").unwrap();
        sh(&main, &["add", "-A"]);
        sh(&main, &["commit", "-q", "-m", "first"]);
        let bare = root.join("origin.git");
        sh(&root, &["init", "-q", "--bare", &bare.display().to_string()]);
        sh(&main, &["remote", "add", "origin", &bare.display().to_string()]);
        let wt = main.join(".trix/worktrees/w");
        sh(&main, &["worktree", "add", "-q", "-b", "trix/w", &wt.display().to_string()]);
        (Repo(root), main, wt)
    }

    #[test]
    fn a_worktree_is_reviewed_committed_pushed_merged_and_cleaned_up() {
        let (_keep, main, wt) = repo("flow");
        std::fs::write(wt.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(wt.join("b.txt"), "new\n").unwrap();
        let mut r = Review::open(7, &wt).unwrap();
        assert!(r.place.worktree);
        assert_eq!((r.place.branch.as_deref(), r.place.base.as_deref()), (Some("trix/w"), Some("main")));
        let paths: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "b.txt"]);
        assert_eq!(r.files[1].status, "untracked");
        assert_eq!((r.added(), r.removed()), (2, 1));
        assert!(r.refusal(&Op::Merge).is_some(), "uncommitted changes aren't merged");

        let said = perform(&Op::Commit("Shout two".into()), &wt).unwrap();
        assert!(said.starts_with("committed ") && said.ends_with("Shout two"), "{said}");
        r.reload().unwrap();
        assert!(r.files.is_empty());
        assert_eq!(r.place.ahead, Some(1));
        assert_eq!(r.refusal(&Op::Merge), None);

        assert_eq!(perform(&Op::Push, &wt).unwrap(), "pushed trix/w to origin");
        assert!(sh(&main, &["ls-remote", "origin"]).contains("refs/heads/trix/w"));

        // A main checkout with changes of its own isn't merged into.
        std::fs::write(main.join("a.txt"), "dirty\n").unwrap();
        assert!(perform(&Op::Merge, &wt).unwrap_err().contains("uncommitted changes on main"));
        sh(&main, &["checkout", "-q", "--", "a.txt"]);
        assert_eq!(perform(&Op::Merge, &wt).unwrap(), "merged trix/w into main");
        assert_eq!(std::fs::read_to_string(main.join("b.txt")).unwrap(), "new\n");

        assert_eq!(perform(&Op::Cleanup, &wt).unwrap(), "removed the worktree and trix/w");
        assert!(!wt.exists());
        assert!(git(&main, &["rev-parse", "--verify", "--quiet", "refs/heads/trix/w"]).is_err());
    }

    #[test]
    fn a_merge_that_conflicts_changes_nothing_and_an_unmerged_branch_is_kept() {
        let (_keep, main, wt) = repo("conflict");
        std::fs::write(wt.join("a.txt"), "one\nworktree\n").unwrap();
        perform(&Op::Commit("theirs".into()), &wt).unwrap();
        std::fs::write(main.join("a.txt"), "one\nmain\n").unwrap();
        sh(&main, &["commit", "-q", "-am", "ours"]);
        let head = sh(&main, &["rev-parse", "HEAD"]);
        let err = perform(&Op::Merge, &wt).unwrap_err();
        assert!(err.starts_with("merge stopped, nothing changed"), "{err}");
        assert_eq!(sh(&main, &["rev-parse", "HEAD"]), head);
        assert_eq!(sh(&main, &["status", "--porcelain", "--untracked-files=no"]), "");
        // Cleaning up keeps the branch git won't delete safely.
        assert_eq!(perform(&Op::Cleanup, &wt).unwrap(), "removed the worktree; kept trix/w, which isn't merged yet");
        assert!(git(&main, &["rev-parse", "--verify", "--quiet", "refs/heads/trix/w"]).is_ok());
    }

    #[test]
    fn a_worktree_with_changes_is_never_removed() {
        let (_keep, _main, wt) = repo("dirty");
        std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
        assert!(perform(&Op::Cleanup, &wt).is_err());
        assert!(wt.join("a.txt").exists());
    }
}
