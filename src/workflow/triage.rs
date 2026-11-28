//! Triage as a workflow: the built-in template, `[triage]` mapped onto it,
//! and the graph that says where a case goes next.
//!
//! The `triage.*` and `gh.*` kinds themselves are `case.rs`: they keep
//! today's prompts, reports and checks. What moved here is who decides
//! what's next (the template's edges), who a role's agent is (the
//! template's nodes, bound to your config) and how long a pane lives (each
//! node's `revisit`).

use super::model::{Args, Count, Node, Revisit, Second, Workflow};
use crate::config::{Role, TriageOpts};
use std::sync::OnceLock;

/// The built-in triage workflow, as shipped: its `{{config.…}}` not bound.
pub fn template() -> &'static Workflow {
    static T: OnceLock<Workflow> = OnceLock::new();
    T.get_or_init(|| super::store::builtin("triage").expect("the built-in triage template"))
}

/// `[triage]` as the template's `{{config.triage.…}}` see it.
fn config_view(o: &TriageOpts) -> serde_json::Value {
    let role = |r: &Role| serde_json::json!({"agent": r.agent, "args": r.args});
    serde_json::json!({"triage": {
        "budget": o.budget, "max_rounds": o.max_rounds, "reviewers": o.reviewers,
        "builder": role(&o.builder), "reviewer": role(&o.reviewer), "reviewer2": role(&o.reviewer2),
        "fixer": role(&o.fixer), "gatekeeper": role(&o.gatekeeper),
    }})
}

/// The template with your `[triage]` settings in: the workflow a case runs.
pub fn bound(opts: &TriageOpts) -> Workflow {
    template()
        .bind(&config_view(opts))
        .expect("the triage template binds to [triage]")
}

fn agent_of(n: &Node) -> Result<Role, String> {
    let agent = n
        .agent
        .clone()
        .ok_or_else(|| format!("a {} node names no agent", n.kind))?;
    Ok(Role {
        agent,
        args: n.args.as_ref().map(Args::list).unwrap_or_default(),
    })
}

/// What a case is made with, read back from a bound triage workflow: its
/// nodes' agents, its round ceiling and its budget.
pub fn opts_of(w: &Workflow) -> Result<TriageOpts, String> {
    let node = |kind: &str| {
        w.of_kind(kind)
            .map(|(_, n)| n)
            .ok_or_else(|| format!("the workflow has no {kind} node"))
    };
    let review = node("triage.review")?;
    let second = review.second.as_ref().map(|Second { agent, args }| Role {
        agent: agent.clone(),
        args: args.as_ref().map(Args::list).unwrap_or_default(),
    });
    Ok(TriageOpts {
        budget: w
            .limits
            .budget_usd
            .as_ref()
            .and_then(|b| b.get())
            .ok_or("limits.budget_usd isn't an amount")?,
        max_rounds: w
            .limits
            .max_iterations
            .as_ref()
            .and_then(Count::get)
            .ok_or("limits.max_iterations isn't a number")?,
        builder: agent_of(node("triage.build")?)?,
        reviewer: agent_of(review)?,
        reviewer2: second.unwrap_or(agent_of(review)?),
        reviewers: review.reviewers.as_ref().and_then(Count::get).unwrap_or(1),
        fixer: agent_of(node("triage.fix")?)?,
        gatekeeper: agent_of(node("triage.gatekeep")?)?,
    })
}

/// Where the node of `kind` leads on `verdict`: the kind of the node the
/// template's edge names, or `@done`.
pub fn next(kind: &str, verdict: &str) -> Option<&'static str> {
    let w = template();
    let (id, _) = w.of_kind(kind)?;
    let to = w.follow(id, verdict)?.to.as_str();
    if to.starts_with('@') {
        return Some(to);
    }
    w.nodes.get(to).map(|n| n.kind.as_str())
}

/// Whether the node of `kind` keeps its pane once its report is in.
pub fn revisit(kind: &str) -> Revisit {
    template()
        .of_kind(kind)
        .map_or(Revisit::Fresh, |(_, n)| n.revisit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::model::{Merge, Push};

    #[test]
    fn the_template_is_triage_as_it_is_today() {
        let w = template();
        assert!(w.builtin && w.name == "triage");
        // Today's builder and fixer push their own commits; you approve the merge.
        assert_eq!((w.policy.push, w.policy.merge), (Push::Allow, Merge::You));
        // Another round on repeat findings, and time only while work runs.
        assert_eq!(w.limits.no_progress.as_ref().unwrap().after(), None);
        assert_eq!(
            crate::workflow::model::duration(w.limits.wall_clock.as_ref().unwrap()),
            Ok(12 * 3600)
        );
        // The fixer keeps its pane between rounds; the others close after their report.
        assert_eq!(revisit("triage.fix"), Revisit::Reuse);
        for kind in ["triage.build", "triage.review", "triage.gatekeep"] {
            assert_eq!(revisit(kind), Revisit::Fresh, "{kind}");
        }
        // Writers own everything, readers nothing: no change triage accepts is sent back.
        for (kind, owns) in [
            ("triage.build", true),
            ("triage.fix", true),
            ("triage.review", false),
            ("triage.gatekeep", false),
        ] {
            let brief = w.of_kind(kind).unwrap().1.brief.as_ref().unwrap();
            assert_eq!(brief.owns == vec!["**".to_string()], owns, "{kind}");
            assert!(!brief.done.is_empty());
        }
    }

    #[test]
    fn every_verdict_the_built_ins_give_has_an_edge() {
        for (kind, verdict, to) in [
            ("gh.resolve_target", "pr", "triage.review"),
            ("gh.resolve_target", "issue", "triage.build"),
            ("triage.build", "pushed", "triage.review"),
            ("triage.review", "findings", "triage.fix"),
            ("triage.review", "nothing_found", "gh.gate"),
            ("triage.review", "unsolved", "triage.fix"),
            ("triage.fix", "pushed", "triage.gatekeep"),
            ("triage.gatekeep", "changes", "triage.fix"),
            ("triage.gatekeep", "pass", "gh.gate"),
            ("gh.gate", "ci_failed", "triage.fix"),
            ("gh.gate", "conflict", "triage.fix"),
            ("gh.gate", "merged", "@done"),
        ] {
            assert_eq!(next(kind, verdict), Some(to), "{kind} on {verdict}");
        }
        assert_eq!(
            next("triage.fix", "pass"),
            None,
            "not a verdict the fixer gives"
        );
        // And every verdict a node declares is one of those.
        for (id, n) in &template().nodes {
            for v in &n.verdicts {
                assert!(
                    template().follow(id, v).is_some(),
                    "{id} declares {v} with no edge"
                );
            }
        }
    }

    #[test]
    fn the_triage_config_maps_onto_the_template_and_back() {
        // Every [triage] key, each role's agent and arguments.
        let role = |agent: &str, args: &[&str]| Role {
            agent: agent.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        let default = TriageOpts::default();
        assert_eq!(opts_of(&bound(&default)), Ok(default));
        let mine = TriageOpts {
            budget: 4.5,
            max_rounds: 3,
            builder: role("codex", &["-m", "o9"]),
            reviewer: role("claude", &["--model", "claude-sonnet-5-5"]),
            reviewer2: role("gemini", &[]),
            reviewers: 1,
            fixer: role("opencode", &["--agent", "build"]),
            gatekeeper: role("copilot", &[]),
        };
        let w = bound(&mine);
        assert_eq!(opts_of(&w), Ok(mine.clone()));
        // Where each key lands in the workflow.
        assert_eq!(w.limits.max_iterations, Some(Count::N(3)));
        assert_eq!(w.limits.budget_usd.as_ref().unwrap().get(), Some(4.5));
        assert_eq!(w.nodes["build"].agent.as_deref(), Some("codex"));
        assert_eq!(
            w.nodes["build"].args,
            Some(Args::List(vec!["-m".into(), "o9".into()]))
        );
        assert_eq!(w.nodes["review"].reviewers, Some(Count::N(1)));
        assert_eq!(w.nodes["review"].second.as_ref().unwrap().agent, "gemini");
        assert_eq!(w.nodes["fix"].agent.as_deref(), Some("opencode"));
        assert_eq!(w.nodes["gatekeep"].agent.as_deref(), Some("copilot"));
        // Nothing is left unbound.
        assert!(!w.to_toml().unwrap().contains("{{config."));
        // A setting that isn't there is said, not guessed.
        let e = template()
            .bind(&serde_json::json!({"triage": {}}))
            .unwrap_err();
        assert!(e.contains("there's no such setting"), "{e}");
    }

    // ── the aliases, the converter, a restored pane ──

    use crate::case::{self, Ask, Case, Cmd, Role as CaseRole, Stage};
    use crate::workflow::cli;
    use crate::workflow::replay::{T0, World};
    use crate::workflow::runner::{self, Run};
    use crate::workflow::view::{NodeState, RunState};

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn every_trix_case_command_and_flag_still_reads_the_same() {
        let id = || "triage-12".to_string();
        for (line, want) in [
            ("ls", Cmd::Ls { json: false }),
            ("ls --json", Cmd::Ls { json: true }),
            (
                "show triage-12",
                Cmd::Show {
                    id: id(),
                    json: false,
                },
            ),
            (
                "show triage-12 --json",
                Cmd::Show {
                    id: id(),
                    json: true,
                },
            ),
            (
                "--json show triage-12",
                Cmd::Show {
                    id: id(),
                    json: true,
                },
            ),
            ("run triage-12", Cmd::Run(id())),
            ("approve triage-12", Cmd::Approve(id())),
            ("abort triage-12", Cmd::Abort(id())),
            ("answer triage-12 n", Cmd::Answer { id: id(), key: 'n' }),
            ("answer triage-12 R", Cmd::Answer { id: id(), key: 'r' }),
            ("answer triage-12 1", Cmd::Answer { id: id(), key: '1' }),
            (
                "submit triage-12 fixer",
                Cmd::Submit {
                    id: id(),
                    role: "fixer".into(),
                    file: None,
                },
            ),
            (
                "submit triage-12 reviewer2 --file .trix-reviewer2.json",
                Cmd::Submit {
                    id: id(),
                    role: "reviewer2".into(),
                    file: Some(".trix-reviewer2.json".into()),
                },
            ),
        ] {
            assert_eq!(case::parse(&words(line)), Some(want), "trix case {line}");
        }
        for line in [
            "",
            "ls extra",
            "show",
            "answer triage-12 no",
            "submit triage-12",
            "submit triage-12 fixer --file",
            "merge triage-12",
        ] {
            assert_eq!(case::parse(&words(line)), None, "trix case {line}");
        }
        // Each role the agents hand in as is still one.
        for role in ["builder", "reviewer", "reviewer2", "fixer", "gatekeeper"] {
            assert!(CaseRole::parse(role).is_some(), "{role}");
        }
    }

    #[test]
    fn the_workflow_commands_are_the_case_commands_under_another_name() {
        // trix triage 123 is trix workflow run triage --input target=123.
        let run = |target: &str| cli::Cmd::Run {
            name: "triage".into(),
            inputs: [("target".to_string(), target.to_string())].into(),
        };
        assert_eq!(
            cli::parse(&words("run triage --input target=123")),
            Ok(run("123"))
        );
        assert_eq!(
            cli::parse(&words("run triage --input=target=#123")),
            Ok(run("#123"))
        );
        assert_eq!(
            cli::parse(&words(
                "run triage --input target=https://github.com/acme/app/issues/7"
            )),
            Ok(run("https://github.com/acme/app/issues/7"))
        );
        assert!(cli::parse(&words("run triage --input")).is_err());
        assert!(cli::parse(&words("run triage --input target")).is_err());
        assert!(cli::parse(&words("run triage --force")).is_err());
        assert_eq!(
            cli::parse(&words("ls --json")),
            Ok(cli::Cmd::Ls { json: true })
        );
        // trix case approve triage-123 is trix workflow approve triage-123,
        // and abort the same: for a triage run they're one command.
        let is_case = |id: &str| id == "triage-123";
        let approve = cli::parse(&words("approve triage-123")).unwrap();
        assert_eq!(
            cli::as_case(&approve, is_case),
            Some(words("approve triage-123"))
        );
        assert_eq!(
            case::parse(&cli::as_case(&approve, is_case).unwrap()),
            Some(Cmd::Approve("triage-123".into()))
        );
        let abort = cli::parse(&words("abort triage-123")).unwrap();
        assert_eq!(
            case::parse(&cli::as_case(&abort, is_case).unwrap()),
            Some(Cmd::Abort("triage-123".into()))
        );
        // A run that isn't a case isn't sent to one.
        assert_eq!(
            cli::as_case(&cli::parse(&words("approve nightly-3")).unwrap(), is_case),
            None
        );
        assert_eq!(cli::as_case(&run("123"), is_case), None);
    }

    /// A case at `stage`, as far along as that stage is reached.
    fn at(w: &mut World, stage: Stage) -> Case {
        let mut c = w.case(&TriageOpts {
            reviewers: 1,
            ..TriageOpts::default()
        });
        c.stage = stage;
        c
    }

    fn states(c: &Case) -> Vec<(String, NodeState)> {
        c.view()
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.state))
            .collect()
    }

    fn pair(id: &str, s: NodeState) -> (String, NodeState) {
        (id.to_string(), s)
    }

    #[test]
    fn a_case_converts_to_a_run_at_every_stage() {
        use NodeState::{Blocked, Done, InReview, Waiting, Working};
        let mut w = World::new("convert");
        let fix: case::FixReport =
            serde_json::from_str(r#"{"commits": ["abc1234"], "tests": "ok", "findings": []}"#)
                .unwrap();
        let pass: case::Verdict =
            serde_json::from_str(r#"{"verdict": "pass", "findings": [], "ci": "green"}"#).unwrap();

        // Choosing: the target's node asks which PR.
        let mut c = at(&mut w, Stage::Choosing);
        c.pr = None;
        c.choices = vec![case::Candidate {
            pr: case::Pr {
                number: 14,
                title: "t".into(),
                url: "u".into(),
                head: "h".into(),
                base: "main".into(),
                sha: "s".into(),
            },
            found: case::Found::Linked,
        }];
        c.ask = Some(Ask::Choose);
        c.why = "which one?".into();
        let v = c.view();
        assert_eq!(
            (v.workflow.as_str(), v.state, v.why.as_str()),
            ("triage", RunState::NeedsYou, "which one?")
        );
        assert_eq!(states(&c)[0], pair("target", Blocked));

        // Building: only a case from an issue has a build node.
        let mut c = at(&mut w, Stage::Building);
        c.pr = None;
        assert_eq!(c.view().state, RunState::Running);
        assert_eq!(
            states(&c)[..2],
            [pair("build", Working), pair("review", Waiting)]
        );
        assert_eq!(c.view().nodes[0].owns, vec!["**".to_string()]);

        // Reviewing.
        let c = at(&mut w, Stage::Reviewing);
        assert_eq!(
            states(&c),
            [
                pair("review", Working),
                pair("fix", Waiting),
                pair("gatekeep", Waiting),
                pair("gate", Waiting)
            ]
        );
        assert_eq!((c.view().cycles, c.view().wasted), (1, 0));

        // Fixing, in the second round: a round is a cycle of cost.
        let mut c = at(&mut w, Stage::Fixing);
        c.round = 2;
        assert_eq!(
            states(&c),
            [
                pair("review", Done),
                pair("fix", Working),
                pair("gatekeep", Waiting),
                pair("gate", Waiting)
            ]
        );
        assert_eq!((c.view().cycles, c.view().nodes[1].visits), (2, 2));

        // Verifying: the fixer's work is with the gatekeeper.
        let mut c = at(&mut w, Stage::Verifying);
        c.fix = Some(fix.clone());
        assert_eq!(
            states(&c),
            [
                pair("review", Done),
                pair("fix", InReview),
                pair("gatekeep", Working),
                pair("gate", Waiting)
            ]
        );
        assert_eq!(c.view().nodes[1].last, "pushed · 1 commit");

        // The gate: asking for the merge needs you; branch rules park it.
        let mut c = at(&mut w, Stage::Gate);
        c.fix = Some(fix.clone());
        c.verdict = Some(pass.clone());
        assert_eq!(
            states(&c),
            [
                pair("review", Done),
                pair("fix", Done),
                pair("gatekeep", Done),
                pair("gate", Working)
            ]
        );
        c.ask = Some(Ask::Merge);
        assert_eq!(
            (c.view().state, states(&c)[3].clone()),
            (RunState::NeedsYou, pair("gate", Blocked))
        );
        c.ask = Some(Ask::Approval);
        assert_eq!(c.view().state, RunState::Parked);

        // Merged: succeeded, every node that ran is done.
        let mut c = at(&mut w, Stage::Merged);
        c.fix = Some(fix.clone());
        c.verdict = Some(pass);
        c.spent_closed = 3.25;
        let v = c.view();
        assert_eq!(
            (v.state, v.done(), v.spent_usd),
            (RunState::Succeeded, 4, 3.25)
        );
        assert_eq!(v.nodes[3].last, "merged");
        assert!(v.state.over());

        // Aborted: what had finished stays done, the rest never ran.
        let mut c = at(&mut w, Stage::Aborted);
        c.fix = Some(fix);
        assert_eq!(c.view().state, RunState::Aborted);
        assert_eq!(
            states(&c),
            [
                pair("review", Done),
                pair("fix", Done),
                pair("gatekeep", Waiting),
                pair("gate", Waiting)
            ]
        );

        // A case.json from before workflows converts as it is.
        let old = r#"{"id": "triage-7", "pr": {"number": 7, "title": "Old", "url": "u", "head": "h", "base": "main", "sha": "s"},
            "repo": "/r", "stage": "fixing", "round": 1, "max_rounds": 2, "budget": 10.0,
            "reviewer": {"agent": "claude", "args": []}, "fixer": {"agent": "codex", "args": []},
            "gatekeeper": {"agent": "claude", "args": []}, "created": 1}"#;
        let c: Case = serde_json::from_str(old).unwrap();
        let v = c.view();
        assert_eq!(
            (v.id.as_str(), v.state, v.goal.as_str()),
            ("triage-7", RunState::Running, "PR #7 Old")
        );
        assert_eq!(
            v.nodes
                .iter()
                .find(|n| n.id == "fix")
                .map(|n| (n.state, n.backend.as_str())),
            Some((Working, "codex"))
        );
        let _ = std::fs::remove_dir_all(&w.base);
    }

    #[test]
    fn a_saved_layouts_case_pane_restores_and_goes_on() {
        // What a layout saved for the case's pane: its command line.
        let saved = "/opt/homebrew/bin/trix case run triage-12";
        let args: Vec<String> = shell_words::split(saved).unwrap()[1..].to_vec();
        assert_eq!(args[0], "case");
        assert_eq!(case::parse(&args[1..]), Some(Cmd::Run("triage-12".into())));

        // The pane starts again mid-case: the run is read from its folder,
        // knows it ran before, and goes on with the agents restore brought
        // back instead of starting them twice.
        let mut w = World::new("layout-restore");
        let mut c = w.case(&TriageOpts {
            reviewers: 1,
            ..TriageOpts::default()
        });
        w.tick(&mut c);
        assert_eq!(w.seen.len(), 1);
        let mut again = <Case as Run>::load(&w.root.clone(), "triage-12").unwrap();
        assert!(again.resumed());
        assert!(
            again.banner().contains("trix triage · triage-12")
                && again.banner().contains("started the reviewer")
        );
        let root = w.root.clone();
        runner::step(&mut again, &root, &mut w);
        assert_eq!(
            (w.seen.len(), again.reviews[0].w.agent.as_deref()),
            (1, Some("triage-12-review")),
            "not started twice"
        );
        assert_eq!(again.created, T0);
        let _ = std::fs::remove_dir_all(&w.base);
    }

    #[test]
    fn a_verdict_the_workflow_has_no_edge_for_asks_instead_of_guessing() {
        let mut w = World::new("no-edge");
        let mut c = at(&mut w, Stage::Fixing);
        assert!(!case::go(&mut c, &mut w, "pass"));
        assert_eq!((c.stage, c.ask), (Stage::Fixing, Some(Ask::Setup)));
        assert!(
            c.why.contains("no edge from triage.fix on pass"),
            "{}",
            c.why
        );
        // And one it has is taken.
        c.ask = None;
        assert!(case::go(&mut c, &mut w, "pushed"));
        assert_eq!(c.stage, Stage::Verifying);
        assert_eq!(case::next_stage(Stage::Gate, "merged"), Some(Stage::Merged));
        assert_eq!(case::node_of(Stage::Aborted), None);
        let _ = std::fs::remove_dir_all(&w.base);
    }
}
