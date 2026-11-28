//! Where workflows live: built into the binary (read-only), in the main
//! checkout's `.trix/workflows/` (the team's) or in
//! `~/.config/trix/workflows/` (yours). The team's shadows yours. A run
//! works from a snapshot taken when it starts, never from the file.

use super::model::Workflow;
use std::path::{Path, PathBuf};

/// The templates shipped in the binary.
const BUILTIN: [(&str, &str); 4] = [
    ("triage", include_str!("templates/triage.toml")),
    ("issue-to-pr", include_str!("templates/issue-to-pr.toml")),
    ("bug-repro", include_str!("templates/bug-repro.toml")),
    ("nightly-deps", include_str!("templates/nightly-deps.toml")),
];

/// Where a workflow was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Team,
    Personal,
    Builtin,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Team => "team",
            Origin::Personal => "personal",
            Origin::Builtin => "built-in",
        }
    }
}

/// The two folders workflows are kept in.
#[derive(Clone, Debug)]
pub struct Stores {
    /// The main checkout's `.trix/workflows`. None outside a repository.
    pub team: Option<PathBuf>,
    pub personal: PathBuf,
}

impl Stores {
    /// For a checkout (its main one) and your config folder.
    pub fn of(repo: Option<&Path>) -> Stores {
        let personal = crate::config::path()
            .parent()
            .unwrap_or(Path::new("/"))
            .join("workflows");
        Stores {
            team: repo.map(|r| r.join(".trix/workflows")),
            personal,
        }
    }

    fn file(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.toml"))
    }

    fn dirs(&self) -> [(Option<&Path>, Origin); 2] {
        [
            (self.team.as_deref(), Origin::Team),
            (Some(self.personal.as_path()), Origin::Personal),
        ]
    }

    /// The workflow named `name`: the team's, else yours, else a built-in.
    pub fn load(&self, name: &str) -> Result<(Workflow, Origin), String> {
        for (dir, origin) in self.dirs() {
            let Some(path) = dir.map(|d| Stores::file(d, name)).filter(|p| p.is_file()) else {
                continue;
            };
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let w = Workflow::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if w.name != name {
                return Err(format!(
                    "{}: it names itself '{}', not '{name}'",
                    path.display(),
                    w.name
                ));
            }
            return Ok((w, origin));
        }
        builtin(name)
            .map(|w| (w, Origin::Builtin))
            .ok_or_else(|| format!("no workflow named '{name}' (trix workflow ls)"))
    }

    /// Every workflow there is, each once, by name.
    pub fn list(&self) -> Vec<(String, Origin)> {
        let mut out: Vec<(String, Origin)> = Vec::new();
        for (dir, origin) in self.dirs() {
            for e in dir
                .and_then(|d| std::fs::read_dir(d).ok())
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = e.path();
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if path.extension().is_some_and(|x| x == "toml")
                    && !out.iter().any(|(n, _)| *n == name)
                {
                    out.push((name, origin));
                }
            }
        }
        for (name, _) in BUILTIN {
            if !out.iter().any(|(n, _)| n == name) {
                out.push((name.to_string(), Origin::Builtin));
            }
        }
        out.sort();
        out
    }

    /// Write a workflow to the team's folder, or yours. A built-in's name
    /// is taken: a fork gets one of its own.
    pub fn save(&self, w: &Workflow, origin: Origin) -> Result<PathBuf, String> {
        if w.builtin || builtin(&w.name).is_some() {
            return Err(format!(
                "'{}' is a built-in workflow, and read-only: save your copy under another name",
                w.name
            ));
        }
        let dir = match origin {
            Origin::Team => self
                .team
                .clone()
                .ok_or("not in a git repository: there's no team folder here")?,
            Origin::Personal => self.personal.clone(),
            Origin::Builtin => return Err("a built-in workflow can't be written".into()),
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = Stores::file(&dir, &w.name);
        write_atomic(&path, w.to_toml()?.as_bytes())?;
        Ok(path)
    }
}

/// A template shipped in the binary.
pub fn builtin(name: &str) -> Option<Workflow> {
    let (_, text) = BUILTIN.iter().find(|(n, _)| *n == name)?;
    Some(Workflow::from_toml(text).unwrap_or_else(|e| panic!("the built-in {name} template: {e}")))
}

pub fn builtin_names() -> Vec<&'static str> {
    BUILTIN.iter().map(|(n, _)| *n).collect()
}

pub fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, p))
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Where runs are kept: `<state>/runs/<id>/`.
pub fn runs_root() -> PathBuf {
    crate::worktree::state_dir().join("runs")
}

/// The workflow as it was when the run started, written into the run's
/// folder. The run reads this and nothing else: an edit to the file while
/// it runs changes the next run, not this one.
pub fn snapshot(run_dir: &Path, w: &Workflow) -> Result<(), String> {
    std::fs::create_dir_all(run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    write_atomic(&run_dir.join("workflow.toml"), w.to_toml()?.as_bytes())
}

pub fn snapshot_of(run_dir: &Path) -> Result<Workflow, String> {
    let p = run_dir.join("workflow.toml");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Workflow::from_toml(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::model::{Count, Push, Revisit, SCHEMA};

    fn stores(name: &str) -> (PathBuf, Stores) {
        let base =
            std::env::temp_dir().join(format!("trix-wf-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let s = Stores {
            team: Some(base.join("repo/.trix/workflows")),
            personal: base.join("config/workflows"),
        };
        (base, s)
    }

    #[test]
    fn every_template_round_trips_through_the_native_format() {
        for name in builtin_names() {
            let w = builtin(name).unwrap();
            assert_eq!((w.schema, w.name.as_str()), (SCHEMA, name));
            let text = w.to_toml().unwrap();
            let again = Workflow::from_toml(&text).unwrap();
            assert_eq!(again, w, "{name}: read back, it's the same workflow");
            assert_eq!(
                again.to_toml().unwrap(),
                text,
                "{name}: and writes the same file"
            );
            // Positions, prompts and briefs come through whole.
            assert_eq!(again.layout, w.layout);
            for (id, n) in &w.nodes {
                assert_eq!(
                    (&again.nodes[id].prompt, &again.nodes[id].brief),
                    (&n.prompt, &n.brief),
                    "{name}.{id}"
                );
            }
        }
        let w = builtin("issue-to-pr").unwrap();
        assert_eq!(w.policy.push, Push::Guarded);
        assert_eq!(w.limits.max_iterations, Some(Count::N(6)));
        assert_eq!(w.nodes["implementer"].revisit(), Revisit::Reuse);
        assert_eq!(w.nodes["tests"].run, vec!["npm", "test"]);
        assert_eq!(w.layout["implementer"], [2, 1]);
        assert!(
            w.nodes["implementer"]
                .prompt
                .as_ref()
                .unwrap()
                .contains("{{fence inputs.issue.body}}")
        );
    }

    #[test]
    fn the_schema_version_and_the_keys_are_checked() {
        let ok = "schema = 1\nname = \"x\"\n[nodes.a]\nkind = \"check\"\nrun = [\"true\"]\n";
        assert!(Workflow::from_toml(ok).is_ok());
        let e = Workflow::from_toml("name = \"x\"\n[nodes]\n").unwrap_err();
        assert!(e.contains("no schema version"), "{e}");
        let e = Workflow::from_toml(&ok.replace("schema = 1", "schema = 2")).unwrap_err();
        assert!(e.contains("schema 2") && e.contains("newer trix"), "{e}");
        let e = Workflow::from_toml(&ok.replace("schema = 1", "schema = 0")).unwrap_err();
        assert!(e.contains("schema 0") && !e.contains("newer"), "{e}");
        // A key this trix doesn't know is refused, not dropped: in the
        // workflow, in a node, in an edge.
        for bad in [
            ok.replace("name = \"x\"", "name = \"x\"\ncolour = \"red\""),
            ok.replace("kind = \"check\"", "kind = \"check\"\nretries = 3"),
            format!("{ok}[[edges]]\nfrom = \"a\"\nto = \"@done\"\non = \"pass\"\nwhen = \"x\"\n"),
        ] {
            let e = Workflow::from_toml(&bad).unwrap_err();
            assert!(e.contains("unknown field"), "{e}");
        }
        assert!(
            Workflow::from_toml(&ok.replace("\"x\"", "\"../x\""))
                .unwrap_err()
                .contains("letters, digits")
        );
        // The file is what's written: nothing unset is spelled out.
        let w = Workflow::from_toml(ok).unwrap();
        let text = w.to_toml().unwrap();
        assert!(text.starts_with("schema = 1\nname = \"x\"\n"), "{text}");
        assert!(
            !text.contains("approvals") && !text.contains("layout") && !text.contains("brief"),
            "{text}"
        );
    }

    #[test]
    fn the_teams_workflow_shadows_yours_and_built_ins_are_read_only() {
        let (base, s) = stores("shadow");
        let mut w = builtin("issue-to-pr").unwrap();
        // A built-in's name is taken.
        assert!(
            s.save(&w, Origin::Personal)
                .unwrap_err()
                .contains("built-in workflow, and read-only")
        );
        assert_eq!(s.load("issue-to-pr").unwrap().1, Origin::Builtin);
        // A fork under its own name: yours, then the team's shadows it.
        w.name = "my-flow".into();
        w.description = "mine".into();
        s.save(&w, Origin::Personal).unwrap();
        assert_eq!(s.load("my-flow").unwrap(), (w.clone(), Origin::Personal));
        let mut team = w.clone();
        team.description = "the team's".into();
        let path = s.save(&team, Origin::Team).unwrap();
        assert!(path.ends_with("repo/.trix/workflows/my-flow.toml"));
        assert_eq!(s.load("my-flow").unwrap(), (team, Origin::Team));
        let names = s.list();
        assert_eq!(names.iter().filter(|(n, _)| n == "my-flow").count(), 1);
        assert!(names.contains(&("my-flow".into(), Origin::Team)));
        assert!(names.contains(&("triage".into(), Origin::Builtin)));
        // A file that names another workflow isn't taken for this one.
        std::fs::write(s.personal.join("other.toml"), w.to_toml().unwrap()).unwrap();
        assert!(
            s.load("other")
                .unwrap_err()
                .contains("names itself 'my-flow'")
        );
        assert!(
            s.load("nope")
                .unwrap_err()
                .contains("no workflow named 'nope'")
        );
        // Outside a repository there's no team folder.
        let alone = Stores {
            team: None,
            personal: s.personal.clone(),
        };
        assert!(
            alone
                .save(&w, Origin::Team)
                .unwrap_err()
                .contains("not in a git repository")
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_run_works_from_the_snapshot_taken_at_its_start() {
        let (base, s) = stores("snapshot");
        let mut w = builtin("bug-repro").unwrap();
        w.name = "repro".into();
        s.save(&w, Origin::Personal).unwrap();
        let run = base.join("runs/repro-1");
        snapshot(&run, &s.load("repro").unwrap().0).unwrap();
        // The file changes while the run is going.
        let mut edited = w.clone();
        edited.limits.wall_clock = Some("9h".into());
        edited.nodes.remove("verifier");
        s.save(&edited, Origin::Personal).unwrap();
        assert_eq!(s.load("repro").unwrap().0, edited);
        // The run still has the workflow it started with.
        assert_eq!(snapshot_of(&run).unwrap(), w);
        assert_ne!(
            snapshot_of(&run).unwrap().content_hash(),
            edited.content_hash()
        );
        assert_eq!(snapshot_of(&run).unwrap().content_hash(), w.content_hash());
        let _ = std::fs::remove_dir_all(base);
    }
}
