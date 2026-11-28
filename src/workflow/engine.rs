//! A workflow run: the generic nodes (agent, judge, supervisor, check,
//! gate) as the runner drives them. Deterministic: what's next is the
//! verdict a node handed in and the workflow's edges, never an LLM's say.
//!
//! A run is a folder, `<state>/runs/<id>/`: `run.json` (this state),
//! `workflow.toml` (the snapshot taken at the start), `reports/` (what
//! agents handed in, checked by the server first), `asks/` (their
//! questions) and `approvals.jsonl`.

use super::check::{self, in_scope};
use super::env::{AgentSpec, Env, Seen};
use super::model::{
    Allow, Count, DONE, EVENTS, FAIL, Merge, Mode, Node, OnLimit, OnStall, Push, Revisit, Workflow,
    duration,
};
use super::prompt::{self, Vars};
use super::runner::Run;
use super::view::{NodeState, NodeView, RunState, RunView};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The version of `run.json`. A run written by a newer trix is refused.
pub const STATE: u32 = 1;

/// How long an agent must stay stopped before it counts, by how trix
/// learns its state: hooks say it at once, a screen needs longer.
fn settle_secs(state_from: &str) -> u64 {
    if state_from.contains("hooks") || state_from.contains("plugin") {
        15
    } else {
        45
    }
}

/// How long an agent given work may sit idle before it counts as stopped,
/// if it's never seen working.
const TAKE_UP_SECS: u64 = 120;

/// The most a look counts toward the wall clock: a runner that was down
/// for an hour didn't work for an hour.
const LOOK_SECS: u64 = 30;

/// Quota: the first wait, how often it's tried, and the longest it waits
/// in all before it escalates.
const QUOTA_BACKOFF: u64 = 300;
const QUOTA_TRIES: u32 = 6;
const QUOTA_MAX: u64 = 2 * 3600;

/// What a backend prints when it's out of quota or rate-limited, lowercase.
const QUOTA_SCREENS: [&str; 9] = [
    "usage limit",
    "rate limit",
    "rate-limit",
    "quota exceeded",
    "exceeded your current quota",
    "too many requests",
    "limit reached",
    "out of credits",
    "resource_exhausted",
];

/// Whether a screen's last lines say the backend is out of quota.
pub fn quota_screen(screen: &str) -> bool {
    let tail: String = screen
        .lines()
        .rev()
        .take(15)
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    QUOTA_SCREENS.iter().any(|p| tail.contains(p))
}

/// What a run asks you, and the keys that answer it (a aborts, always).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    /// Everything passed: y merges.
    Merge,
    /// An input has text from outside the project: s lets the agents read it.
    Outsider,
    /// An agent stopped without its report: n nudges it again.
    NoReport,
    /// A limit was hit (`on_limit: ask`): c raises it and goes on.
    Limit,
    /// Nothing is pending and the goal isn't met: c looks again.
    Stall,
    /// An agent asked what only you can answer: c tells it to use its judgment.
    Question,
    /// Quota retries are used up: r tries again now.
    Quota,
    /// A floor was hit: y lets it through, for this run only.
    Floor,
    /// A node couldn't start: r tries again.
    Setup,
    /// A join wanted every part and got some: c goes on with those.
    Partial,
    /// An agent pane started the run, and `[workflows.agents] run` is ask: y starts it.
    Launch,
}

impl Ask {
    pub fn keys(self) -> &'static [(char, &'static str)] {
        match self {
            Ask::Merge => &[('y', "merge"), ('a', "abort")],
            Ask::Outsider => &[('s', "go on: the agents may read it"), ('a', "abort")],
            Ask::NoReport => &[('n', "nudge it again"), ('a', "abort")],
            Ask::Limit => &[('c', "raise the limit and go on"), ('a', "abort")],
            Ask::Stall => &[('c', "look again"), ('a', "abort")],
            Ask::Question => &[('c', "tell it to use its judgment"), ('a', "abort")],
            Ask::Quota => &[('r', "try again now"), ('a', "abort")],
            Ask::Floor => &[('y', "let it through, for this run only"), ('a', "abort")],
            Ask::Setup => &[('r', "try again"), ('a', "abort")],
            Ask::Partial => &[('c', "go on with what succeeded"), ('a', "abort")],
            Ask::Launch => &[('y', "start it"), ('a', "abort")],
        }
    }
}

/// A working agent as the run watches it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Watch {
    #[serde(default)]
    pub given: u64,
    #[serde(default)]
    pub worked: bool,
    #[serde(default)]
    pub stopped_at: Option<u64>,
    #[serde(default)]
    pub nudged: bool,
}

/// A node that's out of quota: visibly blocked, retried with backoff.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Quota {
    pub since: u64,
    pub tries: u32,
    pub next_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NodeRun {
    pub state: NodeState,
    #[serde(default)]
    pub visits: u32,
    /// Its pane, while it has one.
    #[serde(default)]
    pub agent: Option<String>,
    /// What its next visit starts with: what an edge passed along.
    #[serde(default)]
    pub inbox: Vec<String>,
    /// Mentions queued for it: delivered with the inbox on its next visit
    /// along a declared edge. A mention never wakes it.
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub verdict: Option<String>,
    /// What it last handed in, by field: `summary`, `findings`, `log_tail`,
    /// `commit`, `head`, …
    #[serde(default)]
    pub outputs: BTreeMap<String, serde_json::Value>,
    /// The run branch's head when this visit began: what it changed since
    /// is its own.
    #[serde(default)]
    pub start: Option<String>,
    /// What the last visit left, to tell a wasted cycle: the head it left,
    /// or its failure.
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub watch: Watch,
    #[serde(default)]
    pub quota: Option<Quota>,
    /// How often it was started again after exiting.
    #[serde(default)]
    pub restarts: u32,
    /// Where it works.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// The backend it runs on now: its own, or its quota fallback.
    #[serde(default)]
    pub backend: Option<String>,
    /// Its hand-in was sent back for scope: the next visit is held against
    /// the same starting point, so undoing the change clears it.
    #[serde(default)]
    pub sent_back: bool,
    /// A writer in a worktree of its own: its commits are on the run's branch.
    #[serde(default)]
    pub landed: bool,
    /// The fan-out this is one item of.
    #[serde(default)]
    pub parent: Option<String>,
}

impl Default for NodeRun {
    fn default() -> NodeRun {
        NodeRun {
            state: NodeState::Waiting,
            visits: 0,
            agent: None,
            inbox: Vec::new(),
            mentions: Vec::new(),
            verdict: None,
            outputs: BTreeMap::new(),
            start: None,
            last: None,
            watch: Watch::default(),
            quota: None,
            restarts: 0,
            cwd: None,
            backend: None,
            sent_back: false,
            landed: false,
            parent: None,
        }
    }
}

/// The gate's own progress.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Gate {
    #[serde(default)]
    pub pushed: Option<String>,
    #[serde(default)]
    pub pr: Option<u64>,
    #[serde(default)]
    pub url: String,
    /// Who approved the merge: `you`, or the approver node.
    #[serde(default)]
    pub approved: Option<String>,
    /// Checks were on the base when the run began: a commit with none yet
    /// is waiting for them, not a repository without CI.
    #[serde(default)]
    pub ci_expected: bool,
    /// What you let through a floor for, this run only.
    #[serde(default)]
    pub let_through: Vec<String>,
}

/// What your config said when the run began: a run keeps to it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Opts {
    /// Approver nodes and rule approvals are on: the workflow is in
    /// `[approvals] allow`, and it's trusted as it is.
    #[serde(default)]
    pub approvals: bool,
    #[serde(default)]
    pub ci_paths: Vec<String>,
    #[serde(default)]
    pub floors: Vec<String>,
    #[serde(default)]
    pub notify_each: bool,
    #[serde(default)]
    pub on_finish: Vec<String>,
    /// `owner/repo`, to hold an approval's `repos` against.
    #[serde(default)]
    pub repo_name: String,
    /// Where each backend's state comes from, by kind: how long it settles.
    #[serde(default)]
    pub state_from: BTreeMap<String, String>,
    /// The kinds trix can price.
    #[serde(default)]
    pub priced: Vec<String>,
    /// How deep a run started by a run may go.
    #[serde(default)]
    pub max_depth: u32,
    /// The kinds whose hook decision is verified for the installed
    /// version: only there may a rule answer a permission prompt.
    #[serde(default)]
    pub decision: Vec<String>,
}

/// A question an agent asked, waiting for the supervisor or for you.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Question {
    pub from: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WfRun {
    pub v: u32,
    pub id: String,
    /// The main checkout.
    pub repo: PathBuf,
    pub base: String,
    /// The run's branch: `trix/wf-<id>`.
    pub branch: String,
    /// The workflow as it was when the run began.
    pub workflow: Workflow,
    #[serde(default)]
    pub inputs: BTreeMap<String, serde_json::Value>,
    pub state: RunState,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub ask: Option<Ask>,
    /// The node the question is about.
    #[serde(default)]
    pub about: Option<String>,
    pub nodes: BTreeMap<String, NodeRun>,
    /// The nodes being visited now.
    #[serde(default)]
    pub active: Vec<String>,
    /// Cycles are cost, never progress: 1 for a run that never went back.
    pub cycles: u32,
    #[serde(default)]
    pub wasted: u32,
    /// The commit trix checked out and ran the checks on: the only one
    /// that's judged, pushed or merged.
    #[serde(default)]
    pub pinned: Option<String>,
    #[serde(default)]
    pub closing: Vec<String>,
    #[serde(default)]
    pub spent_closed: f64,
    /// Seconds with a node or check running: what `wall_clock` limits.
    #[serde(default)]
    pub inflight: u64,
    #[serde(default)]
    pub looked: u64,
    /// The limits as they stand: you can raise them at a `Limit` ask.
    pub wall_clock: u64,
    pub ceiling: u32,
    #[serde(default)]
    pub budget: Option<f64>,
    /// The limit last hit, for c to raise.
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub gate: Gate,
    #[serde(default)]
    pub questions: Vec<Question>,
    #[serde(default)]
    pub opts: Opts,
    /// You said the agents may read an input written from outside.
    #[serde(default)]
    pub trusted_input: bool,
    /// Approvals were revoked mid-run: every one is back to ask.
    #[serde(default)]
    pub revoked: bool,
    /// The run that started this one, and how deep that goes.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub depth: u32,
    /// The agent pane that started it, if one did.
    #[serde(default)]
    pub started_by: Option<String>,
    /// The hooks and the git settings that run commands, as they were when
    /// the run began (`policy::fingerprint`).
    #[serde(default)]
    pub fingerprint: Option<BTreeMap<String, String>>,
    /// The blackboard: what nodes wrote for the rest of the run to read.
    #[serde(default)]
    pub board: BTreeMap<String, serde_json::Value>,
    /// The writers that work in worktrees of their own, in the order they
    /// handed in: the order they land in.
    #[serde(default)]
    pub finished: Vec<String>,
    #[serde(default)]
    pub log: Vec<(u64, String)>,
    pub created: u64,
}

pub fn dir_of(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

pub fn load(root: &Path, id: &str) -> Result<WfRun, String> {
    let p = dir_of(root, id).join("run.json");
    let b = std::fs::read(&p).map_err(|_| format!("no run named '{id}' (trix workflow ls)"))?;
    let v: serde_json::Value =
        serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", p.display()))?;
    if v["v"].as_u64() != Some(STATE as u64) {
        return Err(format!(
            "{}: written by another trix (state version {}, this one reads {STATE})",
            p.display(),
            v["v"]
        ));
    }
    serde_json::from_value(v).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn store(root: &Path, r: &WfRun) -> Result<(), String> {
    let d = dir_of(root, &r.id);
    std::fs::create_dir_all(d.join("reports")).map_err(|e| format!("{}: {e}", d.display()))?;
    super::store::write_atomic(
        &d.join("run.json"),
        &serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?,
    )
}

pub fn all(root: &Path) -> Vec<WfRun> {
    let mut v: Vec<WfRun> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| load(root, &e.file_name().to_string_lossy()).ok())
        .collect();
    v.sort_by_key(|r| std::cmp::Reverse(r.created));
    v
}

/// A free id for a run of `name`: `issue-to-pr-42` for an issue, else
/// `nightly-deps-1`, `-2`, …
pub fn new_id(root: &Path, name: &str, hint: Option<&str>) -> String {
    let base = match hint {
        Some(h) => format!("{name}-{h}"),
        None => name.to_string(),
    };
    for i in 1.. {
        let id = match (hint, i) {
            (Some(_), 1) => base.clone(),
            _ => format!("{base}-{i}"),
        };
        if !dir_of(root, &id).exists() {
            return id;
        }
    }
    unreachable!()
}

fn report_file(root: &Path, id: &str, node: &str) -> PathBuf {
    dir_of(root, id)
        .join("reports")
        .join(format!("{node}.json"))
}

pub(super) fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

pub(super) fn first_line(e: &str) -> &str {
    e.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

pub(super) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

impl WfRun {
    /// A run of `w`, not started: every node waits, the entries are next.
    pub fn new(
        id: &str,
        w: Workflow,
        repo: PathBuf,
        base: &str,
        inputs: BTreeMap<String, serde_json::Value>,
        opts: Opts,
        now: u64,
    ) -> WfRun {
        let nodes = w
            .nodes
            .keys()
            .map(|k| (k.clone(), NodeRun::default()))
            .collect();
        let active = w.entries().into_iter().map(String::from).collect();
        WfRun {
            v: STATE,
            id: id.into(),
            repo,
            base: base.into(),
            branch: format!("trix/wf-{id}"),
            wall_clock: w
                .limits
                .wall_clock
                .as_deref()
                .and_then(|t| duration(t).ok())
                .unwrap_or(3 * 3600),
            // A ceiling, never a target: 1 unless the workflow sets one. It
            // bounds every way work goes back: an edge, a scope send-back, a
            // land conflict.
            ceiling: w
                .limits
                .max_iterations
                .as_ref()
                .and_then(Count::get)
                .unwrap_or(1)
                .max(1),
            budget: w.limits.budget_usd.as_ref().and_then(|b| b.get()),
            workflow: w,
            inputs,
            state: RunState::Running,
            why: String::new(),
            ask: None,
            about: None,
            nodes,
            active,
            cycles: 1,
            wasted: 0,
            pinned: None,
            closing: Vec::new(),
            spent_closed: 0.0,
            inflight: 0,
            looked: now,
            limit: None,
            gate: Gate::default(),
            questions: Vec::new(),
            opts,
            trusted_input: false,
            revoked: false,
            parent: None,
            depth: 0,
            started_by: None,
            fingerprint: None,
            board: BTreeMap::new(),
            finished: Vec::new(),
            log: Vec::new(),
            created: now,
        }
    }

    fn autopilot(&self) -> bool {
        self.workflow.mode == Mode::Autopilot
    }

    /// The pane a node's visit runs in. A node that keeps its pane has one
    /// name; one that starts fresh has one per visit, so the pane that's
    /// closing and the one that's starting never share it.
    pub fn pane(&self, id: &str) -> String {
        let n = &self.workflow.nodes[id];
        let visits = self.nodes[id].visits.max(1);
        match n.revisit() {
            Revisit::Fresh if visits > 1 => format!("{}-{id}-{visits}", self.id),
            _ => format!("{}-{id}", self.id),
        }
    }

    /// The run's worktree, where its writers work one after another.
    pub fn tree(&self) -> PathBuf {
        self.repo.join(".trix/worktrees").join(&self.id)
    }

    pub(super) fn note(&mut self, env: &mut dyn Env, text: impl Into<String>) {
        let text = text.into();
        let at = env.now();
        let (_, _, secs) = crate::ledger::local(at);
        env.print(&format!("{:02}:{:02}  {text}", secs / 3600, secs / 60 % 60));
        self.log.push((at, text));
    }

    pub(super) fn ask(
        &mut self,
        env: &mut dyn Env,
        a: Ask,
        about: Option<&str>,
        why: impl Into<String>,
    ) {
        self.ask = Some(a);
        self.about = about.map(String::from);
        self.why = why.into();
        self.state = RunState::NeedsYou;
        let keys: Vec<String> = a.keys().iter().map(|(k, w)| format!("{k}: {w}")).collect();
        self.note(
            env,
            format!("needs you: {}  [{}]", self.why, keys.join(" · ")),
        );
        env.say("Question", &format!("{} · {}", self.id, self.why));
    }

    /// Stop here until you resume it: nothing is lost, nothing runs.
    pub(super) fn park(&mut self, env: &mut dyn Env, why: impl Into<String>) {
        self.why = why.into();
        self.state = RunState::Parked;
        self.note(
            env,
            format!("parked: {} (trix workflow resume {})", self.why, self.id),
        );
        env.say("Question", &format!("{} · parked: {}", self.id, self.why));
        env.notify(&format!("{} is parked", self.id), &self.why.clone());
    }

    pub(super) fn fail(&mut self, env: &mut dyn Env, why: impl Into<String>) {
        self.why = why.into();
        self.state = RunState::Failed;
        self.ask = None;
        self.note(env, format!("failed: {}", self.why));
        self.close_all(env, false);
        env.say("Finished", &format!("{} · failed: {}", self.id, self.why));
        env.notify(&format!("{} failed", self.id), &self.why.clone());
    }

    pub(super) fn succeed(&mut self, env: &mut dyn Env, what: impl Into<String>) {
        self.state = RunState::Succeeded;
        self.ask = None;
        self.why.clear();
        self.active.clear();
        let what = what.into();
        self.note(env, what.clone());
        self.close_all(env, true);
        self.remove_checkouts(env);
        env.say("Finished", &format!("{} · {what}", self.id));
        env.notify(&format!("{} succeeded", self.id), &what);
    }

    /// A limit was hit: ask, park or fail, as the workflow says. Autopilot
    /// parks: a human is needed only when a run is really stuck.
    pub(super) fn limit(&mut self, env: &mut dyn Env, which: &str, what: String) {
        self.limit = Some(which.into());
        let on = self
            .workflow
            .limits
            .on_limit
            .unwrap_or(if self.autopilot() {
                OnLimit::Park
            } else {
                OnLimit::Ask
            });
        match on {
            OnLimit::Ask => self.ask(env, Ask::Limit, None, what),
            OnLimit::Park => self.park(env, what),
            OnLimit::Fail => self.fail(env, what),
        }
    }

    pub(super) fn close_all(&mut self, env: &mut dyn Env, clean: bool) {
        let prefix = format!("{}-", self.id);
        let seen = env.agents().unwrap_or_default();
        for a in seen.iter().filter(|a| a.name.starts_with(&prefix)) {
            if env.close(&a.name, clean).is_ok() {
                self.spent_closed += a.cost;
            }
        }
        self.closing.clear();
        for n in self.nodes.values_mut() {
            n.agent = None;
        }
    }

    /// What the run leaves behind once it's done: the detached checkouts
    /// readers used (nothing in them is kept) and the worktrees of writers
    /// whose commits have landed. Removed without force, so one that
    /// changed stays; a writer's worktree that hasn't landed is never removed.
    fn remove_checkouts(&mut self, env: &mut dyn Env) {
        let own: Vec<(String, PathBuf)> = self
            .nodes
            .iter()
            .filter_map(|(id, n)| Some((id.clone(), n.cwd.clone()?)))
            .filter(|(_, p)| *p != self.tree() && p.is_dir())
            .collect();
        for (id, p) in own {
            let removed = if self.workflow.nodes[&id].writes() {
                super::land::remove_tree(self, env, &id)
            } else {
                super::land::unlock_ours(self, env, &p);
                env.git(
                    &self.repo,
                    &["worktree", "remove", &p.display().to_string()],
                )
                .map(|_| ())
            };
            if let Err(e) = removed {
                self.note(
                    env,
                    format!("kept {}: {}", crate::app::tilde(&p), first_line(&e)),
                );
            }
        }
    }

    /// The run's worktree, made if it isn't there: on its own branch, from
    /// the base as origin has it.
    pub(super) fn ensure_tree(&mut self, env: &mut dyn Env) -> Result<PathBuf, String> {
        let path = self.tree();
        if path.is_dir() {
            return Ok(path);
        }
        let b = self.base.clone();
        let fetched = env.git(
            &self.repo,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+refs/heads/{b}:refs/remotes/origin/{b}"),
            ],
        );
        let start = if fetched.is_ok() {
            format!("refs/remotes/origin/{b}")
        } else {
            b
        };
        env.git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                &self.branch,
                &path.display().to_string(),
                &start,
            ],
        )?;
        Ok(path)
    }

    /// The run branch's head, as trix reads it itself.
    pub(super) fn head(&mut self, env: &mut dyn Env) -> Result<String, String> {
        let tree = self.ensure_tree(env)?;
        Ok(env.git(&tree, &["rev-parse", "HEAD"])?.trim().to_string())
    }

    /// A clean, detached checkout of `sha` for a reader or a check.
    pub(super) fn checkout(
        &mut self,
        env: &mut dyn Env,
        name: &str,
        sha: &str,
    ) -> Result<PathBuf, String> {
        let path = self.repo.join(".trix/worktrees").join(name);
        if !path.is_dir() {
            env.git(
                &self.repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "--detach",
                    &path.display().to_string(),
                    sha,
                ],
            )?;
        }
        Ok(path)
    }

    /// What a template can name, for a node's prompt or an edge's payload.
    pub fn vars(&self, id: &str) -> Vars {
        let mut v = Vars::new();
        let text = |x: &serde_json::Value| match x {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        v.insert("run.id".into(), self.id.clone());
        v.insert("run.state".into(), self.state.label().into());
        v.insert("node".into(), id.into());
        v.insert("repo".into(), self.repo.display().to_string());
        v.insert("worktree".into(), self.tree().display().to_string());
        for (name, value) in &self.inputs {
            v.insert(format!("inputs.{name}"), text(value));
            for (k, x) in value.as_object().into_iter().flatten() {
                v.insert(format!("inputs.{name}.{k}"), text(x));
            }
        }
        for (node, st) in &self.nodes {
            for (k, x) in &st.outputs {
                v.insert(format!("{node}.{k}"), text(x));
            }
            if let Some(verdict) = &st.verdict {
                v.insert(format!("{node}.verdict"), verdict.clone());
            }
        }
        for (k, x) in &self.board {
            v.insert(format!("board.{k}"), text(x));
        }
        // The goal can name the inputs; everything else can name the goal.
        v.insert(
            "goal.text".into(),
            prompt::render(&self.workflow.goal.text, &v),
        );
        if let Some(st) = self.nodes.get(id) {
            v.insert("inbox".into(), st.inbox.join("\n\n"));
        }
        v
    }

    /// Whether a mechanical or attested condition holds on the pinned commit.
    /// CI is the gate's to ask GitHub about: not looked at here.
    pub fn done_when(&self) -> Result<(), String> {
        let d = &self.workflow.goal.done_when;
        let Some(pin) = self.pinned.as_deref() else {
            return Err("no commit was checked".into());
        };
        for c in &d.mechanical {
            if let Some(name) = &c.check {
                let st = &self.nodes[name];
                let on = st.outputs.get("commit").and_then(|c| c.as_str()) == Some(pin);
                if st.verdict.as_deref() != Some("pass") || !on {
                    return Err(format!("the check {name} hasn't passed on {}", short(pin)));
                }
            }
        }
        for c in &d.attested {
            if let Some(name) = &c.judge {
                let st = &self.nodes[name];
                let on = st.outputs.get("commit").and_then(|c| c.as_str()) == Some(pin);
                if st.verdict.as_deref() != Some("pass") || !on {
                    return Err(format!("{name} hasn't voted pass on {}", short(pin)));
                }
            }
        }
        Ok(())
    }

    /// What priced agents spent, and whether some of the spend is unknown:
    /// an agent on a kind trix can't price counts as unknown, never $0.
    pub fn spend(&self, seen: &[Seen]) -> (f64, bool) {
        let prefix = format!("{}-", self.id);
        let sub = format!("{}.", self.id);
        let live: f64 = seen
            .iter()
            .filter(|a| a.name.starts_with(&prefix) || a.name.starts_with(&sub))
            .map(|a| a.cost)
            .sum();
        // What a workflow node's run spent on panes it has closed rolls up.
        let rolled: f64 = self
            .nodes
            .values()
            .filter_map(|n| n.outputs.get("spent")?.as_f64())
            .sum();
        let unknown = self.workflow.nodes.iter().any(|(id, n)| {
            n.is_agent()
                && self.nodes[id].visits > 0
                && n.agent
                    .as_ref()
                    .is_some_and(|k| !self.opts.priced.contains(k))
        });
        (self.spent_closed + live + rolled, unknown)
    }

    /// Whether anything is pending: a node or check at work, CI or GitHub
    /// awaited, a quota timer set, a question waiting for you. When nothing
    /// is and the goal isn't met, the stall monitor fires.
    pub fn pending(&self) -> bool {
        self.ask.is_some() || !self.active.is_empty() || !self.closing.is_empty()
    }
}

// ───────────────────────────── the nodes ─────────────────────────────

impl WfRun {
    /// Follow the edges from `id` on a verdict or a runtime event: one, or
    /// several when the verdict fans out.
    pub(super) fn route(&mut self, env: &mut dyn Env, id: &str, trigger: &str) {
        // One item of a fan-out: it's counted there, and never ends the run.
        if let Some(parent) = self.nodes[id].parent.clone() {
            return super::compose::child_done(self, env, &parent, id, trigger);
        }
        self.active.retain(|a| a != id);
        let edges: Vec<super::model::Edge> = self
            .workflow
            .follow_all(id, trigger)
            .into_iter()
            .cloned()
            .collect();
        if edges.is_empty() {
            // The checker refuses a verdict with no edge; an event with none ends the run.
            let what = if EVENTS.contains(&trigger) {
                "ended with"
            } else {
                "gave"
            };
            return self.fail(
                env,
                format!("{id} {what} {trigger}, and no edge says what follows"),
            );
        }
        let vars = self.vars(id);
        let back_edges = check::back_edges(&self.workflow);
        for edge in edges {
            match edge.to.as_str() {
                DONE => match self.done_when() {
                    Ok(()) => self.succeed(env, format!("done: {}", vars["goal.text"])),
                    Err(why) => self.fail(
                        env,
                        format!("{id} gave {trigger}, which ends the run, but done_when isn't met: {why}"),
                    ),
                },
                FAIL => {
                    let what = if EVENTS.contains(&trigger) { "ended with" } else { "gave" };
                    self.fail(env, format!("{id} {what} {trigger}"))
                }
                to => {
                    let payload: Vec<String> =
                        edge.pass.values().map(|t| prompt::render(t, &vars)).filter(|t| !t.trim().is_empty()).collect();
                    // An edge that leads back is a cycle of cost; going forward again isn't.
                    let at = self.workflow.edges.iter().position(|e| *e == edge);
                    let back = at.is_some_and(|i| back_edges.contains(&i));
                    let target = self.nodes.get_mut(to).expect("the checker refuses an edge to nowhere");
                    target.inbox = payload;
                    // Mentions queued for it arrive now, with what the edge passed.
                    target.inbox.append(&mut target.mentions);
                    target.state = NodeState::Waiting;
                    if !self.active.iter().any(|a| a == to) {
                        self.active.push(to.to_string());
                    }
                    if back {
                        self.cycles += 1;
                        self.note(
                            env,
                            format!("{id} gave {trigger}: back to {to} (cycle {} of at most {})", self.cycles, self.ceiling),
                        );
                        if self.cycles > self.ceiling {
                            let ceiling = self.ceiling;
                            self.limit(
                                env,
                                "cycles",
                                format!(
                                    "the ceiling of {} is reached, and {id} still sends work back",
                                    plural(ceiling as usize, "cycle")
                                ),
                            );
                        }
                    } else {
                        self.note(env, format!("{id} gave {trigger}: on to {to}"));
                    }
                }
            }
            if self.over() {
                return;
            }
        }
    }

    /// A revisit that changed nothing: the same head, the same failure, or
    /// the same verdict on the same commit. `no_progress: 1` stops at the
    /// first one.
    pub(super) fn wasted(&mut self, env: &mut dyn Env, id: &str, what: &str) -> bool {
        self.wasted += 1;
        self.note(env, format!("a wasted cycle: {id} {what}"));
        let after = self
            .workflow
            .limits
            .no_progress
            .as_ref()
            .map_or(Some(1), |n| n.after());
        if after.is_some_and(|n| self.wasted >= n) {
            let wasted = self.wasted;
            self.fail(
                env,
                format!(
                    "no progress: {id} {what} ({})",
                    plural(wasted as usize, "wasted cycle")
                ),
            );
            return true;
        }
        false
    }

    /// Start a node's visit: its checkout, its prompt, its pane.
    pub(super) fn begin(&mut self, env: &mut dyn Env, seen: &[Seen], id: &str) {
        let n = self.workflow.nodes[id].clone();
        if let Some(max) = n.max_visits.filter(|m| self.nodes[id].visits >= *m) {
            return self.limit(
                env,
                "visits",
                format!(
                    "{id} has had its {}, and is asked for another",
                    plural(max as usize, "visit")
                ),
            );
        }
        let writer = n.writes();
        let started = (|| -> Result<(PathBuf, Option<String>), String> {
            if writer {
                // Writers that can run at the same time never share a worktree.
                let tree = if check::isolated(&self.workflow, id) {
                    super::land::own_tree(self, env, id)?
                } else {
                    self.ensure_tree(env)?
                };
                let head = env.git(&tree, &["rev-parse", "HEAD"])?.trim().to_string();
                return Ok((tree, Some(head)));
            }
            // A reader gets a detached checkout of the checked commit, its own each visit.
            let sha = match self.pinned.clone() {
                Some(s) => s,
                None => self.head(env)?,
            };
            let name = format!("{}-{id}-{}", self.id, self.nodes[id].visits + 1);
            Ok((self.checkout(env, &name, &sha)?, Some(sha)))
        })();
        let (cwd, start) = match started {
            Ok(x) => x,
            Err(e) => {
                return self.ask(
                    env,
                    Ask::Setup,
                    Some(id),
                    format!("couldn't make {id}'s checkout: {}", first_line(&e)),
                );
            }
        };
        let st = self.nodes.get_mut(id).unwrap();
        st.visits += 1;
        if !std::mem::take(&mut st.sent_back) {
            st.start = start;
        }
        st.cwd = Some(cwd.clone());
        st.verdict = None;
        let visit = st.visits;
        let inbox = st.inbox.join("\n\n");
        let kind = st.backend.clone().or(n.agent.clone()).unwrap_or_default();
        let name = self.pane(id);
        // A pane kept from the last visit is sent what was passed along.
        let kept = n.revisit() == Revisit::Reuse
            && seen.iter().any(|a| a.name == name && a.state != "exited");
        let sent = if kept {
            env.send(&name, &prompt::again_prompt(id, &n, visit, &inbox))
        } else {
            let text = prompt::node_prompt(id, &n, &self.vars(id));
            let args = check::profile_of(&self.workflow, &n)
                .map(|p| p.args)
                .unwrap_or_default();
            let spec = AgentSpec {
                agent: kind.clone(),
                args,
            };
            // Under push: guarded and gate_only every agent pane gets the guard.
            env.spawn_in(
                &name,
                &spec,
                &cwd,
                &text,
                self.workflow.policy.push != Push::Allow,
            )
        };
        match sent {
            Ok(()) => {
                let now = env.now();
                let st = self.nodes.get_mut(id).unwrap();
                st.inbox.clear();
                st.agent = Some(name.clone());
                st.state = NodeState::Working;
                st.watch = Watch {
                    given: now,
                    ..Watch::default()
                };
                self.note(
                    env,
                    format!(
                        "{} {id} ({kind}, {name}), visit {visit}",
                        if kept { "sent on to" } else { "started" }
                    ),
                );
            }
            Err(e) => {
                self.nodes.get_mut(id).unwrap().visits -= 1;
                self.ask(
                    env,
                    Ask::Setup,
                    Some(id),
                    format!("couldn't start {id}: {}", first_line(&e)),
                );
            }
        }
    }

    /// The files a writer changed since its visit began: what its own
    /// commits touched and is still different now (not what a merge of the
    /// base brought in, and not a change it undid), and what it left
    /// uncommitted.
    pub(super) fn changed(&mut self, env: &mut dyn Env, id: &str) -> Vec<String> {
        let (Some(cwd), Some(start)) = (self.nodes[id].cwd.clone(), self.nodes[id].start.clone())
        else {
            return Vec::new();
        };
        let lines = |text: String| -> Vec<String> {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        };
        let range = format!("{start}..HEAD");
        let own = lines(
            env.git(
                &cwd,
                &[
                    "log",
                    "--first-parent",
                    "--no-merges",
                    "--name-only",
                    "--format=",
                    &range,
                ],
            )
            .unwrap_or_default(),
        );
        let net = lines(
            env.git(&cwd, &["diff", "--name-only", &start, "HEAD"])
                .unwrap_or_default(),
        );
        let mut files: Vec<String> = net.into_iter().filter(|f| own.contains(f)).collect();
        let dirty = env
            .git(&cwd, &["status", "--porcelain", "--untracked-files=all"])
            .unwrap_or_default();
        files.extend(dirty.lines().filter(|l| l.len() > 3).map(|l| {
            l[3..]
                .rsplit(" -> ")
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .to_string()
        }));
        files.sort();
        files.dedup();
        // The report file an agent may write to hand in isn't its work.
        files.retain(|f| !f.starts_with(".trix-"));
        files
    }

    /// A report came in: check a writer's scope, tell a wasted cycle, let
    /// the pane go if the node starts fresh, and follow the verdict's edge.
    pub(super) fn take(&mut self, root: &Path, env: &mut dyn Env, id: &str, text: &str) {
        let n = self.workflow.nodes[id].clone();
        let Ok(report) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let _ = std::fs::rename(
            report_file(root, &self.id, id),
            dir_of(root, &self.id)
                .join("reports")
                .join(format!("{id}-{}.json", self.nodes[id].visits)),
        );
        let verdict = report["verdict"].as_str().unwrap_or_default().to_string();
        let name = self.nodes[id]
            .agent
            .clone()
            .unwrap_or_else(|| self.pane(id));
        let head = match self.nodes[id].cwd.clone() {
            Some(cwd) => env
                .git(&cwd, &["rev-parse", "HEAD"])
                .unwrap_or_default()
                .trim()
                .to_string(),
            None => String::new(),
        };
        if n.writes() {
            // Scope: generic writers only. A built-in keeps its own rules.
            let brief = n.brief.clone().unwrap_or_default();
            let files = self.changed(env, id);
            let out: Vec<&String> = files
                .iter()
                .filter(|f| !in_scope(&brief.owns, f) || in_scope(&brief.must_not_touch, f))
                .collect();
            if !out.is_empty() {
                let list = out
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.note(
                    env,
                    format!(
                        "{id} changed files outside its scope: {list}: sent back (a cycle of cost)"
                    ),
                );
                self.cycles += 1;
                let st = self.nodes.get_mut(id).unwrap();
                st.inbox = vec![format!(
                    "Your hand-in was sent back: you changed files outside your brief: {list}. You own {}{}. Undo those changes (git checkout, or a commit that reverts them), keep the rest, and hand in again.",
                    brief.owns.join(", "),
                    if brief.must_not_touch.is_empty() {
                        String::new()
                    } else {
                        format!(", and must not touch {}", brief.must_not_touch.join(", "))
                    }
                )];
                st.state = NodeState::Waiting;
                st.sent_back = true;
                if self.cycles > self.ceiling {
                    let ceiling = self.ceiling;
                    self.limit(
                        env,
                        "cycles",
                        format!(
                            "the ceiling of {} is reached, and {id} is still out of scope",
                            plural(ceiling as usize, "cycle")
                        ),
                    );
                }
                return;
            }
            self.note(
                env,
                format!("{id} handed in {verdict}: {}", plural(files.len(), "file")),
            );
        } else {
            self.note(env, format!("{id} handed in {verdict}"));
        }
        // A wasted cycle: a writer that came back and left the head where it was;
        // a reader that says the same of the same commit.
        let visits = self.nodes[id].visits;
        let now_is = if n.writes() {
            head.clone()
        } else {
            format!(
                "{verdict}@{}",
                self.nodes[id].start.clone().unwrap_or_default()
            )
        };
        let same = visits > 1 && self.nodes[id].last.as_deref() == Some(now_is.as_str());
        let st = self.nodes.get_mut(id).unwrap();
        st.last = Some(now_is);
        st.verdict = Some(verdict.clone());
        st.outputs = report
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| k != "verdict")
            .collect();
        st.outputs.insert("head".into(), head.clone().into());
        // What it wrote on the blackboard is there for every node after.
        for (k, x) in report["board"].as_object().into_iter().flatten() {
            self.board.insert(k.clone(), x.clone());
        }
        let st = self.nodes.get_mut(id).unwrap();
        // What a reader judged is the commit it was given.
        let judged = if n.writes() {
            head
        } else {
            st.start.clone().unwrap_or_default()
        };
        st.outputs.insert("commit".into(), judged.into());
        st.state = NodeState::Done;
        st.quota = None;
        st.landed = false;
        if n.writes() && check::isolated(&self.workflow, id) {
            self.finished.retain(|f| f != id);
            self.finished.push(id.to_string());
        }
        let st = self.nodes.get_mut(id).unwrap();
        if n.revisit() == Revisit::Fresh {
            st.agent = None;
            if !self.closing.contains(&name) {
                self.closing.push(name);
            }
        }
        if same
            && self.wasted(
                env,
                id,
                if n.writes() {
                    "came back and changed nothing"
                } else {
                    "gave the same verdict on the same commit"
                },
            )
        {
            return;
        }
        if n.kind == "supervisor" {
            return self.answered_by_supervisor(env, id, &verdict);
        }
        self.route(env, id, &verdict);
    }

    /// No report yet: a quota stop is said and waited out, a stopped agent
    /// is nudged once and then yours, one that exited is started again.
    /// An idle agent is never restarted.
    pub(super) fn watch(&mut self, env: &mut dyn Env, seen: &[Seen], id: &str) {
        let n = self.workflow.nodes[id].clone();
        let Some(name) = self.nodes[id].agent.clone() else {
            return;
        };
        let Some(a) = seen.iter().find(|a| a.name == name) else {
            // Closed by hand: its visit starts again.
            let st = self.nodes.get_mut(id).unwrap();
            st.agent = None;
            st.state = NodeState::Waiting;
            st.visits = st.visits.saturating_sub(1);
            return;
        };
        let now = env.now();
        if let Some(q) = self.nodes[id].quota.clone() {
            return self.quota_wait(env, id, &name, q, now);
        }
        let kind = self.nodes[id]
            .backend
            .clone()
            .or(n.agent.clone())
            .unwrap_or_default();
        let settle = settle_secs(
            self.opts
                .state_from
                .get(&kind)
                .map_or("cadence", String::as_str),
        );
        let w = &mut self.nodes.get_mut(id).unwrap().watch;
        if a.state == "working" {
            w.worked = true;
            w.stopped_at = None;
            return;
        }
        if a.state == "exited" {
            let retries = self.workflow.limits.retries.unwrap_or(2);
            let st = self.nodes.get_mut(id).unwrap();
            if st.restarts < retries {
                st.restarts += 1;
                let again = st.restarts;
                st.agent = None;
                st.state = NodeState::Waiting;
                st.visits = st.visits.saturating_sub(1);
                st.inbox.insert(0, "The agent on this node exited before it handed in: pick up where it left off. Look at the checkout first.".into());
                let _ = env.close(&name, false);
                self.note(env, format!("{id} ({name}) exited without its report: started again ({again} of {retries})"));
            } else {
                self.note(
                    env,
                    format!(
                        "{id} ({name}) exited without its report, and its {} are used up",
                        plural(retries as usize, "restart")
                    ),
                );
                self.nodes.get_mut(id).unwrap().state = NodeState::Failed;
                self.route(env, id, "exited");
            }
            return;
        }
        if !matches!(a.state.as_str(), "done" | "idle") {
            return;
        }
        let since = *w.stopped_at.get_or_insert(now);
        if (!w.worked && now.saturating_sub(w.given) < TAKE_UP_SECS)
            || now.saturating_sub(since) < settle
        {
            return;
        }
        // Stopped, with no report: is it out of quota?
        if env.screen(&name).is_some_and(|s| quota_screen(&s)) {
            let wait = n
                .on_quota
                .as_ref()
                .and_then(|q| q.backoff_secs)
                .unwrap_or(QUOTA_BACKOFF);
            let st = self.nodes.get_mut(id).unwrap();
            st.quota = Some(Quota {
                since: now,
                tries: 0,
                next_at: now + wait,
            });
            st.state = NodeState::Blocked;
            self.note(
                env,
                format!(
                    "{id} is blocked: {kind} is out of quota or rate-limited: retry in {}m",
                    wait / 60
                ),
            );
            env.say(
                "Working",
                &format!("{} · {id} blocked: quota · retry {}m", self.id, wait / 60),
            );
            return;
        }
        let nudged = w.nudged;
        if !nudged {
            let w = &mut self.nodes.get_mut(id).unwrap().watch;
            *w = Watch {
                given: now,
                nudged: true,
                ..Watch::default()
            };
            self.note(env, format!("{id} stopped without its report: reminded it"));
            let text = format!(
                "trix workflow run {} is waiting for your report as {id}.\n{}",
                self.id,
                prompt::submit_text(id, &n)
            );
            if env.send(&name, &text).is_ok() {
                return;
            }
        }
        let why = format!(
            "{id} ({name}) stopped {}without its report",
            if nudged { "again " } else { "" }
        );
        if self.autopilot() {
            self.park(env, why)
        } else {
            self.ask(env, Ask::NoReport, Some(id), why)
        }
    }

    /// Out of quota: retried with backoff (doubling), on the fallback
    /// backend if there's one, then escalated. Never silent, and never
    /// counted against the wall clock.
    fn quota_wait(&mut self, env: &mut dyn Env, id: &str, name: &str, q: Quota, now: u64) {
        if now < q.next_at {
            return;
        }
        let n = self.workflow.nodes[id].clone();
        let policy = n.on_quota.clone().unwrap_or_default();
        let (tries, first) = (
            policy.tries.unwrap_or(QUOTA_TRIES),
            policy.backoff_secs.unwrap_or(QUOTA_BACKOFF),
        );
        if q.tries + 1 >= tries || now.saturating_sub(q.since) >= QUOTA_MAX {
            let why = format!(
                "{id} is still out of quota after {} over {}m",
                plural((q.tries + 1) as usize, "retry").replace("retrys", "retries"),
                now.saturating_sub(q.since) / 60
            );
            self.nodes.get_mut(id).unwrap().quota = None;
            return if self.autopilot() {
                self.park(env, why)
            } else {
                self.ask(env, Ask::Quota, Some(id), why)
            };
        }
        // A fallback backend the checker approved: the visit starts again there.
        if let Some(fallback) = policy
            .fallback
            .filter(|f| self.nodes[id].backend.as_ref() != Some(f))
        {
            let _ = env.close(name, false);
            let st = self.nodes.get_mut(id).unwrap();
            st.backend = Some(fallback.clone());
            st.quota = None;
            st.agent = None;
            st.state = NodeState::Waiting;
            st.visits = st.visits.saturating_sub(1);
            return self.note(
                env,
                format!("{id} goes on with {fallback}, its fallback backend"),
            );
        }
        let wait = first << (q.tries + 1).min(10);
        let st = self.nodes.get_mut(id).unwrap();
        st.quota = Some(Quota {
            tries: q.tries + 1,
            next_at: now + wait,
            ..q
        });
        // Still out, by its screen: wait on. Else it goes back to work.
        if env.screen(name).is_some_and(|s| quota_screen(&s))
            && env
                .send(
                    name,
                    "Continue where you left off, then hand in your report.",
                )
                .is_ok()
        {
            self.note(
                env,
                format!(
                    "{id} is still blocked on quota: tried again ({} of {tries}), next in {}m",
                    q.tries + 1,
                    wait / 60
                ),
            );
        }
    }

    /// An agent node, at this look.
    fn agent_node(&mut self, root: &Path, env: &mut dyn Env, seen: &[Seen], id: &str) {
        if self.nodes[id].state == NodeState::Waiting {
            return self.begin(env, seen, id);
        }
        if let Ok(text) = std::fs::read_to_string(report_file(root, &self.id, id)) {
            return self.take(root, env, id, &text);
        }
        // Back from quota: a retry that got it working again unblocks it.
        if self.nodes[id].quota.is_some() {
            let name = self.nodes[id].agent.clone().unwrap_or_default();
            if seen.iter().any(|a| a.name == name && a.state == "working") {
                let st = self.nodes.get_mut(id).unwrap();
                st.quota = None;
                st.state = NodeState::Working;
                self.note(env, format!("{id} is working again"));
            }
        }
        self.watch(env, seen, id);
    }

    /// A check: a command trix runs on a clean, detached checkout of a
    /// commit it resolved itself, in a process of its own with the guard
    /// on. The exit code decides. It runs code the agents wrote, so it's
    /// guarded, never enforced.
    fn check_node(&mut self, env: &mut dyn Env, id: &str) {
        let n = self.workflow.nodes[id].clone();
        let sha = match self.head(env) {
            Ok(s) => s,
            Err(e) => {
                return self.fail(
                    env,
                    format!("couldn't read the run's head for {id}: {}", first_line(&e)),
                );
            }
        };
        let name = format!("{}-check-{id}", self.id);
        let dir = match self.checkout(env, &name, &sha) {
            Ok(d) => d,
            Err(e) => {
                return self.fail(
                    env,
                    format!(
                        "couldn't check {} out for {id}: {}",
                        short(&sha),
                        first_line(&e)
                    ),
                );
            }
        };
        let vars = self.vars(id);
        let argv: Vec<String> = n.run.iter().map(|a| prompt::render(a, &vars)).collect();
        let guard = self.workflow.policy.push != Push::Allow;
        let reruns = self.workflow.limits.flaky_check_reruns.unwrap_or(0);
        let before = env.now();
        let mut out = env.check(&dir, &argv, guard);
        for _ in 0..reruns {
            if out.as_ref().is_ok_and(|o| o.exit_code == 0) || out.is_err() {
                break;
            }
            // A rerun of a flaky check isn't a cycle.
            self.note(env, format!("{id} failed: run once more"));
            out = env.check(&dir, &argv, guard);
        }
        self.inflight += env.now().saturating_sub(before);
        let _ = env.git(
            &self.repo,
            &["worktree", "remove", "--force", &dir.display().to_string()],
        );
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                self.note(env, format!("{id} couldn't run: {}", first_line(&e)));
                self.nodes.get_mut(id).unwrap().state = NodeState::Failed;
                return self.route(env, id, "error");
            }
        };
        let verdict = if out.exit_code == 0 { "pass" } else { "fail" };
        let now_is = format!("{verdict}@{sha}:{}", out.log_tail);
        let st = self.nodes.get_mut(id).unwrap();
        st.visits += 1;
        let same =
            st.visits > 1 && verdict == "fail" && st.last.as_deref() == Some(now_is.as_str());
        st.last = Some(now_is);
        st.verdict = Some(verdict.into());
        st.outputs = BTreeMap::from([
            ("commit".to_string(), sha.clone().into()),
            ("exit_code".to_string(), out.exit_code.into()),
            ("log_tail".to_string(), out.log_tail.clone().into()),
        ]);
        st.state = NodeState::Done;
        // What the check ran on is the commit everything after judges.
        self.pinned = Some(sha.clone());
        self.note(
            env,
            format!(
                "{id} ran on {}: {verdict} (exit {})",
                short(&sha),
                out.exit_code
            ),
        );
        if same && self.wasted(env, id, "failed the same way on the same commit") {
            return;
        }
        self.route(env, id, verdict);
    }

    /// The supervisor answered a question: its answer goes back to the
    /// asker as its next prompt, or the question comes to you.
    fn answered_by_supervisor(&mut self, env: &mut dyn Env, id: &str, verdict: &str) {
        self.active.retain(|a| a != id);
        let Some(q) = (!self.questions.is_empty()).then(|| self.questions.remove(0)) else {
            return;
        };
        let answer = self.nodes[id]
            .outputs
            .get("answer")
            .and_then(|a| a.as_str())
            .unwrap_or_default()
            .to_string();
        if verdict == "escalate" || answer.trim().is_empty() || answer.trim() == "ESCALATE" {
            self.questions.insert(0, q.clone());
            return self.ask(
                env,
                Ask::Question,
                Some(&q.from),
                format!("{} asks, and the supervisor escalated: {}", q.from, q.text),
            );
        }
        self.queue_mentions(env, &answer, id);
        self.reply(
            env,
            &q.from,
            &format!("The supervisor answers your question:\n{answer}"),
        );
        if !self.questions.is_empty() {
            self.ask_supervisor(env);
        }
    }

    /// Send an answer to the node that asked, as its next prompt.
    fn reply(&mut self, env: &mut dyn Env, to: &str, text: &str) {
        match self.nodes.get(to).and_then(|n| n.agent.clone()) {
            Some(name) => {
                let now = env.now();
                if let Err(e) = env.send(&name, text) {
                    self.note(env, format!("couldn't answer {to}: {}", first_line(&e)));
                }
                self.nodes.get_mut(to).unwrap().watch = Watch {
                    given: now,
                    ..Watch::default()
                };
                self.note(env, format!("answered {to}'s question"));
            }
            // Its pane is gone: the answer waits in its inbox.
            None => self.nodes.get_mut(to).unwrap().inbox.push(text.to_string()),
        }
    }

    /// `@node` in a question or an answer: queued to that node's inbox for
    /// its next visit along a declared edge. Never a wake-up, never an edge.
    fn queue_mentions(&mut self, env: &mut dyn Env, text: &str, from: &str) {
        for name in prompt::mentions(text) {
            if self.workflow.nodes.get(&name).is_some_and(Node::is_agent) && name != from {
                self.nodes
                    .get_mut(&name)
                    .unwrap()
                    .mentions
                    .push(format!("{from} mentioned you: {text}"));
                self.note(
                    env,
                    format!("{from} mentioned {name}: queued for its next visit"),
                );
            }
        }
    }

    /// The first waiting question goes to the supervisor node.
    fn ask_supervisor(&mut self, env: &mut dyn Env) {
        let Some((sup, _)) = self
            .workflow
            .of_kind("supervisor")
            .map(|(id, n)| (id.to_string(), n.clone()))
        else {
            return;
        };
        let Some(q) = self.questions.first().cloned() else {
            return;
        };
        let st = self.nodes.get_mut(&sup).unwrap();
        if st.state == NodeState::Working {
            return;
        }
        st.inbox = vec![format!(
            "{} asks:\n{}\n\nAnswer it (verdict answer, with the answer in \"answer\"), or escalate it to the user (verdict escalate).",
            q.from, q.text
        )];
        st.state = NodeState::Waiting;
        if !self.active.contains(&sup) {
            self.active.push(sup.clone());
        }
        self.note(env, format!("{} asked a question: to {sup}", q.from));
    }

    /// Questions agents asked since the last look (`trix wf ask`).
    fn take_questions(&mut self, root: &Path, env: &mut dyn Env) {
        let dir = dir_of(root, &self.id).join("asks");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        for f in files {
            let Some(q) = std::fs::read(&f)
                .ok()
                .and_then(|b| serde_json::from_slice::<Question>(&b).ok())
            else {
                continue;
            };
            let _ = std::fs::remove_file(&f);
            self.queue_mentions(env, &q.text, &q.from);
            self.questions.push(q.clone());
            if self.workflow.of_kind("supervisor").is_some() {
                self.ask_supervisor(env);
            } else if self.ask.is_none() {
                self.ask(
                    env,
                    Ask::Question,
                    Some(&q.from),
                    format!("{} asks: {}", q.from, q.text),
                );
            }
        }
    }
}

impl WfRun {
    /// A kind this trix doesn't run yet.
    pub fn land_unbuilt(&mut self, env: &mut dyn Env, id: &str) {
        let kind = self.workflow.nodes[id].kind.clone();
        self.fail(
            env,
            format!("{id} is a {kind} node, which this trix doesn't run yet"),
        );
    }
}

// ───────────────────────────── the gate ─────────────────────────────

/// CI on a commit, as GitHub has it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ci {
    Green,
    Red,
    Pending,
    None,
}

impl WfRun {
    fn ci(&self, env: &mut dyn Env, sha: &str) -> (Ci, Vec<String>) {
        let Ok(runs) = crate::case::check_runs(&self.repo, sha, &mut |r, a| env.gh(r, a)) else {
            return (Ci::Pending, Vec::new());
        };
        let name = |v: &serde_json::Value| v["name"].as_str().unwrap_or("?").to_string();
        let bad = [
            "failure",
            "cancelled",
            "timed_out",
            "action_required",
            "startup_failure",
        ];
        let failed: Vec<String> = runs
            .iter()
            .filter(|v| v["conclusion"].as_str().is_some_and(|c| bad.contains(&c)))
            .map(name)
            .collect();
        if !failed.is_empty() {
            (Ci::Red, failed)
        } else if runs.iter().any(|v| v["status"] != "completed") {
            (Ci::Pending, Vec::new())
        } else if runs.is_empty() {
            // "No checks yet" is never green.
            (
                if self.gate.ci_expected {
                    Ci::Pending
                } else {
                    Ci::None
                },
                Vec::new(),
            )
        } else {
            (Ci::Green, Vec::new())
        }
    }

    /// The node that may approve the merge in your place: it's named, the
    /// approvals are switched on for this workflow and not revoked, it
    /// didn't write the change, and it voted pass on the checked commit.
    fn approver(&self, sha: &str) -> Result<String, String> {
        let Some(a) = self
            .workflow
            .approvals
            .merge
            .as_ref()
            .filter(|a| a.mode == Allow::Allow)
        else {
            return Err("the workflow names no approver".into());
        };
        if self.workflow.policy.merge != Merge::Approver {
            return Err("the workflow's policy is that you approve the merge".into());
        }
        if !self.opts.approvals {
            return Err(format!(
                "approvals are off for '{}': it isn't in [approvals] allow, or isn't trusted as it is",
                self.workflow.name
            ));
        }
        if self.revoked {
            return Err("approvals were revoked for this run".into());
        }
        let id = a.approver.clone().ok_or("no approver node is named")?;
        let node = self
            .workflow
            .nodes
            .get(&id)
            .ok_or(format!("the approver '{id}' isn't a node of this run"))?;
        if node.writes() {
            return Err(format!("the approver '{id}' wrote the change"));
        }
        let st = &self.nodes[&id];
        if st.verdict.as_deref() != Some("pass")
            || st.outputs.get("commit").and_then(|c| c.as_str()) != Some(sha)
        {
            return Err(format!(
                "the approver '{id}' hasn't voted pass on {}",
                short(sha)
            ));
        }
        if !a.branches.is_empty() && !a.branches.contains(&self.base) {
            return Err(format!(
                "the approval is for {}, and this merges into {}",
                a.branches.join(", "),
                self.base
            ));
        }
        if !a.repos.is_empty() && !a.repos.contains(&self.opts.repo_name) {
            return Err(format!(
                "the approval is for {}, not this repository",
                a.repos.join(", ")
            ));
        }
        Ok(id)
    }

    /// Every approval, as it's made: when, who, by which rule, and what
    /// exactly was approved.
    pub fn audit(&mut self, root: &Path, env: &mut dyn Env, entry: serde_json::Value) {
        let mut e = entry;
        e["at"] = env.now().into();
        e["run"] = self.id.clone().into();
        e["branch"] = self.branch.clone().into();
        let path = dir_of(root, &self.id).join("approvals.jsonl");
        let line = format!("{}\n", serde_json::to_string(&e).unwrap_or_default());
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if self.opts.notify_each {
            env.notify(
                &format!("{}: approved", self.id),
                &format!(
                    "{} by {}",
                    e["what"].as_str().unwrap_or_default(),
                    e["approver"].as_str().unwrap_or_default()
                ),
            );
        }
    }

    /// The gate: push the checked commit, open the PR, wait for CI on that
    /// commit, get the merge approved (by you, or the approver node), and
    /// merge exactly that commit after a fresh look at CI.
    fn gate_node(&mut self, root: &Path, env: &mut dyn Env, id: &str) {
        let n = self.workflow.nodes[id].clone();
        let Some(sha) = self.pinned.clone() else {
            return self.fail(
                env,
                "the gate has no checked commit: no check ran before it",
            );
        };
        if let Err(why) = self.done_when() {
            return self.park(env, format!("done_when isn't met: {why}"));
        }
        // A new commit cancels earlier verdicts: only what was checked is pushed.
        match self.head(env) {
            Ok(head) if head == sha => {}
            Ok(head) => return self.park(env, format!("the run's branch moved to {} since {} was checked: its verdicts don't cover the new commit", short(&head), short(&sha))),
            Err(e) => return self.park(env, format!("couldn't read the run's head: {}", first_line(&e))),
        }
        self.nodes.get_mut(id).unwrap().state = NodeState::Working;
        if self.gate.pushed.as_deref() != Some(&sha) {
            if let Some(stop) = super::policy::before_push(self, env, &sha) {
                return self.floor(env, stop);
            }
            // Trix's own push: a plain one, never forced.
            if let Err(e) = env.git(
                &self.repo,
                &[
                    "push",
                    "origin",
                    &format!("{sha}:refs/heads/{}", self.branch),
                ],
            ) {
                return self.park(
                    env,
                    format!("the push of {} failed: {}", short(&sha), first_line(&e)),
                );
            }
            self.gate.pushed = Some(sha.clone());
            self.note(env, format!("pushed {} to {}", short(&sha), self.branch));
        }
        if self.gate.pr.is_none() {
            match self.open_pr(env) {
                Ok((number, url)) => {
                    self.note(env, format!("PR #{number} is open: {url}"));
                    self.gate.pr = Some(number);
                    self.gate.url = url;
                }
                Err(e) => {
                    return self.park(env, format!("couldn't open the PR: {}", first_line(&e)));
                }
            }
        }
        let number = self.gate.pr.unwrap_or_default();
        let wants_ci = self
            .workflow
            .goal
            .done_when
            .mechanical
            .iter()
            .any(|c| c.ci.is_some());
        match self.ci(env, &sha) {
            (Ci::Pending, _) => {
                return env.say(
                    "Working",
                    &format!("{} · waiting for CI on {}", self.id, short(&sha)),
                );
            }
            (Ci::Red, failed) => {
                self.note(
                    env,
                    format!("CI fails on {}: {}", short(&sha), failed.join(", ")),
                );
                self.gate.approved = None;
                return match self.workflow.follow(id, "ci_failed") {
                    Some(_) => {
                        self.nodes
                            .get_mut(id)
                            .unwrap()
                            .outputs
                            .insert("failed".into(), failed.join(", ").into());
                        self.route(env, id, "ci_failed")
                    }
                    None => self.park(
                        env,
                        format!("CI failed on the checked commit: {}", failed.join(", ")),
                    ),
                };
            }
            (Ci::None, _) if wants_ci => {
                return self.park(
                    env,
                    "done_when asks for green CI, and this repository ran no checks on the commit"
                        .to_string(),
                );
            }
            (Ci::Green | Ci::None, _) => {}
        }
        if self.gate.approved.is_none() {
            match self.approver(&sha) {
                Ok(node) => {
                    self.audit(root, env, serde_json::json!({"what": format!("merge PR #{number}"), "kind": "merge", "node": id, "approver": node, "rule": "approvals.merge", "sha": sha, "command": format!("gh pr merge {number} --match-head-commit {sha}")}));
                    self.note(env, format!("⚡ {node} approves the merge of {}: it voted pass on it, and didn't write it", short(&sha)));
                    self.gate.approved = Some(node);
                }
                Err(why) => {
                    let who = if self.workflow.policy.merge == Merge::Approver {
                        format!(" ({why})")
                    } else {
                        String::new()
                    };
                    return self.ask(
                        env,
                        Ask::Merge,
                        Some(id),
                        format!(
                            "ready to merge PR #{number}: done_when holds on {}{who}",
                            short(&sha)
                        ),
                    );
                }
            }
        }
        if let Some(stop) = super::policy::before_merge(self, env, &sha) {
            return self.floor(env, stop);
        }
        // CI again, right before: it may have changed while the approval was awaited.
        if let (Ci::Red | Ci::Pending, failed) = self.ci(env, &sha) {
            self.gate.approved = None;
            return self.note(
                env,
                if failed.is_empty() {
                    "CI is running again: not merged yet".to_string()
                } else {
                    format!("CI failed since: {}", failed.join(", "))
                },
            );
        }
        let num = number.to_string();
        let method = format!("--{}", n.merge.as_deref().unwrap_or("squash"));
        match env.gh(
            &self.repo,
            &["pr", "merge", &num, &method, "--match-head-commit", &sha],
        ) {
            Ok(_) => {
                self.nodes.get_mut(id).unwrap().state = NodeState::Done;
                self.nodes.get_mut(id).unwrap().verdict = Some("merged".into());
                self.succeed(
                    env,
                    format!(
                        "merged PR #{number} ({}) at {}",
                        method.trim_start_matches('-'),
                        short(&sha)
                    ),
                );
            }
            Err(e) if crate::case::blocked_by_policy(&e) => {
                self.gate.approved = None;
                self.park(
                    env,
                    format!(
                        "the base branch's rules refused the merge of PR #{number}: {}",
                        first_line(&e)
                    ),
                );
            }
            Err(e) => {
                self.gate.approved = None;
                self.park(
                    env,
                    format!("the merge of PR #{number} failed: {}", first_line(&e)),
                );
            }
        }
    }

    /// A floor stopped trix: for you, whatever the approvals say.
    fn floor(&mut self, env: &mut dyn Env, stop: super::policy::Stop) {
        match stop {
            super::policy::Stop::Park(why) => self.park(env, why),
            super::policy::Stop::You(floor, why) => {
                self.limit = Some(floor);
                self.ask(env, Ask::Floor, None, why);
            }
        }
    }

    /// The run branch's PR: the one already open for it, or a new one whose
    /// body is the outcome summary and nothing else.
    fn open_pr(&mut self, env: &mut dyn Env) -> Result<(u64, String), String> {
        let find = |env: &mut dyn Env,
                    branch: &str,
                    repo: &Path|
         -> Result<Option<(u64, String)>, String> {
            let out = env.gh(
                repo,
                &[
                    "pr",
                    "list",
                    "--head",
                    branch,
                    "--state",
                    "open",
                    "--json",
                    "number,url",
                ],
            )?;
            let v: Vec<serde_json::Value> =
                serde_json::from_str(&out).map_err(|e| e.to_string())?;
            Ok(v.first().and_then(|p| {
                Some((
                    p["number"].as_u64()?,
                    p["url"].as_str().unwrap_or_default().to_string(),
                ))
            }))
        };
        let (branch, repo) = (self.branch.clone(), self.repo.clone());
        if let Some(pr) = find(env, &branch, &repo)? {
            return Ok(pr);
        }
        let title = format!("{}: {}", self.workflow.name, self.id);
        let body = super::summary::outcome(self);
        env.gh(
            &repo,
            &[
                "pr", "create", "--head", &branch, "--base", &self.base, "--title", &title,
                "--body", &body,
            ],
        )?;
        find(env, &branch, &repo)?.ok_or_else(|| format!("gh made no PR for {branch}"))
    }
}

// ───────────────────────────── the run on the runner ─────────────────────────────

impl Run for WfRun {
    const SOURCE: &'static str = "workflow";

    fn load(root: &Path, id: &str) -> Result<WfRun, String> {
        load(root, id)
    }
    fn store(&self, root: &Path) -> Result<(), String> {
        store(root, self)
    }
    fn over(&self) -> bool {
        self.state.over()
    }
    fn closing(&mut self) -> &mut Vec<String> {
        &mut self.closing
    }
    fn spent(&mut self, cost: f64) {
        self.spent_closed += cost;
    }

    fn meanwhile(&mut self, root: &Path, env: &mut dyn Env, _seen: &[Seen]) -> bool {
        // The report came after all: no need for your answer.
        if self.ask == Some(Ask::NoReport)
            && self
                .about
                .as_ref()
                .is_some_and(|n| report_file(root, &self.id, n).exists())
        {
            self.ask = None;
            self.state = RunState::Running;
            self.why.clear();
            self.note(env, "the report came in");
        }
        super::policy::control(self, root, env);
        if self.over() {
            return true;
        }
        self.take_questions(root, env);
        false
    }

    fn asks(&self) -> Option<Vec<char>> {
        self.ask.map(|a| a.keys().iter().map(|(k, _)| *k).collect())
    }

    fn answered(&mut self, root: &Path, env: &mut dyn Env, seen: &[Seen], key: char) {
        let Some(a) = self.ask.take() else { return };
        let about = self.about.take();
        self.why.clear();
        self.state = RunState::Running;
        match a {
            Ask::Merge => {
                let sha = self.pinned.clone().unwrap_or_default();
                let pr = self.gate.pr.unwrap_or_default();
                self.audit(root, env, serde_json::json!({"what": format!("merge PR #{pr}"), "kind": "merge", "node": about, "approver": "you", "rule": "your y", "sha": sha}));
                self.gate.approved = Some("you".into());
                self.note(env, "merging, as you said");
            }
            Ask::Outsider => {
                self.trusted_input = true;
                self.note(env, "the agents may read it, as you said");
            }
            Ask::NoReport => {
                let Some(id) = about else { return };
                let Some(name) = self.nodes[&id].agent.clone() else {
                    return;
                };
                let n = self.workflow.nodes[&id].clone();
                if seen.iter().all(|s| s.name != name || s.state == "exited") {
                    let st = self.nodes.get_mut(&id).unwrap();
                    st.agent = None;
                    st.state = NodeState::Waiting;
                    st.visits = st.visits.saturating_sub(1);
                    self.note(env, format!("starting {name} again"));
                } else {
                    let now = env.now();
                    self.nodes.get_mut(&id).unwrap().watch = Watch {
                        given: now,
                        nudged: false,
                        ..Watch::default()
                    };
                    let text = format!(
                        "trix workflow run {} is waiting for your report as {id}.\n{}",
                        self.id,
                        prompt::submit_text(&id, &n)
                    );
                    if let Err(e) = env.send(&name, &text) {
                        self.note(env, format!("couldn't nudge {name}: {e}"));
                    }
                }
            }
            Ask::Limit => match self.limit.take().as_deref() {
                Some("cycles") => {
                    self.ceiling = self.cycles.max(self.ceiling) + 1;
                    self.note(
                        env,
                        format!(
                            "one more cycle, as you said: the ceiling is {}",
                            self.ceiling
                        ),
                    );
                }
                Some("clock") => {
                    let more = self
                        .workflow
                        .limits
                        .wall_clock
                        .as_deref()
                        .and_then(|t| duration(t).ok())
                        .unwrap_or(3600);
                    self.wall_clock += more;
                    self.note(env, format!("the wall clock is raised by {}m", more / 60));
                }
                Some("budget") => {
                    let more = self
                        .workflow
                        .limits
                        .budget_usd
                        .as_ref()
                        .and_then(|b| b.get())
                        .unwrap_or(10.0);
                    self.budget = Some(self.budget.unwrap_or(0.0) + more);
                    self.note(
                        env,
                        format!(
                            "the budget is raised to {}",
                            crate::cost::usd(self.budget.unwrap_or_default())
                        ),
                    );
                }
                Some("visits") => {
                    for (id, n) in self.workflow.nodes.iter_mut() {
                        if let Some(m) = n.max_visits.filter(|m| self.nodes[id].visits >= *m) {
                            n.max_visits = Some(m + 1);
                        }
                    }
                    self.note(env, "one more visit, as you said");
                }
                _ => {}
            },
            Ask::Stall => self.note(env, "looking again, as you said"),
            Ask::Question => {
                if !self.questions.is_empty() {
                    let q = self.questions.remove(0);
                    self.reply(env, &q.from, "The user read your question and says: use your best judgment, and say what you assumed when you hand in.");
                }
            }
            Ask::Quota => {
                if let Some(id) = about {
                    let now = env.now();
                    let st = self.nodes.get_mut(&id).unwrap();
                    st.quota = Some(Quota {
                        since: now,
                        tries: 0,
                        next_at: now,
                    });
                    st.state = NodeState::Blocked;
                    self.note(env, format!("trying {id} again, as you said"));
                }
            }
            Ask::Floor => {
                if let Some(floor) = self.limit.take() {
                    let sha = self.pinned.clone().unwrap_or_default();
                    self.audit(root, env, serde_json::json!({"what": format!("past the floor {floor}"), "kind": "floor", "approver": "you", "rule": "your y, for this run only", "sha": sha}));
                    self.note(
                        env,
                        format!("past the floor {floor}, as you said: for this run only"),
                    );
                    self.gate.let_through.push(floor);
                }
            }
            Ask::Launch => self.note(env, "started, as you said"),
            Ask::Partial => {
                if let Some(part) = self.limit.take() {
                    self.note(env, "going on with what succeeded, as you said");
                    self.gate.let_through.push(part);
                }
            }
            Ask::Setup => {
                if let Some(id) = about {
                    self.nodes.get_mut(&id).unwrap().state = NodeState::Waiting;
                    if !self.active.contains(&id) {
                        self.active.push(id);
                    }
                }
            }
        }
        let _ = key;
    }

    fn abort(&mut self, env: &mut dyn Env, _seen: &[Seen]) {
        self.ask = None;
        self.why.clear();
        self.state = RunState::Aborted;
        self.note(
            env,
            "aborted: the branches are as the agents left them, and the worktrees stay",
        );
        self.close_all(env, false);
        env.say("Finished", &format!("{} · aborted", self.id));
    }

    fn limited(&mut self, env: &mut dyn Env, seen: &[Seen]) -> bool {
        if matches!(
            self.state,
            RunState::Parked | RunState::Paused | RunState::Scheduled
        ) {
            return true;
        }
        // The wall clock counts only time with a node or check running: not
        // while it needs you, is parked or paused, waits on CI or GitHub, or
        // backs off on quota.
        let now = env.now();
        let dt = now.saturating_sub(self.looked).min(LOOK_SECS);
        self.looked = now;
        let prefix = format!("{}-", self.id);
        let working = self.active.iter().any(|id| {
            let st = &self.nodes[id];
            st.state == NodeState::Working
                && st.quota.is_none()
                && st.agent.as_ref().is_some_and(|a| a.starts_with(&prefix))
        });
        if working {
            self.inflight += dt;
        }
        if self.inflight > self.wall_clock {
            let (spent, limit) = (self.inflight / 60, self.wall_clock / 60);
            self.limit(
                env,
                "clock",
                format!("the wall clock is used up: {spent}m of work against {limit}m"),
            );
            return true;
        }
        if let Some(stop) = super::policy::time_of_day(self, env) {
            match stop {
                super::policy::Stop::Park(why) => self.park(env, why),
                super::policy::Stop::You(_, why) => self.fail(env, why),
            }
            return true;
        }
        let (spent, _) = self.spend(seen);
        if let Some(b) = self.budget.filter(|b| spent >= *b) {
            self.limit(
                env,
                "budget",
                format!(
                    "the budget is spent: {} of {}",
                    crate::cost::usd(spent),
                    crate::cost::usd(b)
                ),
            );
            return true;
        }
        false
    }

    fn visit(&mut self, root: &Path, env: &mut dyn Env, seen: &[Seen]) -> bool {
        // What could make git run a command behind trix's back, as it is now.
        if self.fingerprint.is_none() && self.workflow.policy.push != Push::Allow {
            self.fingerprint = Some(super::policy::fingerprint(env, &self.repo));
        }
        // Text from outside the project: you read it first, once.
        if !self.trusted_input
            && let Some(who) = self.outsiders()
        {
            self.ask(env, Ask::Outsider, None, format!("an input has text from outside the project ({who}): read it before the agents work from it"));
            return true;
        }
        for id in self.active.clone() {
            let kind = self.workflow.nodes[&id].kind.clone();
            match kind.as_str() {
                "check" => self.check_node(env, &id),
                "gh.gate" => self.gate_node(root, env, &id),
                "land" => super::land::land_node(self, env, &id),
                "for_each" => super::compose::for_each_node(self, root, env, seen, &id),
                "join" => super::compose::join_node(self, env, &id),
                "workflow" => super::compose::workflow_node(self, root, env, &id),
                _ => self.agent_node(root, env, seen, &id),
            }
            if self.over() || self.ask.is_some() || self.state != RunState::Running {
                return true;
            }
        }
        // The stall monitor: built in, always on. It fires only when
        // nothing is pending and the goal isn't met.
        if !self.pending() {
            let why =
                "nothing is pending and the goal isn't met: no node is at work, and none is next"
                    .to_string();
            let on = self
                .workflow
                .limits
                .on_stall
                .unwrap_or(if self.autopilot() {
                    OnStall::Escalate
                } else {
                    OnStall::Ask
                });
            match on {
                OnStall::Escalate => self.park(env, why),
                OnStall::Ask => self.ask(env, Ask::Stall, None, why),
                OnStall::Fail => self.fail(env, why),
            }
            return true;
        }
        false
    }

    fn working(&self) -> Option<String> {
        let at: Vec<String> = self
            .active
            .iter()
            .map(|id| format!("{id} {}", self.nodes[id].state.label()))
            .collect();
        (self.ask.is_none()
            && self.state == RunState::Running
            && !self
                .active
                .iter()
                .any(|id| self.workflow.nodes[id].kind == "gh.gate"))
        .then(|| format!("{} · {}", self.id, at.join(", ")))
    }

    fn banner(&self) -> String {
        let mut s = format!(
            "trix workflow · {} · {} · {}\n{}\n\n",
            self.id,
            self.workflow.name,
            if self.autopilot() {
                "autopilot"
            } else {
                "supervised"
            },
            self.vars("").get("goal.text").cloned().unwrap_or_default()
        );
        for (at, text) in &self.log {
            let (_, _, secs) = crate::ledger::local(*at);
            s.push_str(&format!(
                "{:02}:{:02}  {text}\n",
                secs / 3600,
                secs / 60 % 60
            ));
        }
        if let Some(a) = self.ask {
            let keys: Vec<String> = a.keys().iter().map(|(k, w)| format!("{k}: {w}")).collect();
            s.push_str(&format!(
                "needs you: {}  [{}]  (type the key, then enter)\n",
                self.why,
                keys.join(" · ")
            ));
        }
        s
    }
    fn question(&self) -> Option<String> {
        self.ask.map(|_| format!("{} · {}", self.id, self.why))
    }
    fn resumed(&self) -> bool {
        !self.log.is_empty() && !self.over()
    }
    fn ended(&self) -> (String, String) {
        (
            format!(
                "\n{}\nRun {} {}. Its log stays here; close the pane when you're done with it.",
                super::summary::outcome(self),
                self.id,
                self.state.label()
            ),
            format!("{} · {}", self.id, self.state.label()),
        )
    }
}

impl WfRun {
    /// Who wrote an input from outside the project, if anyone did.
    fn outsiders(&self) -> Option<String> {
        let who: Vec<String> = self
            .inputs
            .values()
            .filter_map(|v| v["outsiders"].as_array())
            .flatten()
            .filter_map(|w| w.as_str().map(String::from))
            .collect();
        (!who.is_empty()).then(|| who.join(", "))
    }

    /// The run as anyone looks at it.
    pub fn view(&self, seen: &[Seen]) -> RunView {
        let nodes = self
            .workflow
            .nodes
            .iter()
            .map(|(id, n)| {
                let st = &self.nodes[id];
                let last = match (&st.quota, &st.verdict) {
                    (Some(q), _) => format!(
                        "quota · retry {} of {}",
                        q.tries + 1,
                        n.on_quota
                            .as_ref()
                            .and_then(|o| o.tries)
                            .unwrap_or(QUOTA_TRIES)
                    ),
                    (None, Some(v)) => match st.outputs.get("summary").and_then(|s| s.as_str()) {
                        Some(s) if !s.is_empty() => {
                            format!("{v} · {}", s.chars().take(40).collect::<String>())
                        }
                        _ => v.clone(),
                    },
                    _ => String::new(),
                };
                NodeView {
                    id: id.clone(),
                    kind: n.kind.clone(),
                    state: st.state,
                    backend: match n.kind.as_str() {
                        "check" => "check".into(),
                        _ if n.is_agent() => {
                            st.backend.clone().or(n.agent.clone()).unwrap_or_default()
                        }
                        _ => "trix".into(),
                    },
                    owns: n.brief.as_ref().map(|b| b.owns.clone()).unwrap_or_default(),
                    visits: st.visits,
                    last,
                }
            })
            .collect();
        let (spent_usd, unpriced) = self.spend(seen);
        RunView {
            id: self.id.clone(),
            workflow: self.workflow.name.clone(),
            state: self.state,
            goal: self.vars("").get("goal.text").cloned().unwrap_or_default(),
            nodes,
            cycles: self.cycles,
            wasted: self.wasted,
            spent_usd,
            unpriced,
            why: self.why.clone(),
        }
    }
}

// ───────────────────────────── the server's part ─────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Op {
    /// `trix wf submit <node>`: an agent hands in its report.
    Submit {
        node: String,
        report: String,
    },
    /// `trix wf ask`: a question for the supervisor node, or for you.
    Ask {
        text: String,
    },
    Approve,
    Abort,
    Answer {
        key: char,
    },
    Pause,
    Resume,
    Revoke,
    /// A PermissionRequest hook asks whether a rule covers this one request.
    Decide {
        tool: String,
        command: String,
        cwd: String,
    },
    /// The run's pane is about to start: an agent pane may only as `[workflows.agents]` says.
    Start,
    /// Trust a workflow as it is now: yours only.
    Trust {
        name: String,
        hash: String,
    },
}

/// The run an agent's pane belongs to: its panes are named after it.
pub fn run_of(root: &Path, pane: &str) -> Option<(WfRun, String)> {
    all(root)
        .into_iter()
        .filter(|r| !r.state.over())
        .find_map(|r| {
            let node = r
                .nodes
                .iter()
                .find(|(_, st)| st.agent.as_deref() == Some(pane))
                .map(|(id, _)| id.clone())?;
            Some((r, node))
        })
}

/// A report's fields are what the node declares: a verdict it may give,
/// and each output with its type.
pub fn check_report(r: &WfRun, node: &str, text: &str) -> Result<String, String> {
    let n = r
        .workflow
        .nodes
        .get(node)
        .ok_or(format!("'{node}' isn't a node of run {}", r.id))?;
    if !n.is_agent() {
        return Err(format!(
            "{node} is a {}: it doesn't hand in a report",
            n.kind
        ));
    }
    if r.state.over() {
        return Err(format!(
            "run {} is {}: it takes no more reports",
            r.id,
            r.state.label()
        ));
    }
    if r.nodes[node].state != NodeState::Working && r.nodes[node].state != NodeState::Blocked {
        return Err(format!(
            "run {} isn't waiting for {node}'s report: {node} is {}",
            r.id,
            r.nodes[node].state.label()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("the report isn't JSON: {e}"))?;
    let obj = v
        .as_object()
        .ok_or("the report is a JSON object: {\"verdict\": …}")?;
    let verdict = obj
        .get("verdict")
        .and_then(|x| x.as_str())
        .ok_or("verdict: say which one")?;
    let allowed: Vec<&str> = if n.kind == "supervisor" {
        vec!["answer", "escalate"]
    } else {
        n.verdicts.iter().map(String::as_str).collect()
    };
    if !allowed.is_empty() && !allowed.contains(&verdict) {
        return Err(format!(
            "verdict: '{verdict}' isn't one {node} gives ({})",
            allowed.join(", ")
        ));
    }
    for (name, kind) in &n.output {
        let ok = match (obj.get(name), kind.as_str()) {
            (None | Some(serde_json::Value::Null), _) => false,
            (Some(x), k) if k.starts_with("list") => x.is_array(),
            (Some(x), "string") => x.is_string(),
            (Some(x), "int" | "number") => x.is_number(),
            (Some(x), "bool") => x.is_boolean(),
            (Some(_), _) => true,
        };
        // What only one verdict carries isn't asked of the others.
        let wanted = !matches!(verdict, "pass" | "none" | "skipped") || obj.contains_key(name);
        if !ok && wanted {
            return Err(format!("{name}: {node}'s report carries it, as {kind}"));
        }
    }
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

/// `trix wf …` and `trix workflow approve / abort / …`, in the server.
/// `asker`: the agent whose pane the request comes from, as the kernel
/// says (None: you). `Ok` holds what to reply and, for an answer, the key
/// to type into the run's pane.
pub fn handle(
    root: &Path,
    asker: Option<&crate::control::Asker>,
    run: Option<&str>,
    op: Op,
    policy: &super::settings::AgentPolicy,
) -> Result<(String, Option<char>), String> {
    // A check's process is nobody: it can't drive trix at all.
    if let Some(crate::control::Asker::Check(what)) = asker {
        return Err(format!(
            "{what} can't submit, ask, approve, send or start runs: a check runs code the agents wrote"
        ));
    }
    if let Op::Trust { name, hash } = &op {
        if let Some(who) = asker {
            return Err(format!(
                "{} can't trust a workflow: only the user does",
                who.said()
            ));
        }
        super::policy::trust(&super::policy::trust_file(), name, hash)?;
        return Ok((format!("trusted {name} as it is now\n"), None));
    }
    let pane = asker.map(crate::control::Asker::name);
    let found = match (run, pane) {
        (Some(id), _) => load(root, id).map(|r| {
            let node = pane
                .and_then(|p| {
                    r.nodes
                        .iter()
                        .find(|(_, st)| st.agent.as_deref() == Some(p))
                        .map(|(id, _)| id.clone())
                })
                .unwrap_or_default();
            (r, node)
        }),
        (None, Some(p)) => {
            run_of(root, p).ok_or(format!("{p} isn't a node's agent of any run that's going"))
        }
        (None, None) => Err("which run? name it: trix workflow … <run>".into()),
    };
    let (r, mine) = found?;
    let id = r.id.clone();
    match op {
        Op::Submit { node, report } => {
            let want = r
                .nodes
                .get(&node)
                .and_then(|st| st.agent.clone())
                .unwrap_or_else(|| r.pane(&node));
            if let Some(who) = pane.filter(|w| *w != want) {
                return Err(format!(
                    "only {want} hands in {node}'s report for run {id}, not {who}"
                ));
            }
            let kept = check_report(&r, &node, &report)
                .map_err(|e| format!("{e}\nFix the report and submit it again."))?;
            std::fs::create_dir_all(dir_of(root, &id).join("reports"))
                .map_err(|e| e.to_string())?;
            super::store::write_atomic(&report_file(root, &id, &node), kept.as_bytes())?;
            Ok((format!("{node}'s report taken for run {id}\n"), None))
        }
        Op::Ask { text } => {
            if mine.is_empty() {
                return Err(format!(
                    "only a node's agent of run {id} asks its supervisor"
                ));
            }
            if text.trim().is_empty() {
                return Err("ask: the question goes in on stdin".into());
            }
            // A mention reaches an agent node's inbox, and nothing else has one.
            for name in prompt::mentions(&text) {
                match r.workflow.nodes.get(&name) {
                    Some(n) if n.is_agent() => {}
                    Some(n) => {
                        return Err(format!(
                            "@{name} is a {}: it has no inbox, so a mention can't reach it. Mention an agent node, or leave it out",
                            n.kind
                        ));
                    }
                    None => return Err(format!("@{name} isn't a node of run {id}")),
                }
            }
            let dir = dir_of(root, &id).join("asks");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let n = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
            let q = Question {
                from: mine.clone(),
                text: text.trim().to_string(),
            };
            super::store::write_atomic(
                &dir.join(format!("{:04}-{mine}.json", n + 1)),
                &serde_json::to_vec(&q).map_err(|e| e.to_string())?,
            )?;
            let to = r
                .workflow
                .of_kind("supervisor")
                .map_or("the user".to_string(), |(s, _)| s.to_string());
            Ok((
                format!("asked {to}: the answer comes as your next prompt\n"),
                None,
            ))
        }
        Op::Decide { tool, command, cwd } => {
            if mine.is_empty() {
                return Err("not a node's agent".into());
            }
            Ok(
                match super::policy::decide(&r, &mine, &tool, &command, &cwd) {
                    Ok(rule) => {
                        let sha = r.nodes[&mine].start.clone().unwrap_or_default();
                        let entry = serde_json::json!({"what": format!("{tool}: {command}"), "kind": "permission", "node": mine,
                        "approver": "rule", "rule": format!("approvals.permissions[{rule}]"), "tool": tool, "command": command,
                        "cwd": cwd, "sha": sha});
                        super::policy::audit(root, &r, entry, crate::prices::now());
                        ("allow\n".to_string(), None)
                    }
                    // Not allowed by a rule: the prompt is yours, as it always was.
                    Err(why) => (format!("ask: {why}\n"), None),
                },
            )
        }
        Op::Start => {
            super::policy::agent_start(policy, root, &r, asker)?;
            Ok((format!("run {id}: starting\n"), None))
        }
        Op::Trust { .. } => unreachable!(),
        // Yours only, from every agent pane: a gate, and what a run asks.
        Op::Approve | Op::Answer { .. } | Op::Revoke if asker.is_some() => Err(format!(
            "{} can't approve, answer or revoke a run: the user does, in the run's pane, or with trix workflow approve from a terminal",
            asker.map(|a| a.said()).unwrap_or_default()
        )),
        Op::Abort | Op::Pause | Op::Resume if asker.is_some() => {
            super::policy::agent_may(policy, &r, asker, &op)?;
            super::policy::write_control(root, &id, &op)?;
            Ok((format!("run {id}: {}\n", super::policy::op_name(&op)), None))
        }
        _ if r.state.over() => Err(format!("run {id} is already {}", r.state.label())),
        Op::Answer { key: 'y' } => {
            Err("a merge is only ever trix workflow approve, or y in the run's pane".into())
        }
        Op::Answer { key } => {
            let Some(a) = r.ask else {
                return Err(format!(
                    "run {id} isn't asking anything: it's {}",
                    r.state.label()
                ));
            };
            if !a.keys().iter().any(|(k, _)| *k == key) {
                let list: Vec<String> = a.keys().iter().map(|(k, w)| format!("{k}: {w}")).collect();
                return Err(format!(
                    "run {id} asks: {}. '{key}' isn't an answer to it ({})",
                    r.why,
                    list.join(" · ")
                ));
            }
            Ok((format!("run {id}: answered {key}\n"), Some(key)))
        }
        Op::Approve if !matches!(r.ask, Some(Ask::Merge | Ask::Floor)) => Err(format!(
            "run {id} isn't waiting for your approval: it's {}",
            r.state.label()
        )),
        Op::Approve => Ok((format!("run {id}: approved\n"), Some('y'))),
        Op::Abort if r.ask.is_some() => Ok((format!("run {id}: aborting\n"), Some('a'))),
        Op::Abort | Op::Pause | Op::Resume | Op::Revoke => {
            super::policy::write_control(root, &id, &op)?;
            Ok((format!("run {id}: {}\n", super::policy::op_name(&op)), None))
        }
    }
}
