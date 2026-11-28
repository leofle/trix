//! What a workflow is: inputs, a goal, limits, policy, approvals, nodes and
//! edges. The builder owns this as a TOML file; YAML is for import and
//! export only.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The version of the native format. A file from a newer trix is refused,
/// not guessed at.
pub const SCHEMA: u32 = 1;

/// Where a verdict can lead that isn't a node: the run succeeded or failed.
pub const DONE: &str = "@done";
pub const FAIL: &str = "@fail";

/// What a runtime can report about a node that isn't the node's own word.
pub const EVENTS: [&str; 3] = ["error", "timeout", "exited"];

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}
fn no(v: &bool) -> bool {
    !*v
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub schema: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Shipped in the binary: read-only, export it to fork your own.
    #[serde(default, skip_serializing_if = "no")]
    pub builtin: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, Input>,
    #[serde(default)]
    pub goal: Goal,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default, skip_serializing_if = "Approvals::is_empty")]
    pub approvals: Approvals,
    /// Profile name, then agent kind: the arguments that kind starts with.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    #[serde(default, skip_serializing_if = "Notify::is_empty")]
    pub notify: Notify,
    /// Where a run starts. Unset: the nodes no edge leads to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry: Vec<String>,
    pub nodes: BTreeMap<String, Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Where the builder drew each node: column, row.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layout: BTreeMap<String, [i32; 2]>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Gates and prompts come to you.
    #[default]
    Supervised,
    /// Runs unattended within its guardrails; parks when it's stuck.
    Autopilot,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "no")]
    pub required: bool,
    /// The resolver that fetches it before any prompt: `gh.issue`, …
    #[serde(default, skip_serializing_if = "none")]
    pub resolve: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "DoneWhen::is_empty")]
    pub done_when: DoneWhen,
}

/// When the goal is met, evaluated on one commit trix checked out itself.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct DoneWhen {
    /// Decided without an LLM: at least one is required.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mechanical: Vec<Cond>,
    /// An agent's vote: counts, but is never enough alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attested: Vec<Cond>,
}

impl DoneWhen {
    fn is_empty(&self) -> bool {
        self.mechanical.is_empty() && self.attested.is_empty()
    }
}

/// One condition: a check that passed, CI green, or a node's passing vote.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Cond {
    #[serde(default, skip_serializing_if = "none")]
    pub check: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub ci: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub judge: Option<String>,
}

/// A number, or a `{{config.…}}` reference bound when the run starts.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Count {
    N(u32),
    Expr(String),
}

impl Count {
    pub fn get(&self) -> Option<u32> {
        match self {
            Count::N(n) => Some(*n),
            Count::Expr(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Amount {
    Usd(f64),
    Expr(String),
}

impl Amount {
    pub fn get(&self) -> Option<f64> {
        match self {
            Amount::Usd(n) => Some(*n),
            Amount::Expr(_) => None,
        }
    }
}

/// A list of arguments, or a reference to one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Args {
    List(Vec<String>),
    Expr(String),
}

impl Args {
    pub fn list(&self) -> Vec<String> {
        match self {
            Args::List(v) => v.clone(),
            Args::Expr(_) => Vec::new(),
        }
    }
}

/// How many wasted cycles a run survives: a number, or `off`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum NoProgress {
    After(u32),
    Word(String),
}

impl NoProgress {
    /// None: off.
    pub fn after(&self) -> Option<u32> {
        match self {
            NoProgress::After(n) => Some(*n),
            NoProgress::Word(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Mandatory. Counts only time with a node or check running: `3h`.
    #[serde(default, skip_serializing_if = "none")]
    pub wall_clock: Option<String>,
    /// A ceiling, never a target. 1 when no edge leads back.
    #[serde(default, skip_serializing_if = "none")]
    pub max_iterations: Option<Count>,
    #[serde(default, skip_serializing_if = "none")]
    pub no_progress: Option<NoProgress>,
    /// Priced kinds only: an unpriced agent's spend is unknown, never $0.
    #[serde(default, skip_serializing_if = "none")]
    pub budget_usd: Option<Amount>,
    #[serde(default, skip_serializing_if = "none")]
    pub on_limit: Option<OnLimit>,
    #[serde(default, skip_serializing_if = "none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "none")]
    pub flaky_check_reruns: Option<u32>,
    #[serde(default, skip_serializing_if = "none")]
    pub on_stall: Option<OnStall>,
    /// A time of day after which no new work starts, and the run parks.
    #[serde(default, skip_serializing_if = "none")]
    pub deadline: Option<String>,
    /// A time of day at which the run is aborted.
    #[serde(default, skip_serializing_if = "none")]
    pub kill_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnLimit {
    Ask,
    Park,
    Fail,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnStall {
    Escalate,
    Ask,
    Fail,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub push: Push,
    #[serde(default, skip_serializing_if = "none")]
    pub open_pr: Option<String>,
    #[serde(default)]
    pub merge: Merge,
    /// Floors this workflow adds. It can never remove one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floors: Vec<String>,
}

/// Who may push. What trix can back up differs by backend and profile:
/// the checker labels each node enforced, guarded or none.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Push {
    /// Only trix, in the gate. Every agent node must be enforced.
    GateOnly,
    /// Every agent pane and check gets the guard: an accidental push
    /// fails, a determined one doesn't.
    #[default]
    Guarded,
    /// Agents push themselves, with no guard.
    Allow,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Merge {
    /// You approve the merge.
    #[default]
    You,
    /// You, or the approver node named in `approvals.merge`.
    Approver,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Approvals {
    #[serde(default, skip_serializing_if = "none")]
    pub merge: Option<MergeApproval>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermRule>,
}

impl Approvals {
    pub fn is_empty(&self) -> bool {
        self.merge.is_none() && self.permissions.is_empty()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Allow {
    Allow,
    #[default]
    Ask,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct MergeApproval {
    #[serde(default)]
    pub mode: Allow,
    /// A node of this run that didn't write the change and can hand in.
    #[serde(default, skip_serializing_if = "none")]
    pub approver: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require: Vec<String>,
}

/// A permission prompt a rule may answer, through a decision hook tied to
/// that one request. Never an agent, never typed keys.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct PermRule {
    /// A node, or `*`.
    pub node: String,
    #[serde(default)]
    pub mode: Allow,
    /// `rule`. Anything else is an error: agents approve merges only.
    #[serde(default, skip_serializing_if = "none")]
    pub approver: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
    #[serde(default, skip_serializing_if = "no")]
    pub summary: bool,
}

impl Notify {
    fn is_empty(&self) -> bool {
        self.on.is_empty() && !self.summary
    }
}

/// What a node is there to do. The checker holds each role's needs against
/// what the node's backend and profile provide.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Implementer,
    Reviewer,
    Verifier,
    Judge,
    Gatekeeper,
    Supervisor,
    Worker,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Implementer => "implementer",
            Role::Reviewer => "reviewer",
            Role::Verifier => "verifier",
            Role::Judge => "judge",
            Role::Gatekeeper => "gatekeeper",
            Role::Supervisor => "supervisor",
            Role::Worker => "worker",
        }
    }
    /// It reads and reports; it never writes the change.
    pub fn read_only(self) -> bool {
        matches!(
            self,
            Role::Reviewer | Role::Verifier | Role::Judge | Role::Gatekeeper | Role::Supervisor
        )
    }
}

/// What an agent node owns, what it must leave alone, and what done means.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Brief {
    /// Its write scope, as paths or globs. Empty for a read-only role.
    #[serde(default)]
    pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_not_touch: Vec<String>,
    #[serde(default)]
    pub done: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Revisit {
    /// Keeps its pane between visits, and so its context.
    Reuse,
    /// Its pane closes once its report is in; the next visit starts anew.
    Fresh,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    /// The run's worktree, one writer after another.
    Shared,
    /// A worktree of its own, landed by a land step.
    Isolated,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct WorktreeSpec {
    #[serde(default, skip_serializing_if = "none")]
    pub branch: Option<String>,
    /// The commit to check out: `tests.commit`.
    #[serde(default, skip_serializing_if = "none")]
    pub checkout: Option<String>,
    #[serde(default, skip_serializing_if = "no")]
    pub detached: bool,
    #[serde(default, skip_serializing_if = "none")]
    pub setup: Option<String>,
}

/// What a node does when its backend is out of quota.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct OnQuota {
    /// Seconds before the first retry, doubling after: 300 by default.
    #[serde(default, skip_serializing_if = "none")]
    pub backoff_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "none")]
    pub tries: Option<u32>,
    /// Another backend to go on with, checked like the first.
    #[serde(default, skip_serializing_if = "none")]
    pub fallback: Option<String>,
}

/// A second agent for a node that runs two side by side (triage's review).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Second {
    pub agent: String,
    #[serde(default, skip_serializing_if = "none")]
    pub args: Option<Args>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// `agent`, `check`, `judge`, `supervisor`, `land`, `workflow`,
    /// `for_each`, `join`, or a built-in: `gh.gate`, `triage.build`, …
    pub kind: String,
    #[serde(default, skip_serializing_if = "none")]
    pub role: Option<Role>,
    /// `verifier`: fresh context, read-only, failures back to the author.
    #[serde(default, skip_serializing_if = "none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub brief: Option<Brief>,
    /// The backend: `claude`, `codex`, `gemini`, …
    #[serde(default, skip_serializing_if = "none")]
    pub agent: Option<String>,
    /// Extra arguments, on top of the profile's.
    #[serde(default, skip_serializing_if = "none")]
    pub args: Option<Args>,
    #[serde(default, skip_serializing_if = "none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub workspace: Option<Workspace>,
    #[serde(default, skip_serializing_if = "none")]
    pub worktree: Option<WorktreeSpec>,
    #[serde(default, skip_serializing_if = "none")]
    pub revisit: Option<Revisit>,
    #[serde(default, skip_serializing_if = "none")]
    pub max_visits: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdicts: Vec<String>,
    /// Named fields of its report, and their types.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub output: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    /// A check's command, as argv.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run: Vec<String>,
    /// The commit a check runs on: `implementer.head`.
    #[serde(default, skip_serializing_if = "none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub on_quota: Option<OnQuota>,
    /// How many agents work this node side by side (triage's review).
    #[serde(default, skip_serializing_if = "none")]
    pub reviewers: Option<Count>,
    #[serde(default, skip_serializing_if = "none")]
    pub second: Option<Second>,
    /// A gate's merge method: `squash`.
    #[serde(default, skip_serializing_if = "none")]
    pub merge: Option<String>,
    /// A workflow node's workflow, or a for_each's body.
    #[serde(default, skip_serializing_if = "none")]
    pub workflow: Option<String>,
    /// What a for_each fans out over: a node's list output.
    #[serde(default, skip_serializing_if = "none")]
    pub over: Option<String>,
    /// At most this many of a for_each at once.
    #[serde(default, skip_serializing_if = "none")]
    pub parallel: Option<u32>,
    /// A join's rule: `all`, `any`, a number, or `both_or_ask`.
    #[serde(default, skip_serializing_if = "none")]
    pub wait: Option<String>,
    /// A land step's answer to a conflict: `author` or `fail`.
    #[serde(default, skip_serializing_if = "none")]
    pub on_conflict: Option<String>,
}

/// The kinds that run an agent in a pane: they need a brief and can hand in.
pub const AGENT_KINDS: [&str; 7] = [
    "agent",
    "judge",
    "supervisor",
    "triage.build",
    "triage.review",
    "triage.fix",
    "triage.gatekeep",
];

/// Built-in kinds that keep their own prompts, reports and checks.
pub const BUILTIN_KINDS: [&str; 7] = [
    "triage.build",
    "triage.review",
    "triage.fix",
    "triage.gatekeep",
    "gh.resolve_target",
    "gh.gate",
    "gh.merge",
];

impl Node {
    /// It runs an agent in a pane: an agent kind, or a fan-out of agents.
    pub fn is_agent(&self) -> bool {
        AGENT_KINDS.contains(&self.kind.as_str())
            || (self.kind == "for_each" && self.workflow.is_none())
    }
    pub fn is_builtin(&self) -> bool {
        BUILTIN_KINDS.contains(&self.kind.as_str())
    }
    /// The role it plays: named, or what its kind and preset imply.
    pub fn role(&self) -> Option<Role> {
        if let Some(r) = self.role {
            return Some(r);
        }
        match (self.kind.as_str(), self.preset.as_deref()) {
            (_, Some("verifier")) => Some(Role::Verifier),
            ("judge", _) => Some(Role::Judge),
            ("supervisor", _) => Some(Role::Supervisor),
            ("triage.build" | "triage.fix", _) => Some(Role::Implementer),
            ("triage.review", _) => Some(Role::Verifier),
            ("triage.gatekeep", _) => Some(Role::Gatekeeper),
            ("agent", _) => Some(match &self.brief {
                Some(b) if b.owns.is_empty() => Role::Reviewer,
                _ => Role::Implementer,
            }),
            _ => None,
        }
    }
    /// It writes to the repository: an agent with a write scope.
    pub fn writes(&self) -> bool {
        self.is_agent() && self.brief.as_ref().is_some_and(|b| !b.owns.is_empty())
    }
    /// A verifier, judge or supervisor starts fresh unless it says
    /// otherwise; a writer keeps its pane.
    pub fn revisit(&self) -> Revisit {
        self.revisit.unwrap_or(match self.role() {
            Some(Role::Implementer | Role::Worker) => Revisit::Reuse,
            _ => Revisit::Fresh,
        })
    }
}

/// One trigger, or several that lead the same way.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Triggers {
    One(String),
    Many(Vec<String>),
}

impl Triggers {
    pub fn list(&self) -> Vec<&str> {
        match self {
            Triggers::One(s) => vec![s.as_str()],
            Triggers::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// A node, or `*` for every node.
    pub from: String,
    /// A node, `@done` or `@fail`.
    pub to: String,
    /// A verdict the source declares, or a runtime event.
    pub on: Triggers,
    #[serde(default, skip_serializing_if = "none")]
    pub guard: Option<String>,
    /// What the target finds in its inbox: `inbox = "{{tests.log_tail}}"`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pass: BTreeMap<String, String>,
    /// The question a built-in asks you before it takes this edge.
    #[serde(default, skip_serializing_if = "none")]
    pub ask: Option<String>,
}

impl Workflow {
    /// A workflow from its native TOML. A file from a newer format is
    /// refused, and so is a key this trix doesn't know.
    pub fn from_toml(text: &str) -> Result<Workflow, String> {
        let schema = toml::from_str::<toml::Table>(text)
            .map_err(|e| e.to_string())?
            .get("schema")
            .and_then(toml::Value::as_integer)
            .ok_or("no schema version: a workflow file starts with schema = 1")?;
        if schema != SCHEMA as i64 {
            return Err(format!(
                "schema {schema} isn't one this trix reads (it reads {SCHEMA}): {}",
                if schema > SCHEMA as i64 {
                    "it was written by a newer trix"
                } else {
                    "it isn't a version there ever was"
                }
            ));
        }
        let w: Workflow = toml::from_str(text).map_err(|e| e.to_string())?;
        if w.name.trim().is_empty()
            || !w
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("name '{}': letters, digits, - and _ only", w.name));
        }
        Ok(w)
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| e.to_string())
    }

    /// Where `from` leads on `trigger`: the first edge that names it, the
    /// node's own before a `*` one.
    pub fn follow(&self, from: &str, trigger: &str) -> Option<&Edge> {
        let on = |e: &&Edge| e.on.list().contains(&trigger);
        self.edges
            .iter()
            .filter(|e| e.from == from)
            .find(on)
            .or_else(|| self.edges.iter().filter(|e| e.from == "*").find(on))
    }

    /// The edges that leave `from`, `*` ones included.
    pub fn out(&self, from: &str) -> impl Iterator<Item = &Edge> {
        self.edges
            .iter()
            .filter(move |e| e.from == from || e.from == "*")
    }

    /// The first node of a kind, with its id.
    pub fn of_kind(&self, kind: &str) -> Option<(&str, &Node)> {
        self.nodes
            .iter()
            .find(|(_, n)| n.kind == kind)
            .map(|(id, n)| (id.as_str(), n))
    }

    /// Where a run starts: the nodes named, or those no edge leads to.
    pub fn entries(&self) -> Vec<&str> {
        if !self.entry.is_empty() {
            return self.entry.iter().map(String::as_str).collect();
        }
        self.nodes
            .keys()
            .map(String::as_str)
            .filter(|id| !self.edges.iter().any(|e| e.to == *id && e.from != *id))
            .collect()
    }

    /// Every `{{config.a.b}}` that is a whole value, replaced by what
    /// `config` has there: a number stays a number, a list a list.
    pub fn bind(&self, config: &serde_json::Value) -> Result<Workflow, String> {
        let mut v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        bind_value(&mut v, config)?;
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    /// A hash of everything a run would act on: what trust is pinned to.
    pub fn content_hash(&self) -> String {
        let text = serde_json::to_string(self).unwrap_or_default();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}

fn bind_value(v: &mut serde_json::Value, config: &serde_json::Value) -> Result<(), String> {
    match v {
        serde_json::Value::String(s) => {
            if let Some(path) = s
                .strip_prefix("{{config.")
                .and_then(|r| r.strip_suffix("}}"))
                .filter(|p| !p.contains("{{"))
            {
                let mut at = config;
                for key in path.trim().split('.') {
                    at = at
                        .get(key)
                        .ok_or_else(|| format!("{{{{config.{path}}}}}: there's no such setting"))?;
                }
                *v = at.clone();
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                bind_value(x, config)?;
            }
        }
        serde_json::Value::Object(o) => {
            for (_, x) in o.iter_mut() {
                bind_value(x, config)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `3h`, `90m`, `45s`, `1h30m`: seconds.
pub fn duration(text: &str) -> Result<u64, String> {
    let bad = || format!("'{text}' isn't a length of time: 3h, 90m, 45s, 1h30m");
    let (mut total, mut digits) = (0u64, String::new());
    for c in text.trim().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: u64 = digits.parse().map_err(|_| bad())?;
        digits.clear();
        total += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            'd' => 86400,
            _ => return Err(bad()),
        };
    }
    if !digits.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(total)
}
