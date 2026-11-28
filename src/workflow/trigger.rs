//! Triggers: what starts a run without you. Another workflow's run ending,
//! an issue or PR on GitHub (polled through `gh`), files that changed, or
//! a time (`cron`, see `sched.rs`). Each thing a trigger sees fires it once.

use super::model::Trigger;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What happened that a trigger might fire on.
#[derive(Clone, Debug, PartialEq)]
pub enum Fact {
    /// A run of `workflow` ended in `state`.
    RunEnded {
        run: String,
        workflow: String,
        state: String,
    },
    /// An open issue or PR with these labels.
    GitHub {
        kind: String,
        number: u64,
        labels: Vec<String>,
    },
    /// The default branch's head moved, changing these files.
    Files { sha: String, paths: Vec<String> },
}

/// A run to start: of this workflow, with these inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    pub workflow: String,
    pub inputs: BTreeMap<String, String>,
    /// What fired it, for the run's log.
    pub because: String,
}

/// What has fired already: a trigger fires once for each thing it sees.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Fired(pub BTreeSet<String>);

impl Fired {
    pub fn load(file: &Path) -> Fired {
        std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, file: &Path) -> Result<(), String> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        super::store::write_atomic(
            file,
            &serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
    }
}

/// The key a trigger and a fact fire under, if the trigger is about the fact.
fn key(
    workflow: &str,
    t: &Trigger,
    f: &Fact,
) -> Option<(String, BTreeMap<String, String>, String)> {
    let mut inputs = BTreeMap::new();
    match (t.on.as_str(), f) {
        (
            "workflow",
            Fact::RunEnded {
                run,
                workflow: w,
                state,
            },
        ) => {
            if t.workflow.as_deref() != Some(w.as_str())
                || t.state.as_deref().unwrap_or("succeeded") != state
            {
                return None;
            }
            Some((
                format!("{workflow}<-run:{run}:{state}"),
                inputs,
                format!("{run} {state}"),
            ))
        }
        (
            "github",
            Fact::GitHub {
                kind,
                number,
                labels,
            },
        ) => {
            if t.kind.as_deref().unwrap_or("issue") != kind
                || t.label.as_ref().is_some_and(|l| !labels.contains(l))
            {
                return None;
            }
            if let Some(name) = &t.input {
                inputs.insert(name.clone(), number.to_string());
            }
            Some((
                format!("{workflow}<-github:{kind}:{number}"),
                inputs,
                format!("{kind} #{number}"),
            ))
        }
        ("files", Fact::Files { sha, paths }) => {
            let hit = paths
                .iter()
                .find(|p| t.paths.iter().any(|g| super::check::overlap(g, p)))?;
            Some((
                format!("{workflow}<-files:{sha}"),
                inputs,
                format!("{hit} changed in {}", &sha[..sha.len().min(7)]),
            ))
        }
        _ => None,
    }
}

/// The runs these facts start, each once: a fact that already fired a
/// trigger doesn't fire it again, however often it's seen.
pub fn due(workflows: &[(String, Vec<Trigger>)], facts: &[Fact], fired: &mut Fired) -> Vec<Start> {
    let mut out = Vec::new();
    for (workflow, triggers) in workflows {
        for t in triggers {
            for f in facts {
                let Some((k, inputs, because)) = key(workflow, t, f) else {
                    continue;
                };
                if fired.0.insert(k) {
                    out.push(Start {
                        workflow: workflow.clone(),
                        inputs,
                        because,
                    });
                }
            }
        }
    }
    out
}

/// The open issues or PRs with a label, as `gh` lists them.
pub fn github_facts(kind: &str, json: &str) -> Vec<Fact> {
    let v: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    v.iter()
        .filter_map(|i| {
            let labels = i["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str().map(String::from))
                .collect();
            Some(Fact::GitHub {
                kind: kind.into(),
                number: i["number"].as_u64()?,
                labels,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(on: &str) -> Trigger {
        Trigger {
            on: on.into(),
            ..Trigger::default()
        }
    }

    #[test]
    fn a_trigger_fires_once_for_each_thing_it_sees() {
        let flows = vec![
            (
                "deploy".to_string(),
                vec![Trigger {
                    workflow: Some("issue-to-pr".into()),
                    ..t("workflow")
                }],
            ),
            (
                "issue-to-pr".to_string(),
                vec![Trigger {
                    label: Some("trix".into()),
                    input: Some("issue".into()),
                    ..t("github")
                }],
            ),
            (
                "nightly-deps".to_string(),
                vec![Trigger {
                    paths: vec!["package.json".into(), "**/Cargo.toml".into()],
                    ..t("files")
                }],
            ),
        ];
        let mut fired = Fired::default();
        let ended = |run: &str, state: &str| Fact::RunEnded {
            run: run.into(),
            workflow: "issue-to-pr".into(),
            state: state.into(),
        };
        let issue = |n: u64, labels: &[&str]| Fact::GitHub {
            kind: "issue".into(),
            number: n,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        };
        let facts = vec![
            ended("issue-to-pr-42", "succeeded"),
            issue(57, &["trix", "bug"]),
            issue(58, &["bug"]),
            Fact::Files {
                sha: "abc1234def".into(),
                paths: vec!["src/a.rs".into(), "crates/x/Cargo.toml".into()],
            },
        ];
        let starts = due(&flows, &facts, &mut fired);
        assert_eq!(starts.len(), 3, "{starts:?}");
        assert_eq!(
            (starts[0].workflow.as_str(), starts[0].because.as_str()),
            ("deploy", "issue-to-pr-42 succeeded")
        );
        // The issue's number is the run's input; the unlabelled issue starts nothing.
        assert_eq!(
            (
                starts[1].workflow.as_str(),
                starts[1].inputs.get("issue").map(String::as_str)
            ),
            ("issue-to-pr", Some("57"))
        );
        assert_eq!(
            (starts[2].workflow.as_str(), starts[2].because.as_str()),
            ("nightly-deps", "crates/x/Cargo.toml changed in abc1234")
        );
        // Seen again, at every poll: nothing fires twice.
        assert!(due(&flows, &facts, &mut fired).is_empty());
        assert!(due(&flows, &facts, &mut fired).is_empty());
        // Something new does: another run, another issue, another commit.
        let more = vec![
            ended("issue-to-pr-43", "succeeded"),
            ended("issue-to-pr-44", "failed"),
            issue(59, &["trix"]),
        ];
        let starts = due(&flows, &more, &mut fired);
        assert_eq!(
            starts
                .iter()
                .map(|s| s.because.as_str())
                .collect::<Vec<_>>(),
            ["issue-to-pr-43 succeeded", "issue #59"],
            "a failed run isn't what was asked for"
        );
        // What fired survives a restart of the server.
        let file =
            std::env::temp_dir().join(format!("trix-triggers-{}/fired.json", std::process::id()));
        fired.save(&file).unwrap();
        let mut again = Fired::load(&file);
        assert_eq!(again, fired);
        assert!(due(&flows, &facts, &mut again).is_empty());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
        // What gh lists becomes facts.
        let listed =
            r#"[{"number": 57, "labels": [{"name": "trix"}]}, {"number": 60, "labels": []}]"#;
        assert_eq!(
            github_facts("issue", listed),
            [issue(57, &["trix"]), issue(60, &[])]
        );
        assert!(
            Fired::load(Path::new("/nonexistent/fired.json"))
                .0
                .is_empty()
        );
    }
}
