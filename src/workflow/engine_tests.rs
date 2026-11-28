//! Runs of generic workflows, end to end: a real temporary repository,
//! scripted agents and a fake GitHub, all through `Env`. No network, no
//! real backend, no login.

use super::engine::{self, Ask, Op, Opts, WfRun};
use super::model::*;
use super::replay::{T0, World, green, runs};
use super::runner::{self, Run};
use super::settings::{AgentPolicy, FLOORS};
use super::store::builtin;
use super::view::{NodeState, RunState};
use crate::control::Asker;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn opts() -> Opts {
    Opts {
        approvals: false,
        ci_paths: Vec::new(),
        floors: FLOORS.iter().map(|f| f.to_string()).collect(),
        notify_each: true,
        on_finish: Vec::new(),
        repo_name: "acme/app".into(),
        max_depth: 2,
        decision: vec!["claude".into()],
        state_from: [
            ("claude", "hooks"),
            ("codex", "hooks"),
            ("gemini", "title, screen"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
        priced: vec!["claude".into(), "codex".into()],
    }
}

/// issue-to-pr on the fixture repository, whose test command is a script:
/// green once `src/done` is there.
pub fn issue_to_pr() -> Workflow {
    let mut w = builtin("issue-to-pr").unwrap();
    let script = "test -f src/done && echo '212 passed' || { echo '2 of 212 failed'; exit 1; }";
    w.nodes.get_mut("tests").unwrap().run = vec!["sh".into(), "-c".into(), script.into()];
    w
}

pub fn issue(body: &str) -> BTreeMap<String, serde_json::Value> {
    let v = serde_json::json!({"number": 42, "title": "The window never resets", "body": body,
        "url": "https://example.com/issues/42", "outsiders": []});
    BTreeMap::from([("issue".to_string(), v)])
}

pub fn root(w: &World) -> PathBuf {
    w.base.join("runs")
}

pub fn start(w: &mut World, wf: Workflow, o: Opts) -> WfRun {
    let id = format!("{}-42", wf.name);
    let r = WfRun::new(
        &id,
        wf,
        w.repo.clone(),
        "main",
        issue("After 60s a client is still refused."),
        o,
        T0,
    );
    engine::store(&root(w), &r).unwrap();
    r
}

pub fn tick(w: &mut World, r: &mut WfRun) {
    let root = root(w);
    if runner::step(r, &root, w) {
        engine::store(&root, r).unwrap();
    }
}

pub fn ticks(w: &mut World, r: &mut WfRun, n: u32) {
    for _ in 0..n {
        tick(w, r);
    }
}

/// A report handed in by the node's own agent, through the server's check.
pub fn submit(w: &mut World, r: &WfRun, node: &str, json: &str) -> Result<String, String> {
    let pane = r.nodes[node].agent.clone().unwrap_or_else(|| r.pane(node));
    op(
        w,
        Some(&pane),
        Some(&r.id),
        Op::Submit {
            node: node.into(),
            report: json.into(),
        },
    )
    .map(|(out, _)| out)
}

pub fn op(
    w: &mut World,
    pane: Option<&str>,
    run: Option<&str>,
    o: Op,
) -> Result<(String, Option<char>), String> {
    let who = pane.map(|p| Asker::Agent(p.into()));
    let r = engine::handle(&root(w), who.as_ref(), run, o, &AgentPolicy::default());
    if let Ok((_, Some(key))) = &r {
        w.keys.push_back(*key);
    }
    r
}

/// What an agent does in its checkout: a file, committed.
pub fn edit(w: &mut World, r: &WfRun, node: &str, file: &str, text: &str) -> String {
    let dir = r.nodes[node].cwd.clone().unwrap();
    std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
    std::fs::write(dir.join(file), text).unwrap();
    w.sh(&dir, &["add", "-A"]);
    w.commit(&dir, &format!("{node}: {file}"))
}

pub const READY: &str = r#"{"verdict": "ready", "summary": "added the reset"}"#;

/// To the implementer at work on its first visit.
pub fn begun(name: &str, o: Opts) -> (World, WfRun) {
    let mut w = World::new(name);
    let mut r = start(&mut w, issue_to_pr(), o);
    tick(&mut w, &mut r);
    assert_eq!(
        r.nodes["implementer"].state,
        NodeState::Working,
        "{:?}",
        r.log
    );
    (w, r)
}

/// From there to the gate's turn: a green check, the reviewer's and the judge's pass.
pub fn to_gate(w: &mut World, r: &mut WfRun) {
    edit(w, r, "implementer", "src/done", "ok\n");
    submit(w, r, "implementer", READY).unwrap();
    ticks(w, r, 3);
    assert_eq!(r.nodes["reviewer"].state, NodeState::Working, "{:?}", r.log);
    submit(w, r, "reviewer", r#"{"verdict": "pass", "findings": []}"#).unwrap();
    ticks(w, r, 2);
    submit(
        w,
        r,
        "judge",
        r#"{"verdict": "pass", "reasons": "it meets the goal"}"#,
    )
    .unwrap();
    tick(w, r);
    assert_eq!(r.active, ["gate"], "{:?}", r.log);
}

pub fn done(w: World) {
    let _ = std::fs::remove_dir_all(&w.base);
}

#[test]
fn issue_to_pr_runs_end_to_end_with_zero_human_actions() {
    let (mut w, mut r) = begun(
        "wf-e2e",
        Opts {
            approvals: true,
            ..opts()
        },
    );
    // The implementer: Codex, guarded, in the run's worktree on the run's branch.
    let s = w.spawned[0].clone();
    assert_eq!(
        (s.name.as_str(), s.kind.as_str(), s.guarded),
        ("issue-to-pr-42-implementer", "codex", true)
    );
    assert_eq!(s.args, ["-s", "workspace-write", "-a", "never"]);
    assert_eq!(s.cwd, r.tree());
    assert_eq!(
        w.sh(&s.cwd, &["branch", "--show-current"]).trim(),
        "trix/wf-issue-to-pr-42"
    );
    // Its prompt: the goal, the issue fenced as data, its brief, how to hand in.
    assert!(
        s.prompt
            .starts_with("Goal: Issue #42 is solved by a PR with tests and green CI."),
        "{}",
        s.prompt
    );
    let fence = s
        .prompt
        .lines()
        .find(|l| l.starts_with("===== outside text"))
        .unwrap();
    assert!(
        s.prompt.contains(&format!(
            "{fence}\nAfter 60s a client is still refused.\n{fence}"
        )),
        "{}",
        s.prompt
    );
    assert!(
        s.prompt.contains("- You own: src/**, test/**.")
            && s.prompt.contains("trix wf submit implementer")
    );
    assert!(
        !s.prompt.contains("Address this first"),
        "nothing was passed along yet"
    );

    // It hands in; the check runs on trix's own detached checkout, and fails.
    let first = edit(&mut w, &r, "implementer", "src/fix.js", "reset();\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(
        (r.nodes["tests"].verdict.as_deref(), r.pinned.as_deref()),
        (Some("fail"), Some(first.as_str()))
    );
    let (dir, argv, guarded) = w.checks[0].clone();
    assert!(
        dir.ends_with("issue-to-pr-42-check-tests") && argv[0] == "sh" && guarded && !dir.exists()
    );
    // The failure goes back to the implementer: a cycle of cost, sent to its kept pane.
    assert_eq!(
        (r.cycles, r.wasted, r.active.clone()),
        (2, 0, vec!["implementer".to_string()])
    );
    tick(&mut w, &mut r);
    assert_eq!(
        w.spawned.len(),
        1,
        "reuse: its pane was kept, not started again"
    );
    let (to, again) = w.sent.last().unwrap().clone();
    assert_eq!(to, "issue-to-pr-42-implementer");
    assert!(
        again.starts_with("Visit 2. Address this, then hand in again:\n2 of 212 failed"),
        "{again}"
    );

    // Fixed: the check passes, and that commit is the one everything after judges.
    let second = edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 3);
    assert_eq!(
        (r.nodes["tests"].verdict.as_deref(), r.pinned.as_deref()),
        (Some("pass"), Some(second.as_str()))
    );
    // The reviewer: Claude on the read-only profile, a fresh detached checkout of that commit.
    let s = w.spawned[1].clone();
    assert_eq!(
        (s.name.as_str(), s.kind.as_str(), s.guarded),
        ("issue-to-pr-42-reviewer", "claude", true)
    );
    assert!(
        s.args.contains(&"dontAsk".to_string())
            && s.args.iter().any(|a| a.starts_with("Read,Glob,Grep"))
    );
    assert!(s.cwd != r.tree() && w.sh(&s.cwd, &["rev-parse", "HEAD"]).trim() == second);
    assert!(
        s.prompt
            .starts_with(&format!("Review commit {second} against:"))
            && s.prompt.contains("212 passed"),
        "{}",
        s.prompt
    );
    submit(
        &mut w,
        &r,
        "reviewer",
        r#"{"verdict": "pass", "findings": []}"#,
    )
    .unwrap();
    tick(&mut w, &mut r);
    // Fresh: its pane closes once its report is in. The implementer's stays.
    assert_eq!(r.closing, ["issue-to-pr-42-reviewer"]);
    w.state("issue-to-pr-42-reviewer", "done");
    tick(&mut w, &mut r);
    assert!(!w.seen.iter().any(|a| a.name == "issue-to-pr-42-reviewer"));
    assert!(
        w.seen
            .iter()
            .any(|a| a.name == "issue-to-pr-42-implementer")
    );
    // The judge: Gemini, a third vendor.
    let s = w.spawned[2].clone();
    assert_eq!(
        (s.name.as_str(), s.kind.as_str(), s.args.len()),
        ("issue-to-pr-42-judge", "gemini", 0)
    );
    submit(
        &mut w,
        &r,
        "judge",
        r#"{"verdict": "pass", "reasons": "it meets the goal"}"#,
    )
    .unwrap();
    tick(&mut w, &mut r);
    w.state("issue-to-pr-42-judge", "done");

    // The gate: trix pushes the checked commit, opens the PR, sees CI green on
    // that commit, the judge approves (it voted pass and didn't write it), and
    // trix merges exactly that commit.
    tick(&mut w, &mut r);
    assert_eq!(r.state, RunState::Succeeded, "{:?}", r.log);
    assert_eq!(
        w.head("trix/wf-issue-to-pr-42"),
        second,
        "what was checked is what was pushed"
    );
    assert_eq!(
        w.hub
            .prs
            .last()
            .map(|p| (p.number, p.state, p.head.as_str())),
        Some((20, "MERGED", "trix/wf-issue-to-pr-42"))
    );
    assert_eq!(r.gate.approved.as_deref(), Some("judge"));
    let audit =
        std::fs::read_to_string(engine::dir_of(&root(&w), &r.id).join("approvals.jsonl")).unwrap();
    let entry: serde_json::Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(
        (
            entry["approver"].as_str(),
            entry["rule"].as_str(),
            entry["sha"].as_str()
        ),
        (
            Some("judge"),
            Some("approvals.merge"),
            Some(second.as_str())
        )
    );
    assert!(
        entry["command"]
            .as_str()
            .unwrap()
            .contains("--match-head-commit")
    );
    // Exactly the cycles the goal needed, nothing wasted, and nobody was asked.
    assert_eq!((r.cycles, r.wasted), (2, 0));
    assert!(
        w.keys.is_empty() && !r.log.iter().any(|(_, l)| l.starts_with("needs you")),
        "{:?}",
        r.log
    );
    assert!(w.seen.is_empty(), "every pane is closed");
    assert!(
        w.notified
            .iter()
            .any(|(t, _)| t == "issue-to-pr-42 succeeded")
    );
    // Three vendors ran it.
    let kinds: Vec<&str> = w.spawned.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, ["codex", "claude", "gemini"]);
    done(w);
}

#[test]
fn with_approvals_off_the_merge_is_yours_and_never_an_agents() {
    let (mut w, mut r) = begun("wf-yours", opts());
    to_gate(&mut w, &mut r);
    tick(&mut w, &mut r);
    // policy.merge is approver, but the workflow isn't in [approvals] allow.
    assert_eq!((r.state, r.ask), (RunState::NeedsYou, Some(Ask::Merge)));
    assert!(
        r.why.contains("approvals are off for 'issue-to-pr'"),
        "{}",
        r.why
    );
    assert_eq!(w.hub.prs.last().unwrap().state, "OPEN");
    // No agent pane approves: not a node of the run, not one outside it.
    for pane in ["issue-to-pr-42-implementer", "alpha dog"] {
        let e = op(&mut w, Some(pane), Some(&r.id), Op::Approve).unwrap_err();
        assert!(e.contains("can't approve, answer or revoke a run"), "{e}");
    }
    // Nor a check's process, for anything.
    let check = Asker::Check("a check of run issue-to-pr-42".into());
    for o in [
        Op::Approve,
        Op::Abort,
        Op::Ask { text: "hi".into() },
        Op::Submit {
            node: "judge".into(),
            report: "{}".into(),
        },
    ] {
        let e = engine::handle(
            &root(&w),
            Some(&check),
            Some(&r.id),
            o,
            &AgentPolicy::default(),
        )
        .unwrap_err();
        assert!(e.contains("a check runs code the agents wrote"), "{e}");
    }
    // A merge is only ever approve: answer y is refused.
    assert!(
        op(&mut w, None, Some(&r.id), Op::Answer { key: 'y' })
            .unwrap_err()
            .contains("only ever trix workflow approve")
    );
    assert!(w.keys.is_empty());
    // You approve.
    op(&mut w, None, Some(&r.id), Op::Approve).unwrap();
    tick(&mut w, &mut r);
    tick(&mut w, &mut r);
    assert_eq!(
        (r.state, r.gate.approved.as_deref()),
        (RunState::Succeeded, Some("you"))
    );
    let audit =
        std::fs::read_to_string(engine::dir_of(&root(&w), &r.id).join("approvals.jsonl")).unwrap();
    assert!(audit.contains("\"approver\":\"you\""), "{audit}");
    done(w);
}

const LINEAR: &str = r#"
schema = 1
name = "linear"
[goal]
text = "The file is there."
[goal.done_when]
mechanical = [{ check = "ok" }]
[limits]
wall_clock = "1h"
[nodes.writer]
kind = "agent"
agent = "claude"
profile = "edit-only"
verdicts = ["ready"]
brief = { owns = ["src/**"], done = "The file is committed." }
[nodes.ok]
kind = "check"
run = ["test", "-f", "src/file"]
[[edges]]
from = "writer"
to = "ok"
on = "ready"
[[edges]]
from = "ok"
to = "@done"
on = "pass"
[[edges]]
from = "ok"
to = "@fail"
on = "fail"
"#;

#[test]
fn a_linear_flow_finishes_in_one_pass() {
    let wf = Workflow::from_toml(LINEAR).unwrap();
    let m = super::caps::Matrix::shipped();
    assert_eq!(
        super::check::check(&wf, &m, &super::check::Ctx::ideal(&m)).errors(),
        0
    );
    let mut w = World::new("wf-linear");
    let mut r = start(&mut w, wf, opts());
    assert_eq!((r.ceiling, r.cycles), (1, 1));
    tick(&mut w, &mut r);
    edit(&mut w, &r, "writer", "src/file", "x\n");
    submit(&mut w, &r, "writer", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(
        (r.state, r.cycles, r.wasted),
        (RunState::Succeeded, 1, 0),
        "{:?}",
        r.log
    );
    assert_eq!(r.nodes["writer"].visits, 1);
    // No gate, so nothing was pushed: the result is the commit on the run's branch.
    assert!(w.hub.prs.iter().all(|p| p.number == 12));
    // The same flow whose check fails ends the run: its edge leads to @fail.
    let mut w2 = World::new("wf-linear-fail");
    let mut r2 = start(&mut w2, Workflow::from_toml(LINEAR).unwrap(), opts());
    tick(&mut w2, &mut r2);
    edit(&mut w2, &r2, "writer", "src/other", "x\n");
    submit(&mut w2, &r2, "writer", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(
        (r2.state, r2.why.as_str()),
        (RunState::Failed, "ok gave fail")
    );
    done(w);
    done(w2);
}

#[test]
fn the_run_stops_at_the_first_wasted_cycle() {
    let (mut w, mut r) = begun("wf-wasted", opts());
    edit(&mut w, &r, "implementer", "src/fix.js", "reset();\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 3);
    assert_eq!((r.cycles, r.nodes["implementer"].visits), (2, 2));
    // It hands in again without changing anything.
    submit(&mut w, &r, "implementer", READY).unwrap();
    tick(&mut w, &mut r);
    assert_eq!((r.state, r.wasted), (RunState::Failed, 1));
    assert_eq!(
        r.why,
        "no progress: implementer came back and changed nothing (1 wasted cycle)"
    );
    assert_eq!(
        r.nodes["tests"].visits, 1,
        "the check wasn't run again for nothing"
    );
    // With no_progress off (as triage has it) the run goes another round.
    let mut w2 = World::new("wf-wasted-off");
    let mut wf = issue_to_pr();
    wf.limits.no_progress = Some(NoProgress::Word("off".into()));
    let mut r2 = start(&mut w2, wf, opts());
    tick(&mut w2, &mut r2);
    edit(&mut w2, &r2, "implementer", "src/fix.js", "reset();\n");
    submit(&mut w2, &r2, "implementer", READY).unwrap();
    ticks(&mut w2, &mut r2, 3);
    submit(&mut w2, &r2, "implementer", READY).unwrap();
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(
        (r2.state, r2.wasted, r2.nodes["tests"].visits),
        (RunState::Running, 2, 2),
        "{:?}",
        r2.log
    );
    done(w);
    done(w2);
}

#[test]
fn a_mention_never_wakes_a_node_and_arrives_on_its_next_visit() {
    let (mut w, mut r) = begun("wf-mention", opts());
    let me = "issue-to-pr-42-implementer";
    // A mention of something with no inbox is refused when it's submitted.
    for (text, why) in [
        (
            "@tests please run again",
            "@tests is a check: it has no inbox",
        ),
        (
            "@gate hold the merge",
            "@gate is a gh.gate: it has no inbox",
        ),
        ("@nobody look", "@nobody isn't a node of run"),
    ] {
        let e = op(&mut w, Some(me), None, Op::Ask { text: text.into() }).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
    assert!(
        op(&mut w, None, Some(&r.id), Op::Ask { text: "hi".into() })
            .unwrap_err()
            .contains("only a node's agent")
    );
    // A mention of an agent node is taken.
    let q = "Is the reset per client? @reviewer mind the shared-window case in src/fix.js";
    let said = op(&mut w, Some(me), None, Op::Ask { text: q.into() })
        .unwrap()
        .0;
    assert!(said.contains("asked the user"), "{said}");
    tick(&mut w, &mut r);
    // No supervisor: the question comes to you. The reviewer isn't woken:
    // not active, not started, no edge was added.
    assert_eq!(
        (r.ask, r.about.as_deref()),
        (Some(Ask::Question), Some("implementer"))
    );
    assert_eq!(
        (
            r.nodes["reviewer"].state,
            r.nodes["reviewer"].visits,
            r.nodes["reviewer"].mentions.len()
        ),
        (NodeState::Waiting, 0, 1)
    );
    assert_eq!(
        (r.active.clone(), w.spawned.len(), r.workflow.edges.len()),
        (
            vec!["implementer".to_string()],
            1,
            issue_to_pr().edges.len()
        )
    );
    // Your answer goes back to the asker as its next prompt.
    op(&mut w, None, Some(&r.id), Op::Answer { key: 'c' }).unwrap();
    tick(&mut w, &mut r);
    assert!(w.sent.last().unwrap().1.contains("use your best judgment"));
    // On the reviewer's visit along its declared edge, the mention is in its prompt.
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 3);
    let p = &w.spawned[1].prompt;
    assert!(p.contains("Passed along to you:\nimplementer mentioned you: Is the reset per client? @reviewer mind the shared-window case"), "{p}");
    assert!(r.nodes["reviewer"].mentions.is_empty());
    done(w);
}

#[test]
fn a_question_goes_to_the_supervisor_and_its_answer_back_to_the_asker() {
    let mut w = World::new("wf-supervisor");
    let mut wf = issue_to_pr();
    wf.nodes.insert(
        "lead".into(),
        Node {
            kind: "supervisor".into(),
            agent: Some("claude".into()),
            profile: Some("read-only".into()),
            brief: Some(Brief {
                owns: vec![],
                must_not_touch: vec![],
                done: "Each question answered, or escalated.".into(),
            }),
            ..Node::default()
        },
    );
    let m = super::caps::Matrix::shipped();
    assert_eq!(
        super::check::check(&wf, &m, &super::check::Ctx::ideal(&m)).errors(),
        0,
        "a supervisor needs no edge"
    );
    let mut r = start(&mut w, wf, opts());
    tick(&mut w, &mut r);
    let me = "issue-to-pr-42-implementer";
    let said = op(
        &mut w,
        Some(me),
        None,
        Op::Ask {
            text: "Per client, or shared?".into(),
        },
    )
    .unwrap()
    .0;
    assert!(said.contains("asked lead"), "{said}");
    ticks(&mut w, &mut r, 2);
    assert_eq!(w.spawned[1].name, "issue-to-pr-42-lead");
    assert!(
        w.spawned[1]
            .prompt
            .contains("implementer asks:\nPer client, or shared?"),
        "{}",
        w.spawned[1].prompt
    );
    // The supervisor never answers a permission prompt: it answers questions.
    submit(
        &mut w,
        &r,
        "lead",
        r#"{"verdict": "answer", "answer": "Per client. @judge the issue says so in its title"}"#,
    )
    .unwrap();
    tick(&mut w, &mut r);
    let (to, text) = w.sent.last().unwrap().clone();
    assert_eq!(to, me);
    assert!(
        text.starts_with("The supervisor answers your question:\nPer client."),
        "{text}"
    );
    // A mention in the answer is queued too, and wakes nobody.
    assert_eq!(
        (r.nodes["judge"].mentions.len(), r.nodes["judge"].visits),
        (1, 0)
    );
    assert!(r.ask.is_none() && r.active == ["implementer"]);
    // ESCALATE: the question comes to you.
    op(
        &mut w,
        Some(me),
        None,
        Op::Ask {
            text: "May I delete the old API?".into(),
        },
    )
    .unwrap();
    w.state("issue-to-pr-42-lead", "done");
    ticks(&mut w, &mut r, 2);
    submit(&mut w, &r, "lead", r#"{"verdict": "escalate"}"#).unwrap();
    tick(&mut w, &mut r);
    assert_eq!(r.ask, Some(Ask::Question));
    assert!(
        r.why
            .contains("the supervisor escalated: May I delete the old API?"),
        "{}",
        r.why
    );
    done(w);
}

#[test]
fn an_out_of_scope_change_goes_back_and_a_base_merge_doesnt_count() {
    let (mut w, mut r) = begun("wf-scope", opts());
    // README.md isn't in src/** or test/**; .github is on the must-not-touch list.
    edit(&mut w, &r, "implementer", "README.md", "# mine now\n");
    edit(
        &mut w,
        &r,
        "implementer",
        ".github/workflows/ci.yml",
        "on: push\n",
    );
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    tick(&mut w, &mut r);
    // Sent back to the node, as a cycle of cost; the check didn't run.
    assert_eq!(
        (r.nodes["implementer"].state, r.cycles, r.active.clone()),
        (NodeState::Waiting, 2, vec!["implementer".to_string()])
    );
    assert!(w.checks.is_empty());
    tick(&mut w, &mut r);
    let again = &w.sent.last().unwrap().1;
    assert!(
        again.contains("you changed files outside your brief: .github/workflows/ci.yml, README.md"),
        "{again}"
    );
    // It undoes them. Then main moves, and it merges the base in: the base's
    // files aren't its own, however far outside its scope they are.
    let dir = r.nodes["implementer"].cwd.clone().unwrap();
    w.sh(&dir, &["rm", "-q", ".github/workflows/ci.yml"]);
    w.sh(&dir, &["checkout", "origin/main", "--", "README.md"]);
    w.commit(&dir, "undo");
    w.upstream("main", "docs/guide.md", "# guide\n");
    w.sh(&dir, &["fetch", "--quiet", "origin"]);
    w.sh(&dir, &["merge", "--quiet", "--no-edit", "origin/main"]);
    submit(&mut w, &r, "implementer", READY).unwrap();
    tick(&mut w, &mut r);
    assert_eq!(r.nodes["implementer"].state, NodeState::Done, "{:?}", r.log);
    assert!(
        r.log
            .iter()
            .any(|(_, l)| l == "implementer handed in ready: 1 file"),
        "{:?}",
        r.log
    );
    assert_eq!(r.active, ["tests"]);
    // A reader's checkout is discarded after its visit: it isn't scope-checked.
    ticks(&mut w, &mut r, 2);
    let reviewer = r.nodes["reviewer"].cwd.clone().unwrap();
    std::fs::write(reviewer.join("scratch.txt"), "notes\n").unwrap();
    submit(
        &mut w,
        &r,
        "reviewer",
        r#"{"verdict": "pass", "findings": []}"#,
    )
    .unwrap();
    tick(&mut w, &mut r);
    assert_eq!(r.nodes["reviewer"].state, NodeState::Done);
    done(w);
}

#[test]
fn the_stall_monitor_fires_only_when_nothing_is_pending() {
    let (mut w, mut r) = begun("wf-stall", opts());
    // A node at work is pending: looks come and go, and the monitor is quiet.
    w.later(600);
    ticks(&mut w, &mut r, 3);
    assert!(r.pending() && r.ask.is_none() && r.state == RunState::Running);
    // Nothing at work, nothing next, the goal not met: it fires.
    r.active.clear();
    assert!(!r.pending());
    tick(&mut w, &mut r);
    assert_eq!(
        (r.state, r.ask),
        (RunState::NeedsYou, Some(Ask::Stall)),
        "supervised: on_stall asks"
    );
    assert!(
        r.why
            .starts_with("nothing is pending and the goal isn't met")
    );
    // A question waiting for you is pending: it doesn't fire again on top.
    let said = r.log.len();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.log.len(), said);
    // On autopilot it parks and tells you, instead of hanging.
    let mut wf = issue_to_pr();
    wf.mode = Mode::Autopilot;
    let mut w2 = World::new("wf-stall-auto");
    let mut r2 = start(&mut w2, wf, opts());
    r2.active.clear();
    tick(&mut w2, &mut r2);
    assert_eq!((r2.state, r2.ask), (RunState::Parked, None));
    assert!(
        w2.notified
            .iter()
            .any(|(t, b)| t == "issue-to-pr-42 is parked" && b.starts_with("nothing is pending"))
    );
    // Parked, nothing runs until you resume it.
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(w2.spawned.len(), 0);
    done(w);
    done(w2);
}

#[test]
fn a_quota_stop_is_visible_backs_off_off_the_clock_and_escalates() {
    let (mut w, mut r) = begun("wf-quota", opts());
    let pane = "issue-to-pr-42-implementer";
    tick(&mut w, &mut r);
    w.later(60);
    tick(&mut w, &mut r);
    let worked = r.inflight;
    assert_eq!(
        worked, 30,
        "time with a node running counts, a look at a time"
    );
    // It stops, and its screen says why.
    w.screens.insert(
        pane.into(),
        "⏺ Working…\nYou've hit your usage limit. Your limit resets at 3am.\n".into(),
    );
    w.state(pane, "idle");
    tick(&mut w, &mut r);
    w.later(20);
    tick(&mut w, &mut r);
    // Blocked, and said: on the node, in the log, in the overview. Not a nudge.
    assert_eq!(r.nodes["implementer"].state, NodeState::Blocked);
    assert!(
        r.log
            .last()
            .unwrap()
            .1
            .contains("implementer is blocked: codex is out of quota or rate-limited: retry in 5m")
    );
    let view = r.view(&w.seen);
    assert_eq!(
        (
            view.blocked(),
            view.nodes
                .iter()
                .find(|n| n.id == "implementer")
                .unwrap()
                .last
                .as_str()
        ),
        (1, "quota · retry 1 of 6")
    );
    assert!(
        !w.sent
            .iter()
            .any(|(_, t)| t.contains("waiting for your report"))
    );
    // Backoff: 5 minutes, doubling. None of it counts against the wall clock.
    let clock = r.inflight;
    let mut waits = Vec::new();
    for _ in 0..4 {
        let q = r.nodes["implementer"].quota.clone().unwrap();
        waits.push(q.next_at - w.now);
        w.now = q.next_at;
        tick(&mut w, &mut r);
        w.state(pane, "idle");
    }
    assert_eq!(waits, [300, 600, 1200, 2400]);
    assert_eq!(r.inflight, clock, "backoff time is off the clock");
    // Six tries or two hours, whichever comes first: here the two hours. It
    // escalates. Never silent.
    let q = r.nodes["implementer"].quota.clone().unwrap();
    w.now = q.next_at;
    tick(&mut w, &mut r);
    assert_eq!(
        (r.state, r.ask, r.about.as_deref()),
        (RunState::NeedsYou, Some(Ask::Quota), Some("implementer"))
    );
    assert!(
        r.why
            .starts_with("implementer is still out of quota after 5 retries over 155m"),
        "{}",
        r.why
    );
    assert_eq!(
        w.spawned.len(),
        1,
        "blocked isn't exited: it was never started again"
    );
    // The screens the shipped patterns know, and ones they mustn't take for quota.
    for s in [
        "You've hit your usage limit. Your limit resets at 3am.",
        "■ You've hit your usage limit. Try again in 2h 5m.",
        "Quota exceeded for quota metric 'Generate Content API requests per minute'.",
        "API Error: 429 Too Many Requests",
        "Rate limit reached for requests",
        "error: RESOURCE_EXHAUSTED",
    ] {
        assert!(engine::quota_screen(s), "{s}");
    }
    for s in [
        "All 212 tests passed.",
        "Added a quota field to the config.",
        "? for shortcuts",
    ] {
        assert!(!engine::quota_screen(s), "{s}");
    }
    // On autopilot the escalation parks; a fallback backend takes over instead.
    let mut wf = issue_to_pr();
    wf.mode = Mode::Autopilot;
    wf.nodes.get_mut("implementer").unwrap().on_quota = Some(OnQuota {
        fallback: Some("claude".into()),
        ..OnQuota::default()
    });
    let mut w2 = World::new("wf-quota-fallback");
    let mut r2 = start(&mut w2, wf, opts());
    ticks(&mut w2, &mut r2, 2);
    w2.screens.insert(pane.into(), "Rate limit reached".into());
    w2.state(pane, "idle");
    tick(&mut w2, &mut r2);
    w2.later(20);
    tick(&mut w2, &mut r2);
    assert_eq!(r2.nodes["implementer"].state, NodeState::Blocked);
    w2.later(301);
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(
        (w2.spawned.len(), w2.spawned[1].kind.as_str()),
        (2, "claude"),
        "{:?}",
        r2.log
    );
    assert_eq!(r2.nodes["implementer"].state, NodeState::Working);
    done(w);
    done(w2);
}

#[test]
fn an_idle_agent_is_nudged_never_restarted_and_an_exited_one_is_started_again() {
    let (mut w, mut r) = begun("wf-idle", opts());
    let pane = "issue-to-pr-42-implementer";
    tick(&mut w, &mut r);
    // Idle at its prompt, no report: after the settle time, one reminder.
    w.state(pane, "idle");
    tick(&mut w, &mut r);
    w.later(10);
    tick(&mut w, &mut r);
    assert!(w.sent.is_empty(), "not before the settle time");
    w.later(10);
    tick(&mut w, &mut r);
    assert!(w.sent.last().unwrap().1.starts_with(
        "trix workflow run issue-to-pr-42 is waiting for your report as implementer."
    ));
    assert_eq!(w.spawned.len(), 1, "nudged, never restarted");
    // It stops again: yours. Still not restarted.
    tick(&mut w, &mut r);
    w.state(pane, "done");
    tick(&mut w, &mut r);
    w.later(20);
    tick(&mut w, &mut r);
    assert_eq!((r.ask, w.spawned.len()), (Some(Ask::NoReport), 1));
    assert!(r.why.contains("stopped again without its report"));
    // The report comes after all: no need for your answer.
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    tick(&mut w, &mut r);
    assert!(
        r.ask.is_none() && r.nodes["implementer"].state == NodeState::Done,
        "{:?}",
        r.log
    );
    // An agent that exits is started again, with a hand-off prompt, twice.
    let (mut w, mut r) = begun("wf-exit", opts());
    for again in 1..=2 {
        w.state(pane, "exited");
        tick(&mut w, &mut r);
        assert_eq!(r.nodes["implementer"].restarts, again);
        tick(&mut w, &mut r);
        assert_eq!(w.spawned.len() as u32, again + 1);
        assert!(
            w.spawned
                .last()
                .unwrap()
                .prompt
                .contains("exited before it handed in: pick up where it left off")
        );
    }
    // A third exit is the `exited` event: its edge ends the run.
    w.state(pane, "exited");
    tick(&mut w, &mut r);
    assert_eq!(
        (r.state, r.why.as_str()),
        (RunState::Failed, "implementer ended with exited")
    );
    done(w);
}

#[test]
fn injected_failures_recover() {
    // An agent that can't start: asked, then started on r.
    let mut w = World::new("wf-recover");
    w.refuse.push("issue-to-pr-42-implementer".into());
    let mut r = start(
        &mut w,
        issue_to_pr(),
        Opts {
            approvals: true,
            ..opts()
        },
    );
    tick(&mut w, &mut r);
    assert_eq!(r.ask, Some(Ask::Setup));
    assert!(
        r.why
            .contains("couldn't start implementer: codex isn't installed")
    );
    w.refuse.clear();
    op(&mut w, None, Some(&r.id), Op::Answer { key: 'r' }).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.nodes["implementer"].state, NodeState::Working);
    // A flaky check is run once more, and that isn't a cycle.
    r.workflow.limits.flaky_check_reruns = Some(1);
    let flaky = "test -f ../flaked || { touch ../flaked; echo flake; exit 1; }; test -f src/done";
    r.workflow.nodes.get_mut("tests").unwrap().run = vec!["sh".into(), "-c".into(), flaky.into()];
    to_gate(&mut w, &mut r);
    assert_eq!(
        (
            w.checks.len(),
            r.cycles,
            r.nodes["tests"].verdict.as_deref()
        ),
        (2, 1, Some("pass"))
    );
    // The push fails: parked, with why. Resumed, it goes on from there.
    let away = w.base.join("origin.away");
    std::fs::rename(&w.origin, &away).unwrap();
    tick(&mut w, &mut r);
    assert_eq!(r.state, RunState::Parked);
    assert!(r.why.starts_with("the push of"), "{}", r.why);
    std::fs::rename(&away, &w.origin).unwrap();
    op(&mut w, None, Some(&r.id), Op::Resume).unwrap();
    // CI is still running, then red with no edge for it: parked again.
    w.hub.ci = runs(&[("test", "in_progress", None)]);
    ticks(&mut w, &mut r, 2);
    assert_eq!(
        (r.state, r.gate.pr),
        (RunState::Running, Some(20)),
        "{:?}",
        r.log
    );
    w.hub.ci = runs(&[("test", "completed", Some("failure"))]);
    tick(&mut w, &mut r);
    assert_eq!(r.why, "CI failed on the checked commit: test");
    // Green again, but the merge fails once: parked, resumed, merged.
    w.hub.ci = green();
    w.hub.merge_errors = vec!["GraphQL: Something went wrong".into()];
    op(&mut w, None, Some(&r.id), Op::Resume).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.state, RunState::Parked);
    assert!(
        r.why
            .contains("the merge of PR #20 failed: GraphQL: Something went wrong"),
        "{}",
        r.why
    );
    op(&mut w, None, Some(&r.id), Op::Resume).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.state, RunState::Succeeded, "{:?}", r.log);
    // Paused, nothing runs; aborted, branches and worktrees stay.
    let (mut w2, mut r2) = begun("wf-pause", opts());
    op(&mut w2, None, Some(&r2.id), Op::Pause).unwrap();
    tick(&mut w2, &mut r2);
    assert_eq!(r2.state, RunState::Paused);
    edit(&mut w2, &r2, "implementer", "src/done", "ok\n");
    submit(&mut w2, &r2, "implementer", READY).unwrap();
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(
        r2.nodes["implementer"].state,
        NodeState::Working,
        "its report waits"
    );
    op(&mut w2, None, Some(&r2.id), Op::Resume).unwrap();
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(r2.nodes["implementer"].state, NodeState::Done);
    op(&mut w2, None, Some(&r2.id), Op::Abort).unwrap();
    tick(&mut w2, &mut r2);
    assert_eq!(r2.state, RunState::Aborted);
    assert!(r2.tree().is_dir() && w2.seen.is_empty());
    done(w);
    done(w2);
}

#[test]
fn a_report_is_checked_against_what_the_node_declares() {
    let (mut w, mut r) = begun("wf-report", opts());
    // Only the node's own agent hands in its report.
    let e = op(
        &mut w,
        Some("issue-to-pr-42-judge"),
        Some(&r.id),
        Op::Submit {
            node: "implementer".into(),
            report: READY.into(),
        },
    )
    .unwrap_err();
    assert!(
        e.contains("only issue-to-pr-42-implementer hands in implementer's report"),
        "{e}"
    );
    for (report, why) in [
        ("not json", "the report isn't JSON"),
        ("[1]", "a JSON object"),
        (r#"{"summary": "x"}"#, "verdict: say which one"),
        (
            r#"{"verdict": "pass"}"#,
            "'pass' isn't one implementer gives (ready)",
        ),
    ] {
        let e = submit(&mut w, &r, "implementer", report).unwrap_err();
        assert!(
            e.contains(why) && e.ends_with("Fix the report and submit it again."),
            "{e}"
        );
    }
    assert!(
        submit(&mut w, &r, "tests", READY)
            .unwrap_err()
            .contains("tests is a check: it doesn't hand in a report")
    );
    assert!(
        submit(
            &mut w,
            &r,
            "judge",
            r#"{"verdict": "pass", "reasons": "x"}"#
        )
        .unwrap_err()
        .contains("isn't waiting for judge's report")
    );
    // A field the node declares, with its type.
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 3);
    let e = submit(
        &mut w,
        &r,
        "reviewer",
        r#"{"verdict": "changes", "findings": "broken"}"#,
    )
    .unwrap_err();
    assert!(
        e.contains("findings: reviewer's report carries it, as list<finding>"),
        "{e}"
    );
    let e = submit(&mut w, &r, "reviewer", r#"{"verdict": "changes"}"#).unwrap_err();
    assert!(e.contains("findings:"), "{e}");
    // Changes go back to the author with the findings; the verdict is what was judged.
    submit(&mut w, &r, "reviewer", r#"{"verdict": "changes", "findings": [{"file": "src/fix.js", "line": 1, "what": "no test"}]}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    assert!(
        w.sent.last().unwrap().1.contains("\"what\": \"no test\""),
        "{}",
        w.sent.last().unwrap().1
    );
    assert_eq!(
        (r.cycles, r.nodes["reviewer"].outputs["commit"].as_str()),
        (2, r.pinned.as_deref())
    );
    // A run that's over takes no more.
    op(&mut w, None, Some(&r.id), Op::Abort).unwrap();
    tick(&mut w, &mut r);
    assert!(
        submit(&mut w, &r, "implementer", READY)
            .unwrap_err()
            .contains("it takes no more reports")
    );
    // run.json carries its version, and another's is refused.
    let file = engine::dir_of(&root(&w), &r.id).join("run.json");
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replacen("\"v\": 1", "\"v\": 2", 1);
    std::fs::write(&file, text).unwrap();
    assert!(
        <WfRun as Run>::load(&root(&w), &r.id)
            .unwrap_err()
            .contains("written by another trix")
    );
    done(w);
}

// ── starting a run, and the commands ──

fn issue_api(w: &mut World, association: &str) {
    w.hub.answers = vec![
        ("api repos/{owner}/{repo}".into(), r#"{"full_name": "acme/app", "default_branch": "main"}"#.into()),
        (
            "api repos/{owner}/{repo}/issues/42".into(),
            serde_json::json!({"number": 42, "title": "The window never resets", "body": "After 60s a client is still refused.",
                "html_url": "https://example.com/issues/42", "user": {"login": "ann"}, "author_association": association})
            .to_string(),
        ),
    ];
}

#[test]
fn a_run_starts_checked_with_its_inputs_resolved_and_its_snapshot_taken() {
    use super::launch::{Launch, start as launch};
    let mut w = World::new("wf-start");
    let m = super::caps::Matrix::shipped();
    let ctx = super::check::Ctx::ideal(&m);
    let settings = super::settings::Settings::default();
    let (repo, runs_dir) = (w.repo.clone(), root(&w));
    let stores = super::store::Stores {
        team: None,
        personal: w.base.join("workflows"),
    };
    let l = Launch {
        stores: &stores,
        repo: &repo,
        m: &m,
        ctx: &ctx,
        settings: &settings,
        trusted: true,
    };
    let given = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    issue_api(&mut w, "OWNER");
    // An input it needs, an input it doesn't have, a value that isn't one.
    let e = launch(&mut w, &runs_dir, &l, &issue_to_pr(), &given(&[])).unwrap_err();
    assert_eq!(e, "issue-to-pr needs --input issue=<int>");
    assert!(
        launch(
            &mut w,
            &runs_dir,
            &l,
            &issue_to_pr(),
            &given(&[("pr", "7")])
        )
        .unwrap_err()
        .contains("'pr' isn't an input of issue-to-pr (issue)")
    );
    assert!(
        launch(
            &mut w,
            &runs_dir,
            &l,
            &issue_to_pr(),
            &given(&[("issue", "soon")])
        )
        .unwrap_err()
        .contains("isn't an issue number")
    );
    // A workflow with an error doesn't start, and says exactly what check says.
    let mut broken = issue_to_pr();
    broken.limits.wall_clock = None;
    let e = launch(&mut w, &runs_dir, &l, &broken, &given(&[("issue", "42")])).unwrap_err();
    assert_eq!(e, super::check::check(&broken, &m, &ctx).render());
    assert!(e.starts_with("issue-to-pr: 1 error, 2 warnings\n"), "{e}");
    // Untrusted, or changed since trust was given: it doesn't start either.
    let untrusted = super::check::Ctx {
        trusted: Some(false),
        ..ctx.clone()
    };
    let lu = Launch {
        stores: &stores,
        repo: &repo,
        m: &m,
        ctx: &untrusted,
        settings: &settings,
        trusted: false,
    };
    assert!(
        launch(
            &mut w,
            &runs_dir,
            &lu,
            &issue_to_pr(),
            &given(&[("issue", "42")])
        )
        .unwrap_err()
        .contains("it can't run until you trust it")
    );
    assert!(
        w.spawned.is_empty() && !runs_dir.join("issue-to-pr-42").exists(),
        "nothing was started"
    );

    // It starts: named after its issue, its inputs resolved before any prompt.
    let mut r = launch(
        &mut w,
        &runs_dir,
        &l,
        &issue_to_pr(),
        &given(&[("issue", "#42")]),
    )
    .unwrap();
    assert_eq!(
        (r.id.as_str(), r.base.as_str(), r.branch.as_str()),
        ("issue-to-pr-42", "main", "trix/wf-issue-to-pr-42")
    );
    assert_eq!(r.inputs["issue"]["title"], "The window never resets");
    assert_eq!(
        (r.opts.repo_name.as_str(), r.opts.approvals),
        ("acme/app", false),
        "approvals need [approvals] allow too"
    );
    assert_eq!(r.opts.floors, FLOORS, "the three floors are on");
    assert_eq!(r.opts.priced, ["claude", "codex"]);
    // The snapshot: the run reads it, whatever becomes of the file.
    assert_eq!(
        super::store::snapshot_of(&engine::dir_of(&runs_dir, &r.id)).unwrap(),
        issue_to_pr()
    );
    assert_eq!(engine::load(&runs_dir, &r.id).unwrap(), r);
    tick(&mut w, &mut r);
    assert!(
        w.spawned[0]
            .prompt
            .contains("After 60s a client is still refused.")
    );
    // A second run for the same issue gets a name of its own.
    assert_eq!(
        engine::new_id(&runs_dir, "issue-to-pr", Some("42")),
        "issue-to-pr-42-2"
    );
    assert_eq!(
        engine::new_id(&runs_dir, "nightly-deps", None),
        "nightly-deps-1"
    );

    // Text from outside the project: you read it before any agent does.
    let mut w2 = World::new("wf-start-outside");
    issue_api(&mut w2, "NONE");
    let (repo2, runs2) = (w2.repo.clone(), root(&w2));
    let l2 = Launch {
        stores: &stores,
        repo: &repo2,
        m: &m,
        ctx: &ctx,
        settings: &settings,
        trusted: true,
    };
    let mut r2 = launch(
        &mut w2,
        &runs2,
        &l2,
        &issue_to_pr(),
        &given(&[("issue", "42")]),
    )
    .unwrap();
    tick(&mut w2, &mut r2);
    assert_eq!(r2.ask, Some(Ask::Outsider));
    assert!(
        r2.why.contains("@ann (NONE)") && w2.spawned.is_empty(),
        "{}",
        r2.why
    );
    op(&mut w2, None, Some(&r2.id), Op::Answer { key: 's' }).unwrap();
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(w2.spawned.len(), 1);
    // With approvals allowed in your config and the file trusted, they're on.
    let on = super::settings::Settings {
        approvals: super::settings::ApprovalOpts {
            allow: vec!["issue-to-pr".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(super::launch::opts(&issue_to_pr(), &m, &ctx, &on, true, "acme/app").approvals);
    assert!(
        !super::launch::opts(&issue_to_pr(), &m, &ctx, &on, false, "acme/app").approvals,
        "not when it isn't trusted"
    );
    done(w);
    done(w2);
}

#[test]
fn the_run_and_wf_commands_read_their_arguments() {
    use super::cli::{self, Cmd, WfCmd};
    let words = |l: &str| l.split_whitespace().map(String::from).collect::<Vec<_>>();
    let id = || "issue-to-pr-42".to_string();
    for (line, want) in [
        ("pane issue-to-pr-42", Cmd::Pane(id())),
        (
            "show issue-to-pr-42",
            Cmd::Show {
                id: id(),
                json: false,
            },
        ),
        (
            "show issue-to-pr-42 --json",
            Cmd::Show {
                id: id(),
                json: true,
            },
        ),
        ("approve issue-to-pr-42", Cmd::Approve(id())),
        ("abort issue-to-pr-42", Cmd::Abort(id())),
        ("pause issue-to-pr-42", Cmd::Pause(id())),
        ("resume issue-to-pr-42", Cmd::Resume(id())),
        ("revoke issue-to-pr-42", Cmd::Revoke(id())),
        (
            "answer issue-to-pr-42 C",
            Cmd::Answer { id: id(), key: 'c' },
        ),
        (
            "run issue-to-pr --input issue=42",
            Cmd::Run {
                name: "issue-to-pr".into(),
                inputs: [("issue".to_string(), "42".to_string())].into(),
            },
        ),
    ] {
        assert_eq!(cli::parse(&words(line)), Ok(want), "trix workflow {line}");
    }
    assert!(
        cli::parse(&words("answer issue-to-pr-42 yes")).is_err()
            && cli::parse(&words("approve")).is_err()
    );
    // What the agents run.
    assert_eq!(
        cli::parse_wf(&words("submit implementer")),
        Ok(WfCmd::Submit {
            node: "implementer".into(),
            file: None
        })
    );
    assert_eq!(
        cli::parse_wf(&words("submit implementer --file .trix-implementer.json")),
        Ok(WfCmd::Submit {
            node: "implementer".into(),
            file: Some(".trix-implementer.json".into())
        })
    );
    assert_eq!(cli::parse_wf(&words("ask")), Ok(WfCmd::Ask { file: None }));
    assert_eq!(
        cli::parse_wf(&words("ask --file q.txt")),
        Ok(WfCmd::Ask {
            file: Some("q.txt".into())
        })
    );
    assert!(
        cli::parse_wf(&words("submit")).is_err() && cli::parse_wf(&words("approve x")).is_err()
    );
    // A run, shown: what it's for, where each node is, what it cost so far.
    let (mut w, mut r) = begun("wf-show", opts());
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 3);
    let text = cli::show(&r, &w.seen);
    assert!(text.starts_with("issue-to-pr-42 · issue-to-pr · running\nIssue #42 is solved by a PR with tests and green CI.\n"), "{text}");
    assert!(
        text.contains(
            "2 of 5 done · 0 blocked · cycles 1 (0 wasted) of at most 6 · $2.00 · 0m of 180m"
        ),
        "{text}"
    );
    let row = |id: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(id))
            .unwrap()
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        row("implementer")[..4],
        ["implementer", "done", "codex", "src/**,test/**"]
    );
    assert_eq!(row("tests")[..3], ["tests", "done", "check"]);
    assert_eq!(row("reviewer")[..3], ["reviewer", "working", "claude"]);
    assert_eq!(row("gate")[..3], ["gate", "waiting", "trix"]);
    // An unpriced agent's spend is unknown, never $0.
    submit(
        &mut w,
        &r,
        "reviewer",
        r#"{"verdict": "pass", "findings": []}"#,
    )
    .unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.spend(&w.seen), (3.0, true));
    assert!(cli::show(&r, &w.seen).contains("· $3.00 + ? ·"));
    done(w);
}

// ── workspaces and landing ──

/// Two writers no edge puts in order, a land step, a check on what landed,
/// a verifier on a third backend, and the gate. `prep` runs before both.
pub fn pair(api_owns: &str, ui_owns: &str) -> Workflow {
    let text = format!(
        r#"
schema = 1
name = "pair"
entry = ["prep"]
[goal]
text = "Both halves are in."
[goal.done_when]
mechanical = [{{ check = "tests" }}]
attested = [{{ judge = "verifier" }}]
[limits]
wall_clock = "1h"
max_iterations = 4
[policy]
push = "guarded"
merge = "you"
[nodes.prep]
kind = "agent"
agent = "claude"
profile = "edit-only"
verdicts = ["ready"]
brief = {{ owns = ["notes/**", "src/**"], done = "The ground is laid." }}
[nodes.api]
kind = "agent"
agent = "claude"
profile = "edit-only"
verdicts = ["ready"]
brief = {{ owns = ["{api_owns}"], done = "The API half is committed." }}
[nodes.ui]
kind = "agent"
agent = "codex"
profile = "edit-and-test"
verdicts = ["ready"]
brief = {{ owns = ["{ui_owns}"], done = "The UI half is committed." }}
[nodes.land]
kind = "land"
on_conflict = "author"
[nodes.tests]
kind = "check"
run = ["sh", "-c", "test -f src/api/a && test -f src/ui/b"]
commit = "land.head"
[nodes.verifier]
kind = "agent"
preset = "verifier"
agent = "gemini"
profile = "instruct-read-only"
verdicts = ["pass", "changes"]
brief = {{ owns = [], done = "Both halves are what was asked." }}
[nodes.gate]
kind = "gh.gate"
[[edges]]
from = "prep"
to = "api"
on = "ready"
[[edges]]
from = "prep"
to = "ui"
on = "ready"
[[edges]]
from = "api"
to = "land"
on = "ready"
[[edges]]
from = "ui"
to = "land"
on = "ready"
[[edges]]
from = "land"
to = "tests"
on = "landed"
[[edges]]
from = "tests"
to = "verifier"
on = "pass"
[[edges]]
from = "tests"
to = "@fail"
on = "fail"
[[edges]]
from = "verifier"
to = "gate"
on = "pass"
[[edges]]
from = "verifier"
to = "@fail"
on = "changes"
"#
    );
    Workflow::from_toml(&text).unwrap()
}

/// prep hands in, leaving a staged and an untracked file behind, and both
/// halves start.
fn halves(name: &str, wf: Workflow) -> (World, WfRun) {
    let mut w = World::new(name);
    let mut r = start(&mut w, wf, opts());
    tick(&mut w, &mut r);
    let tree = r.tree();
    edit(&mut w, &r, "prep", "notes/plan.md", "the plan\n");
    std::fs::write(tree.join("notes/staged.md"), "staged, not committed\n").unwrap();
    w.sh(&tree, &["add", "notes/staged.md"]);
    std::fs::write(tree.join("notes/untracked.md"), "untracked\n").unwrap();
    submit(&mut w, &r, "prep", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.active, ["api", "ui"], "{:?}", r.log);
    assert_eq!(
        (r.nodes["api"].state, r.nodes["ui"].state),
        (NodeState::Working, NodeState::Working)
    );
    (w, r)
}

#[test]
fn writers_in_order_share_one_worktree() {
    let mut wf = issue_to_pr();
    let docs = Node {
        kind: "agent".into(),
        agent: Some("claude".into()),
        profile: Some("edit-only".into()),
        verdicts: vec!["ready".into()],
        brief: Some(Brief {
            owns: vec!["test/**".into()],
            must_not_touch: vec![],
            done: "The tests are documented.".into(),
        }),
        ..Node::default()
    };
    wf.nodes.insert("docs".into(), docs);
    for e in wf
        .edges
        .iter_mut()
        .filter(|e| e.from == "implementer" && e.to == "tests")
    {
        e.to = "docs".into();
    }
    let on = |from: &str, to: &str| Edge {
        from: from.into(),
        to: to.into(),
        on: Triggers::One("ready".into()),
        guard: None,
        pass: BTreeMap::new(),
        ask: None,
    };
    wf.edges.push(on("docs", "tests"));
    assert!(!super::check::isolated(&wf, "implementer") && !super::check::isolated(&wf, "docs"));
    let mut w = World::new("wf-shared");
    let mut r = start(&mut w, wf, opts());
    tick(&mut w, &mut r);
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(&mut w, &r, "implementer", READY).unwrap();
    ticks(&mut w, &mut r, 2);
    // One after another, in the run's worktree: the second sees the first's work.
    assert_eq!(
        (
            r.nodes["implementer"].cwd.clone(),
            r.nodes["docs"].cwd.clone()
        ),
        (Some(r.tree()), Some(r.tree()))
    );
    assert!(r.tree().join("src/done").is_file() && r.finished.is_empty());
    // Each one's changes are what changed during its own visit.
    edit(&mut w, &r, "docs", "test/README.md", "how to run\n");
    submit(&mut w, &r, "docs", READY).unwrap();
    tick(&mut w, &mut r);
    assert!(
        r.log
            .iter()
            .any(|(_, l)| l == "docs handed in ready: 1 file"),
        "{:?}",
        r.log
    );
    done(w);
}

#[test]
fn writers_that_can_run_at_the_same_time_get_their_own_worktrees_and_land() {
    // Scopes that don't overlap: they're isolated all the same.
    let wf = pair("src/api/**", "src/ui/**");
    let m = super::caps::Matrix::shipped();
    let report = super::check::check(&wf, &m, &super::check::Ctx::ideal(&m));
    assert_eq!(
        (report.errors(), report.has("scope-overlap")),
        (0, false),
        "{}",
        report.render()
    );
    assert!(
        super::check::isolated(&wf, "api")
            && super::check::isolated(&wf, "ui")
            && !super::check::isolated(&wf, "prep")
    );
    let (mut w, mut r) = halves("wf-pair", wf);
    let (api, ui) = (
        r.nodes["api"].cwd.clone().unwrap(),
        r.nodes["ui"].cwd.clone().unwrap(),
    );
    assert!(api != ui && api != r.tree() && ui != r.tree());
    // Each on a branch of its own from the run's branch, with an index of its own.
    assert_eq!(
        w.sh(&api, &["branch", "--show-current"]).trim(),
        "trix/wf-pair-42-api"
    );
    assert_eq!(
        w.sh(&ui, &["branch", "--show-current"]).trim(),
        "trix/wf-pair-42-ui"
    );
    let index = |w: &mut World, d: &std::path::Path| {
        w.sh(
            d,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
    };
    let (ia, iu, ir) = (
        index(&mut w, &api),
        index(&mut w, &ui),
        index(&mut w, &r.tree()),
    );
    assert!(
        ia != iu && ia != ir && iu != ir,
        "no two agents share an index"
    );
    // What was uncommitted in the run's worktree came along: staged stays
    // staged, untracked comes too, and what prep committed is there.
    for tree in [&api, &ui] {
        assert_eq!(
            std::fs::read_to_string(tree.join("notes/plan.md")).unwrap(),
            "the plan\n"
        );
        assert_eq!(
            std::fs::read_to_string(tree.join("notes/untracked.md")).unwrap(),
            "untracked\n"
        );
        let status = w.sh(tree, &["status", "--porcelain"]);
        assert!(
            status.contains("A  notes/staged.md") && status.contains("?? notes/untracked.md"),
            "{status}"
        );
    }
    // The UI half hands in first; the landing waits for the API half.
    for tree in [&api, &ui] {
        w.sh(tree, &["rm", "-q", "--cached", "notes/staged.md"]);
        std::fs::remove_file(tree.join("notes/staged.md")).unwrap();
        std::fs::remove_file(tree.join("notes/untracked.md")).unwrap();
    }
    edit(&mut w, &r, "ui", "src/ui/b", "b\n");
    submit(&mut w, &r, "ui", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(
        (r.active.clone(), r.nodes["land"].visits),
        (vec!["api".to_string(), "land".to_string()], 0)
    );
    // Nothing is removed before it has landed: an attempt is refused.
    let e = super::land::remove_tree(&mut r, &mut w, "ui").unwrap_err();
    assert_eq!(
        e,
        "ui's worktree isn't removed: 1 commit on trix/wf-pair-42-ui hasn't landed on trix/wf-pair-42"
    );
    assert!(ui.is_dir());
    edit(&mut w, &r, "api", "src/api/a", "a\n");
    submit(&mut w, &r, "api", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    // Landed in the order they finished, onto the run's branch.
    assert_eq!(r.finished, ["ui", "api"]);
    let landed: Vec<&str> = r
        .log
        .iter()
        .map(|(_, l)| l.as_str())
        .filter(|l| l.starts_with("landed "))
        .collect();
    assert_eq!(
        landed,
        [
            "landed ui: 1 commit onto trix/wf-pair-42",
            "landed api: 1 commit onto trix/wf-pair-42"
        ]
    );
    assert!(r.tree().join("src/api/a").is_file() && r.tree().join("src/ui/b").is_file());
    // The check runs on the landed commit, which trix pins; the verifier
    // (a third backend, never an author) judges that commit.
    tick(&mut w, &mut r);
    let head = w.sh(&r.tree(), &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(
        (r.pinned.as_deref(), r.nodes["tests"].verdict.as_deref()),
        (Some(head.as_str()), Some("pass"))
    );
    assert_eq!(
        r.nodes["land"].outputs["head"].as_str(),
        Some(head.as_str())
    );
    tick(&mut w, &mut r);
    let v = w.spawned.last().unwrap().clone();
    assert_eq!(
        (
            v.kind.as_str(),
            w.sh(&v.cwd, &["rev-parse", "HEAD"]).trim().to_string()
        ),
        ("gemini", head.clone())
    );
    submit(&mut w, &r, "verifier", r#"{"verdict": "pass"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.ask, Some(Ask::Merge), "{:?}", r.log);
    // The PR's body is the outcome summary: nodes, verdicts, the check, the landing.
    let body = w.hub.pr_bodies.last().unwrap().clone();
    assert!(
        body.contains("| land | land | trix | 1 | landed |")
            && body.contains("| tests | check | check | 1 | pass (exit 0) on"),
        "{body}"
    );
    assert!(
        body.contains("| ui | agent | codex | 1 | ready |")
            && body.contains("Cycles: 1 (0 wasted)"),
        "{body}"
    );
    op(&mut w, None, Some(&r.id), Op::Approve).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.state, RunState::Succeeded, "{:?}", r.log);
    // Landed, their worktrees and branches are gone; the run's stays.
    assert!(!api.is_dir() && !ui.is_dir() && r.tree().is_dir());
    assert!(
        !w.sh(&r.repo.clone(), &["branch", "--list", "trix/wf-pair-42-*"])
            .contains("trix/wf-pair-42-api")
    );
    done(w);
}

#[test]
fn a_land_conflict_goes_back_to_its_author() {
    // Overlapping scopes: flagged while it's built, isolated all the same.
    let wf = pair("src/**", "src/**");
    let m = super::caps::Matrix::shipped();
    let report = super::check::check(&wf, &m, &super::check::Ctx::ideal(&m));
    assert_eq!(report.errors(), 0, "{}", report.render());
    assert!(
        report
            .find("scope-overlap")
            .unwrap()
            .text
            .contains("api and ui can run at the same time")
    );
    let (mut w, mut r) = halves("wf-conflict", wf);
    for node in ["api", "ui"] {
        let tree = r.nodes[node].cwd.clone().unwrap();
        w.sh(&tree, &["rm", "-q", "--cached", "notes/staged.md"]);
        std::fs::remove_file(tree.join("notes/staged.md")).unwrap();
        std::fs::remove_file(tree.join("notes/untracked.md")).unwrap();
    }
    // Both change the same file, each its own way.
    edit(&mut w, &r, "api", "src/shared.txt", "the API's way\n");
    edit(&mut w, &r, "api", "src/api/a", "a\n");
    submit(&mut w, &r, "api", r#"{"verdict": "ready"}"#).unwrap();
    edit(&mut w, &r, "ui", "src/shared.txt", "the UI's way\n");
    edit(&mut w, &r, "ui", "src/ui/b", "b\n");
    submit(&mut w, &r, "ui", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    // The first lands. The second conflicts and goes back to its author,
    // with the files that conflict: a cycle of cost.
    assert_eq!(
        (r.nodes["api"].landed, r.nodes["ui"].landed, r.cycles),
        (true, false, 2),
        "{:?}",
        r.log
    );
    assert_eq!(
        (r.active.clone(), r.nodes["ui"].state),
        (vec!["ui".to_string()], NodeState::Waiting)
    );
    tick(&mut w, &mut r);
    let (to, text) = w
        .sent
        .last()
        .unwrap_or_else(|| panic!("{:?} {:?}", r.log, r.ask))
        .clone();
    assert_eq!(to, "pair-42-ui");
    assert!(
        text.contains("these files conflict: src/shared.txt")
            && text.contains("git rebase trix/wf-pair-42"),
        "{text}"
    );
    // Its worktree is as it left it: the rebase was taken back.
    let ui = r.nodes["ui"].cwd.clone().unwrap();
    assert_eq!(
        std::fs::read_to_string(ui.join("src/shared.txt")).unwrap(),
        "the UI's way\n"
    );
    assert!(w.sh(&ui, &["status", "--porcelain"]).trim().is_empty());
    // It brings the run's branch in, keeps what both meant, and hands in again.
    w.sh(&ui, &["reset", "--quiet", "--hard", "trix/wf-pair-42"]);
    edit(
        &mut w,
        &r,
        "ui",
        "src/shared.txt",
        "the API's way\nand the UI's\n",
    );
    edit(&mut w, &r, "ui", "src/ui/b", "b\n");
    submit(&mut w, &r, "ui", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 3);
    assert_eq!(
        (r.nodes["ui"].landed, r.nodes["tests"].verdict.as_deref()),
        (true, Some("pass")),
        "{:?}",
        r.log
    );
    assert_eq!(
        std::fs::read_to_string(r.tree().join("src/shared.txt")).unwrap(),
        "the API's way\nand the UI's\n"
    );
    // on_conflict: fail ends the run instead.
    let mut wf = pair("src/**", "src/**");
    wf.nodes.get_mut("land").unwrap().on_conflict = Some("fail".into());
    let (mut w2, mut r2) = halves("wf-conflict-fail", wf);
    for (node, text) in [("api", "one\n"), ("ui", "two\n")] {
        let tree = r2.nodes[node].cwd.clone().unwrap();
        w2.sh(&tree, &["rm", "-q", "--cached", "notes/staged.md"]);
        std::fs::remove_file(tree.join("notes/staged.md")).unwrap();
        std::fs::remove_file(tree.join("notes/untracked.md")).unwrap();
        edit(&mut w2, &r2, node, "src/shared.txt", text);
        submit(&mut w2, &r2, node, r#"{"verdict": "ready"}"#).unwrap();
    }
    ticks(&mut w2, &mut r2, 2);
    assert_eq!(r2.state, RunState::Failed);
    assert!(
        r2.why.starts_with(
            "landing ui onto trix/wf-pair-42 conflicted: these files conflict: src/shared.txt"
        ),
        "{}",
        r2.why
    );
    // What didn't land is still there: nothing was removed.
    assert!(r2.nodes["ui"].cwd.clone().unwrap().is_dir());
    done(w);
    done(w2);
}

// ── composition ──

/// nightly-deps on the fixture repository: its check is green once any
/// upgrade landed.
fn nightly() -> Workflow {
    let mut w = builtin("nightly-deps").unwrap();
    w.nodes.get_mut("tests").unwrap().run =
        vec!["sh".into(), "-c".into(), "ls upgraded-* >/dev/null".into()];
    w.limits.deadline = None;
    w.nodes
        .get_mut("upgrade")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .owns = vec!["upgraded-*".into(), "package.json".into()];
    w
}

#[test]
fn a_fan_out_runs_an_agent_per_item_and_the_join_takes_partial_failure() {
    let mut w = World::new("wf-foreach");
    let mut r = start(&mut w, nightly(), opts());
    tick(&mut w, &mut r);
    submit(
        &mut w,
        &r,
        "scan",
        r#"{"verdict": "found", "packages": ["left-pad", "lodash", "react"]}"#,
    )
    .unwrap();
    ticks(&mut w, &mut r, 3);
    // One upgrader per package, at most three... here two at once (parallel).
    assert_eq!(
        r.nodes["upgrade"].outputs["instances"],
        serde_json::json!(["upgrade-1", "upgrade-2", "upgrade-3"])
    );
    let started: Vec<&str> = w.spawned.iter().skip(1).map(|s| s.name.as_str()).collect();
    assert_eq!(
        started,
        [
            "nightly-deps-42-upgrade-1",
            "nightly-deps-42-upgrade-2",
            "nightly-deps-42-upgrade-3"
        ]
    );
    // Each is told its own item, in a worktree and on a branch of its own.
    let p = &w.spawned[1].prompt;
    assert!(
        p.starts_with("Upgrade the dependency left-pad to its newest release")
            && p.contains("trix wf submit upgrade-1"),
        "{p}"
    );
    let trees: Vec<_> = (1..=3)
        .map(|i| r.nodes[&format!("upgrade-{i}")].cwd.clone().unwrap())
        .collect();
    assert!(trees[0] != trees[1] && trees[1] != trees[2] && trees.iter().all(|t| *t != r.tree()));
    assert_eq!(
        w.sh(&trees[1], &["branch", "--show-current"]).trim(),
        "trix/wf-nightly-deps-42-upgrade-2"
    );
    // One upgrades, one is skipped, one exits past its restarts: a failed
    // item, not a failed run.
    edit(&mut w, &r, "upgrade-1", "upgraded-left-pad", "1.3.0\n");
    submit(&mut w, &r, "upgrade-1", r#"{"verdict": "upgraded"}"#).unwrap();
    submit(
        &mut w,
        &r,
        "upgrade-2",
        r#"{"verdict": "skipped", "summary": "its tests need node 22"}"#,
    )
    .unwrap();
    tick(&mut w, &mut r);
    assert_eq!(
        r.nodes["collect"].visits, 0,
        "the join waits for every item"
    );
    for _ in 0..3 {
        w.state(&r.nodes["upgrade-3"].agent.clone().unwrap(), "exited");
        ticks(&mut w, &mut r, 2);
    }
    assert_eq!(
        (r.state, r.nodes["upgrade-3"].state),
        (RunState::Running, NodeState::Failed),
        "{:?}",
        r.log
    );
    ticks(&mut w, &mut r, 2);
    // The join: some succeeded, so the run goes on with those.
    assert_eq!(
        r.nodes["collect"].verdict.as_deref(),
        Some("some"),
        "{:?}",
        r.log
    );
    assert!(
        r.log
            .iter()
            .any(|(_, l)| l == "collect: 1 of 3 succeeded: some"),
        "{:?}",
        r.log
    );
    ticks(&mut w, &mut r, 3);
    // What upgraded landed; the check ran on the landed commit.
    assert!(r.tree().join("upgraded-left-pad").is_file());
    assert_eq!(
        r.nodes["tests"].verdict.as_deref(),
        Some("pass"),
        "{:?}",
        r.log
    );
    // With parallel = 1 they run one after another.
    let mut one = nightly();
    one.nodes.get_mut("upgrade").unwrap().parallel = Some(1);
    let mut w2 = World::new("wf-foreach-one");
    let mut r2 = start(&mut w2, one, opts());
    tick(&mut w2, &mut r2);
    submit(
        &mut w2,
        &r2,
        "scan",
        r#"{"verdict": "found", "packages": ["a", "b"]}"#,
    )
    .unwrap();
    ticks(&mut w2, &mut r2, 3);
    assert_eq!(w2.spawned.len(), 2, "scan, then the first upgrader only");
    submit(&mut w2, &r2, "upgrade-1", r#"{"verdict": "skipped"}"#).unwrap();
    ticks(&mut w2, &mut r2, 3);
    assert_eq!(w2.spawned.last().unwrap().name, "nightly-deps-42-upgrade-2");
    // None succeeded: the join says none, and the template ends there.
    submit(&mut w2, &r2, "upgrade-2", r#"{"verdict": "skipped"}"#).unwrap();
    ticks(&mut w2, &mut r2, 3);
    assert_eq!(
        r2.nodes["collect"].verdict.as_deref(),
        Some("none"),
        "{:?}",
        r2.log
    );
    // both_or_ask: every part was wanted, some came: yours to say.
    let mut both = nightly();
    both.nodes.get_mut("collect").unwrap().wait = Some("both_or_ask".into());
    let mut w3 = World::new("wf-foreach-both");
    let mut r3 = start(&mut w3, both, opts());
    tick(&mut w3, &mut r3);
    submit(
        &mut w3,
        &r3,
        "scan",
        r#"{"verdict": "found", "packages": ["a", "b"]}"#,
    )
    .unwrap();
    ticks(&mut w3, &mut r3, 3);
    edit(&mut w3, &r3, "upgrade-1", "upgraded-a", "2\n");
    submit(&mut w3, &r3, "upgrade-1", r#"{"verdict": "upgraded"}"#).unwrap();
    submit(&mut w3, &r3, "upgrade-2", r#"{"verdict": "skipped"}"#).unwrap();
    ticks(&mut w3, &mut r3, 3);
    assert_eq!(
        (r3.ask, r3.why.as_str()),
        (
            Some(Ask::Partial),
            "1 of 2 parts of collect succeeded: go on with those?"
        )
    );
    op(&mut w3, None, Some(&r3.id), Op::Answer { key: 'c' }).unwrap();
    ticks(&mut w3, &mut r3, 2);
    assert_eq!(r3.nodes["collect"].verdict.as_deref(), Some("some"));
    done(w);
    done(w2);
    done(w3);
}

const OUTER: &str = r#"
schema = 1
name = "outer"
[goal]
text = "The inner workflow ran."
[goal.done_when]
mechanical = [{ check = "ok" }]
[limits]
wall_clock = "1h"
[nodes.inner]
kind = "workflow"
workflow = "INNER"
[nodes.ok]
kind = "check"
run = ["true"]
[[edges]]
from = "inner"
to = "ok"
on = "succeeded"
[[edges]]
from = "inner"
to = "@fail"
on = "failed"
[[edges]]
from = "ok"
to = "@done"
on = "pass"
[[edges]]
from = "ok"
to = "@fail"
on = "fail"
"#;

#[test]
fn a_workflow_node_runs_a_nested_run_rolls_its_spend_up_and_keeps_to_the_depth_limit() {
    use super::launch::{Launch, start as launch};
    let mut w = World::new("wf-nested");
    let stores = super::store::Stores {
        team: None,
        personal: w.base.join("workflows"),
    };
    let mut linear = Workflow::from_toml(LINEAR).unwrap();
    linear.name = "inner".into();
    stores
        .save(&linear, super::store::Origin::Personal)
        .unwrap();
    let outer = Workflow::from_toml(&OUTER.replace("INNER", "inner")).unwrap();
    // An outer that names itself inside: as deep as you let it.
    let mut deep = Workflow::from_toml(&OUTER.replace("INNER", "deep")).unwrap();
    deep.name = "deep".into();
    stores.save(&deep, super::store::Origin::Personal).unwrap();
    let m = super::caps::Matrix::shipped();
    let ctx = super::check::Ctx::ideal(&m);
    let settings = super::settings::Settings::default();
    let (repo, runs_dir) = (w.repo.clone(), root(&w));
    let l = Launch {
        stores: &stores,
        repo: &repo,
        m: &m,
        ctx: &ctx,
        settings: &settings,
        trusted: true,
    };
    let mut r = launch(&mut w, &runs_dir, &l, &outer, &BTreeMap::new()).unwrap();
    // The inner workflow was checked and snapshotted with the outer one.
    assert_eq!(
        super::store::snapshot_of(&engine::dir_of(&runs_dir, &r.id).join("sub/inner")).unwrap(),
        linear
    );
    tick(&mut w, &mut r);
    // A run of its own, one level deeper, in a pane of its own.
    assert_eq!(w.opened, ["outer-1.inner"]);
    let mut sub = engine::load(&runs_dir, "outer-1.inner").unwrap();
    assert_eq!(
        (sub.depth, sub.parent.as_deref(), sub.workflow.name.as_str()),
        (1, Some("outer-1"), "inner")
    );
    assert_eq!(r.nodes["inner"].state, NodeState::Working);
    // The outer waits while the inner works.
    tick(&mut w, &mut sub);
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.active, ["inner"]);
    // What the inner's agents spend shows in the outer's total, live and closed.
    assert_eq!(sub.spend(&w.seen).0, 1.0);
    assert_eq!(r.spend(&w.seen).0, 1.0, "its live agent's spend rolls up");
    edit(&mut w, &sub, "writer", "src/file", "x\n");
    submit(&mut w, &sub, "writer", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut sub, 2);
    assert_eq!(sub.state, RunState::Succeeded, "{:?}", sub.log);
    // The inner's outcome is the node's verdict, and the outer goes on.
    ticks(&mut w, &mut r, 2);
    assert_eq!(
        (r.state, r.nodes["inner"].verdict.as_deref()),
        (RunState::Succeeded, Some("succeeded")),
        "{:?}",
        r.log
    );
    assert_eq!(
        r.spend(&w.seen).0,
        1.0,
        "and what it spent on panes it closed"
    );
    // The depth limit: max_depth 2 lets a run start a run that starts a
    // run, and no deeper.
    let mut d0 = launch(&mut w, &runs_dir, &l, &deep, &BTreeMap::new()).unwrap();
    assert_eq!(d0.opts.max_depth, 2);
    tick(&mut w, &mut d0);
    let mut d1 = engine::load(&runs_dir, "deep-1.inner").unwrap();
    tick(&mut w, &mut d1);
    let mut d2 = engine::load(&runs_dir, "deep-1.inner.inner").unwrap();
    assert_eq!((d1.depth, d2.depth), (1, 2));
    tick(&mut w, &mut d2);
    assert!(
        engine::load(&runs_dir, "deep-1.inner.inner.inner").is_err(),
        "a third level isn't started"
    );
    assert_eq!(d2.state, RunState::Failed);
    assert!(
        d2.log
            .iter()
            .any(|(_, l)| l.contains("3 levels deep, and max_depth is 2: not started")),
        "{:?}",
        d2.log
    );
    // A failed inner run fails the outer's node, and so on up.
    ticks(&mut w, &mut d1, 2);
    ticks(&mut w, &mut d0, 2);
    assert_eq!((d1.state, d0.state), (RunState::Failed, RunState::Failed));
    // A workflow node whose workflow doesn't check keeps the outer from starting.
    let mut bad = linear.clone();
    bad.limits.wall_clock = None;
    stores.save(&bad, super::store::Origin::Personal).unwrap();
    let e = launch(&mut w, &runs_dir, &l, &outer, &BTreeMap::new()).unwrap_err();
    assert!(
        e.starts_with("the workflow node's workflow doesn't check:\ninner: 1 error"),
        "{e}"
    );
    done(w);
}

#[test]
fn what_a_node_writes_on_the_blackboard_every_later_node_can_read() {
    let mut wf = issue_to_pr();
    wf.nodes.get_mut("judge").unwrap().prompt =
        Some("The plan was: {{board.plan}}. Does {{tests.commit}} meet it?".into());
    let m = super::caps::Matrix::shipped();
    assert_eq!(
        super::check::check(&wf, &m, &super::check::Ctx::ideal(&m)).errors(),
        0
    );
    let mut w = World::new("wf-board");
    let mut r = start(&mut w, wf, opts());
    tick(&mut w, &mut r);
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(
        &mut w,
        &r,
        "implementer",
        r#"{"verdict": "ready", "board": {"plan": "reset the window at 60s"}}"#,
    )
    .unwrap();
    ticks(&mut w, &mut r, 3);
    assert_eq!(r.board["plan"], "reset the window at 60s");
    submit(
        &mut w,
        &r,
        "reviewer",
        r#"{"verdict": "pass", "findings": []}"#,
    )
    .unwrap();
    ticks(&mut w, &mut r, 2);
    assert!(
        w.spawned
            .last()
            .unwrap()
            .prompt
            .starts_with("The plan was: reset the window at 60s. Does ")
    );
    done(w);
}
