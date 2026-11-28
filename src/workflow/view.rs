//! A run as anyone looks at it: the overview, `trix workflow ls`, the
//! summary. Every kind of run turns into this, triage cases included.

// Built ahead of the steps that use each part: this comes off once they do.
#![allow(dead_code)]

use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Waiting on a cron start, a trigger or another workflow.
    Scheduled,
    /// A node or check is working.
    Running,
    /// A gate with no approver node, an escalation, or an unmatched prompt.
    NeedsYou,
    /// A deadline, a limit with park, a stall, quota retries used up,
    /// branch rules, or a changed hooks fingerprint at the gate.
    Parked,
    Paused,
    /// done_when verified, and merged for gate runs.
    Succeeded,
    Failed,
    /// Stopped. Branches and worktrees are left as they are.
    Aborted,
}

impl RunState {
    pub fn label(self) -> &'static str {
        match self {
            RunState::Scheduled => "scheduled",
            RunState::Running => "running",
            RunState::NeedsYou => "needs you",
            RunState::Parked => "parked",
            RunState::Paused => "paused",
            RunState::Succeeded => "succeeded",
            RunState::Failed => "failed",
            RunState::Aborted => "aborted",
        }
    }
    pub fn over(self) -> bool {
        matches!(
            self,
            RunState::Succeeded | RunState::Failed | RunState::Aborted
        )
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Waiting,
    Working,
    /// Handed in, and what it handed in is being checked.
    InReview,
    /// Out of quota, or waiting for you.
    Blocked,
    Failed,
    Done,
}

impl NodeState {
    pub fn label(self) -> &'static str {
        match self {
            NodeState::Waiting => "waiting",
            NodeState::Working => "working",
            NodeState::InReview => "in review",
            NodeState::Blocked => "blocked",
            NodeState::Failed => "failed",
            NodeState::Done => "done",
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct NodeView {
    pub id: String,
    pub kind: String,
    pub state: NodeState,
    /// The backend, `check` or `trix`.
    pub backend: String,
    pub owns: Vec<String>,
    pub visits: u32,
    /// Its last word, in a few words: `ready · 4 files`, `quota · retry 10m`.
    pub last: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RunView {
    pub id: String,
    pub workflow: String,
    pub state: RunState,
    pub goal: String,
    pub nodes: Vec<NodeView>,
    pub cycles: u32,
    pub wasted: u32,
    /// What priced agents spent. Unpriced ones are unknown, never $0.
    pub spent_usd: f64,
    pub unpriced: bool,
    /// Why it needs you or is parked.
    pub why: String,
}

impl RunView {
    pub fn done(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.state == NodeState::Done)
            .count()
    }
    pub fn blocked(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.state == NodeState::Blocked)
            .count()
    }
}
