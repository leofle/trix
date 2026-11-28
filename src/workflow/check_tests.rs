//! One test per row of the predictions table and the graph-check table.

use super::caps::{Installed, Level, Local, Matrix, Protection};
use super::check::*;
use super::model::*;
use super::store::{builtin, builtin_names};
use std::collections::BTreeMap;

fn wf() -> Workflow {
    builtin("issue-to-pr").unwrap()
}

fn ideal() -> (Matrix, Ctx) {
    let m = Matrix::shipped();
    let ctx = Ctx::ideal(&m);
    (m, ctx)
}

fn run(w: &Workflow) -> Report {
    let (m, ctx) = ideal();
    check(w, &m, &ctx)
}

fn codes(r: &Report, level: Severity) -> Vec<&'static str> {
    r.problems
        .iter()
        .filter(|p| p.level == level)
        .map(|p| p.code)
        .collect()
}

/// Claude's hook decision probed and passed on the installed version.
fn verified() -> (Matrix, Ctx) {
    let local = BTreeMap::from([(
        "claude".to_string(),
        Local {
            version: "2.3.1".into(),
            trix: "1.8.0".into(),
            verified: true,
            permission_decision: Some("verified".into()),
            results: BTreeMap::new(),
        },
    )]);
    let m = Matrix::load(&toml::Table::new(), &local).unwrap();
    let mut ctx = Ctx::ideal(&m);
    ctx.installed.0.insert("claude".into(), "2.3.1".into());
    (m, ctx)
}

fn agent(kind: &str, profile: &str, owns: &[&str]) -> Node {
    Node {
        kind: "agent".into(),
        agent: Some(kind.into()),
        profile: Some(profile.into()),
        verdicts: vec!["ready".into()],
        brief: Some(Brief {
            owns: owns.iter().map(|s| s.to_string()).collect(),
            must_not_touch: vec![],
            done: "done".into(),
        }),
        ..Node::default()
    }
}

fn edge(from: &str, to: &str, on: &str) -> Edge {
    Edge {
        from: from.into(),
        to: to.into(),
        on: Triggers::One(on.into()),
        guard: None,
        pass: BTreeMap::new(),
        ask: None,
    }
}

fn rule(node: &str, tools: &[&str]) -> PermRule {
    PermRule {
        node: node.into(),
        mode: Allow::Allow,
        approver: Some("rule".into()),
        tools: tools.iter().map(|s| s.to_string()).collect(),
        paths: vec![],
    }
}

/// A claude profile in a prompting mode with these tools.
fn asking(w: &mut Workflow, name: &str, tools: &str) {
    let args = ["--permission-mode", "default", "--allowedTools", tools]
        .iter()
        .map(|s| s.to_string())
        .collect();
    w.profiles
        .insert(name.into(), BTreeMap::from([("claude".to_string(), args)]));
}

#[test]
fn issue_to_pr_checks_to_exactly_0_errors_and_2_warnings() {
    let r = run(&wf());
    assert_eq!((r.errors(), r.warnings()), (0, 2), "{}", r.render());
    // 1: one workflow-level warning listing every guarded node, agents and checks.
    let p = r.find("push-guarded").unwrap();
    assert_eq!(
        (p.at.clone(), p.text.as_str()),
        (
            At::Workflow,
            "push guarded, not enforced: implementer, judge, tests"
        )
    );
    // 2: the judge is read-only by instruction only.
    let p = r.find("read-only-by-instruction").unwrap();
    assert_eq!(p.at, At::Node("judge".into()));
    // No quota warning: all three kinds have a signal. The reviewer is enforced.
    assert!(!r.has("no-quota-signal") && !r.has("dead-rule") && !r.has("merge-approver"));
    assert_eq!(r.labels["reviewer"].protection, Protection::Enforced);
    assert_eq!(r.labels["reviewer"].read_only, Some(true));
    assert_eq!(
        (
            r.labels["implementer"].protection,
            r.labels["judge"].protection
        ),
        (Protection::Guarded, Protection::Guarded)
    );
    // Three vendors pass check.
    let kinds: Vec<&str> = r.labels.values().map(|l| l.kind.as_str()).collect();
    assert_eq!(kinds, ["codex", "gemini", "claude"]);
    // The check carries an info badge, not a warning.
    assert_eq!(
        r.at(&At::Node("tests".into()))
            .map(|p| (p.level, p.code))
            .collect::<Vec<_>>(),
        [(Severity::Info, "check-guarded")]
    );
    assert!(
        r.render()
            .starts_with("issue-to-pr: 0 errors, 2 warnings\n"),
        "{}",
        r.render()
    );
    assert!(r.runnable());
}

#[test]
fn codex_on_read_only_can_never_hand_in() {
    let mut w = wf();
    w.nodes.get_mut("reviewer").unwrap().agent = Some("codex".into());
    let r = run(&w);
    let p = r.find("codex-read-only").unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Error, At::Node("reviewer".into()))
    );
    assert_eq!(
        p.text,
        "Codex in read-only can't reach Trix, so it can never hand in"
    );
    assert_eq!(
        p.fixes.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(),
        [
            "switch to claude",
            "use instruct-read-only (⚠ not enforced)"
        ]
    );
    assert!(!r.runnable());
    // The same for a judge and for a codex profile that says -s read-only itself.
    let mut j = wf();
    j.nodes.get_mut("judge").unwrap().agent = Some("codex".into());
    j.profiles.insert(
        "instruct-read-only".into(),
        BTreeMap::from([(
            "codex".to_string(),
            vec!["-s".to_string(), "read-only".to_string()],
        )]),
    );
    assert_eq!(
        run(&j).find("codex-read-only").unwrap().at,
        At::Node("judge".into())
    );
    // Either fix clears it; the second leaves an honest warning.
    let mut a = w.clone();
    assert!(apply(&mut a, &p.fixes[0].op));
    assert!(!run(&a).has("codex-read-only"));
    let mut b = w.clone();
    assert!(apply(&mut b, &p.fixes[1].op));
    let r = run(&b);
    assert!(
        !r.has("codex-read-only")
            && r.problems.iter().any(
                |p| p.code == "read-only-by-instruction" && p.at == At::Node("reviewer".into())
            )
    );
}

#[test]
fn gate_only_needs_every_agent_enforced() {
    let enforced = |w: &mut Workflow| {
        w.policy.push = Push::GateOnly;
        for id in ["reviewer", "judge"] {
            let n = w.nodes.get_mut(id).unwrap();
            n.agent = Some("claude".into());
            n.profile = Some("read-only".into());
        }
        let n = w.nodes.get_mut("implementer").unwrap();
        n.agent = Some("claude".into());
        n.profile = Some("edit-only".into());
    };
    // Every agent on a safe-list profile: no error, and no guarded warning.
    let mut ok = wf();
    enforced(&mut ok);
    let r = run(&ok);
    assert_eq!(r.errors(), 0, "{}", r.render());
    assert!(
        !r.has("push-guarded")
            && r.labels
                .values()
                .all(|l| l.protection == Protection::Enforced)
    );
    // Codex workspace-write: it needs network for trix, so it can't block a push.
    let mut w = ok.clone();
    w.nodes.get_mut("implementer").unwrap().agent = Some("codex".into());
    w.nodes.get_mut("implementer").unwrap().profile = Some("edit-and-test".into());
    let p = run(&w).find("gate-only-not-enforced").cloned().unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Error, At::Node("implementer".into()))
    );
    assert!(
        p.text.contains("codex can't be kept from pushing"),
        "{}",
        p.text
    );
    assert!(
        p.fixes
            .iter()
            .any(|f| f.op == FixOp::SetPush(Push::Guarded))
    );
    // Claude with a test runner, with git:*, with bare Bash, with a shell.
    for (tools, off) in [
        ("Read,Edit,Bash(npm test:*)", "Bash(npm test:*)"),
        ("Read,Edit,Bash(git:*)", "Bash(git:*)"),
        ("Read,Edit,Bash", "Bash"),
        ("Read,Edit,Bash(git push:*)", "Bash(git push:*)"),
    ] {
        let mut w = ok.clone();
        let args = ["--permission-mode", "dontAsk", "--allowedTools", tools]
            .iter()
            .map(|s| s.to_string())
            .collect();
        w.profiles.insert(
            "mine".into(),
            BTreeMap::from([("claude".to_string(), args)]),
        );
        w.nodes.get_mut("implementer").unwrap().profile = Some("mine".into());
        let r = run(&w);
        let p = r
            .find("gate-only-not-enforced")
            .unwrap_or_else(|| panic!("{tools}: {}", r.render()));
        assert!(
            p.text.contains(off) && p.text.contains("isn't on the safe list"),
            "{}",
            p.text
        );
        assert_eq!(r.labels["implementer"].protection, Protection::Guarded);
    }
    // The shipped edit-and-test is one of those: npm test runs code the agent can edit.
    let mut w = ok.clone();
    w.nodes.get_mut("implementer").unwrap().profile = Some("edit-and-test".into());
    assert!(run(&w).has("gate-only-not-enforced"));
    // A kind with no profile that's verified or documented.
    let mut w = ok.clone();
    w.nodes.get_mut("judge").unwrap().agent = Some("gemini".into());
    w.nodes.get_mut("judge").unwrap().profile = Some("instruct-read-only".into());
    assert_eq!(
        run(&w).find("gate-only-not-enforced").unwrap().at,
        At::Node("judge".into())
    );
    // The push fix settles it honestly: guarded, with its warning.
    assert!(apply(&mut w, &FixOp::SetPush(Push::Guarded)));
    let r = run(&w);
    assert!(!r.has("gate-only-not-enforced") && r.has("push-guarded"));
}

#[test]
fn push_guarded_is_one_warning_and_push_allow_guards_nothing() {
    let r = run(&wf());
    assert_eq!(
        r.problems
            .iter()
            .filter(|p| p.code == "push-guarded")
            .count(),
        1
    );
    // push: allow: agents push themselves, nothing is guarded, and it's said.
    let mut w = wf();
    w.policy.push = Push::Allow;
    let r = run(&w);
    assert!(!r.has("push-guarded"));
    assert!(r.labels.values().all(|l| l.protection == Protection::None));
    assert_eq!(Protection::None.label(), "none");
}

#[test]
fn a_writer_whose_scope_covers_ci_configuration_is_warned() {
    let mut w = wf();
    let b = w
        .nodes
        .get_mut("implementer")
        .unwrap()
        .brief
        .as_mut()
        .unwrap();
    b.must_not_touch.clear();
    b.owns.push(".github/workflows/ci.yml".into());
    let p = run(&w).find("ci-config-owned").cloned().unwrap();
    assert_eq!(
        (p.level, p.at),
        (Severity::Warning, At::Node("implementer".into()))
    );
    assert!(
        p.text
            .contains("CI-config floor will park the gate for you")
    );
    // .github/actions, and a path from [workflows] ci_paths.
    for (owns, extra) in [
        (".github/actions/**", vec![]),
        ("**", vec![]),
        ("ci/pipeline.yml", vec!["ci/**".to_string()]),
    ] {
        let mut w = wf();
        let b = w
            .nodes
            .get_mut("implementer")
            .unwrap()
            .brief
            .as_mut()
            .unwrap();
        b.must_not_touch.clear();
        b.owns = vec![owns.into()];
        let (m, mut ctx) = ideal();
        ctx.ci_paths = extra;
        assert!(check(&w, &m, &ctx).has("ci-config-owned"), "{owns}");
    }
    assert!(
        !run(&wf()).has("ci-config-owned"),
        "src/** and test/** aren't CI config"
    );
    // Under push: allow the floor can't apply, and nothing is promised.
    w.policy.push = Push::Allow;
    assert!(!run(&w).has("ci-config-owned"));
}

#[test]
fn a_permission_rule_needs_a_verified_decision_hook() {
    // On a kind with no decision API, or an unverified one: an error.
    for (node, kind) in [("judge", "gemini"), ("implementer", "codex")] {
        let mut w = wf();
        w.approvals
            .permissions
            .push(rule(node, &["Bash(npm install:*)"]));
        let r = run(&w);
        let p = r
            .find("rule-no-decision")
            .unwrap_or_else(|| panic!("{}", r.render()));
        assert_eq!((p.level, p.at.clone()), (Severity::Error, At::Rule(0)));
        assert!(p.text.contains(kind), "{}", p.text);
        assert_eq!(p.fixes[0].op, FixOp::RemoveRule(0));
    }
    for kind in [
        "opencode",
        "copilot",
        "muse",
        "grok",
        "cursor",
        "antigravity",
        "amp",
        "custom",
    ] {
        let mut w = wf();
        w.nodes.get_mut("judge").unwrap().agent = Some(kind.into());
        w.approvals.permissions.push(rule("judge", &["Bash(ls:*)"]));
        assert!(run(&w).has("rule-no-decision"), "{kind}");
    }
    // On Claude as shipped (documented, not probed here): a warning, the
    // rule is suspended, and the fix is to run the probe.
    let mut w = wf();
    w.nodes.get_mut("implementer").unwrap().agent = Some("claude".into());
    asking(&mut w, "edit-and-test", "Read,Edit,Write");
    w.approvals
        .permissions
        .push(rule("implementer", &["Bash(npm install:*)"]));
    let r = run(&w);
    let p = r
        .find("rule-documented")
        .unwrap_or_else(|| panic!("{}", r.render()));
    assert_eq!(
        (p.level, p.at.clone(), p.fixes[0].op.clone()),
        (
            Severity::Warning,
            At::Rule(0),
            FixOp::Probe("claude".into())
        )
    );
    assert!(p.text.contains("suspended") && p.text.contains("park the run"));
    assert!(!r.has("rule-no-decision") && r.errors() == 0);
    // Verified for the installed version: nothing to say about it.
    let (m, ctx) = verified();
    let r = check(&w, &m, &ctx);
    assert!(
        !r.has("rule-documented") && !r.has("rule-no-decision") && !r.has("dead-rule"),
        "{}",
        r.render()
    );
    assert_eq!(r.labels["implementer"].decision, Level::Verified);
    // npm install runs package scripts: the node is guarded for it.
    assert_eq!(r.labels["implementer"].protection, Protection::Guarded);
    // A rule with mode: ask (the catch-all) is never a problem.
    let mut w = wf();
    w.approvals.permissions.push(PermRule {
        node: "*".into(),
        mode: Allow::Ask,
        ..PermRule::default()
    });
    assert_eq!(run(&w).errors(), 0);
}

#[test]
fn an_agent_cant_approve_a_permission_prompt() {
    let mut w = wf();
    w.approvals.permissions.push(PermRule {
        approver: Some("judge".into()),
        ..rule("implementer", &["Bash(ls:*)"])
    });
    let p = run(&w).find("rule-agent-approver").cloned().unwrap();
    assert_eq!((p.level, p.at.clone()), (Severity::Error, At::Rule(0)));
    assert!(p.text.contains("agent approvers approve merges only"));
    assert!(apply(&mut w, &p.fixes[0].op));
    assert_eq!(w.approvals.permissions[0].approver.as_deref(), Some("rule"));
    assert!(!run(&w).has("rule-agent-approver"));
}

#[test]
fn the_merge_approver_is_a_node_that_didnt_write_the_change_and_can_hand_in() {
    let with = |a: Option<&str>| {
        let mut w = wf();
        w.approvals.merge.as_mut().unwrap().approver = a.map(String::from);
        w
    };
    assert!(!run(&with(Some("judge"))).has("merge-approver"));
    for (a, why) in [
        (None, "no merge approver is named"),
        (Some("nobody"), "isn't a node of this workflow"),
        (Some("tests"), "isn't an agent node"),
        (Some("implementer"), "wrote the change"),
    ] {
        let p = run(&with(a)).find("merge-approver").cloned().unwrap();
        assert_eq!(
            (p.level, p.at.clone()),
            (Severity::Error, At::Approvals),
            "{a:?}"
        );
        assert!(p.text.contains(why), "{}", p.text);
        // The fix is the suggested node: the judge comes first.
        assert_eq!(p.fixes[0].op, FixOp::SetApprover(Some("judge".into())));
    }
    // One that can't hand in.
    let mut w = with(Some("judge"));
    w.nodes.get_mut("judge").unwrap().agent = Some("codex".into());
    w.profiles.insert(
        "instruct-read-only".into(),
        BTreeMap::from([(
            "codex".to_string(),
            vec!["-s".to_string(), "read-only".to_string()],
        )]),
    );
    let p = run(&w).find("merge-approver").cloned().unwrap();
    assert!(p.text.contains("can't hand in"), "{}", p.text);
    // The order: a judge, else a verifier, else a gatekeeper, else you.
    assert_eq!(
        p.fixes[0].op,
        FixOp::SetApprover(Some("reviewer".into())),
        "the judge can't, so the verifier"
    );
    let mut w = with(None);
    w.nodes.remove("judge");
    w.edges.retain(|e| e.from != "judge" && e.to != "judge");
    w.goal.done_when.attested.clear();
    w.edges.push(edge("reviewer", "gate", "pass"));
    w.edges
        .retain(|e| !(e.from == "reviewer" && e.to == "judge"));
    assert_eq!(
        run(&w).find("merge-approver").unwrap().fixes[0].op,
        FixOp::SetApprover(Some("reviewer".into()))
    );
    w.nodes.get_mut("reviewer").unwrap().preset = None;
    w.nodes.get_mut("reviewer").unwrap().role = Some(Role::Gatekeeper);
    assert_eq!(
        run(&w).find("merge-approver").unwrap().fixes[0].op,
        FixOp::SetApprover(Some("reviewer".into())),
        "a gatekeeper"
    );
    w.nodes.get_mut("reviewer").unwrap().role = Some(Role::Reviewer);
    let p = run(&w).find("merge-approver").cloned().unwrap();
    assert_eq!(
        p.fixes[0].op,
        FixOp::SetApprover(None),
        "none of the three: you approve"
    );
    assert!(apply(&mut w, &p.fixes[0].op));
    assert_eq!(
        (w.policy.merge, w.approvals.merge.clone()),
        (Merge::You, None)
    );
    assert!(!run(&w).has("merge-approver"));
    // policy.merge: approver with no approvals block at all.
    let mut w = wf();
    w.approvals.merge = None;
    assert!(run(&w).has("merge-approver"));
}

#[test]
fn a_rule_that_can_never_fire_is_dead() {
    let (m, ctx) = verified();
    let mut w = wf();
    w.nodes.get_mut("implementer").unwrap().agent = Some("claude".into());
    // dontAsk: no prompt ever comes.
    w.approvals
        .permissions
        .push(rule("implementer", &["Bash(npm install:*)"]));
    let p = check(&w, &m, &ctx).find("dead-rule").cloned().unwrap();
    assert_eq!((p.level, p.at.clone()), (Severity::Warning, At::Rule(0)));
    assert!(p.text.contains("never shows a prompt"), "{}", p.text);
    // A prompting mode, and the profile already allows the tool.
    asking(&mut w, "edit-and-test", "Read,Edit,Bash(npm install:*)");
    let p = check(&w, &m, &ctx).find("dead-rule").cloned().unwrap();
    assert!(
        p.text.contains("already allows Bash(npm install:*)"),
        "{}",
        p.text
    );
    // A tool the profile doesn't allow, in a prompting mode: it can fire.
    asking(&mut w, "edit-and-test", "Read,Edit");
    assert!(!check(&w, &m, &ctx).has("dead-rule"));
    // Codex with -a never is dead too (and unverified, which is the error).
    let mut c = wf();
    c.approvals
        .permissions
        .push(rule("implementer", &["shell"]));
    let r = run(&c);
    assert!(r.has("dead-rule") && r.has("rule-no-decision"));
}

#[test]
fn writers_that_can_run_at_the_same_time_with_overlapping_scopes_are_flagged() {
    let mut w = wf();
    w.nodes.insert(
        "docs".into(),
        agent("claude", "edit-only", &["src/**/*.md", "README.md"]),
    );
    w.edges.push(edge("docs", "tests", "ready"));
    w.entry = vec!["implementer".into(), "docs".into()];
    let r = run(&w);
    let p = r
        .find("scope-overlap")
        .unwrap_or_else(|| panic!("{}", r.render()));
    assert_eq!(p.level, Severity::Warning);
    assert!(
        p.text
            .contains("docs and implementer can run at the same time")
            && p.text.contains("likely land conflict"),
        "{}",
        p.text
    );
    // They get separate worktrees either way, so a land step is needed.
    assert!(isolated(&w, "docs") && isolated(&w, "implementer") && r.has("land-missing"));
    // Scopes that don't overlap: no overlap warning, still isolated.
    w.nodes
        .get_mut("docs")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .owns = vec!["docs/**".into()];
    let r = run(&w);
    assert!(!r.has("scope-overlap") && r.has("land-missing"));
    // An edge that orders them: one after another in the run's worktree.
    let mut o = wf();
    o.nodes
        .insert("docs".into(), agent("claude", "edit-only", &["src/**"]));
    o.edges
        .retain(|e| !(e.from == "implementer" && e.to == "tests"));
    o.edges.push(edge("implementer", "docs", "ready"));
    o.edges.push(edge("docs", "tests", "ready"));
    let r = run(&o);
    assert!(
        !r.has("scope-overlap") && !r.has("land-missing"),
        "{}",
        r.render()
    );
    assert!(!isolated(&o, "docs") && !concurrent(&o, "docs", "implementer"));
    // What overlaps.
    for (a, b, yes) in [
        ("src/**", "src/a.rs", true),
        ("src/**", "test/**", false),
        ("**", "anything/at/all", true),
        ("package.json", "package-lock.json", false),
        ("*.md", "src/**", false),
        ("src/*.rs", "src/a.rs", true),
        ("src/*.rs", "src/sub/a.rs", false),
        ("src/**/*.md", "src/a/b/c.md", true),
        (".github/**", ".github/workflows/ci.yml", true),
        ("./src/a.rs", "src/a.rs", true),
    ] {
        assert_eq!(overlap(a, b), yes, "{a} vs {b}");
        assert_eq!(overlap(b, a), yes, "{b} vs {a}");
    }
    assert!(
        in_scope(&["src/**".into(), "test/**".into()], "test/x/y.js")
            && !in_scope(&["src/**".into()], "README.md")
    );
}

#[test]
fn every_agent_node_needs_a_brief_and_a_check_needs_none() {
    let mut w = wf();
    w.nodes.get_mut("judge").unwrap().brief = None;
    let p = run(&w).find("brief-missing").cloned().unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Error, At::Node("judge".into()))
    );
    // The fix suggests one from the role.
    assert!(apply(&mut w, &p.fixes[0].op));
    assert!(
        w.nodes["judge"]
            .brief
            .as_ref()
            .unwrap()
            .done
            .contains("pass or fail vote")
    );
    assert_eq!(run(&w).errors(), 0);
    // A brief with no done, and a writer with no write scope.
    let mut w = wf();
    w.nodes
        .get_mut("reviewer")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .done = " ".into();
    w.nodes
        .get_mut("implementer")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .owns
        .clear();
    let r = run(&w);
    assert_eq!(
        r.find("brief-no-done").unwrap().at,
        At::Node("reviewer".into())
    );
    assert_eq!(
        r.find("writer-no-scope").unwrap().at,
        At::Node("implementer".into())
    );
    // Neither the check nor the gate has one, and neither is asked for one.
    let ok = wf();
    assert!(ok.nodes["tests"].brief.is_none() && ok.nodes["gate"].brief.is_none());
    assert!(!run(&ok).has("brief-missing"));
}

#[test]
fn a_writer_cant_own_what_a_brief_says_not_to_touch() {
    // Its own.
    let mut w = wf();
    w.nodes
        .get_mut("implementer")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .must_not_touch
        .push("src/vendor/**".into());
    let p = run(&w).find("must-not-touch").cloned().unwrap();
    assert_eq!(
        (p.level, p.at),
        (Severity::Error, At::Node("implementer".into()))
    );
    assert!(p.text.contains("its own brief"));
    // Another node's.
    let mut w = wf();
    w.nodes
        .get_mut("reviewer")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .must_not_touch
        .push("test/fixtures/**".into());
    let p = run(&w).find("must-not-touch").cloned().unwrap();
    assert!(
        p.text.contains("reviewer's brief says must not be touched"),
        "{}",
        p.text
    );
}

#[test]
fn a_verifier_is_never_the_author_and_never_shares_its_context() {
    let mut w = wf();
    w.nodes
        .get_mut("reviewer")
        .unwrap()
        .brief
        .as_mut()
        .unwrap()
        .owns = vec!["src/**".into()];
    let p = run(&w).find("verifier-is-author").cloned().unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Error, At::Node("reviewer".into()))
    );
    assert!(p.text.contains("write scope"));
    let mut w = wf();
    w.nodes.get_mut("reviewer").unwrap().revisit = Some(Revisit::Reuse);
    let p = run(&w).find("verifier-is-author").cloned().unwrap();
    assert!(p.text.contains("keeps its pane"));
    assert!(apply(&mut w, &p.fixes[0].op));
    assert!(!run(&w).has("verifier-is-author"));
    // A verifier starts fresh unless it says otherwise.
    assert_eq!(wf().nodes["reviewer"].revisit(), Revisit::Fresh);
}

#[test]
fn too_many_writers_per_verifier_or_a_verifier_on_the_writers_backend_is_warned() {
    // Five writers in order, one verifier.
    let mut w = wf();
    let mut last = "implementer".to_string();
    w.edges
        .retain(|e| !(e.from == "implementer" && e.to == "tests"));
    for i in 1..=4 {
        let id = format!("w{i}");
        w.nodes.insert(
            id.clone(),
            agent("codex", "edit-and-test", &[&format!("part{i}/**")]),
        );
        w.edges.push(edge(&last, &id, "ready"));
        last = id;
    }
    w.edges.push(edge(&last, "tests", "ready"));
    let r = run(&w);
    let p = r
        .find("verifier-ratio")
        .unwrap_or_else(|| panic!("{}", r.render()));
    assert!(
        p.level == Severity::Warning && p.text.starts_with("5 writers for 1 verifier"),
        "{}",
        p.text
    );
    assert!(!run(&wf()).has("verifier-ratio"));
    // A fan-out counts for as many as run at once: three is fine.
    assert!(!run(&builtin("nightly-deps").unwrap()).has("verifier-ratio"));
    // The verifier on the same backend as every writer.
    let mut w = wf();
    w.nodes.get_mut("implementer").unwrap().agent = Some("claude".into());
    let r = run(&w);
    let p = r.find("verifier-same-backend").unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Warning, At::Node("reviewer".into()))
    );
    assert!(
        matches!(&p.fixes[0].op, FixOp::SetAgent { node, kind } if node == "reviewer" && kind != "claude")
    );
    assert!(
        !run(&wf()).has("verifier-same-backend"),
        "codex writes, claude verifies"
    );
}

#[test]
fn writers_in_their_own_worktrees_need_a_land_step_they_can_reach() {
    let mut w = builtin("nightly-deps").unwrap();
    assert!(isolated(&w, "upgrade") && !run(&w).has("land-missing"));
    // No land step at all.
    w.nodes.remove("land");
    w.edges.retain(|e| e.from != "land" && e.to != "land");
    w.edges.push(edge("collect", "tests", "some"));
    w.nodes.get_mut("tests").unwrap().commit = Some("upgrade.head".into());
    let r = run(&w);
    let p = r
        .find("land-missing")
        .unwrap_or_else(|| panic!("{}", r.render()));
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Error, At::Node("upgrade".into()))
    );
    assert!(p.text.contains("there's no land step"));
    // The proposed fix adds one, wired in after the writers.
    assert!(apply(&mut w, &p.fixes[0].op));
    assert!(
        w.nodes.contains_key("land") && !run(&w).has("land-missing"),
        "{}",
        run(&w).render()
    );
    // A land step that's there but can't be reached from the writer.
    let mut w = builtin("nightly-deps").unwrap();
    w.edges.retain(|e| !(e.from == "collect" && e.to == "land"));
    w.edges.push(edge("collect", "@fail", "some"));
    let r = run(&w);
    assert!(
        r.find("land-missing")
            .unwrap()
            .text
            .contains("no land step can be reached"),
        "{}",
        r.render()
    );
    // workspace: isolated on a lone writer asks for one too.
    let mut w = wf();
    w.nodes.get_mut("implementer").unwrap().workspace = Some(Workspace::Isolated);
    assert!(run(&w).has("land-missing"));
    assert!(
        !run(&wf()).has("land-missing"),
        "the only writer works in the run's worktree"
    );
}

#[test]
fn an_expensive_topology_is_flagged_before_launch() {
    let cheap = run(&wf());
    assert!(!cheap.has("expensive-topology"));
    assert_eq!(
        (
            cheap.estimate.peak_parallel,
            cheap.estimate.min_cycles,
            cheap.estimate.ceiling
        ),
        (1, 1, 6)
    );
    // Unpriced kinds are unknown, never $0.
    assert!(
        cheap.estimate.unknown && cheap.estimate.cost().ends_with("+ ?"),
        "{}",
        cheap.estimate.cost()
    );
    // A wide fan-out.
    let mut w = builtin("nightly-deps").unwrap();
    w.nodes.get_mut("upgrade").unwrap().parallel = Some(8);
    let r = run(&w);
    let p = r
        .find("expensive-topology")
        .unwrap_or_else(|| panic!("{}", r.render()));
    assert!(
        p.level == Severity::Warning && p.text.contains("8 agents at once"),
        "{}",
        p.text
    );
    // A high loop ceiling times parallel branches.
    let mut w = builtin("nightly-deps").unwrap();
    w.limits.max_iterations = Some(Count::N(4));
    w.edges.push(edge("verifier", "upgrade", "changes"));
    w.edges
        .retain(|e| !(e.from == "verifier" && e.to == "@fail"));
    assert!(
        run(&w)
            .find("expensive-topology")
            .unwrap()
            .text
            .contains("a ceiling of 4 cycles times 3 parallel agents")
    );
    // An estimate over your confirm_over.
    let (m, mut ctx) = ideal();
    ctx.confirm_over = Some(0.25);
    let r = check(&wf(), &m, &ctx);
    assert!(
        r.find("expensive-topology")
            .unwrap()
            .text
            .contains("against $0.25"),
        "{}",
        r.render()
    );
    // A linear flow is one cycle.
    let mut l = builtin("bug-repro").unwrap();
    l.edges.retain(|e| e.to != "reproducer");
    l.edges.push(edge("shows", "@fail", "fail"));
    l.edges.push(edge("verifier", "@fail", "changes"));
    let r = run(&l);
    assert_eq!(
        (r.estimate.min_cycles, r.estimate.ceiling, r.errors()),
        (1, 1, 0),
        "{}",
        r.render()
    );
}

#[test]
fn a_kind_with_no_quota_signal_is_warned() {
    let mut w = wf();
    w.nodes.get_mut("judge").unwrap().agent = Some("custom".into());
    let p = run(&w).find("no-quota-signal").cloned().unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Warning, At::Node("judge".into()))
    );
    assert!(p.text.contains("looks like an idle agent"));
    // custom can't resume either, which matters for a reused pane only.
    assert!(!run(&w).has("reuse-cant-resume"));
    w.nodes.get_mut("judge").unwrap().revisit = Some(Revisit::Reuse);
    assert!(run(&w).has("reuse-cant-resume"));
    // A fallback backend answers the quota warning.
    w.nodes.get_mut("judge").unwrap().on_quota = Some(OnQuota {
        fallback: Some("gemini".into()),
        ..OnQuota::default()
    });
    assert!(!run(&w).has("no-quota-signal"));
}

#[test]
fn an_upgraded_backend_drops_to_documented_and_never_stops_a_run() {
    let (m, mut ctx) = verified();
    let mut w = wf();
    w.nodes.get_mut("implementer").unwrap().agent = Some("claude".into());
    asking(&mut w, "edit-and-test", "Read,Edit");
    w.approvals
        .permissions
        .push(rule("implementer", &["Bash(npm install:*)"]));
    assert!(!check(&w, &m, &ctx).has("backend-version"));
    // Claude upgrades past the matrix's range.
    ctx.installed.0.insert("claude".into(), "3.1.0".into());
    let r = check(&w, &m, &ctx);
    let p = r.find("backend-version").unwrap();
    assert_eq!((p.level, p.at.clone()), (Severity::Warning, At::Workflow));
    assert!(
        p.text
            .contains("claude 3.1.0 is outside the matrix's range (2.0.0 to 2.99.99)")
            && p.text.contains("suspended"),
        "{}",
        p.text
    );
    assert_eq!(p.fixes[0].op, FixOp::Probe("claude".into()));
    // Its rules are suspended (a warning), not an error: a scheduled run still starts.
    assert!(
        r.has("rule-documented") && r.errors() == 0 && r.runnable(),
        "{}",
        r.render()
    );
    assert_eq!(r.labels["implementer"].level, Level::Documented);
    // Said once, however many nodes run on it.
    assert_eq!(
        r.problems
            .iter()
            .filter(|p| p.code == "backend-version")
            .count(),
        1
    );
}

#[test]
fn a_missing_backend_and_untrusted_codex_hooks_are_said() {
    let (m, mut ctx) = ideal();
    ctx.installed.0.remove("gemini");
    let r = check(&wf(), &m, &ctx);
    let p = r.find("backend-missing").unwrap();
    assert_eq!(
        (p.level, p.at.clone(), p.text.as_str()),
        (
            Severity::Error,
            At::Node("judge".into()),
            "gemini isn't installed"
        )
    );
    assert!(matches!(&p.fixes[1].op, FixOp::SetAgent { node, .. } if node == "judge"));
    assert!(!r.runnable());
    let mut w = wf();
    w.nodes.get_mut("judge").unwrap().agent = Some("hal9000".into());
    assert!(run(&w).has("backend-unknown"));
    // Codex's hooks not trusted yet: state comes from notify only.
    let (m, mut ctx) = ideal();
    ctx.codex_hooks_trusted = false;
    let r = check(&wf(), &m, &ctx);
    let p = r.find("codex-hooks").unwrap();
    assert!(
        p.level == Severity::Warning
            && p.text.ends_with(": implementer")
            && p.fixes[0].label.contains("/hooks")
    );
    assert_eq!(r.warnings(), 3);
    // Only what's installed is ever suggested.
    assert_eq!(
        Installed::detect(&Matrix(BTreeMap::new())),
        Installed::default()
    );
}

#[test]
fn a_budget_cant_be_the_only_limit_on_an_unpriced_kind() {
    let mut w = builtin("nightly-deps").unwrap();
    let n = w.nodes.get_mut("upgrade").unwrap();
    n.agent = Some("gemini".into());
    n.profile = Some("instruct-read-only".into());
    n.max_visits = None;
    let p = run(&w).find("budget-only-unpriced").cloned().unwrap();
    assert_eq!(
        (p.level, p.at.clone()),
        (Severity::Warning, At::Node("upgrade".into()))
    );
    assert!(apply(&mut w, &p.fixes[0].op));
    assert!(!run(&w).has("budget-only-unpriced"));
    // In issue-to-pr the judge is unpriced, and the cycle ceiling limits it.
    assert!(!run(&wf()).has("budget-only-unpriced"));
}

#[test]
fn the_graph_rules_are_checked() {
    // An unreachable node.
    let mut w = wf();
    w.nodes.insert(
        "island".into(),
        Node {
            kind: "check".into(),
            run: vec!["true".into()],
            ..Node::default()
        },
    );
    w.edges.push(edge("island", "@done", "pass"));
    w.edges.push(edge("island", "@fail", "fail"));
    let r = run(&w);
    assert_eq!(
        r.find("unreachable").map(|p| (p.level, p.at.clone())),
        Some((Severity::Error, At::Node("island".into())))
    );
    // A declared verdict with no edge: a build-time error, never a runtime surprise.
    let mut w = wf();
    w.nodes
        .get_mut("reviewer")
        .unwrap()
        .verdicts
        .push("unsure".into());
    let p = run(&w).find("verdict-no-edge").cloned().unwrap();
    assert_eq!(p.at, At::Node("reviewer".into()));
    assert!(p.text.contains("'unsure' has no edge"));
    let mut w = wf();
    w.edges
        .retain(|e| !(e.from == "tests" && e.to == "implementer"));
    assert!(
        run(&w)
            .find("verdict-no-edge")
            .unwrap()
            .text
            .contains("'fail'"),
        "a check always gives pass and fail"
    );
    // An edge on something its source never says; a verdict named like an event.
    let mut w = wf();
    w.edges.push(edge("judge", "gate", "maybe"));
    assert!(
        run(&w)
            .find("edge-unknown")
            .unwrap()
            .text
            .contains("'maybe' isn't a verdict judge declares")
    );
    let mut w = wf();
    w.nodes
        .get_mut("judge")
        .unwrap()
        .verdicts
        .push("timeout".into());
    assert!(run(&w).has("verdict-is-event"));
    // wall_clock is mandatory.
    let mut w = wf();
    w.limits.wall_clock = None;
    let p = run(&w).find("no-wall-clock").cloned().unwrap();
    assert_eq!(p.level, Severity::Error);
    assert!(apply(&mut w, &p.fixes[0].op) && !run(&w).has("no-wall-clock"));
    w.limits.wall_clock = Some("soon".into());
    assert!(run(&w).has("no-wall-clock"));
    // A loop with no ceiling: no max_iterations, and a node on it without max_visits.
    let mut w = wf();
    w.limits.max_iterations = None;
    let r = run(&w);
    assert_eq!(
        r.find("loop-no-ceiling").map(|p| p.level),
        Some(Severity::Error),
        "{}",
        r.render()
    );
    for id in ["implementer", "tests", "reviewer", "judge"] {
        w.nodes.get_mut(id).unwrap().max_visits = Some(3);
    }
    assert!(
        !run(&w).has("loop-no-ceiling"),
        "max_visits on every node of the loop is a ceiling too"
    );
    // A land step that sends conflicts back to their author is a loop too.
    let mut n = builtin("nightly-deps").unwrap();
    n.limits.max_iterations = None;
    let r = run(&n);
    assert_eq!(
        r.find("loop-no-ceiling").map(|p| p.at.clone()),
        Some(At::Node("land".into())),
        "{}",
        r.render()
    );
    n.nodes.get_mut("land").unwrap().on_conflict = Some("fail".into());
    assert!(!run(&n).has("loop-no-ceiling"));
    // A linear flow needs neither.
    let mut l = wf();
    l.limits.max_iterations = None;
    l.edges.retain(|e| e.to != "implementer");
    for (from, on) in [
        ("tests", "fail"),
        ("reviewer", "changes"),
        ("judge", "fail"),
    ] {
        l.edges.push(edge(from, "@fail", on));
    }
    let r = run(&l);
    assert!(
        !r.has("loop-no-ceiling") && r.errors() == 0,
        "{}",
        r.render()
    );
    assert!(back_edges(&l).is_empty() && back_edges(&wf()).len() == 3);
    // No mechanical done_when condition.
    let mut w = wf();
    w.goal.done_when.mechanical.clear();
    assert_eq!(
        run(&w).find("no-mechanical").map(|p| p.level),
        Some(Severity::Error)
    );
    // An unknown template reference, in a prompt, an edge and a commit.
    let mut w = wf();
    w.nodes.get_mut("judge").unwrap().prompt =
        Some("Look at {{tests.stdout}} and {{inputs.pr.title}}".into());
    w.edges[1]
        .pass
        .insert("inbox".into(), "{{linter.report}}".into());
    w.nodes.get_mut("tests").unwrap().commit = Some("builder.head".into());
    let r = run(&w);
    let texts: Vec<&str> = r
        .problems
        .iter()
        .filter(|p| p.code == "unknown-ref")
        .map(|p| p.text.as_str())
        .collect();
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t.contains("tests has no output 'stdout'"))
            && texts.iter().any(|t| t.contains("no input 'pr'"))
    );
    // Outside text that isn't fenced.
    let mut w = wf();
    w.nodes.get_mut("judge").unwrap().prompt = Some("The issue says: {{inputs.issue.body}}".into());
    let p = run(&w).find("unfenced").cloned().unwrap();
    assert_eq!((p.level, p.at), (Severity::Error, At::Node("judge".into())));
    assert!(
        !run(&wf()).has("unfenced"),
        "the number is safe, and the body is fenced"
    );
    // A loop where progress can't be measured: no diff, no check.
    let mut w = wf();
    w.edges.push(edge("judge", "reviewer", "fail"));
    w.edges
        .retain(|e| !(e.from == "judge" && e.to == "implementer"));
    let r = run(&w);
    assert_eq!(
        r.find("loop-no-progress").map(|p| p.level),
        Some(Severity::Warning),
        "{}",
        r.render()
    );
    assert!(!run(&wf()).has("loop-no-progress"));
    // Untrusted, or changed since trust was given: blocks the run, not the edit.
    let (m, mut ctx) = ideal();
    ctx.trusted = Some(false);
    let r = check(&wf(), &m, &ctx);
    assert_eq!(
        (r.count(Severity::Blocks), r.errors(), r.runnable()),
        (1, 0, false)
    );
    assert!(
        r.find("untrusted")
            .unwrap()
            .text
            .contains("changed since trust was given")
    );
}

#[test]
fn every_template_passes_check() {
    let (m, ctx) = ideal();
    for name in builtin_names() {
        let w = builtin(name).unwrap();
        let r = check(&w, &m, &ctx);
        assert_eq!(r.errors(), 0, "{}", r.render());
    }
    // Triage as a case runs it, bound to [triage].
    let w = super::triage::bound(&crate::config::TriageOpts::default());
    let r = check(&w, &m, &ctx);
    assert_eq!(r.errors(), 0, "{}", r.render());
    // push: allow, so nothing is guarded and the CI floor can't apply.
    assert!(!r.has("push-guarded") && !r.has("ci-config-owned"));
}

#[test]
fn check_fix_applies_what_trix_can_fix() {
    let mut w = wf();
    w.limits.wall_clock = None;
    w.nodes.get_mut("judge").unwrap().brief = None;
    w.nodes.get_mut("reviewer").unwrap().agent = Some("codex".into());
    w.approvals.merge.as_mut().unwrap().approver = None;
    let (m, ctx) = ideal();
    assert_eq!(check(&w, &m, &ctx).errors(), 4);
    let did = fix_all(&mut w, &m, &ctx);
    assert_eq!(did.len(), 4, "{did:?}");
    assert_eq!(
        check(&w, &m, &ctx).errors(),
        0,
        "{}",
        check(&w, &m, &ctx).render()
    );
    assert_eq!(w.nodes["reviewer"].agent.as_deref(), Some("claude"));
    // What only you can decide is left, and said.
    let mut w = wf();
    w.goal.done_when.mechanical.clear();
    assert!(fix_all(&mut w, &m, &ctx).is_empty());
}

#[test]
fn the_check_and_capabilities_commands_read_their_flags() {
    use super::cli::{self, Cmd};
    let words = |l: &str| l.split_whitespace().map(String::from).collect::<Vec<_>>();
    assert_eq!(
        cli::parse(&words("check issue-to-pr")),
        Ok(Cmd::Check {
            name: "issue-to-pr".into(),
            fix: false
        })
    );
    assert_eq!(
        cli::parse(&words("check --fix issue-to-pr")),
        Ok(Cmd::Check {
            name: "issue-to-pr".into(),
            fix: true
        })
    );
    assert_eq!(
        cli::parse(&words("check issue-to-pr --fix")),
        Ok(Cmd::Check {
            name: "issue-to-pr".into(),
            fix: true
        })
    );
    assert_eq!(
        cli::parse(&words("capabilities")),
        Ok(Cmd::Capabilities {
            kind: None,
            verify: false
        })
    );
    assert_eq!(
        cli::parse(&words("capabilities --kind codex --verify")),
        Ok(Cmd::Capabilities {
            kind: Some("codex".into()),
            verify: true
        })
    );
    assert!(
        cli::parse(&words("capabilities --kind")).is_err()
            && cli::parse(&words("capabilities --all")).is_err()
    );
    // The table shows each entry at the level it counts at here, and says what it can't do.
    let (m, ctx) = ideal();
    let t = cli::capabilities_text(&m, &ctx.installed, None);
    let row = |k: &str| {
        t.lines()
            .find(|l| l.starts_with(k))
            .unwrap()
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(row("codex")[2..5], ["documented", "workspace", "by"], "{t}");
    assert_eq!(row("claude")[2], "documented");
    assert!(row("custom").last().unwrap() == "none" && t.contains("unverified (counts as no)"));
    assert_eq!(
        cli::capabilities_text(&m, &ctx.installed, Some("gemini"))
            .lines()
            .filter(|l| l.starts_with("gemini"))
            .count(),
        1
    );
    let none = Installed::default();
    assert!(
        cli::capabilities_text(&m, &none, Some("amp"))
            .lines()
            .nth(1)
            .unwrap()
            .contains(" - ")
    );
}

#[test]
fn every_way_in_gives_the_same_report() {
    // The builder's API, the command line, import, dry run and run start
    // all hand back what `check` says, as the same text. Fixtures: every
    // template, and one of each kind of trouble.
    let (m, ctx) = ideal();
    let mut fixtures: Vec<Workflow> = builtin_names()
        .into_iter()
        .map(|n| builtin(n).unwrap())
        .collect();
    let mut broken = wf();
    broken.nodes.get_mut("reviewer").unwrap().agent = Some("codex".into());
    broken.limits.wall_clock = None;
    fixtures.push(broken);
    let mut warned = wf();
    warned.nodes.get_mut("judge").unwrap().agent = Some("custom".into());
    fixtures.push(warned);
    for w in &fixtures {
        let api = check(w, &m, &ctx).render();
        for (way, text) in super::surfaces(w, &m, &ctx) {
            assert_eq!(text, api, "{} via {way}", w.name);
        }
    }
    assert_eq!(super::surfaces(&wf(), &m, &ctx).len(), super::SURFACES);
}
