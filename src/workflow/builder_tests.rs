//! The builder, driven by keys and crossterm mouse events on a test backend.

use super::builder::*;
use super::check::{At, Ctx, Severity, check};
use super::model::Mode as Mode2;
use super::model::{Amount, Revisit, Workflow};
use super::store::builtin;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn ideal() -> (super::caps::Matrix, Ctx) {
    let m = super::caps::Matrix::shipped();
    let ctx = Ctx::ideal(&m);
    (m, ctx)
}

fn builder(w: Workflow) -> Builder {
    let (m, ctx) = ideal();
    Builder::new(w, m, ctx)
}

fn key(b: &mut Builder, code: KeyCode) -> Outcome {
    b.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ch(b: &mut Builder, c: char) -> Outcome {
    key(b, KeyCode::Char(c))
}

/// Type a line into the open input, replacing what's there, and keep it.
fn line(b: &mut Builder, text: &str) {
    while matches!(&b.mode, Mode::Input { text, .. } if !text.is_empty()) {
        key(b, KeyCode::Backspace);
    }
    for c in text.chars() {
        ch(b, c);
    }
    key(b, KeyCode::Enter);
}

/// The screen, as text.
fn screen(b: &mut Builder, width: u16, height: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(|f| b.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..height {
        for x in 0..width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn select(b: &mut Builder, id: &str) {
    for _ in 0..b.w.nodes.len() {
        if b.selected.as_deref() == Some(id) {
            return;
        }
        key(b, KeyCode::Tab);
    }
    panic!("no node {id}");
}

#[test]
fn every_check_shows_live_on_its_node_as_you_build() {
    // The PRD's picture: a Codex reviewer on read-only.
    let mut w = builtin("issue-to-pr").unwrap();
    w.nodes.get_mut("reviewer").unwrap().agent = Some("codex".into());
    let mut b = builder(w);
    select(&mut b, "reviewer");
    let text = screen(&mut b, 120, 36);
    // The header counts, the node's mark, the inspector's needs-and-has, the bar.
    assert!(
        text.contains("issue-to-pr · builder")
            && text.contains("1 error")
            && text.contains("3 warnings"),
        "{text}"
    );
    assert!(
        text.contains("reviewer ✗") && text.contains("INSPECTOR · reviewer"),
        "{text}"
    );
    assert!(
        text.contains("✗ reviewer: Codex in read-only can't reach Trix"),
        "{text}"
    );
    assert!(
        text.contains("f fix: switch to claude") && text.contains("F fix: use instruct-read"),
        "{text}"
    );
    assert!(
        b.badges("reviewer")
            .contains(&("hand-in ✗".to_string(), Severity::Error))
    );
    assert!(
        b.needs_has("reviewer")
            .contains(&("hand in".to_string(), "✗ no".to_string(), false))
    );
    assert!(!b.can_run());
    // r is off while an error remains, and says so.
    assert_eq!(ch(&mut b, 'r'), Outcome::Stay);
    assert!(
        b.message
            .contains("run and rehearse stay off while any error remains")
    );
    // f applies the first fix, and the problem is gone at once, everywhere.
    ch(&mut b, 'f');
    assert_eq!(b.w.nodes["reviewer"].agent.as_deref(), Some("claude"));
    let text = screen(&mut b, 120, 36);
    assert!(
        text.contains("0 errors") && !text.contains("can't reach Trix") && b.can_run(),
        "{text}"
    );
    assert!(b.needs_has("reviewer").contains(&(
        "read-only".to_string(),
        "✓ enforced".to_string(),
        true
    )));
    // The backend picker lists each installed kind with its fit and why.
    let fits = b.backends("reviewer");
    let of = |k: &str| fits.iter().find(|(kind, _, _)| kind == k).unwrap().clone();
    assert_eq!(of("claude").1, Fit::Fits);
    assert_eq!(of("gemini").1, Fit::Limits);
    assert!(
        of("gemini").2.contains("read-only by instruction only"),
        "{}",
        of("gemini").2
    );
    assert!(
        fits[0].1 == Fit::Fits && fits.last().unwrap().1 >= Fit::Limits,
        "those that fit come first"
    );
    // Adding an agent asks for its brief first: owns, must not touch, done.
    ch(&mut b, 'a');
    assert!(matches!(&b.mode, Mode::Input { then: Then::Owns(id), .. } if id == "agent"));
    line(&mut b, "docs/**");
    line(&mut b, ".github/**");
    line(&mut b, "The docs say what changed.");
    let brief = b.w.nodes["agent"].brief.clone().unwrap();
    assert_eq!(
        (brief.owns, brief.must_not_touch, brief.done.as_str()),
        (
            vec!["docs/**".to_string()],
            vec![".github/**".to_string()],
            "The docs say what changed."
        )
    );
    // Not wired yet: it shows on the node and in the bar, live.
    let text = screen(&mut b, 120, 36);
    assert!(
        text.contains("agent ✗") && text.contains("✗ agent: "),
        "{text}"
    );
    assert!(
        b.report
            .at(&At::Node("agent".into()))
            .any(|p| p.code == "unreachable")
    );
    // c connects: pick the target, name the verdict.
    select(&mut b, "judge");
    ch(&mut b, 'c');
    let Mode::Pick { items, .. } = b.mode.clone() else {
        panic!("{:?}", b.mode)
    };
    for _ in 0..items.iter().position(|i| i == "agent").unwrap() {
        key(&mut b, KeyCode::Down);
    }
    key(&mut b, KeyCode::Enter);
    line(&mut b, "docs");
    select(&mut b, "agent");
    ch(&mut b, 'c');
    let Mode::Pick { items, .. } = b.mode.clone() else {
        panic!("{:?}", b.mode)
    };
    for _ in 0..items.iter().position(|i| i == "gate").unwrap() {
        key(&mut b, KeyCode::Down);
    }
    key(&mut b, KeyCode::Enter);
    line(&mut b, "ready");
    assert!(
        !b.report.has("unreachable") && b.report.errors() == 0,
        "{}",
        b.report.render()
    );
    // A second writer whose scope overlaps nothing, in order: no warning for it.
    assert!(!b.report.has("scope-overlap"));
    // The builder's problems are the checker's, word for word.
    let (m, ctx) = ideal();
    assert_eq!(b.problems().render(), check(&b.w, &m, &ctx).render());
}

#[test]
fn every_action_works_from_the_keys() {
    let mut b = builder(builtin("issue-to-pr").unwrap());
    // tab and shift+tab select; arrows move the node, and where it is is kept.
    select(&mut b, "implementer");
    let before = b.w.layout["implementer"];
    key(&mut b, KeyCode::Right);
    key(&mut b, KeyCode::Down);
    assert_eq!(b.w.layout["implementer"], [before[0] + 2, before[1] + 1]);
    b.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(b.w.layout["implementer"][0], (before[0] + 2 - 8).max(0));
    key(&mut b, KeyCode::BackTab);
    assert_ne!(b.selected.as_deref(), Some("implementer"));
    // u undoes, ctrl+r redoes.
    let moved = b.w.layout["implementer"];
    ch(&mut b, 'u');
    assert_ne!(b.w.layout["implementer"], moved);
    b.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(b.w.layout["implementer"], moved);
    // k, j, g add a check, a judge, a gate; x removes the selected node.
    let n = b.w.nodes.len();
    ch(&mut b, 'k');
    assert_eq!(
        (
            b.w.nodes.len(),
            b.w.nodes["check"].kind.as_str(),
            b.selected.as_deref()
        ),
        (n + 1, "check", Some("check"))
    );
    ch(&mut b, 'x');
    assert_eq!(b.w.nodes.len(), n);
    ch(&mut b, 'j');
    key(&mut b, KeyCode::Esc);
    assert_eq!(b.w.nodes["judge-2"].kind, "judge");
    ch(&mut b, 'x');
    // e edits a check's command.
    select(&mut b, "tests");
    ch(&mut b, 'e');
    line(&mut b, "cargo test --locked");
    assert_eq!(b.w.nodes["tests"].run, ["cargo", "test", "--locked"]);
    // v adds a verifier for the selected writer, wired in: fresh, read-only,
    // on another backend, its changes going back to the author.
    let mut b = builder(builtin("bug-repro").unwrap());
    select(&mut b, "shows");
    ch(&mut b, 'v');
    assert!(b.message.contains("select one first"), "v is for a writer");
    select(&mut b, "reproducer");
    ch(&mut b, 'v');
    let v = &b.w.nodes["verifier-2"];
    assert_eq!(
        (
            v.preset.as_deref(),
            v.revisit(),
            v.agent.as_deref() != Some("claude")
        ),
        (Some("verifier"), Revisit::Fresh, true)
    );
    assert!(
        b.w.edges
            .iter()
            .any(|e| e.from == "reproducer" && e.to == "verifier-2")
    );
    assert!(
        b.w.edges
            .iter()
            .any(|e| e.from == "verifier-2" && e.to == "reproducer" && e.on.list() == ["changes"])
    );
    assert!(
        b.w.edges
            .iter()
            .any(|e| e.from == "verifier-2" && e.to == "shows" && e.on.list() == ["pass"])
    );
    assert_eq!(b.report.errors(), 0, "{}", b.report.render());
    // L lays the graph out: every node gets a place of its own.
    ch(&mut b, 'L');
    let mut places: Vec<[i32; 2]> = b.w.layout.values().copied().collect();
    places.sort();
    places.dedup();
    assert_eq!(places.len(), b.w.nodes.len());
    // A opens the Approvals tab; m offers only nodes that didn't write the change.
    let mut b = builder(builtin("issue-to-pr").unwrap());
    ch(&mut b, 'A');
    let text = screen(&mut b, 120, 36);
    assert!(
        text.contains("merge: judge approves")
            && text.contains("verified for the installed version"),
        "{text}"
    );
    ch(&mut b, 'm');
    let Mode::Pick { items, .. } = b.mode.clone() else {
        panic!("{:?}", b.mode)
    };
    assert_eq!(items, ["judge", "reviewer"], "never the implementer");
    key(&mut b, KeyCode::Down);
    key(&mut b, KeyCode::Enter);
    assert_eq!(
        b.w.approvals.merge.as_ref().unwrap().approver.as_deref(),
        Some("reviewer")
    );
    // F lists the fixes; ? the keys; enter the inspector; esc closes.
    let mut broken = builtin("issue-to-pr").unwrap();
    broken.limits.wall_clock = None;
    let mut b = builder(broken);
    ch(&mut b, 'F');
    assert!(
        matches!(&b.mode, Mode::Pick { items, .. } if items == &["set wall_clock: 3h".to_string()])
    );
    key(&mut b, KeyCode::Enter);
    assert_eq!(b.report.errors(), 0);
    ch(&mut b, '?');
    assert!(screen(&mut b, 120, 36).contains("mouse: click selects, drag moves"));
    key(&mut b, KeyCode::Esc);
    // s, d, r, q are the builder's four ways out.
    assert!(b.dirty);
    assert_eq!(
        (
            ch(&mut b, 's'),
            ch(&mut b, 'd'),
            ch(&mut b, 'r'),
            ch(&mut b, 'q')
        ),
        (Outcome::Save, Outcome::DryRun, Outcome::Run, Outcome::Quit)
    );
    // The launch warning: peak parallelism and cost, unknown where it's unpriced.
    assert_eq!(
        b.launch_text(),
        "launch: at most 1 agent at once, 1 to 6 cycles, cost ~$2.00 + ?."
    );
    let mut pricey = builtin("nightly-deps").unwrap();
    pricey.limits.budget_usd = Some(Amount::Usd(1.0));
    let mut b = builder(pricey);
    assert!(
        b.launch_text().contains("That's over $1.00: press r again"),
        "{}",
        b.launch_text()
    );
    assert!(screen(&mut b, 120, 36).contains("peak 3"), "the cost badge");
}

#[test]
fn the_mouse_selects_and_drags_and_the_layout_fits_100_and_80_columns() {
    let mut b = builder(builtin("issue-to-pr").unwrap());
    let text = screen(&mut b, 120, 36);
    assert!(
        text.contains("INSPECTOR") && text.contains("ADD") && text.contains("implementer"),
        "{text}"
    );
    let mouse = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    // Find the judge on the screen, as a user's eye would, and click it.
    let (row, col) = text
        .lines()
        .enumerate()
        .find_map(|(y, l)| {
            // On the canvas, right of the palette: not the palette's own "j judge".
            let from = l.char_indices().nth(16).map_or(l.len(), |(i, _)| i);
            let x = l[from..].find("judge").filter(|_| y > 0 && y < 30)?;
            Some((y as u16, l[..from + x].chars().count() as u16))
        })
        .expect("the judge is on the canvas");
    b.mouse(mouse(MouseEventKind::Down(MouseButton::Left), col + 1, row));
    b.mouse(mouse(MouseEventKind::Up(MouseButton::Left), col + 1, row));
    assert_eq!(b.selected.as_deref(), Some("judge"), "a click selects");
    // Drag it: where it's dropped is where it stays, and u puts it back.
    let before = b.w.layout["judge"];
    b.mouse(mouse(MouseEventKind::Down(MouseButton::Left), col + 1, row));
    for step in 1..=6 {
        b.mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            col + 1 + step,
            row + step.min(3),
        ));
    }
    b.mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        col + 7,
        row + 3,
    ));
    let after = b.w.layout["judge"];
    assert!(
        after[0] > before[0] && after[1] > before[1],
        "dragged right and down: {before:?} to {after:?}"
    );
    assert!(b.dirty);
    ch(&mut b, 'u');
    assert_eq!(b.w.layout["judge"], before);
    // A click outside the canvas does nothing.
    b.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 2));
    assert_eq!(b.selected.as_deref(), Some("judge"));
    // At 100 columns the inspector is a panel; at 80 it's an overlay on enter.
    let wide = screen(&mut b, 100, 30);
    assert!(
        wide.contains("INSPECTOR · judge") && wide.contains("blocks push"),
        "{wide}"
    );
    let narrow = screen(&mut b, 80, 24);
    assert!(
        !narrow.contains("INSPECTOR")
            && narrow.contains("issue-to-pr · builder")
            && narrow.contains("a add"),
        "{narrow}"
    );
    key(&mut b, KeyCode::Enter);
    let narrow = screen(&mut b, 80, 24);
    assert!(
        narrow.contains("INSPECTOR · judge") && narrow.contains("read-only"),
        "{narrow}"
    );
    assert!(narrow.lines().all(|l| l.chars().count() == 80));
}

#[test]
fn the_wizard_yields_a_valid_workflow_for_every_template() {
    let (m, ctx) = ideal();
    for template in ["triage", "issue-to-pr", "bug-repro", "nightly-deps"] {
        for autopilot in [false, true] {
            let a = Answers {
                template: template.into(),
                name: format!("my-{template}"),
                autopilot,
                ..Answers::default()
            };
            let w = wizard(&a, &m, &ctx).unwrap();
            let w = if template == "triage" {
                w.bind(&serde_json::json!({"triage": {"budget": 10.0, "max_rounds": 2, "reviewers": 2,
                "builder": {"agent": "claude", "args": []}, "reviewer": {"agent": "claude", "args": []}, "reviewer2": {"agent": "claude", "args": []},
                "fixer": {"agent": "claude", "args": []}, "gatekeeper": {"agent": "claude", "args": []}}})).unwrap()
            } else {
                w
            };
            let r = check(&w, &m, &ctx);
            assert_eq!(
                r.errors(),
                0,
                "{template}, autopilot {autopilot}:\n{}",
                r.render()
            );
            assert_eq!(
                (w.name.as_str(), w.builtin, w.mode == Mode2::Autopilot),
                (format!("my-{template}").as_str(), false, autopilot)
            );
        }
    }
    // What you chose is in it: the goal, a backend per role, the limits.
    let a = Answers {
        template: "issue-to-pr".into(),
        name: "mine".into(),
        goal: Some("Issue #{{inputs.issue.number}} is fixed.".into()),
        backends: [
            ("implementer".to_string(), "claude".to_string()),
            ("judge".to_string(), "claude".to_string()),
        ]
        .into(),
        wall_clock: Some("90m".into()),
        budget: Some(4.0),
        autopilot: false,
    };
    let w = wizard(&a, &m, &ctx).unwrap();
    assert_eq!(
        (
            w.goal.text.as_str(),
            w.limits.wall_clock.as_deref(),
            w.limits.budget_usd.as_ref().unwrap().get()
        ),
        (
            "Issue #{{inputs.issue.number}} is fixed.",
            Some("90m"),
            Some(4.0)
        )
    );
    // Each backend gets the profile its role takes there: the judge, read-only on Claude, is enforced.
    assert_eq!(
        (
            w.nodes["implementer"].profile.as_deref(),
            w.nodes["judge"].profile.as_deref()
        ),
        (Some("edit-and-test"), Some("read-only"))
    );
    assert_eq!(check(&w, &m, &ctx).labels["judge"].read_only, Some(true));
    // A blank one is yours to fill, and says what it lacks.
    let blank = wizard(
        &Answers {
            template: "blank".into(),
            name: "new".into(),
            ..Answers::default()
        },
        &m,
        &ctx,
    )
    .unwrap();
    assert!(blank.nodes.is_empty() && check(&blank, &m, &ctx).has("no-mechanical"));
    for (a, why) in [
        (
            Answers {
                template: "nope".into(),
                name: "x".into(),
                ..Answers::default()
            },
            "isn't a template",
        ),
        (
            Answers {
                template: "bug-repro".into(),
                name: "x".into(),
                backends: [("nobody".to_string(), "claude".to_string())].into(),
                ..Answers::default()
            },
            "isn't a node",
        ),
        (
            Answers {
                template: "bug-repro".into(),
                name: "x".into(),
                backends: [("verifier".to_string(), "hal".to_string())].into(),
                ..Answers::default()
            },
            "isn't a backend",
        ),
        (
            Answers {
                template: "bug-repro".into(),
                name: "x y".into(),
                ..Answers::default()
            },
            "letters, digits",
        ),
    ] {
        assert!(wizard(&a, &m, &ctx).unwrap_err().contains(why));
    }
}
