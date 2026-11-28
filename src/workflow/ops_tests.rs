//! The run log, the overview, scheduling and notifications.

use super::engine::{self, Ask, Op, Opts};
use super::engine_tests::*;
use super::model::*;
use super::overview;
use super::replay::World;
use super::runlog;
use super::runner::Run;
use super::view::RunState;

const CANARY: &str = "CANARY-7f3a91-do-not-leak";

#[test]
fn the_log_records_everything_and_a_canary_never_leaves_the_machine() {
    // The issue's text carries a canary. Agents echo it in what they hand in.
    let mut w = World::new("ops-log");
    let id = "issue-to-pr-42";
    let body = format!("After 60s a client is still refused. {CANARY}");
    let mut r = engine::WfRun::new(
        id,
        issue_to_pr(),
        w.repo.clone(),
        "main",
        issue(&body),
        Opts {
            approvals: true,
            ..opts()
        },
        super::replay::T0,
    );
    r.opts.on_finish = vec![
        "~/bin/notify-run".into(),
        "{{run.id}}".into(),
        "{{run.state}}".into(),
    ];
    engine::store(&root(&w), &r).unwrap();
    tick(&mut w, &mut r);
    let pane = "issue-to-pr-42-implementer";
    op(
        &mut w,
        Some(pane),
        None,
        Op::Ask {
            text: format!("@reviewer is {CANARY} a real id?"),
        },
    )
    .unwrap();
    tick(&mut w, &mut r);
    op(&mut w, None, Some(id), Op::Answer { key: 'c' }).unwrap();
    tick(&mut w, &mut r);
    edit(&mut w, &r, "implementer", "src/done", "ok\n");
    submit(
        &mut w,
        &r,
        "implementer",
        &format!(r#"{{"verdict": "ready", "summary": "fixed {CANARY}"}}"#),
    )
    .unwrap();
    ticks(&mut w, &mut r, 3);
    submit(
        &mut w,
        &r,
        "reviewer",
        r#"{"verdict": "pass", "findings": []}"#,
    )
    .unwrap();
    ticks(&mut w, &mut r, 2);
    submit(
        &mut w,
        &r,
        "judge",
        &format!(r#"{{"verdict": "pass", "reasons": "{CANARY} is solved"}}"#),
    )
    .unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.state, RunState::Succeeded, "{:?}", r.log);
    // The log has every assignment (the prompt as rendered, with its brief),
    // question, mention, answer, diff summary, check result and approval.
    let dir = engine::dir_of(&root(&w), id);
    let lines: Vec<runlog::Entry> = runlog::read(&dir).into_iter().map(Result::unwrap).collect();
    let of = |kind: &str| lines.iter().filter(|e| e.kind == kind).collect::<Vec<_>>();
    let assigned: Vec<&str> = of("assign")
        .iter()
        .map(|e| e.data["node"].as_str().unwrap())
        .collect();
    assert_eq!(assigned, ["implementer", "reviewer", "judge"]);
    assert!(
        of("assign")[0].data["prompt"]
            .as_str()
            .unwrap()
            .contains("trix wf submit implementer")
    );
    assert_eq!(
        of("assign")[0].data["brief"]["owns"],
        serde_json::json!(["src/**", "test/**"])
    );
    assert_eq!(
        (
            of("question").len(),
            of("mention").len(),
            of("answer").len()
        ),
        (1, 1, 1)
    );
    assert_eq!(of("mention")[0].data["to"], "reviewer");
    let reports = of("report");
    assert_eq!(reports.len(), 3);
    assert_eq!(
        (
            reports[0].data["verdict"].as_str(),
            &reports[0].data["files"]
        ),
        (Some("ready"), &serde_json::json!(["src/done"]))
    );
    assert_eq!(
        (
            of("check").len(),
            of("check")[0].data["exit_code"].as_i64(),
            of("check")[0].data["verdict"].as_str()
        ),
        (1, Some(0), Some("pass"))
    );
    assert_eq!(
        (
            of("approval").len(),
            of("approval")[0].data["approver"].as_str()
        ),
        (1, Some("judge"))
    );
    assert!(
        of("note")
            .iter()
            .any(|e| e.data.as_str().unwrap().starts_with("merged PR #20"))
    );
    // In order, chained, and as written.
    assert_eq!(runlog::verify(&dir), Ok(lines.len()));
    assert!(
        lines
            .windows(2)
            .all(|p| p[1].n == p[0].n + 1 && p[1].at >= p[0].at)
    );
    // The log stays local, and it does hold the prompt, canary and all.
    assert!(
        std::fs::read_to_string(runlog::file(&dir))
            .unwrap()
            .contains(CANARY)
    );
    // What leaves the machine never does: the PR's body, the end-of-run
    // report, the notification, the on_finish command.
    let pr = w.hub.pr_bodies.last().unwrap().clone();
    let report = r.ended().0;
    let summary = super::summary::outcome(&r);
    for (what, text) in [
        ("the PR body", &pr),
        ("the run report", &report),
        ("the summary", &summary),
    ] {
        assert!(!text.contains(CANARY), "{what} leaked the canary:\n{text}");
        assert!(
            !text.contains("refused") && !text.contains(&w.base.display().to_string()),
            "{what}: issue text or a local path"
        );
        assert!(
            !text.contains("trix wf submit") && !text.contains("212 passed"),
            "{what}: a prompt or a command's log"
        );
    }
    assert!(
        w.notified
            .iter()
            .all(|(t, b)| !t.contains(CANARY) && !b.contains(CANARY)),
        "{:?}",
        w.notified
    );
    assert!(
        pr.contains("| implementer | agent | codex | 1 | ready |")
            && pr.contains("Merge approved by the approver node `judge`"),
        "{pr}"
    );
    // on_finish ran once, with the run's id and state.
    assert_eq!(
        w.finished,
        [vec![
            "~/bin/notify-run".to_string(),
            id.to_string(),
            "succeeded".to_string()
        ]]
    );
    // An edit to the log is seen.
    let text = std::fs::read_to_string(runlog::file(&dir)).unwrap();
    std::fs::write(
        runlog::file(&dir),
        text.replace("\"approver\":\"judge\"", "\"approver\":\"you\""),
    )
    .unwrap();
    assert!(
        runlog::verify(&dir)
            .unwrap_err()
            .iter()
            .any(|l| l.contains("was edited"))
    );
    done(w);
}

#[test]
fn a_landing_is_in_the_log_too() {
    let mut w = World::new("ops-land");
    let mut r = start(&mut w, pair("src/api/**", "src/ui/**"), opts());
    tick(&mut w, &mut r);
    submit(&mut w, &r, "prep", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 2);
    edit(&mut w, &r, "api", "src/api/a", "a\n");
    submit(&mut w, &r, "api", r#"{"verdict": "ready"}"#).unwrap();
    edit(&mut w, &r, "ui", "src/ui/b", "b\n");
    submit(&mut w, &r, "ui", r#"{"verdict": "ready"}"#).unwrap();
    ticks(&mut w, &mut r, 3);
    let lines: Vec<runlog::Entry> = runlog::read(&engine::dir_of(&root(&w), &r.id))
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let landed: Vec<(&str, u64)> = lines
        .iter()
        .filter(|e| e.kind == "land")
        .map(|e| {
            (
                e.data["node"].as_str().unwrap(),
                e.data["commits"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(landed, [("api", 1), ("ui", 1)]);
    done(w);
}

#[test]
fn the_overview_counts_match_the_run_and_n_visits_everything_that_needs_you() {
    let (mut w, mut r) = begun("ops-overview", opts());
    let text = overview::render(&r, &w.seen, None, 100);
    assert!(
        text.starts_with("run issue-to-pr-42 · supervised · running · goal: Issue #42 is solved"),
        "{text}"
    );
    assert!(
        text.contains("0 of 5 done · 0 blocked · 0 need you · cycles 1 (0 wasted) · $1.00"),
        "{text}"
    );
    assert!(overview::needs(&r, &w.seen).is_empty() && overview::next(&[], None).is_none());
    // An agent at a prompt no rule answered needs you.
    w.state("issue-to-pr-42-implementer", "needs you");
    let needs = overview::needs(&r, &w.seen);
    assert_eq!(
        (
            needs.len(),
            needs[0].pane.as_str(),
            needs[0].node.as_deref()
        ),
        (1, "issue-to-pr-42-implementer", Some("implementer"))
    );
    w.state("issue-to-pr-42-implementer", "working");
    // On to the gate, with the judge's pane at a prompt too: two items.
    to_gate(&mut w, &mut r);
    tick(&mut w, &mut r);
    assert_eq!(r.ask, Some(Ask::Merge));
    w.state("issue-to-pr-42-implementer", "needs you");
    let needs = overview::needs(&r, &w.seen);
    assert_eq!(needs.len(), 2);
    assert_eq!(
        (needs[0].pane.as_str(), needs[0].node.as_deref()),
        ("issue-to-pr-42", Some("gate")),
        "the gate, in the run's pane"
    );
    // The counts are the run's.
    let v = r.view(&w.seen);
    let text = overview::render(&r, &w.seen, Some(0), 100);
    assert!(
        text.contains(&format!(
            "{} of {} done · {} blocked · 2 need you · cycles {} ({} wasted)",
            v.done(),
            v.nodes.len(),
            v.blocked(),
            r.cycles,
            r.wasted
        )),
        "{text}"
    );
    assert_eq!((v.done(), v.nodes.len(), v.unpriced), (4, 5, true));
    assert!(text.contains("+ ?"), "an unpriced judge: unknown, never $0");
    assert!(
        text.contains("needs you: ready to merge PR #20")
            && text.lines().any(|l| l.starts_with("▶gate")),
        "{text}"
    );
    // n goes to each item once, then round again.
    let mut at = None;
    let mut visited = Vec::new();
    for _ in 0..needs.len() {
        at = overview::next(&needs, at);
        visited.push(needs[at.unwrap()].pane.clone());
    }
    assert_eq!(visited, ["issue-to-pr-42", "issue-to-pr-42-implementer"]);
    assert_eq!(overview::next(&needs, at), Some(0));
    // At 80 columns every line still fits.
    for width in [100, 80] {
        let text = overview::render(&r, &w.seen, Some(0), width);
        assert!(
            text.lines().take(9).all(|l| l.chars().count() <= width),
            "{width}:\n{text}"
        );
    }
    // A parked run is something that needs you.
    r.ask = None;
    r.state = RunState::Parked;
    r.why = "the deadline (06:00) has passed".into();
    assert_eq!(
        overview::needs(&r, &[])[0].what,
        "parked: the deadline (06:00) has passed"
    );
    done(w);
}

/// A time of day `secs` after `from`, as the machine's clock reads it.
fn hhmm(from: u64, secs: u64) -> String {
    let s = crate::ledger::local(from + secs).2;
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

#[test]
fn on_a_mocked_clock_only_work_counts_and_a_deadline_parks() {
    let (mut w, mut r) = begun("ops-clock", opts());
    // Time with a node running counts.
    w.later(600);
    tick(&mut w, &mut r);
    w.later(20);
    tick(&mut w, &mut r);
    assert_eq!(r.inflight, 50, "two looks, at most 30 s each");
    to_gate(&mut w, &mut r);
    tick(&mut w, &mut r);
    assert_eq!(r.ask, Some(Ask::Merge));
    // A run waiting at a gate overnight doesn't use up its wall clock.
    let before = r.inflight;
    for _ in 0..20 {
        w.later(3600);
        tick(&mut w, &mut r);
    }
    assert_eq!(
        (r.inflight, r.state),
        (before, RunState::NeedsYou),
        "20 hours at the gate, a 3 hour wall clock"
    );
    assert!(r.inflight < r.wall_clock);
    op(&mut w, None, Some(&r.id), Op::Approve).unwrap();
    ticks(&mut w, &mut r, 2);
    assert_eq!(r.state, RunState::Succeeded);
    // The wall clock itself: work past it stops the run, by on_limit.
    let (mut w2, mut r2) = begun("ops-clock-limit", opts());
    r2.wall_clock = 60;
    for _ in 0..3 {
        w2.later(30);
        tick(&mut w2, &mut r2);
    }
    assert_eq!(r2.state, RunState::Parked);
    assert!(
        r2.why.starts_with("the wall clock is used up"),
        "{}",
        r2.why
    );
    // A deadline: a time of day. Past it no new work starts, and the run parks.
    let mut wf = issue_to_pr();
    let mut w3 = World::new("ops-deadline");
    wf.limits.deadline = Some(hhmm(w3.now, 600));
    wf.limits.kill_at = Some(hhmm(w3.now, 7200));
    let mut r3 = start(&mut w3, wf, opts());
    tick(&mut w3, &mut r3);
    w3.later(300);
    tick(&mut w3, &mut r3);
    assert_eq!(r3.state, RunState::Running, "not yet");
    w3.later(400);
    tick(&mut w3, &mut r3);
    assert_eq!(r3.state, RunState::Parked);
    assert!(
        r3.why.starts_with("the deadline (") && r3.why.contains("no new work starts"),
        "{}",
        r3.why
    );
    assert!(
        w3.notified
            .iter()
            .any(|(t, _)| t == "issue-to-pr-42 is parked")
    );
    // Resumed, it goes on past the deadline; at the kill time it's aborted.
    op(&mut w3, None, Some(&r3.id), Op::Resume).unwrap();
    ticks(&mut w3, &mut r3, 2);
    assert_eq!(r3.state, RunState::Running);
    w3.later(7000);
    tick(&mut w3, &mut r3);
    assert_eq!(r3.state, RunState::Aborted, "{:?}", r3.log);
    assert!(r3.log.iter().any(|(_, l)| l.starts_with("kill_at ")));
    done(w);
    done(w2);
    done(w3);
}

#[test]
fn the_step_13_commands_read_their_arguments_and_the_digest_says_what_ended() {
    use super::cli::{self, Cmd};
    let words = |l: &str| l.split_whitespace().map(String::from).collect::<Vec<_>>();
    let id = || "issue-to-pr-42".to_string();
    assert_eq!(
        cli::parse(&words("log issue-to-pr-42")),
        Ok(Cmd::Log {
            id: id(),
            verify: false
        })
    );
    assert_eq!(
        cli::parse(&words("log issue-to-pr-42 --verify")),
        Ok(Cmd::Log {
            id: id(),
            verify: true
        })
    );
    assert_eq!(
        cli::parse(&words("audit issue-to-pr-42")),
        Ok(Cmd::Audit(id()))
    );
    assert_eq!(
        cli::parse(&words("overview issue-to-pr-42")),
        Ok(Cmd::Overview(id()))
    );
    assert_eq!(cli::parse(&words("scheduler")), Ok(Cmd::Scheduler));
    assert_eq!(
        cli::parse(&words("units systemd")),
        Ok(Cmd::Units {
            kind: "systemd".into(),
            yes: false
        })
    );
    assert_eq!(
        cli::parse(&words("units launchd --yes")),
        Ok(Cmd::Units {
            kind: "launchd".into(),
            yes: true
        })
    );
    assert_eq!(cli::parse(&words("schedule ls")), Ok(Cmd::ScheduleLs));
    assert_eq!(
        cli::parse(&words("schedule rm nightly-deps")),
        Ok(Cmd::ScheduleRm("nightly-deps".into()))
    );
    let args: Vec<String> = [
        "schedule",
        "nightly-deps",
        "--cron",
        "0 2 * * *",
        "--deadline",
        "06:00",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let want = Cmd::Schedule {
        name: "nightly-deps".into(),
        cron: "0 2 * * *".into(),
        deadline: Some("06:00".into()),
        kill_at: None,
        inputs: Default::default(),
    };
    assert_eq!(cli::parse(&args), Ok(want));
    let bad: Vec<String> = ["schedule", "nightly-deps", "--cron", "every night"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(
        cli::parse(&bad)
            .unwrap_err()
            .contains("isn't a cron expression")
    );
    assert!(
        cli::parse(&words("schedule nightly-deps"))
            .unwrap_err()
            .contains("needs --cron")
    );
    // The digest: what ended, or stopped for you, in the last day.
    let (w, mut r) = begun("ops-digest", opts());
    let mut views = vec![r.view(&[])];
    let ended = std::collections::BTreeMap::from([("issue-to-pr-42".to_string(), 1_000u64)]);
    assert_eq!(
        super::sched::digest(&views, &ended, 0),
        None,
        "a run still going isn't news"
    );
    r.state = RunState::Parked;
    r.why = "quota".into();
    views = vec![r.view(&[])];
    assert_eq!(
        super::sched::digest(&views, &ended, 0).as_deref(),
        Some("issue-to-pr-42 · parked: quota")
    );
    assert_eq!(
        super::sched::digest(&views, &ended, 2_000),
        None,
        "not what ended before yesterday"
    );
    let _ = Mode::Autopilot;
    done(w);
}
