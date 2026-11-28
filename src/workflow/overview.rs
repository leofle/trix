//! The run overview: the goal, how far the run is, one row per node, and
//! a jump to the next item that needs you.

use super::engine::WfRun;
use super::env::Seen;
use super::view::{NodeState, RunState};

/// Something in a run that waits for you.
#[derive(Clone, Debug, PartialEq)]
pub struct Need {
    /// The node it's about, if it's about one.
    pub node: Option<String>,
    /// The pane to open for it: the run's own, or an agent's.
    pub pane: String,
    pub what: String,
}

/// Everything in the run that needs you now: a gate with no approver, an
/// escalation, a quota escalation, a parked run, and each agent sitting at
/// a prompt no rule answered.
pub fn needs(run: &WfRun, seen: &[Seen]) -> Vec<Need> {
    let mut out = Vec::new();
    if run.ask.is_some() {
        out.push(Need {
            node: run.about.clone(),
            pane: run.id.clone(),
            what: run.why.clone(),
        });
    } else if run.state == RunState::Parked {
        out.push(Need {
            node: None,
            pane: run.id.clone(),
            what: format!("parked: {}", run.why),
        });
    }
    for (id, st) in &run.nodes {
        let Some(pane) = &st.agent else { continue };
        if seen
            .iter()
            .any(|a| a.name == *pane && a.state == "needs you")
        {
            out.push(Need {
                node: Some(id.clone()),
                pane: pane.clone(),
                what: format!("{id} is at a prompt no rule answered"),
            });
        }
    }
    out
}

/// `n`: the item after this one, round and round.
pub fn next(needs: &[Need], at: Option<usize>) -> Option<usize> {
    if needs.is_empty() {
        return None;
    }
    Some(at.map_or(0, |i| (i + 1) % needs.len()))
}

/// The overview as text, `width` columns wide. `at`: the item `n` is on.
pub fn render(run: &WfRun, seen: &[Seen], at: Option<usize>, width: usize) -> String {
    let v = run.view(seen);
    let needs = needs(run, seen);
    let mode = if run.workflow.mode == super::model::Mode::Autopilot {
        "autopilot"
    } else {
        "supervised"
    };
    let spent = if v.unpriced {
        format!("{} + ?", crate::cost::usd(v.spent_usd))
    } else {
        crate::cost::usd(v.spent_usd)
    };
    let cut = |s: &str, n: usize| -> String {
        if s.chars().count() > n {
            format!(
                "{}…",
                s.chars().take(n.saturating_sub(1)).collect::<String>()
            )
        } else {
            s.to_string()
        }
    };
    let head = format!("run {} · {mode} · {} · goal: ", v.id, v.state.label());
    let mut s = format!(
        "{head}{}\n",
        cut(&v.goal, width.saturating_sub(head.chars().count()))
    );
    s.push_str(&format!(
        "{} of {} done · {} blocked · {} need you · cycles {} ({} wasted) · {spent}\n",
        v.done(),
        v.nodes.len(),
        v.blocked(),
        needs.len(),
        v.cycles,
        v.wasted
    ));
    let owns_w = if width >= 100 { 22 } else { 12 };
    s.push_str(&format!(
        "{:<14} {:<10} {:<10} {:<owns_w$} {}\n",
        "node", "state", "backend", "owns", "last"
    ));
    for n in &v.nodes {
        let owns = if n.owns.is_empty() {
            "–".to_string()
        } else {
            n.owns.join(",")
        };
        let state = match n.state {
            // What it handed in is with the nodes after it.
            NodeState::Done
                if run.active.iter().any(|a| a != &n.id)
                    && run.nodes[&n.id].verdict.is_some()
                    && n.kind != "check"
                    && run.workflow.nodes[&n.id].writes()
                    && run.pinned.is_none() =>
            {
                NodeState::InReview
            }
            st => st,
        };
        let mark = if needs
            .iter()
            .enumerate()
            .any(|(i, need)| need.node.as_ref() == Some(&n.id) && Some(i) == at)
        {
            "▶"
        } else {
            " "
        };
        s.push_str(&format!(
            "{mark}{:<13} {:<10} {:<10} {:<owns_w$} {}\n",
            cut(&n.id, 13),
            state.label(),
            cut(&n.backend, 10),
            cut(&owns, owns_w),
            cut(&n.last, width.saturating_sub(40 + owns_w))
        ));
    }
    if let Some(need) = at.and_then(|i| needs.get(i)) {
        let tail = format!(" (pane {})", need.pane);
        s.push_str(&format!(
            "needs you: {}{tail}\n",
            cut(&need.what, width.saturating_sub(11 + tail.chars().count()))
        ));
    }
    s.push_str(
        "n next needing you · enter open pane · l log · y approve · R revoke · p pause · a abort\n",
    );
    s
}
