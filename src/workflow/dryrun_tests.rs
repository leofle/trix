//! Dry run and rehearsal.

use super::check::Ctx;
use super::dryrun::*;
use super::engine_tests::{issue_to_pr, pair};
use super::model::*;
use super::replay::{World, compare_in};
use super::settings::Settings;
use super::store::builtin;
use super::view::RunState;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

fn ideal() -> (super::caps::Matrix, Ctx) {
    let m = super::caps::Matrix::shipped();
    let ctx = Ctx::ideal(&m);
    (m, ctx)
}

fn sim(w: &Workflow, scenario: Scenario) -> DryRun {
    let (m, ctx) = ideal();
    dry_run(
        w,
        &m,
        &ctx,
        &Settings::default(),
        scenario,
        super::cli::sample_inputs(w),
    )
}

#[test]
fn a_dry_run_shows_every_prompt_and_what_would_be_approved_without_you() {
    let w = builtin("issue-to-pr").unwrap();
    let d = sim(&w, Scenario::Happy);
    // The checker first, then the path: one cycle, to the gate, which is yours.
    assert!(d.ran && d.check.starts_with("issue-to-pr: 0 errors, 2 warnings"));
    assert_eq!(
        (d.state, d.cycles, d.wasted),
        (RunState::NeedsYou, 1, 0),
        "{}",
        d.render(false)
    );
    assert!(
        d.why
            .starts_with("ready to merge PR #1: done_when holds on"),
        "{}",
        d.why
    );
    assert!(
        d.why.contains("approvals are off for 'issue-to-pr'"),
        "{}",
        d.why
    );
    assert!(
        d.auto.is_empty()
            && d.render(false)
                .contains("no approval would be made without you")
    );
    // Every prompt each node would get, per visit, with its inbox filled in.
    let who: Vec<(&str, u32)> = d.prompts.iter().map(|(n, v, _)| (n.as_str(), *v)).collect();
    assert_eq!(who, [("implementer", 1), ("reviewer", 1), ("judge", 1)]);
    compare_in("dryrun", "issue-to-pr-happy", &d.prompts_text());
    // With approvals switched on in your config, the judge's is marked.
    let (m, mut ctx) = ideal();
    ctx.approvals_allow = vec!["issue-to-pr".into()];
    let allow = super::settings::ApprovalOpts {
        allow: vec!["issue-to-pr".into()],
        ..Default::default()
    };
    let on = Settings {
        approvals: allow,
        ..Default::default()
    };
    let d = dry_run(
        &w,
        &m,
        &ctx,
        &on,
        Scenario::Happy,
        super::cli::sample_inputs(&w),
    );
    assert_eq!(d.state, RunState::Succeeded, "{}", d.render(false));
    assert_eq!(d.auto.len(), 1);
    assert!(
        d.auto[0].starts_with("⚡ judge approves the merge of"),
        "{}",
        d.auto[0]
    );
    // Each failure edge once: every loop is shown going round, with what's passed along.
    let d = sim(&w, Scenario::EachFailure);
    assert!(d.cycles > 1 && d.wasted == 0, "{}", d.render(false));
    let again = d
        .prompts
        .iter()
        .find(|(n, v, _)| n == "implementer" && *v == 2)
        .unwrap();
    assert!(
        again
            .2
            .starts_with("Visit 2. Address this, then hand in again:\n(simulated failure)"),
        "{}",
        again.2
    );
    compare_in("dryrun", "issue-to-pr-failures", &d.prompts_text());
    // Every template dry-runs, and none is left unfinished by the simulation.
    for name in super::store::builtin_names()
        .into_iter()
        .filter(|n| *n != "triage")
    {
        let d = sim(&builtin(name).unwrap(), Scenario::Happy);
        let ended = matches!(d.state, RunState::NeedsYou | RunState::Succeeded);
        assert!(
            d.ran && d.looks < 400 && ended,
            "{name}: {}",
            d.render(false)
        );
    }
    assert_eq!(
        Scenario::parse("stall=judge"),
        Ok(Scenario::Stall("judge".into()))
    );
    let scripted = BTreeMap::from([(
        "judge".to_string(),
        vec!["fail".to_string(), "pass".to_string()],
    )]);
    assert_eq!(
        Scenario::parse("judge=fail,pass"),
        Ok(Scenario::Script(scripted))
    );
    assert!(Scenario::parse("sunny").is_err());
}

#[test]
fn seeded_defects_are_reported_before_any_agent_runs() {
    // An error the checker finds: nothing is simulated, and it says why.
    let mut w = issue_to_pr();
    w.limits.wall_clock = None;
    let d = sim(&w, Scenario::Happy);
    assert!(!d.ran && d.prompts.is_empty());
    assert!(d.render(true).contains("no wall_clock"));
    assert!(
        d.render(true)
            .ends_with("not simulated: the checker found errors\n")
    );
    // The worst case: what stops it is said. Here the implementer's
    // max_visits, before the ceiling of six cycles.
    let d = sim(&issue_to_pr(), Scenario::Worst);
    assert_eq!(d.state, RunState::Parked, "{}", d.render(false));
    assert_eq!(
        d.why,
        "implementer has had its 4 visits, and is asked for another"
    );
    assert_eq!(
        d.cycles, 5,
        "four visits, and the fifth cycle that asked for one more"
    );
    // A defect only a run shows: done_when names a node that never votes
    // pass, so the run reaches its end and fails there.
    let mut w = builtin("bug-repro").unwrap();
    w.goal.done_when.attested = vec![Cond {
        judge: Some("reproducer".into()),
        ..Cond::default()
    }];
    let (m, ctx) = ideal();
    assert_eq!(
        super::check::check(&w, &m, &ctx).errors(),
        0,
        "the checker can't see it"
    );
    let d = sim(&w, Scenario::Happy);
    assert_eq!(d.state, RunState::Failed);
    assert!(
        d.why
            .contains("done_when isn't met: reproducer hasn't voted pass"),
        "{}",
        d.why
    );
    // A node that goes quiet: reminded once, then it's yours (never a hang).
    let d = sim(&issue_to_pr(), Scenario::Stall("reviewer".into()));
    assert_eq!(d.state, RunState::NeedsYou, "{}", d.render(false));
    assert!(
        d.why.contains("reviewer") && d.why.contains("stopped again without its report"),
        "{}",
        d.why
    );
    assert!(
        d.log
            .iter()
            .any(|l| l == "reviewer stopped without its report: reminded it")
    );
    // A quota stop: blocked and said, retried with backoff, then escalated.
    let d = sim(&issue_to_pr(), Scenario::Quota("implementer".into()));
    let blocked = "implementer is blocked: codex is out of quota or rate-limited";
    assert!(
        d.log.iter().any(|l| l.contains(blocked)),
        "{}",
        d.render(false)
    );
    assert_eq!(d.state, RunState::NeedsYou);
    assert!(
        d.why.starts_with("implementer is still out of quota after"),
        "{}",
        d.why
    );
    // A land conflict: back to its author, as on_conflict says.
    let d = sim(&pair("src/**", "src/**"), Scenario::Conflict);
    let conflicted = |l: &String| {
        l.starts_with("landing ") && l.contains("conflicted: src/conflicted-in-simulation")
    };
    assert!(d.log.iter().any(conflicted), "{}", d.render(false));
    let sent_back = |(_, v, p): &(String, u32, String)| {
        *v == 2 && p.contains("couldn't land on the run's branch")
    };
    assert!(
        d.cycles == 2 && d.prompts.iter().any(sent_back),
        "{}",
        d.render(false)
    );
    // On autopilot the same stall parks instead of asking.
    let mut auto = issue_to_pr();
    auto.mode = Mode::Autopilot;
    assert_eq!(
        sim(&auto, Scenario::Stall("reviewer".into())).state,
        RunState::Parked
    );
}

#[test]
fn in_a_rehearsal_nothing_an_agent_does_reaches_the_real_remote() {
    use super::env::Env;
    let mut w = World::new("rehearsal");
    let refs = |w: &mut World| {
        w.sh(
            &w.origin.clone(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    };
    let before = refs(&mut w);
    let trees = w.sh(&w.repo.clone(), &["worktree", "list"]);
    let home = w.base.clone();
    let git = |dir: &std::path::Path, args: &[&str]| -> Result<String, String> {
        let mut c = std::process::Command::new("git");
        c.current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", &home);
        let o = c.output().map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).into())
        }
    };
    let r = prepare(&w.repo, git).unwrap();
    // The clone's origin is the local bare mirror, not the fixture's remote.
    let url = git(&r.clone, &["remote", "get-url", "origin"]).unwrap();
    assert_eq!(
        std::path::Path::new(url.trim()).canonicalize().unwrap(),
        r.mirror.canonicalize().unwrap()
    );
    assert_eq!(
        git(&r.mirror, &["rev-parse", "--is-bare-repository"])
            .unwrap()
            .trim(),
        "true"
    );
    // A gh that works whenever it finds a login, as yours does.
    let bin = w.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = "#!/bin/sh\nif [ -n \"$GH_TOKEN$GITHUB_TOKEN\" ] || [ -f \"${GH_CONFIG_DIR:-$HOME/.config/gh}/hosts.yml\" ]; then\n  \
              echo \"$*\" >> \"$(dirname \"$0\")/../gh-reached\"; exit 0\nfi\necho 'gh: run gh auth login' >&2\nexit 4\n";
    std::fs::write(bin.join("gh"), gh).unwrap();
    std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(w.base.join(".config/gh")).unwrap();
    std::fs::write(w.base.join(".config/gh/hosts.yml"), "github.com:\n").unwrap();
    // What an agent in a rehearsal pane tries. Every pane and check there
    // gets the guard, whatever the workflow's push policy says.
    let script = "echo x > f && git add f && git -c user.name=a -c user.email=a@example.com commit -qm x\n\
                  git push origin HEAD:refs/heads/sneak && echo PUSHED\n\
                  gh pr create --title x --body y && echo CREATED\n\
                  gh pr merge 12 --squash && echo MERGED\n\
                  gh api repos/acme/app/issues -f title=x && echo API\ntrue";
    let argv = ["sh".to_string(), "-c".to_string(), script.to_string()];
    let out = Rehearse {
        inner: &mut w,
        pr: false,
    }
    .check(&r.clone, &argv, false)
    .unwrap();
    for word in ["PUSHED", "CREATED", "MERGED", "API"] {
        assert!(!out.log_tail.contains(word), "{word}: {}", out.log_tail);
    }
    assert!(
        w.checks.last().unwrap().2,
        "the guard was on though the caller didn't ask for it"
    );
    assert!(!w.base.join("gh-reached").exists());
    // Trix's own push, PR and merge are stubbed: nothing goes through gh or git.
    let mut env = Rehearse {
        inner: &mut w,
        pr: false,
    };
    assert_eq!(
        env.git(&r.clone, &["push", "origin", "HEAD:refs/heads/trix/wf-x"]),
        Ok(String::new())
    );
    assert_eq!(
        env.gh(&r.clone, &["pr", "list", "--head", "x"]).unwrap(),
        "[]"
    );
    env.gh(&r.clone, &["pr", "create", "--head", "x"]).unwrap();
    assert!(
        env.gh(&r.clone, &["pr", "list", "--head", "x"])
            .unwrap()
            .contains("no PR was opened")
    );
    env.gh(&r.clone, &["pr", "merge", "0", "--squash"]).unwrap();
    assert!(w.hub.pr_bodies.is_empty() && w.hub.prs.len() == 1 && w.hub.prs[0].state == "OPEN");
    // Even the unguarded push would only have reached the mirror.
    assert!(
        git(&r.mirror, &["for-each-ref"])
            .unwrap()
            .lines()
            .all(|l| !l.contains("sneak"))
    );
    // The real remote is as it was, and afterwards nothing is left behind.
    assert_eq!(refs(&mut w), before);
    r.cleanup();
    assert!(!r.base.exists() && !r.clone.exists() && !r.mirror.exists());
    assert_eq!(w.sh(&w.repo.clone(), &["worktree", "list"]), trees);
    let _ = std::fs::remove_dir_all(&w.base);
}
