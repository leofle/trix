//! The Folder field of the new-agent form: the path being typed, completed
//! against the folders on disk, and folders proposed from what trix already
//! knows (where its agents are, where you started some before, the
//! repositories beside this one).

use std::path::{Component, Path, PathBuf};

use crate::app::{expand, tilde};

/// The most the list holds. Past that you type another letter, not scroll.
const MOST: usize = 50;
/// The proposals shown before anything is typed: ↓ walks through them to
/// reach the next field.
const UNASKED: usize = 8;
/// The entries read of one folder, so a huge one can't stall a keystroke.
const READ: usize = 5000;

/// Why a folder is on the list, said beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// It completes the path typed.
    Path,
    /// An agent is in it now.
    Open,
    /// An agent was started in it before.
    Recent,
    /// A git repository: this one, or one beside it.
    Repo,
}

impl Why {
    pub fn label(self) -> &'static str {
        match self {
            Why::Path => "",
            Why::Open => "open",
            Why::Recent => "recent",
            Why::Repo => "repo",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// What the field would hold: a completion ends in `/`, ready for the
    /// next part; a proposal is a whole folder and doesn't.
    pub text: String,
    pub why: Why,
}

/// What tab did in the folder field.
#[derive(Debug, PartialEq, Eq)]
pub enum Tab {
    /// The field takes this text.
    Fill(String),
    /// Several folders fit and none further: the first on the list is chosen.
    Chose,
    /// Nothing to complete: on to the next field.
    Next,
}

pub struct Folders {
    /// Where a relative path starts from: the folder the form opened on.
    base: PathBuf,
    proposals: Vec<(PathBuf, Why)>,
    pub list: Vec<Suggestion>,
    /// The one ↑↓ is on; `None` is the field itself.
    pub sel: Option<usize>,
    /// The first on screen when not all fit; drawing keeps it.
    pub top: std::cell::Cell<usize>,
    /// The field was edited. Until then its text is where the form opened,
    /// not something to match folders against.
    typed: bool,
}

impl Folders {
    pub fn new(base: PathBuf, proposals: Vec<(PathBuf, Why)>, text: &str) -> Self {
        let mut f = Folders { base, proposals, list: Vec::new(), sel: None, top: Default::default(), typed: false };
        f.build(text);
        f
    }

    /// The folder `text` names: `~` is home, and a relative path starts from
    /// where the form opened.
    pub fn resolve(&self, text: &str) -> PathBuf {
        tidy(&self.base.join(expand(text)))
    }

    /// The field's text changed: the list follows it.
    pub fn edited(&mut self, text: &str) {
        self.typed = true;
        self.sel = None;
        self.top.set(0);
        self.build(text);
    }

    pub fn chosen(&self) -> Option<&Suggestion> {
        self.sel.and_then(|i| self.list.get(i))
    }

    /// ↓ and ↑: from the field into the list and back. False when the move
    /// leaves the field: ↑ on the field itself, ↓ past the last folder.
    pub fn step(&mut self, down: bool) -> bool {
        let next = match (self.sel, down) {
            (None, true) if !self.list.is_empty() => Some(0),
            (Some(i), true) if i + 1 < self.list.len() => Some(i + 1),
            (Some(i), false) => i.checked_sub(1),
            _ => {
                self.sel = None;
                return false;
            }
        };
        self.sel = next;
        true
    }

    /// Tab, as a shell has it: the folder chosen on the list; else as much
    /// of the name as every folder that fits agrees on, and the whole of it
    /// with its `/` when only one does. With nothing to add, a path that is
    /// a folder is done, and tab moves on; one that isn't yet picks from
    /// the list.
    pub fn tab(&mut self, text: &str) -> Tab {
        if let Some(s) = self.chosen() {
            return Tab::Fill(s.text.clone());
        }
        let text = text.trim();
        if !self.typed || text.is_empty() {
            return Tab::Next;
        }
        if !text.ends_with('/') {
            let fits = self.completions(text);
            if let [one] = &fits[..] {
                return Tab::Fill(one.clone());
            }
            let common = common_prefix(&fits);
            if common.chars().count() > text.chars().count() {
                return Tab::Fill(common);
            }
        }
        if self.resolve(text).is_dir() || self.list.is_empty() {
            return Tab::Next;
        }
        self.sel = Some(0);
        Tab::Chose
    }

    fn build(&mut self, text: &str) {
        let text = text.trim();
        let asked = self.typed && !text.is_empty();
        let here = (!text.is_empty()).then(|| self.resolve(text));
        let mut list: Vec<Suggestion> = Vec::new();
        if asked {
            // A folder named in full is no news under itself; tab still
            // gives it its `/`.
            let fits = self.completions(text).into_iter().filter(|c| Some(self.resolve(c)) != here);
            list.extend(fits.take(MOST).map(|text| Suggestion { text, why: Why::Path }));
        }
        let on_disk: Vec<PathBuf> = list.iter().map(|s| self.resolve(&s.text)).collect();
        let query = text.trim_end_matches('/').to_lowercase();
        let mut known: Vec<(u8, Suggestion)> = Vec::new();
        for (path, why) in &self.proposals {
            // Not the folder already in the field, nor one listed above.
            if here.as_ref() == Some(path) || on_disk.contains(path) {
                continue;
            }
            let shown = tilde(path);
            let rank = if !asked {
                0
            } else {
                let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                // The folder's own name first, and its start before its middle.
                if name.starts_with(&query) {
                    0
                } else if name.contains(&query) {
                    1
                } else if shown.to_lowercase().contains(&query) || path.to_string_lossy().to_lowercase().contains(&query) {
                    2
                } else {
                    continue;
                }
            };
            known.push((rank, Suggestion { text: shown, why: *why }));
        }
        known.sort_by_key(|(rank, _)| *rank);
        list.extend(known.into_iter().map(|(_, s)| s));
        list.truncate(if self.typed { MOST } else { UNASKED });
        self.list = list;
    }

    /// The folders on disk that the last part of `text` starts: what the
    /// field would hold for each, `/` at the end. Hidden ones only for a
    /// part that starts with a dot. A name's case counts, unless no folder
    /// fits then: `proj` finds `Projects`.
    fn completions(&self, text: &str) -> Vec<String> {
        if text == "~" {
            return vec!["~/".into()];
        }
        let (head, part) = text.rfind('/').map_or(("", text), |i| text.split_at(i + 1));
        let Ok(entries) = std::fs::read_dir(self.resolve(head)) else { return Vec::new() };
        let low = part.to_lowercase();
        let (mut exact, mut loose) = (Vec::new(), Vec::new());
        for e in entries.take(READ).flatten() {
            let Ok(name) = e.file_name().into_string() else { continue };
            if (name.starts_with('.') && !part.starts_with('.')) || !name.to_lowercase().starts_with(&low) {
                continue;
            }
            // A link counts when it leads to a folder.
            let folder = match e.file_type() {
                Ok(t) if t.is_symlink() => e.path().is_dir(),
                Ok(t) => t.is_dir(),
                Err(_) => false,
            };
            if folder {
                let fits = if name.starts_with(part) { &mut exact } else { &mut loose };
                fits.push(name);
            }
        }
        let mut names = if exact.is_empty() { loose } else { exact };
        names.sort_by_key(|n| n.to_lowercase());
        names.into_iter().map(|n| format!("{head}{n}/")).collect()
    }
}

/// The folders to propose from `dir`, the best guess first: where agents
/// are open, where some were started before, then this repository's main
/// checkout and the repositories beside it. Each once, and only those still
/// there. It costs a stat per recent folder and per neighbour of the
/// repository, and runs no git.
pub fn proposals(dir: &Path, open: &[PathBuf], recent: &[String]) -> Vec<(PathBuf, Why)> {
    let mut all: Vec<(PathBuf, Why)> = open.iter().map(|p| (tidy(p), Why::Open)).collect();
    all.extend(recent.iter().map(PathBuf::from).filter(|p| p.is_dir()).map(|p| (p, Why::Recent)));
    // An agent in a trix worktree: its repository is a likely place too.
    all.extend(open.iter().filter_map(|p| checkout_of(p)).map(|p| (p, Why::Repo)));
    if let Some(repo) = repo_of(dir) {
        let mut beside: Vec<PathBuf> = repo
            .parent()
            .and_then(|p| std::fs::read_dir(p).ok())
            .into_iter()
            .flat_map(|d| d.take(READ).flatten())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .filter(|p| *p != repo && p.join(".git").exists())
            .collect();
        beside.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        all.push((repo, Why::Repo));
        all.extend(beside.into_iter().map(|p| (p, Why::Repo)));
    }
    let mut seen = std::collections::HashSet::new();
    all.retain(|(p, _)| seen.insert(p.clone()));
    all
}

/// The main checkout of the repository `dir` is in, by the `.git` above it
/// alone. Home is not looked at, nor above it: a home kept in git would
/// make every user's folder a neighbour.
fn repo_of(dir: &Path) -> Option<PathBuf> {
    let home = expand("~");
    let top = dir.ancestors().take_while(|a| *a != home && a.parent().is_some()).find(|a| a.join(".git").exists())?;
    Some(checkout_of(top).unwrap_or_else(|| top.to_path_buf()))
}

/// The checkout a trix worktree (`<repo>/.trix/worktrees/<name>`) belongs to.
fn checkout_of(dir: &Path) -> Option<PathBuf> {
    let at = dir.ancestors().find(|a| a.ends_with(".trix/worktrees"))?;
    Some(at.parent()?.parent()?.to_path_buf())
}

/// `path` without its `.`, `..` and doubled or trailing slashes, as written:
/// links are not followed, so `/tmp` stays `/tmp`.
fn tidy(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            // `/..` is `/`.
            Component::ParentDir if out.has_root() => {}
            c => out.push(c),
        }
    }
    out
}

/// What every one of `all` starts with.
fn common_prefix(all: &[String]) -> String {
    let Some(first) = all.first() else { return String::new() };
    let n = all.iter().map(|s| first.chars().zip(s.chars()).take_while(|(a, b)| a == b).count()).min().unwrap_or(0);
    first.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own with these folders in it, and a file.
    fn tree(dirs: &[&str]) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("folders-test-{}-{n}", std::process::id()));
        for d in dirs {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("alpha.txt"), "").unwrap();
        root
    }

    fn texts(f: &Folders) -> Vec<&str> {
        f.list.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn a_typed_path_lists_the_folders_that_complete_it() {
        let root = tree(&["alpha", "alps/high", "beta", ".hidden", "Gamma"]);
        let r = root.display().to_string();
        let mut f = Folders::new(root.clone(), Vec::new(), &r);
        assert!(f.list.is_empty(), "nothing typed, nothing to propose");
        // Folders only, in order, each ready for its next part.
        f.edited(&format!("{r}/al"));
        assert_eq!(texts(&f), [format!("{r}/alpha/"), format!("{r}/alps/")], "not alpha.txt");
        // A path ending in / lists what's in it, hidden folders left out
        // until a dot asks for them.
        f.edited(&format!("{r}/"));
        assert_eq!(texts(&f), [format!("{r}/alpha/"), format!("{r}/alps/"), format!("{r}/beta/"), format!("{r}/Gamma/")]);
        f.edited(&format!("{r}/."));
        assert_eq!(texts(&f), [format!("{r}/.hidden/")]);
        // Case counts, unless nothing fits then.
        f.edited(&format!("{r}/g"));
        assert_eq!(texts(&f), [format!("{r}/Gamma/")]);
        // A relative path starts from where the form opened, and keeps the
        // text as typed.
        f.edited("al");
        assert_eq!(texts(&f), ["alpha/", "alps/"]);
        f.edited("./alps/");
        assert_eq!(texts(&f), ["./alps/high/"]);
        f.edited("alps/../b");
        assert_eq!(texts(&f), ["alps/../beta/"]);
        assert_eq!(f.resolve("alps/../beta/"), root.join("beta"));
        // The folder named in full isn't listed under itself, only the one
        // it starts the name of.
        f.edited(&format!("{r}/alp"));
        assert_eq!(texts(&f), [format!("{r}/alpha/"), format!("{r}/alps/")]);
        f.edited(&format!("{r}/alps"));
        assert!(f.list.is_empty());
        f.edited(&format!("{r}/nothing/here"));
        assert!(f.list.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn home_is_a_tilde() {
        let home = expand("~");
        let mut f = Folders::new(PathBuf::from("/"), Vec::new(), "/");
        assert_eq!(f.resolve("~"), tidy(&home));
        // Home itself isn't listed under `~`; tab gives it its `/`.
        f.edited("~");
        assert!(f.list.is_empty());
        assert_eq!(f.tab("~"), Tab::Fill("~/".into()));
        // What's in home is listed as `~/…`, as typed.
        f.edited("~/");
        assert!(f.list.iter().all(|s| s.text.starts_with("~/") && s.text.ends_with('/')), "{:?}", f.list);
    }

    #[test]
    fn tab_completes_as_far_as_the_folders_agree() {
        let root = tree(&["alpha", "alps", "beta/one", "beta/two", "solo", "solo-two"]);
        let r = root.display().to_string();
        let mut f = Folders::new(root.clone(), Vec::new(), &r);
        // Untouched, the field has nothing to complete: tab is the form's.
        assert_eq!(f.tab(&r), Tab::Next);
        let tab = |f: &mut Folders, text: &str| {
            f.edited(text);
            f.tab(text)
        };
        // One folder fits: all of it, and the / to go on from.
        assert_eq!(tab(&mut f, &format!("{r}/b")), Tab::Fill(format!("{r}/beta/")));
        // Several: as far as they agree.
        assert_eq!(tab(&mut f, &format!("{r}/a")), Tab::Fill(format!("{r}/alp")));
        // Nothing more to add and not a folder yet: the list's first is
        // chosen, and tab again takes it.
        assert_eq!(tab(&mut f, &format!("{r}/alp")), Tab::Chose);
        assert_eq!(f.sel, Some(0));
        assert_eq!(f.tab(&format!("{r}/alp")), Tab::Fill(format!("{r}/alpha/")));
        // A folder, whole: tab moves on, though it starts another's name
        // and has folders in it.
        assert_eq!(tab(&mut f, &format!("{r}/solo")), Tab::Next);
        assert_eq!(tab(&mut f, &format!("{r}/beta/")), Tab::Next);
        assert_eq!(f.sel, None);
        // No such folder, and nothing to offer.
        assert_eq!(tab(&mut f, &format!("{r}/zzz")), Tab::Next);
        // A different case is put right.
        assert_eq!(tab(&mut f, &format!("{r}/BET")), Tab::Fill(format!("{r}/beta/")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn proposals_show_unasked_and_narrow_as_you_type() {
        let root = tree(&["work/trix", "work/atlas", "work/trinket", "old/trix-notes"]);
        let at = |p: &str| root.join(p);
        let known = vec![(at("work/atlas"), Why::Open), (at("work/trix"), Why::Open), (at("old/trix-notes"), Why::Recent), (at("work/trinket"), Why::Repo)];
        let shown = |p: &str| tilde(&at(p));
        let mut f = Folders::new(at("work/trix"), known, &shown("work/trix"));
        // Before anything is typed: all of them but the folder the field
        // already holds, in the order given.
        assert_eq!(texts(&f), [shown("work/atlas"), shown("old/trix-notes"), shown("work/trinket")]);
        assert_eq!(f.list[1].why, Why::Recent);
        // A word narrows them by name: its start first, then anywhere in
        // the name, then anywhere in the path.
        f.edited("tri");
        assert_eq!(texts(&f), [shown("work/trix"), shown("old/trix-notes"), shown("work/trinket")]);
        f.edited("not");
        assert_eq!(texts(&f), [shown("old/trix-notes")]);
        f.edited("WORK");
        assert_eq!(texts(&f), [shown("work/atlas"), shown("work/trix"), shown("work/trinket")], "by the path, whatever the case");
        // A folder on disk that completes the path comes first, and isn't
        // said twice for being known too.
        let typed = format!("{}/at", at("work").display());
        f.edited(&typed);
        assert_eq!(f.list, [Suggestion { text: format!("{}/atlas/", at("work").display()), why: Why::Path }]);
        // The field emptied: everything known, where the form opened too.
        f.edited("");
        assert_eq!(f.list.len(), 4);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn up_and_down_go_through_the_list_and_out_of_it() {
        let root = tree(&["a", "b"]);
        let mut f = Folders::new(root.clone(), vec![(root.join("a"), Why::Open), (root.join("b"), Why::Recent)], "");
        assert!(!f.step(false), "up from the field leaves it");
        assert!(f.step(true));
        assert_eq!(f.chosen().map(|s| s.why), Some(Why::Open));
        assert!(f.step(true));
        assert_eq!(f.sel, Some(1));
        assert!(f.step(false) && f.step(false), "back up to the field");
        assert_eq!(f.sel, None);
        f.sel = Some(1);
        assert!(!f.step(true), "down past the last leaves the field");
        assert_eq!(f.sel, None);
        // Typing puts the choice down.
        f.sel = Some(0);
        f.edited("zzz");
        assert_eq!(f.sel, None);
        // With no list, both leave.
        assert!(!f.step(true) && !f.step(false));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn proposals_are_open_then_recent_then_the_repositories_around() {
        let root = tree(&["code/trix/.git", "code/trix/.trix/worktrees/fix/src", "code/atlas/.git", "code/notes", "code/.cache/.git", "code/zeta/.git", "elsewhere"]);
        let at = |p: &str| root.join(p);
        let recent = [at("elsewhere").display().to_string(), at("gone").display().to_string(), at("code/atlas").display().to_string()];
        // From inside a worktree: its repository is the main checkout.
        let got = proposals(&at("code/trix/.trix/worktrees/fix/src"), &[at("code/trix/.trix/worktrees/fix"), at("elsewhere")], &recent);
        assert_eq!(
            got,
            [
                (at("code/trix/.trix/worktrees/fix"), Why::Open),
                (at("elsewhere"), Why::Open),
                // Once, as the first thing it is; the folder that's gone, not at all.
                (at("code/atlas"), Why::Recent),
                (at("code/trix"), Why::Repo),
                // Beside it: git repositories only, hidden ones left out.
                (at("code/zeta"), Why::Repo),
            ]
        );
        // Not in a repository: only what's open and recent.
        assert_eq!(proposals(&at("elsewhere"), &[], &recent[..1]), [(at("elsewhere"), Why::Recent)]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paths_are_tidied_as_written() {
        for (from, to) in [("/a/b/../c/", "/a/c"), ("/a/./b", "/a/b"), ("/..", "/"), ("/a//b", "/a/b"), ("../x", "../x")] {
            assert_eq!(tidy(Path::new(from)), Path::new(to), "{from}");
        }
        assert_eq!(common_prefix(&["ab/".into(), "abc/".into()]), "ab");
        assert_eq!(common_prefix(&[]), "");
    }
}
