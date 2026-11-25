//! `trix triage <PR>`: a case. Three agents, each with one job, take a pull
//! request from findings to a merge, and trix runs the case, not an agent.
//!
//! - The **Reviewer** reads the PR in a checkout of its own and reports
//!   what's wrong: file, line, how it fails.
//! - The **Fixer** fixes each finding on the PR's branch, with a test, and
//!   pushes.
//! - The **Gatekeeper**, in a fresh checkout and without the Fixer's
//!   reasoning, checks each fix and that CI is green: pass, or changes,
//!   which go back to the Fixer (`max_rounds`).
//! - Then trix merges, squashed, the commit the Gatekeeper checked, once CI
//!   is green and you say so. No agent can merge or approve.
//!
//! The case runs in a pane of its own (`trix case run <id>`), which is where
//! it asks you things: it shows as needing you, like an agent. Agents hand
//! in their work with `trix case submit`, which the server checks against
//! the case before it's taken, so a report that doesn't fit goes back to the
//! agent that wrote it, at once.
//!
//! Each case is a folder in `<state>/cases/<id>/`: `case.json` and the
//! reports (`reviewer-1.json`, `fixer-1.json`, `gatekeeper-1.json`, …).
//!
//! Who's asking is the kernel's word, not the environment's: the server
//! walks the asking process's parents to the pane it runs in. A report is
//! taken only from its role's agent, and an agent's pane can't approve or
//! abort; `trix case approve` has the server type the key into the case's
//! pane, the one place the runner takes answers from.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ───────────────────────────── the case ─────────────────────────────

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Reviewer,
    Fixer,
    Gatekeeper,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Reviewer => "reviewer",
            Role::Fixer => "fixer",
            Role::Gatekeeper => "gatekeeper",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        [Role::Reviewer, Role::Fixer, Role::Gatekeeper].into_iter().find(|r| r.name() == s)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Reviewing,
    Fixing,
    Verifying,
    /// Waiting for CI, then for you.
    Gate,
    Merged,
    Aborted,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Reviewing => "reviewing",
            Stage::Fixing => "fixing",
            Stage::Verifying => "verifying",
            Stage::Gate => "at the merge gate",
            Stage::Merged => "merged",
            Stage::Aborted => "aborted",
        }
    }
    pub fn over(self) -> bool {
        matches!(self, Stage::Merged | Stage::Aborted)
    }
    /// The role whose report the stage waits for.
    fn role(self) -> Option<Role> {
        match self {
            Stage::Reviewing => Some(Role::Reviewer),
            Stage::Fixing => Some(Role::Fixer),
            Stage::Verifying => Some(Role::Gatekeeper),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The branch, in this repository.
    pub head: String,
    pub base: String,
    /// The head commit when the case started.
    pub sha: String,
}

/// What the case asks you, and the keys that answer it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    /// The Reviewer found nothing: m takes it to the gate, a aborts.
    NothingFound,
    /// The rounds are used up, with changes still asked for: r for one more.
    RoundLimit,
    /// CI failed at the gate, with no rounds left: r for one more.
    CiFailed,
    /// The case's budget is spent: b for as much again.
    Budget,
    /// An agent stopped without its report, or exited: n to nudge it (or
    /// start it again).
    NoReport,
    /// Everything passed: y merges.
    Merge,
    /// The merge failed: y tries again.
    MergeFailed,
    /// Setting a role up failed (a worktree, an agent): r tries again.
    Setup,
}

impl Ask {
    pub fn keys(self) -> &'static [(char, &'static str)] {
        match self {
            Ask::NothingFound => &[('m', "go to the merge gate anyway"), ('a', "abort")],
            Ask::RoundLimit | Ask::CiFailed => &[('r', "one more round"), ('a', "abort")],
            Ask::Budget => &[('b', "add the budget again"), ('a', "abort")],
            Ask::NoReport => &[('n', "nudge it again"), ('a', "abort")],
            Ask::Merge => &[('y', "merge"), ('a', "abort")],
            Ask::MergeFailed | Ask::Setup => &[('r', "try again"), ('a', "abort")],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoleAgent {
    pub agent: String,
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Case {
    pub id: String,
    pub pr: Pr,
    /// The main checkout.
    pub repo: PathBuf,
    pub stage: Stage,
    pub round: u32,
    pub max_rounds: u32,
    pub budget: f64,
    /// What b adds when the budget is spent: the budget the case began with.
    #[serde(default = "ten")]
    pub budget_step: f64,
    pub reviewer: RoleAgent,
    pub fixer: RoleAgent,
    pub gatekeeper: RoleAgent,
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// The findings this round's fix answers.
    #[serde(default)]
    pub open: Vec<String>,
    #[serde(default)]
    pub fix: Option<FixReport>,
    #[serde(default)]
    pub verdict: Option<Verdict>,
    /// The commit that was checked (by the Gatekeeper, or the Reviewer when
    /// it found nothing): the only one that's merged.
    #[serde(default)]
    pub checked: Option<String>,
    /// The PR had CI checks when the case began: then a commit with none
    /// yet is waiting for them, not a repo without CI.
    #[serde(default = "yes")]
    pub ci_expected: bool,
    /// The agent working on this stage, once it's started.
    #[serde(default)]
    pub agent: Option<String>,
    /// Agents done with their part, to close once they've stopped working.
    #[serde(default)]
    pub closing: Vec<String>,
    /// What closed agents spent.
    #[serde(default)]
    pub spent_closed: f64,
    /// The agent was reminded once to hand in its report.
    #[serde(default)]
    pub nudged: bool,
    #[serde(default)]
    pub ask: Option<Ask>,
    /// Why it asks, in a line.
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub log: Vec<(u64, String)>,
    pub created: u64,
}

impl Case {
    pub fn new(id: &str, pr: Pr, repo: PathBuf, opts: &crate::config::TriageOpts, now: u64) -> Case {
        let role = |r: &crate::config::Role| RoleAgent { agent: r.agent.clone(), args: r.args.clone() };
        Case {
            id: id.into(),
            pr,
            repo,
            stage: Stage::Reviewing,
            round: 1,
            max_rounds: opts.max_rounds,
            budget: opts.budget,
            budget_step: opts.budget,
            reviewer: role(&opts.reviewer),
            fixer: role(&opts.fixer),
            gatekeeper: role(&opts.gatekeeper),
            findings: Vec::new(),
            open: Vec::new(),
            fix: None,
            verdict: None,
            checked: None,
            ci_expected: true,
            agent: None,
            closing: Vec::new(),
            spent_closed: 0.0,
            nudged: false,
            ask: None,
            why: String::new(),
            log: Vec::new(),
            created: now,
        }
    }

    /// The agent name a role gets: `triage-12-review`, `triage-12-gate-2`.
    pub fn agent_name(&self, role: Role) -> String {
        match role {
            Role::Reviewer => format!("{}-review", self.id),
            Role::Fixer => format!("{}-fix", self.id),
            Role::Gatekeeper => format!("{}-gate-{}", self.id, self.round),
        }
    }

    fn role_agent(&self, role: Role) -> &RoleAgent {
        match role {
            Role::Reviewer => &self.reviewer,
            Role::Fixer => &self.fixer,
            Role::Gatekeeper => &self.gatekeeper,
        }
    }

    /// "fixing · round 2", "needs you: …".
    pub fn status(&self) -> String {
        if self.ask.is_some() {
            return format!("needs you: {}", self.why);
        }
        match self.stage {
            Stage::Fixing | Stage::Verifying if self.round > 1 => format!("{} · round {}", self.stage.label(), self.round),
            s => s.label().into(),
        }
    }
}

fn ten() -> f64 {
    10.0
}

fn yes() -> bool {
    true
}

// ───────────────────────────── reports ─────────────────────────────

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Important,
    Minor,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub file: String,
    #[serde(default)]
    pub line: Option<u64>,
    pub severity: Severity,
    pub summary: String,
    /// How it fails: the inputs or state, and the wrong result.
    pub failure: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Findings {
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Fixed,
    NotABug,
    WontFix,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FixOutcome {
    pub id: String,
    pub outcome: Outcome,
    #[serde(default)]
    pub note: Option<String>,
    /// The test that covers it.
    #[serde(default)]
    pub test: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FixReport {
    pub commits: Vec<String>,
    /// What was run, and how it went.
    pub tests: String,
    pub findings: Vec<FixOutcome>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    Pass,
    Changes,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ci {
    Green,
    Red,
    Pending,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub verdict: Pass,
    pub findings: Vec<Check>,
    /// Anything the fixes broke, or the Reviewer missed.
    #[serde(default)]
    pub new: Vec<Finding>,
    pub ci: Ci,
    #[serde(default)]
    pub notes: Option<String>,
}

fn check_finding(f: &Finding, ids: &mut BTreeSet<String>) -> Result<(), String> {
    if f.id.trim().is_empty() || f.summary.trim().is_empty() || f.failure.trim().is_empty() || f.file.trim().is_empty() {
        return Err(format!("finding '{}': id, file, summary and failure must all be filled in", f.id));
    }
    if !ids.insert(f.id.clone()) {
        return Err(format!("finding id '{}' is used twice: each needs its own", f.id));
    }
    Ok(())
}

/// Each of `want` answered exactly once in `got`.
fn covers(what: &str, want: &[String], got: &[&str]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for id in got {
        if !want.iter().any(|w| w == id) {
            return Err(format!("{what}: '{id}' isn't one of this round's findings ({})", want.join(", ")));
        }
        if !seen.insert(*id) {
            return Err(format!("{what}: '{id}' is answered twice"));
        }
    }
    let missing: Vec<&str> = want.iter().map(String::as_str).filter(|w| !seen.contains(w)).collect();
    if !missing.is_empty() {
        return Err(format!("{what}: no answer for {}", missing.join(", ")));
    }
    Ok(())
}

/// A report `role` hands in, checked against where the case is. `Ok` is
/// the report as it will be kept.
pub fn check_report(c: &Case, role: Role, text: &str) -> Result<String, String> {
    if c.stage.over() {
        return Err(format!("case {} is {}: it takes no more reports", c.id, c.stage.label()));
    }
    if c.stage.role() != Some(role) || c.ask.is_some_and(|a| a == Ask::RoundLimit) {
        return Err(format!("case {} isn't waiting for the {}'s report: it's {}", c.id, role.name(), c.status()));
    }
    let bad = |e: serde_json::Error| format!("the report isn't what the {} hands in: {e}", role.name());
    let kept = match role {
        Role::Reviewer => {
            let r: Findings = serde_json::from_str(text).map_err(bad)?;
            let mut ids = BTreeSet::new();
            for f in &r.findings {
                check_finding(f, &mut ids)?;
            }
            serde_json::to_string_pretty(&r)
        }
        Role::Fixer => {
            let r: FixReport = serde_json::from_str(text).map_err(bad)?;
            covers("findings", &c.open, &r.findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>())?;
            if let Some(bad) = r.commits.iter().find(|sha| !(7..=40).contains(&sha.len()) || !sha.bytes().all(|b| b.is_ascii_hexdigit())) {
                return Err(format!("commits: '{bad}' isn't a commit id (7 to 40 hex digits)"));
            }
            if r.findings.iter().any(|f| f.outcome == Outcome::Fixed) && r.commits.is_empty() {
                return Err("commits: a finding is fixed, but no commit is named: push the fix and list its commits".into());
            }
            if r.findings.iter().any(|f| f.outcome != Outcome::Fixed && f.note.as_deref().is_none_or(|n| n.trim().is_empty())) {
                return Err("findings: say why in `note` for each one that isn't fixed".into());
            }
            if r.tests.trim().is_empty() {
                return Err("tests: say what you ran and how it went".into());
            }
            serde_json::to_string_pretty(&r)
        }
        Role::Gatekeeper => {
            let r: Verdict = serde_json::from_str(text).map_err(bad)?;
            covers("findings", &c.open, &r.findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>())?;
            let mut ids: BTreeSet<String> = c.findings.iter().map(|f| f.id.clone()).collect();
            for f in &r.new {
                check_finding(f, &mut ids)?;
            }
            let failing = r.findings.iter().any(|f| !f.ok) || r.new.iter().any(|f| f.severity == Severity::Important) || r.ci == Ci::Red;
            if r.verdict == Pass::Pass && failing {
                return Err("verdict: pass needs every finding ok, nothing important that's new, and CI not red; otherwise it's changes".into());
            }
            if r.verdict == Pass::Changes && !failing {
                return Err("verdict: changes needs a finding that isn't ok, something important that's new, or red CI to say what to change".into());
            }
            if r.findings.iter().any(|f| !f.ok && f.reason.as_deref().is_none_or(|n| n.trim().is_empty())) {
                return Err("findings: say why in `reason` for each one that isn't ok".into());
            }
            serde_json::to_string_pretty(&r)
        }
    };
    kept.map_err(|e| e.to_string())
}

// ───────────────────────────── on disk ─────────────────────────────

pub fn root() -> PathBuf {
    crate::worktree::state_dir().join("cases")
}

pub fn dir_of(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

pub fn load(root: &Path, id: &str) -> Result<Case, String> {
    let p = dir_of(root, id).join("case.json");
    let b = std::fs::read(&p).map_err(|_| format!("no case named '{id}' (trix case ls)"))?;
    serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn store(root: &Path, c: &Case) -> Result<(), String> {
    let d = dir_of(root, &c.id);
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    write_atomic(&d.join("case.json"), &serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?)
}

fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, p)).map_err(|e| format!("{}: {e}", p.display()))
}

fn report_path(root: &Path, id: &str, role: Role, round: u32) -> PathBuf {
    dir_of(root, id).join(format!("{}-{round}.json", role.name()))
}

pub fn all(root: &Path) -> Vec<Case> {
    let mut v: Vec<Case> = std::fs::read_dir(root).into_iter().flatten().flatten().filter_map(|e| load(root, &e.file_name().to_string_lossy()).ok()).collect();
    v.sort_by_key(|c| std::cmp::Reverse(c.created));
    v
}

/// A free id for PR `n`: `triage-12`, then `triage-12-2` once that one is
/// over. One still running is an error: two cases can't share a branch.
pub fn new_id(root: &Path, n: u64) -> Result<String, String> {
    let base = format!("triage-{n}");
    for i in 1.. {
        let id = if i == 1 { base.clone() } else { format!("{base}-{i}") };
        match load(root, &id) {
            Err(_) => return Ok(id),
            Ok(c) if !c.stage.over() => return Err(format!("case {id} is already running for PR #{n}: trix case show {id}")),
            Ok(_) => {}
        }
    }
    unreachable!()
}

// ───────────────────────────── the server's part ─────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Op {
    Submit { role: Role, report: String },
    Approve,
    Abort,
}

/// `trix case submit / approve / abort`, in the server. `agent`: the agent
/// whose pane the request comes from, as the kernel says (a shell pane, or
/// a terminal outside trix, is you). A report is taken only from its role's
/// agent, and no agent approves or aborts. `Ok` holds what to reply, and
/// for approve and abort the key to type into the case's pane.
pub fn handle(root: &Path, agent: Option<&str>, id: &str, op: Op) -> Result<(String, Option<char>), String> {
    let c = load(root, id)?;
    match op {
        Op::Submit { role, report } => {
            let want = c.agent_name(role);
            if let Some(who) = agent.filter(|w| *w != want) {
                return Err(format!("only {want} hands in the {}'s report for case {id}, not {who}", role.name()));
            }
            let kept = check_report(&c, role, &report).map_err(|e| format!("{e}\nFix the report and submit it again."))?;
            write_atomic(&report_path(root, id, role, c.round), kept.as_bytes())?;
            Ok((format!("{} report taken for case {id}\n", role.name()), None))
        }
        Op::Approve | Op::Abort if agent.is_some() => Err("an agent can't approve or abort a case: the user does, in the case's pane, or with trix case approve from a terminal".into()),
        Op::Approve | Op::Abort if c.stage.over() => Err(format!("case {id} is already {}", c.stage.label())),
        Op::Approve if c.ask != Some(Ask::Merge) => Err(format!("case {id} isn't waiting to be merged: it's {}", c.status())),
        Op::Approve => Ok((format!("case {id}: merging\n"), Some('y'))),
        Op::Abort => Ok((format!("case {id}: aborting\n"), Some('a'))),
    }
}

// ───────────────────────────── the runner ─────────────────────────────

/// An agent as the runner sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub name: String,
    /// `trix ls --json`'s: working, needs-you, done, idle, starting, exited.
    pub state: String,
    pub cost: f64,
}

/// Everything the runner does outside itself, so a test can play it.
pub trait Env {
    fn now(&self) -> u64;
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, String>;
    fn gh(&mut self, repo: &Path, args: &[&str]) -> Result<String, String>;
    fn spawn(&mut self, name: &str, agent: &RoleAgent, cwd: &Path, prompt: &str) -> Result<(), String>;
    fn send(&mut self, name: &str, text: &str) -> Result<(), String>;
    /// Close an agent; `Err` while it's still working.
    fn close(&mut self, name: &str, clean: bool) -> Result<(), String>;
    fn agents(&mut self) -> Result<Vec<Seen>, String>;
    /// A key typed into the pane: yours, or `trix case approve / abort`'s,
    /// which the server types there.
    fn answer(&mut self) -> Option<char>;
    /// The pane's state and activity: `Working`, `Question` or `Finished`.
    fn say(&mut self, event: &str, detail: &str);
    fn print(&mut self, line: &str);
}

fn note(c: &mut Case, env: &mut dyn Env, text: impl Into<String>) {
    let text = text.into();
    let at = env.now();
    let (_, _, secs) = crate::ledger::local(at);
    env.print(&format!("{:02}:{:02}  {text}", secs / 3600, secs / 60 % 60));
    c.log.push((at, text));
}

fn ask(c: &mut Case, env: &mut dyn Env, a: Ask, why: impl Into<String>) {
    c.ask = Some(a);
    c.why = why.into();
    let keys: Vec<String> = a.keys().iter().map(|(k, w)| format!("{k}: {w}")).collect();
    note(c, env, format!("needs you: {}  [{}]", c.why, keys.join(" · ")));
    env.say("Question", &format!("{} · {}", c.id, c.why));
}

fn worktree(c: &Case, role: Role) -> PathBuf {
    let dir = match role {
        Role::Gatekeeper => format!("{}-gate-{}", c.id, c.round),
        r => format!("{}-{}", c.id, if r == Role::Reviewer { "review" } else { "fix" }),
    };
    c.repo.join(".trix/worktrees").join(dir)
}

/// The PR's branch as origin has it now: fetched, and its commit.
fn fetch_head(c: &Case, env: &mut dyn Env) -> Result<String, String> {
    let head = &c.pr.head;
    env.git(&c.repo, &["fetch", "--quiet", "origin", &format!("+refs/heads/{head}:refs/remotes/origin/{head}")])?;
    Ok(env.git(&c.repo, &["rev-parse", &format!("refs/remotes/origin/{head}")])?.trim().to_string())
}

/// Make the role's checkout, if it isn't there yet.
fn make_worktree(c: &Case, role: Role, env: &mut dyn Env) -> Result<PathBuf, String> {
    let path = worktree(c, role);
    if path.is_dir() {
        return Ok(path);
    }
    let p = path.display().to_string();
    match role {
        Role::Reviewer => env.git(&c.repo, &["worktree", "add", "--quiet", "--detach", &p, &c.pr.sha])?,
        Role::Fixer => {
            fetch_head(c, env)?;
            env.git(&c.repo, &["worktree", "add", "--quiet", "-B", &format!("trix/{}-fix", c.id), &p, &format!("refs/remotes/origin/{}", c.pr.head)])?
        }
        Role::Gatekeeper => {
            let sha = fetch_head(c, env)?;
            env.git(&c.repo, &["worktree", "add", "--quiet", "--detach", &p, &sha])?
        }
    };
    Ok(path)
}

const SUBMIT_NOTE: &str = "Hand it in by piping the JSON on stdin, with a heredoc, so no file lands in the checkout:";

fn submit_line(c: &Case, role: Role) -> String {
    format!("trix case submit {} {} <<'EOF'\n<the JSON>\nEOF", c.id, role.name())
}

/// What the role is told as it starts.
pub fn prompt(c: &Case, role: Role) -> String {
    let pr = &c.pr;
    let head = format!("You're the {} in trix triage case {}, for pull request #{} \"{}\" ({}): branch {} into {}.", role.name(), c.id, pr.number, pr.title, pr.url, pr.head, pr.base);
    let rules = "Follow the repository's own rules (CLAUDE.md, AGENTS.md). If the submit command says the report is wrong, fix it and submit again.";
    match role {
        Role::Reviewer => format!(
            "{head}\n\nYour folder is a detached checkout of the PR's head ({sha}). Find the real bugs this PR brings in or leaves: wrong results, crashes, lost data, security holes, broken tests. Read the diff (git diff origin/{base}...HEAD) and the code around it; build and run the tests if that helps. Don't edit, commit or push anything: you only report. Report only what you're confident of, no style points.\n\n\
             Each finding: an id (F1, F2, …), the file, the line, a severity (important: a real bug; minor: worth fixing, low risk), a one-line summary, and how it fails (the inputs or state, and the wrong result). If you find nothing, hand in {{\"findings\": []}}.\n\n\
             {SUBMIT_NOTE}\n{submit}\n\nThe JSON: {{\"findings\": [{{\"id\": \"F1\", \"file\": \"src/x.rs\", \"line\": 12, \"severity\": \"important\", \"summary\": \"…\", \"failure\": \"…\"}}], \"notes\": \"optional\"}}\n\n{rules}",
            sha = pr.sha,
            base = pr.base,
            submit = submit_line(c, role),
        ),
        Role::Fixer => format!(
            "{head}\n\nYour folder is on branch trix/{id}-fix, which starts at the PR's branch as origin has it. The Reviewer found:\n\n{findings}\n\n\
             For each finding, fix it with a test that fails without the fix, or, if it isn't a bug, say why. Commit, then push to the PR's branch with: git push origin HEAD:{branch} (never force). Don't merge the PR.\n\n\
             Then hand in what you did. {SUBMIT_NOTE}\n{submit}\n\nThe JSON: {{\"commits\": [\"<sha>\"], \"tests\": \"what you ran, and how it went\", \"findings\": [{{\"id\": \"F1\", \"outcome\": \"fixed\", \"test\": \"the test's name\"}}, {{\"id\": \"F2\", \"outcome\": \"not_a_bug\", \"note\": \"why\"}}]}}. outcome is fixed, not_a_bug or wont_fix; each finding gets one, and a note for any that isn't fixed. trix checks the commits are on origin before it takes the report.\n\n{rules}",
            id = c.id,
            findings = serde_json::to_string_pretty(&open_findings(c)).unwrap_or_default(),
            branch = pr.head,
            submit = submit_line(c, role),
        ),
        Role::Gatekeeper => format!(
            "{head}\n\nYour folder is a fresh checkout of the PR's branch after the Fixer's work ({sha}). You check that work on your own: you haven't seen the Fixer's reasoning, and you don't need it.\n\n\
             The findings:\n{findings}\n\nWhat the Fixer says it did:\n{fix}\n\n\
             For each finding, is it really fixed (read the code and the test, run the tests)? For one called not_a_bug or wont_fix, is that right? Did the fixes break anything else? Check CI with: gh pr checks {n}. Don't edit, commit or push anything.\n\n\
             {SUBMIT_NOTE}\n{submit}\n\nThe JSON: {{\"verdict\": \"pass\", \"findings\": [{{\"id\": \"F1\", \"ok\": true}}, {{\"id\": \"F2\", \"ok\": false, \"reason\": \"why, and what to change\"}}], \"new\": [], \"ci\": \"green\"}}. verdict is pass or changes; ci is green, red, pending or none; new lists anything else you found, as findings with new ids. Pass only when every finding is ok, nothing important is new, and CI isn't red.\n\n{rules}",
            sha = c.checked.as_deref().unwrap_or("its head"),
            findings = serde_json::to_string_pretty(&open_findings(c)).unwrap_or_default(),
            fix = c.fix.as_ref().and_then(|f| serde_json::to_string_pretty(f).ok()).unwrap_or_default(),
            n = pr.number,
            submit = submit_line(c, role),
        ),
    }
}

fn open_findings(c: &Case) -> Vec<&Finding> {
    c.findings.iter().filter(|f| c.open.contains(&f.id)).collect()
}

/// Start the stage's agent, if it isn't running: its checkout, then the
/// agent. False when that failed (the case then asks you).
fn ensure_agent(c: &mut Case, role: Role, seen: &[Seen], env: &mut dyn Env) -> bool {
    let name = c.agent_name(role);
    if seen.iter().any(|a| a.name == name) {
        c.agent = Some(name);
        return true;
    }
    let started = make_worktree(c, role, env).and_then(|path| {
        if role == Role::Gatekeeper {
            c.checked = Some(env.git(&path, &["rev-parse", "HEAD"])?.trim().to_string());
        }
        let p = prompt(c, role);
        env.spawn(&name, c.role_agent(role), &path, &p).map(|_| path)
    });
    match started {
        Ok(path) => {
            note(c, env, format!("started the {} ({}, {name}) in {}", role.name(), c.role_agent(role).agent, crate::app::tilde(&path)));
            c.agent = Some(name);
            c.nudged = false;
            true
        }
        Err(e) => {
            ask(c, env, Ask::Setup, format!("couldn't start the {}: {e}", role.name()));
            false
        }
    }
}

/// The current agent's work is in: close it once it stops, and go on.
fn done_with(c: &mut Case, name: &str) {
    c.agent = None;
    if !c.closing.iter().any(|n| n == name) {
        c.closing.push(name.to_string());
    }
}

fn cost_of(c: &Case, seen: &[Seen]) -> f64 {
    let prefix = format!("{}-", c.id);
    c.spent_closed + seen.iter().filter(|a| a.name.starts_with(&prefix)).map(|a| a.cost).sum::<f64>()
}

/// Runs `gh` with these arguments in this folder.
type Gh<'a> = dyn FnMut(&Path, &[&str]) -> Result<String, String> + 'a;

/// The check runs on `sha`, as GitHub has them.
pub fn check_runs(repo: &Path, sha: &str, gh: &mut Gh) -> Result<Vec<serde_json::Value>, String> {
    let out = gh(repo, &["api", &format!("repos/{{owner}}/{{repo}}/commits/{sha}/check-runs?per_page=100")])?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(v["check_runs"].as_array().cloned().unwrap_or_default())
}

/// CI on the commit that was checked (not whatever the PR's head is now),
/// and which checks failed. Right after a push GitHub may have no checks
/// for it yet: for a PR that had them, that's waiting, not "no CI".
fn ci(c: &Case, env: &mut dyn Env) -> (Ci, Vec<String>) {
    let sha = c.checked.clone().unwrap_or_else(|| c.pr.sha.clone());
    let Ok(runs) = check_runs(&c.repo, &sha, &mut |r, a| env.gh(r, a)) else { return (Ci::Pending, Vec::new()) };
    let name = |v: &serde_json::Value| v["name"].as_str().unwrap_or("?").to_string();
    let failed: Vec<String> = runs.iter().filter(|v| matches!(v["conclusion"].as_str(), Some("failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure"))).map(name).collect();
    if !failed.is_empty() {
        (Ci::Red, failed)
    } else if runs.iter().any(|v| v["status"] != "completed") {
        (Ci::Pending, Vec::new())
    } else if runs.is_empty() {
        (if c.ci_expected { Ci::Pending } else { Ci::None }, Vec::new())
    } else {
        (Ci::Green, Vec::new())
    }
}

/// Another round: the Fixer gets what to change, and answers these.
fn next_round(c: &mut Case, env: &mut dyn Env, what: String, open: Vec<String>) {
    c.round += 1;
    c.open = open;
    c.stage = Stage::Fixing;
    c.fix = None;
    c.nudged = false;
    note(c, env, format!("round {}: back to the fixer", c.round));
    let fixer = c.agent_name(Role::Fixer);
    // Never started (nothing was found, and CI failed): it starts from its
    // prompt, which lists what's open.
    if !env.agents().unwrap_or_default().iter().any(|a| a.name == fixer && a.state != "exited") {
        c.agent = None;
        return;
    }
    let text = format!(
        "Round {} of case {}. {what}\n\nAnswer these findings: {}. Fix, commit, push with git push origin HEAD:{} (never force), and hand in a new report the same way:\n{}",
        c.round,
        c.id,
        c.open.join(", "),
        c.pr.head,
        submit_line(c, Role::Fixer)
    );
    c.agent = Some(fixer.clone());
    if let Err(e) = env.send(&fixer, &text) {
        ask(c, env, Ask::NoReport, format!("couldn't tell the fixer about round {}: {e}", c.round));
    }
}

/// One look at the case: take what came in, start what's next, ask you
/// what only you decide. Called every couple of seconds; returns whether
/// the case changed.
pub fn step(c: &mut Case, root: &Path, env: &mut dyn Env) -> bool {
    let before = c.clone();
    let mut seen = env.agents().unwrap_or_default();
    // Close those done with their part, once they've stopped working.
    let mut closed = Vec::new();
    for name in c.closing.clone() {
        match seen.iter().find(|a| a.name == name) {
            None => closed.push(name),
            Some(a) if a.state != "working" && a.state != "starting" => {
                if env.close(&name, false).is_ok() {
                    c.spent_closed += a.cost;
                    closed.push(name);
                }
            }
            Some(_) => {}
        }
    }
    c.closing.retain(|n| !closed.contains(n));
    // Their cost is in spent_closed now, not to be counted again.
    seen.retain(|a| !closed.contains(&a.name));
    if c.stage.over() {
        return *c != before;
    }

    // The report came after all: no need for your answer.
    if c.ask == Some(Ask::NoReport) && c.stage.role().is_some_and(|r| take_report(root, c, r).is_some()) {
        c.ask = None;
        c.why.clear();
        note(c, env, "the report came in");
    }
    if let Some(a) = c.ask {
        let Some(key) = env.answer() else { return *c != before };
        if key == 'a' {
            abort(c, env, &seen);
            return true;
        }
        if !a.keys().iter().any(|(k, _)| *k == key) {
            return *c != before;
        }
        c.ask = None;
        c.why.clear();
        match a {
            Ask::NothingFound => {
                note(c, env, "nothing to fix: to the merge gate, as you said");
                // What the reviewer read: nothing pushed since is merged.
                c.checked = Some(c.pr.sha.clone());
                c.stage = Stage::Gate;
            }
            Ask::RoundLimit | Ask::CiFailed => {
                c.max_rounds = c.max_rounds.max(c.round + 1);
                let open = c.open.clone();
                let why = match (a, &c.verdict) {
                    (Ask::RoundLimit, Some(v)) => verdict_text(v),
                    _ => format!("CI fails on the PR: see gh pr checks {}.", c.pr.number),
                };
                next_round(c, env, why, open);
            }
            Ask::Budget => {
                c.budget += c.budget_step;
                note(c, env, format!("budget raised to {}", crate::cost::usd(c.budget)));
            }
            Ask::NoReport => {
                c.nudged = false;
                let Some(name) = c.agent.clone() else { return true };
                if seen.iter().any(|a| a.name == name && a.state == "exited") {
                    // Started again, from its prompt, at the next look.
                    let _ = env.close(&name, false);
                    c.agent = None;
                    note(c, env, format!("starting {name} again"));
                } else if let Err(e) = nudge(c, env, &name) {
                    note(c, env, format!("couldn't nudge {name}: {e}"));
                }
            }
            Ask::Merge | Ask::MergeFailed => merge(c, env),
            Ask::Setup => c.agent = None,
        }
        return true;
    }

    let spent = cost_of(c, &seen);
    if spent >= c.budget {
        ask(c, env, Ask::Budget, format!("the budget is spent: {} of {}", crate::cost::usd(spent), crate::cost::usd(c.budget)));
        return true;
    }

    match c.stage {
        Stage::Reviewing | Stage::Fixing | Stage::Verifying => {
            let role = c.stage.role().unwrap();
            // Just started: the agents seen were looked at before it was.
            let started = c.agent.is_none() && !seen.iter().any(|a| a.name == c.agent_name(role));
            if c.agent.is_none() && !ensure_agent(c, role, &seen, env) {
                return true;
            }
            if let Some(text) = take_report(root, c, role) {
                take(c, root, env, role, &text);
            } else if let Some(name) = c.agent.clone().filter(|_| !started) {
                watch(c, env, &seen, &name, role);
            }
        }
        Stage::Gate => gate(c, env),
        Stage::Merged | Stage::Aborted => {}
    }
    // At the gate, it says what it waits for itself.
    if c.ask.is_none() && !c.stage.over() && c.stage != Stage::Gate {
        env.say("Working", &format!("{} · {}", c.id, c.status()));
    }
    *c != before
}

fn take_report(root: &Path, c: &Case, role: Role) -> Option<String> {
    std::fs::read_to_string(report_path(root, &c.id, role, c.round)).ok()
}

/// A report is in: what it changes.
fn take(c: &mut Case, root: &Path, env: &mut dyn Env, role: Role, text: &str) {
    let name = c.agent.clone().unwrap_or_else(|| c.agent_name(role));
    match role {
        Role::Reviewer => {
            let Ok(r) = serde_json::from_str::<Findings>(text) else { return };
            done_with(c, &name);
            let important = r.findings.iter().filter(|f| f.severity == Severity::Important).count();
            note(c, env, format!("the reviewer found {} ({important} important)", plural(r.findings.len(), "finding")));
            c.open = r.findings.iter().map(|f| f.id.clone()).collect();
            c.findings = r.findings;
            if c.findings.is_empty() {
                ask(c, env, Ask::NothingFound, "the reviewer found nothing to fix");
            } else {
                c.stage = Stage::Fixing;
                c.nudged = false;
            }
        }
        Role::Fixer => {
            if c.fix.is_some() {
                return;
            }
            let Ok(r) = serde_json::from_str::<FixReport>(text) else { return };
            // On origin, or the Gatekeeper would check something else.
            let head = fetch_head(c, env);
            let missing: Vec<&String> = r.commits.iter().filter(|sha| !head.as_ref().is_ok_and(|h| env.git(&c.repo, &["merge-base", "--is-ancestor", sha, h]).is_ok())).collect();
            if !missing.is_empty() || head.is_err() {
                let list = missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                let _ = std::fs::remove_file(report_path(root, &c.id, role, c.round));
                note(c, env, format!("the fixer's commits aren't on origin/{}: {list}", c.pr.head));
                let _ = env.send(&name, &format!("Your report for case {} names commits that aren't on origin/{} yet: {list}. Push them with git push origin HEAD:{} (never force), then hand in the report again.", c.id, c.pr.head, c.pr.head));
                return;
            }
            let fixed = r.findings.iter().filter(|f| f.outcome == Outcome::Fixed).count();
            note(c, env, format!("the fixer fixed {fixed} of {} ({})", r.findings.len(), plural(r.commits.len(), "commit")));
            c.fix = Some(r);
            c.agent = None;
            c.stage = Stage::Verifying;
            c.nudged = false;
        }
        Role::Gatekeeper => {
            let Ok(v) = serde_json::from_str::<Verdict>(text) else { return };
            done_with(c, &name);
            c.findings.extend(v.new.iter().cloned());
            let failing: Vec<String> = v.findings.iter().filter(|f| !f.ok).map(|f| f.id.clone()).chain(v.new.iter().map(|f| f.id.clone())).collect();
            note(c, env, format!("the gatekeeper says {}", if v.verdict == Pass::Pass { "pass".to_string() } else { format!("changes: {}", failing.join(", ")) }));
            let pass = v.verdict == Pass::Pass;
            let what = verdict_text(&v);
            c.verdict = Some(v);
            if pass {
                c.stage = Stage::Gate;
            } else if c.round < c.max_rounds {
                next_round(c, env, what, failing);
            } else {
                c.open = failing;
                ask(c, env, Ask::RoundLimit, format!("{} done and the gatekeeper still asks for changes", plural(c.round as usize, "round")));
            }
        }
    }
}

fn verdict_text(v: &Verdict) -> String {
    let mut s = String::from("The gatekeeper asks for changes:");
    for f in v.findings.iter().filter(|f| !f.ok) {
        s.push_str(&format!("\n- {}: {}", f.id, f.reason.as_deref().unwrap_or("not fixed")));
    }
    for f in &v.new {
        s.push_str(&format!("\n- {} (new, {}): {} — {}:{} — {}", f.id, if f.severity == Severity::Important { "important" } else { "minor" }, f.summary, f.file, f.line.map_or(String::new(), |l| l.to_string()), f.failure));
    }
    if v.ci == Ci::Red {
        s.push_str("\n- CI is red.");
    }
    s
}

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn nudge(c: &mut Case, env: &mut dyn Env, name: &str) -> Result<(), String> {
    let role = c.stage.role().unwrap_or(Role::Reviewer);
    c.nudged = true;
    env.send(name, &format!("trix case {} is waiting for your report as the {}. {SUBMIT_NOTE}\n{}", c.id, role.name(), submit_line(c, role)))
}

/// No report yet: an agent that stopped gets one reminder, then you're asked.
fn watch(c: &mut Case, env: &mut dyn Env, seen: &[Seen], name: &str, role: Role) {
    let Some(a) = seen.iter().find(|a| a.name == name) else {
        // Gone (closed by hand): start it again.
        c.agent = None;
        return;
    };
    match a.state.as_str() {
        "exited" => ask(c, env, Ask::NoReport, format!("the {} ({name}) exited without its report", role.name())),
        "done" | "idle" if !c.nudged => {
            note(c, env, format!("the {} stopped without its report: reminded it", role.name()));
            if nudge(c, env, name).is_err() {
                ask(c, env, Ask::NoReport, format!("the {} ({name}) stopped without its report", role.name()));
            }
        }
        "done" | "idle" => ask(c, env, Ask::NoReport, format!("the {} ({name}) stopped again without its report", role.name())),
        _ => {}
    }
}

fn gate(c: &mut Case, env: &mut dyn Env) {
    let (state, failed) = ci(c, env);
    match state {
        Ci::Pending => env.say("Working", &format!("{} · waiting for CI on {}", c.id, c.checked.as_deref().map_or("the PR", |s| &s[..s.len().min(7)]))),
        Ci::Red => {
            // A finding like any other, so it's fixed and checked the same way.
            let id = format!("CI{}", c.round);
            let what = format!("CI fails on the PR: {}", failed.join(", "));
            note(c, env, what.clone());
            if !c.findings.iter().any(|f| f.id == id) {
                c.findings.push(Finding { id: id.clone(), file: "CI".into(), line: None, severity: Severity::Important, summary: what.clone(), failure: format!("gh pr checks {} shows them failing", c.pr.number) });
            }
            if c.round < c.max_rounds {
                next_round(c, env, format!("{what}. See gh pr checks {}.", c.pr.number), vec![id]);
            } else {
                c.open = vec![id];
                ask(c, env, Ask::CiFailed, format!("CI failed ({}) and the rounds are used up", failed.join(", ")));
            }
        }
        Ci::Green | Ci::None => {
            let ci = if state == Ci::None { "no CI checks on this PR" } else { "CI green" };
            ask(c, env, Ask::Merge, format!("ready to merge PR #{}: {ci}, the gatekeeper passed it", c.pr.number));
        }
    }
}

fn merge(c: &mut Case, env: &mut dyn Env) {
    // CI again, right before: it may have changed while you looked.
    if let (Ci::Red | Ci::Pending, failed) = ci(c, env) {
        ask(c, env, Ask::MergeFailed, if failed.is_empty() { "CI is running again: not merged yet".into() } else { format!("CI failed since: {}", failed.join(", ")) });
        return;
    }
    // Only the commit that was checked: GitHub refuses the merge if the
    // branch moved since. Never a merge without it.
    let Some(sha) = c.checked.clone() else {
        ask(c, env, Ask::MergeFailed, "no checked commit to pin the merge to: not merged");
        return;
    };
    let n = c.pr.number.to_string();
    match env.gh(&c.repo, &["pr", "merge", &n, "--squash", "--match-head-commit", &sha]) {
        Ok(_) => {
            c.stage = Stage::Merged;
            note(c, env, format!("merged PR #{} (squash)", c.pr.number));
            close_all(c, env, true);
            remove_readonly(c, env);
            env.say("Finished", &format!("{} · merged #{}", c.id, c.pr.number));
        }
        Err(e) => ask(c, env, Ask::MergeFailed, format!("the merge failed: {}", e.lines().next().unwrap_or("").trim())),
    }
}

fn close_all(c: &mut Case, env: &mut dyn Env, clean: bool) {
    let seen = env.agents().unwrap_or_default();
    let prefix = format!("{}-", c.id);
    let ours: Vec<&Seen> = seen.iter().filter(|a| a.name.starts_with(&prefix)).collect();
    let mut said = Vec::new();
    for a in &ours {
        // With --clean, a kept worktree is an error though the agent closed.
        if let Err(e) = env.close(&a.name, clean) {
            said.push(format!("{}: {}", a.name, e.lines().next().unwrap_or_default()));
        }
    }
    let after = env.agents().unwrap_or_default();
    for a in ours {
        if !after.iter().any(|x| x.name == a.name) {
            c.spent_closed += a.cost;
        }
    }
    c.closing.clear();
    c.agent = None;
    for line in said {
        note(c, env, line);
    }
}

/// The reviewer's and gatekeepers' checkouts: detached and only read, so
/// nothing in them is lost. Removed without force: one that changed stays.
fn remove_readonly(c: &mut Case, env: &mut dyn Env) {
    let mut paths = vec![worktree(c, Role::Reviewer)];
    let round = c.round;
    for r in 1..=round {
        c.round = r;
        paths.push(worktree(c, Role::Gatekeeper));
    }
    c.round = round;
    for p in paths.into_iter().filter(|p| p.is_dir()) {
        if let Err(e) = env.git(&c.repo, &["worktree", "remove", &p.display().to_string()]) {
            note(c, env, format!("kept {}: {}", crate::app::tilde(&p), e.lines().next().unwrap_or_default()));
        }
    }
}

fn abort(c: &mut Case, env: &mut dyn Env, _seen: &[Seen]) {
    c.ask = None;
    c.why.clear();
    c.stage = Stage::Aborted;
    note(c, env, "aborted: the PR is as the fixer left it, and the worktrees stay");
    close_all(c, env, false);
    env.say("Finished", &format!("{} · aborted", c.id));
}

// ───────────────────────────── the real world ─────────────────────────────

/// The runner in its pane: git, gh and the workspace for real.
struct Live {
    keys: std::sync::mpsc::Receiver<char>,
    /// What the pane last said: said again only when it changes.
    said: (String, String),
}

fn run_cmd(prog: &str, repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err })
    }
}

fn control(req: crate::control::Request) -> Result<String, String> {
    let msg = crate::session::ClientMsg::Control { protocol: crate::control::CONTROL, from: crate::control::caller(), req };
    match crate::session::control(&msg, Some(50)) {
        Ok(r) if r.code == 0 => Ok(r.out),
        Ok(r) => Err(r.err.trim().to_string()),
        Err(e) => Err(e),
    }
}

impl Env for Live {
    fn now(&self) -> u64 {
        crate::prices::now()
    }
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, String> {
        run_cmd("git", repo, args)
    }
    fn gh(&mut self, repo: &Path, args: &[&str]) -> Result<String, String> {
        run_cmd("gh", repo, args)
    }
    fn spawn(&mut self, name: &str, agent: &RoleAgent, cwd: &Path, prompt: &str) -> Result<(), String> {
        let command = (!agent.args.is_empty()).then(|| shell_words::join(&agent.args));
        control(crate::control::Request::Spawn { kind: agent.agent.clone(), name: Some(name.into()), cwd: cwd.to_path_buf(), command, prompt: Some(prompt.into()), worktree: false }).map(|_| ())
    }
    fn send(&mut self, name: &str, text: &str) -> Result<(), String> {
        control(crate::control::Request::Send { target: name.into(), text: text.into(), force: false }).map(|_| ())
    }
    fn close(&mut self, name: &str, clean: bool) -> Result<(), String> {
        control(crate::control::Request::Close { target: name.into(), force: false, clean }).map(|_| ())
    }
    fn agents(&mut self) -> Result<Vec<Seen>, String> {
        let out = control(crate::control::Request::List { json: true })?;
        let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        Ok(v["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| Seen { name: a["name"].as_str().unwrap_or_default().into(), state: a["state"].as_str().unwrap_or_default().into(), cost: a["spend"]["cost_usd"].as_f64().unwrap_or(0.0) })
            .collect())
    }
    fn answer(&mut self) -> Option<char> {
        self.keys.try_recv().ok()
    }
    fn say(&mut self, event: &str, detail: &str) {
        if self.said != (event.to_string(), detail.to_string()) {
            self.said = (event.into(), detail.into());
            crate::hooks::say("triage", event, detail);
        }
    }
    fn print(&mut self, line: &str) {
        println!("{line}");
    }
}

/// `trix case run <id>`: the case's pane. Runs until the case is over, then
/// stays, so its log is there to read.
pub fn run_pane(id: &str) -> i32 {
    let root = root();
    let mut c = match load(&root, id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("trix: {e}");
            return 1;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
            if let Some(k) = line.trim().chars().next() {
                let _ = tx.send(k.to_ascii_lowercase());
            }
            line.clear();
        }
    });
    let mut env = Live { keys: rx, said: Default::default() };
    println!("trix triage · {} · PR #{} {}\n{}\nReviewer {}, fixer {}, gatekeeper {} · budget {} · up to {}\n", c.id, c.pr.number, c.pr.title, c.pr.url, c.reviewer.agent, c.fixer.agent, c.gatekeeper.agent, crate::cost::usd(c.budget), plural(c.max_rounds as usize, "round"));
    for (at, text) in &c.log {
        let (_, _, secs) = crate::ledger::local(*at);
        println!("{:02}:{:02}  {text}", secs / 3600, secs / 60 % 60);
    }
    if let Some(a) = c.ask {
        env.say("Question", &format!("{} · {}", c.id, c.why));
        let keys: Vec<String> = a.keys().iter().map(|(k, w)| format!("{k}: {w}")).collect();
        println!("needs you: {}  [{}]  (type the key, then enter)", c.why, keys.join(" · "));
    }
    // Restored with the workspace: let restore bring the case's agents
    // back before looking for them, or they'd be started twice.
    if !c.log.is_empty() && !c.stage.over() {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    let mut said_over = false;
    loop {
        if step(&mut c, &root, &mut env) {
            if let Err(e) = store(&root, &c) {
                eprintln!("trix: {e}");
            }
            if c.ask.is_some() {
                println!("(type the key, then enter)");
            }
        }
        if c.stage.over() && c.closing.is_empty() && !said_over {
            said_over = true;
            println!("\nCase {} is {}. Its log stays here; close the pane when you're done with it.", c.id, c.stage.label());
            env.say("Finished", &format!("{} · {}", c.id, c.stage.label()));
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

// ───────────────────────────── the command line ─────────────────────────────

const HELP: &str = "\
usage:
  trix triage <PR number or URL>   start a case: a reviewer, a fixer and a gatekeeper take
                                    the PR from findings to a merge you approve
  trix case ls [--json]             every case and where it is
  trix case show <id> [--json]      one case: its findings, what was done, its log
  trix case approve <id>            merge a case waiting at the gate (or press y in its pane)
  trix case abort <id>              stop a case (or press a in its pane)
  trix case submit <id> <role>      for the agents: hand in a report, JSON on stdin
";

/// `trix triage <PR>`.
pub fn triage(args: &[String]) -> i32 {
    let [pr] = args else {
        eprint!("{HELP}");
        return 2;
    };
    match start(pr) {
        Ok(id) => {
            println!("{id}");
            0
        }
        Err(e) => {
            eprintln!("trix: triage: {e}");
            1
        }
    }
}

fn start(pr: &str) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let common = run_cmd("git", &cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).map_err(|_| "not in a git repository".to_string())?;
    let repo = Path::new(common.trim()).parent().ok_or("can't find the main checkout")?.to_path_buf();
    let out = run_cmd("gh", &repo, &["pr", "view", pr, "--json", "number,title,url,headRefName,headRefOid,baseRefName,isCrossRepository,state"]).map_err(|e| format!("gh pr view {pr}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    if v["state"] != "OPEN" {
        return Err(format!("PR {pr} is {}, not open", v["state"].as_str().unwrap_or("?").to_lowercase()));
    }
    if v["isCrossRepository"].as_bool() == Some(true) {
        return Err(format!("PR {pr} comes from a fork: trix triage fixes PRs whose branch is in this repository for now"));
    }
    let pr = Pr {
        number: v["number"].as_u64().ok_or("no PR number")?,
        title: v["title"].as_str().unwrap_or_default().into(),
        url: v["url"].as_str().unwrap_or_default().into(),
        head: v["headRefName"].as_str().ok_or("no head branch")?.into(),
        base: v["baseRefName"].as_str().unwrap_or("main").into(),
        sha: v["headRefOid"].as_str().ok_or("no head commit")?.into(),
    };
    let config = crate::config::load().unwrap_or_default();
    let root = root();
    let id = new_id(&root, pr.number)?;
    let mut c = Case::new(&id, pr, repo.clone(), &config.triage, crate::prices::now());
    c.ci_expected = check_runs(&repo, &c.pr.sha, &mut |r, a| run_cmd("gh", r, a)).map_or(true, |runs| !runs.is_empty());
    store(&root, &c)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = shell_words::join([exe.display().to_string(), "case".into(), "run".into(), id.clone()]);
    if let Err(e) = control(crate::control::Request::Spawn { kind: "custom".into(), name: Some(id.clone()), cwd: repo, command: Some(command), prompt: None, worktree: false }) {
        let _ = std::fs::remove_dir_all(dir_of(&root, &id));
        return Err(e);
    }
    Ok(id)
}

/// `trix case …`.
pub fn run(args: &[String]) -> i32 {
    let root = root();
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--json").collect();
    let say = |r: Result<String, String>| match r {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("trix: case: {e}");
            1
        }
    };
    match words.as_slice() {
        ["ls"] if json => say(serde_json::to_string_pretty(&all(&root).iter().map(summary).collect::<Vec<_>>()).map(|s| s + "\n").map_err(|e| e.to_string())),
        ["ls"] => say(Ok(listing(&all(&root)))),
        ["show", id] if json => say(load(&root, id).and_then(|c| serde_json::to_string_pretty(&c).map(|s| s + "\n").map_err(|e| e.to_string()))),
        ["show", id] => say(load(&root, id).map(|c| show(&c))),
        ["run", id] => run_pane(id),
        ["approve", id] => say(control(crate::control::Request::Case { id: id.to_string(), op: Op::Approve })),
        ["abort", id] => say(control(crate::control::Request::Case { id: id.to_string(), op: Op::Abort })),
        ["submit", id, role] => {
            let Some(role) = Role::parse(role) else {
                return say(Err(format!("'{role}' isn't a role (reviewer, fixer, gatekeeper)")));
            };
            let mut report = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut report).is_err() || report.trim().is_empty() {
                return say(Err("submit: pipe the report's JSON in on stdin".into()));
            }
            say(control(crate::control::Request::Case { id: id.to_string(), op: Op::Submit { role, report } }))
        }
        _ => {
            eprint!("{HELP}");
            2
        }
    }
}

fn summary(c: &Case) -> serde_json::Value {
    serde_json::json!({
        "id": c.id, "pr": c.pr.number, "title": c.pr.title, "url": c.pr.url,
        "stage": c.stage, "round": c.round, "needs_you": c.ask.is_some(), "why": c.why,
        "findings": c.findings.len(), "budget_usd": c.budget, "spent_closed_usd": c.spent_closed,
    })
}

fn listing(cases: &[Case]) -> String {
    if cases.is_empty() {
        return "no cases yet: trix triage <PR> starts one\n".into();
    }
    let w = cases.iter().map(|c| c.id.len()).max().unwrap_or(0);
    cases.iter().map(|c| format!("{:<w$}  #{:<5} {}  ·  {}\n", c.id, c.pr.number, c.status(), c.pr.title)).collect()
}

fn show(c: &Case) -> String {
    let mut s = format!("{} · PR #{} {}\n{}\n{} · round {} of {} · budget {}\n", c.id, c.pr.number, c.pr.title, c.pr.url, c.status(), c.round, c.max_rounds, crate::cost::usd(c.budget));
    if !c.findings.is_empty() {
        s.push_str("\nfindings:\n");
        for f in &c.findings {
            let outcome = c.fix.as_ref().and_then(|x| x.findings.iter().find(|o| o.id == f.id)).map(|o| format!(" → {:?}", o.outcome).to_lowercase()).unwrap_or_default();
            let ok = c.verdict.as_ref().and_then(|v| v.findings.iter().find(|k| k.id == f.id)).map(|k| if k.ok { ", checked ok" } else { ", not ok" }).unwrap_or("");
            s.push_str(&format!("  {} {:?} {}:{}  {}{outcome}{ok}\n", f.id, f.severity, f.file, f.line.map_or(String::new(), |l| l.to_string()), f.summary).replace("Important", "important").replace("Minor", "minor"));
        }
    }
    if !c.log.is_empty() {
        s.push_str("\nlog:\n");
        for (at, text) in &c.log {
            let (_, day, secs) = crate::ledger::local(*at);
            s.push_str(&format!("  {day:>2}d {:02}:{:02}  {text}\n", secs / 3600, secs / 60 % 60));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        seen: Vec<Seen>,
        prompts: Vec<(String, String)>,
        sent: Vec<(String, String)>,
        closed: Vec<(String, bool)>,
        pushed: Vec<String>,
        ci: String,
        merged: Vec<Vec<String>>,
        keys: Vec<char>,
        said: Vec<(String, String)>,
        now: u64,
    }

    impl Env for Fake {
        fn now(&self) -> u64 {
            self.now
        }
        fn git(&mut self, _repo: &Path, args: &[&str]) -> Result<String, String> {
            match args {
                ["worktree", "add", ..] => {
                    let p = args.iter().find(|a| a.contains(".trix/worktrees")).unwrap();
                    std::fs::create_dir_all(p).unwrap();
                    Ok(String::new())
                }
                ["rev-parse", ..] => Ok("headsha\n".into()),
                ["merge-base", "--is-ancestor", sha, _] => if self.pushed.iter().any(|p| p == sha) { Ok(String::new()) } else { Err("no".into()) },
                _ => Ok(String::new()),
            }
        }
        fn gh(&mut self, _repo: &Path, args: &[&str]) -> Result<String, String> {
            match args {
                ["api", path] if path.contains("/check-runs") => Ok(self.ci.clone()),
                ["pr", "merge", ..] => {
                    self.merged.push(args.iter().map(|s| s.to_string()).collect());
                    Ok(String::new())
                }
                _ => Err("unexpected".into()),
            }
        }
        fn spawn(&mut self, name: &str, _agent: &RoleAgent, _cwd: &Path, prompt: &str) -> Result<(), String> {
            self.seen.push(Seen { name: name.into(), state: "working".into(), cost: 1.0 });
            self.prompts.push((name.into(), prompt.into()));
            Ok(())
        }
        fn send(&mut self, name: &str, text: &str) -> Result<(), String> {
            self.sent.push((name.into(), text.into()));
            Ok(())
        }
        fn close(&mut self, name: &str, clean: bool) -> Result<(), String> {
            self.closed.push((name.into(), clean));
            self.seen.retain(|a| a.name != name);
            Ok(())
        }
        fn agents(&mut self) -> Result<Vec<Seen>, String> {
            Ok(self.seen.clone())
        }
        fn answer(&mut self) -> Option<char> {
            (!self.keys.is_empty()).then(|| self.keys.remove(0))
        }
        fn say(&mut self, event: &str, detail: &str) {
            self.said.push((event.into(), detail.into()));
        }
        fn print(&mut self, _line: &str) {}
    }

    fn fixture(name: &str) -> (PathBuf, Case) {
        let base = std::env::temp_dir().join(format!("trix-case-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, repo) = (base.join("cases"), base.join("repo"));
        std::fs::create_dir_all(&repo).unwrap();
        let pr = Pr { number: 12, title: "Add rate limits".into(), url: "https://example.com/pr/12".into(), head: "rate-limits".into(), base: "main".into(), sha: "prsha".into() };
        let c = Case::new("triage-12", pr, repo, &crate::config::TriageOpts::default(), 1_795_000_000);
        store(&root, &c).unwrap();
        (root, c)
    }

    /// Handed in by the role's own agent, as the server would see it.
    fn submit(root: &Path, c: &mut Case, role: Role, json: &str) -> crate::control::Reply {
        let name = c.agent_name(role);
        submit_as(root, c, Some(&name), role, json)
    }

    fn submit_as(root: &Path, c: &mut Case, agent: Option<&str>, role: Role, json: &str) -> crate::control::Reply {
        store(root, c).unwrap();
        match handle(root, agent, &c.id, Op::Submit { role, report: json.into() }) {
            Ok((out, _)) => crate::control::Reply::ok(out),
            Err(e) => crate::control::Reply { code: 1, out: String::new(), err: e },
        }
    }

    fn runs(list: &[(&str, &str, Option<&str>)]) -> String {
        let runs: Vec<serde_json::Value> = list.iter().map(|(n, status, conclusion)| serde_json::json!({"name": n, "status": status, "conclusion": conclusion})).collect();
        serde_json::json!({"total_count": runs.len(), "check_runs": runs}).to_string()
    }

    fn set_state(env: &mut Fake, name: &str, state: &str) {
        env.seen.iter_mut().find(|a| a.name == name).unwrap().state = state.into();
    }

    const FINDINGS: &str = r#"{"findings": [{"id": "F1", "file": "src/limit.rs", "line": 40, "severity": "important", "summary": "the window never resets", "failure": "after 60s a client is still refused"}]}"#;

    #[test]
    fn a_case_goes_from_findings_through_two_rounds_to_a_merge() {
        let (root, mut c) = fixture("flow");
        let mut env = Fake { now: 1_795_000_000, ci: runs(&[("test", "in_progress", None)]), ..Fake::default() };
        // The reviewer starts, told how to hand in its report.
        step(&mut c, &root, &mut env);
        assert_eq!(c.agent.as_deref(), Some("triage-12-review"));
        assert!(env.prompts[0].1.contains("trix case submit triage-12 reviewer <<'EOF'") && env.prompts[0].1.contains("prsha"), "{}", env.prompts[0].1);
        // Its findings: on to the fixer; the reviewer closes once it stops.
        assert_eq!(submit(&root, &mut c, Role::Reviewer, FINDINGS).code, 0);
        step(&mut c, &root, &mut env);
        assert_eq!((c.stage, c.open.clone(), c.closing.clone()), (Stage::Fixing, vec!["F1".to_string()], vec!["triage-12-review".to_string()]));
        step(&mut c, &root, &mut env);
        assert_eq!(c.agent.as_deref(), Some("triage-12-fix"));
        let fix_prompt = &env.prompts[1].1;
        assert!(fix_prompt.contains("the window never resets") && fix_prompt.contains("git push origin HEAD:rate-limits"), "{fix_prompt}");
        set_state(&mut env, "triage-12-review", "done");
        step(&mut c, &root, &mut env);
        assert_eq!(env.closed, vec![("triage-12-review".to_string(), false)]);
        assert!((c.spent_closed - 1.0).abs() < 1e-9);
        // A report naming a commit that isn't pushed goes back to the fixer.
        let fix = r#"{"commits": ["c1c1c1c"], "tests": "cargo test: ok", "findings": [{"id": "F1", "outcome": "fixed", "test": "window_resets"}]}"#;
        assert_eq!(submit(&root, &mut c, Role::Fixer, fix).code, 0);
        step(&mut c, &root, &mut env);
        assert_eq!(c.stage, Stage::Fixing);
        assert!(env.sent.last().unwrap().1.contains("aren't on origin/rate-limits"), "{:?}", env.sent);
        env.pushed.push("c1c1c1c".into());
        submit(&root, &mut c, Role::Fixer, fix);
        step(&mut c, &root, &mut env);
        assert_eq!(c.stage, Stage::Verifying);
        // The gatekeeper, fresh, asks for changes: round 2.
        step(&mut c, &root, &mut env);
        assert_eq!((c.agent.as_deref(), c.checked.as_deref()), (Some("triage-12-gate-1"), Some("headsha")));
        let gate_prompt = &env.prompts[2].1;
        assert!(gate_prompt.contains("window_resets") && gate_prompt.contains("gh pr checks 12"), "{gate_prompt}");
        let changes = r#"{"verdict": "changes", "findings": [{"id": "F1", "ok": false, "reason": "it resets, but per process, not per client"}], "new": [], "ci": "pending"}"#;
        assert_eq!(submit(&root, &mut c, Role::Gatekeeper, changes).code, 0);
        step(&mut c, &root, &mut env);
        assert_eq!((c.stage, c.round), (Stage::Fixing, 2));
        let (to, text) = env.sent.last().unwrap();
        assert!(to == "triage-12-fix" && text.contains("per process, not per client"), "{text}");
        let fix2 = r#"{"commits": ["c2c2c2c"], "tests": "cargo test: ok", "findings": [{"id": "F1", "outcome": "fixed"}]}"#;
        env.pushed.push("c2c2c2c".into());
        submit(&root, &mut c, Role::Fixer, fix2);
        step(&mut c, &root, &mut env);
        step(&mut c, &root, &mut env);
        assert_eq!(c.agent.as_deref(), Some("triage-12-gate-2"));
        let pass = r#"{"verdict": "pass", "findings": [{"id": "F1", "ok": true}], "ci": "green"}"#;
        assert_eq!(submit(&root, &mut c, Role::Gatekeeper, pass).code, 0);
        step(&mut c, &root, &mut env);
        assert_eq!(c.stage, Stage::Gate);
        // CI still running: no question yet. Green: it asks you.
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, None);
        env.ci = runs(&[("test", "completed", Some("success"))]);
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::Merge));
        assert_eq!(env.said.last().unwrap().0, "Question");
        // An agent can't approve, you can: the server types y into the pane.
        store(&root, &c).unwrap();
        assert!(handle(&root, Some("triage-12-fix"), &c.id, Op::Approve).unwrap_err().contains("an agent can't approve"));
        assert_eq!(handle(&root, None, &c.id, Op::Approve).unwrap().1, Some('y'));
        env.keys.push('y');
        step(&mut c, &root, &mut env);
        assert_eq!(c.stage, Stage::Merged);
        assert_eq!(env.merged, vec![vec!["pr", "merge", "12", "--squash", "--match-head-commit", "headsha"].into_iter().map(String::from).collect::<Vec<_>>()]);
        assert!(env.closed.iter().any(|(n, clean)| n == "triage-12-fix" && *clean), "{:?}", env.closed);
        assert_eq!(env.said.last().unwrap(), &("Finished".to_string(), "triage-12 · merged #12".to_string()));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn reports_that_dont_fit_go_back_to_their_agent() {
        let (root, mut c) = fixture("reports");
        // Not its turn.
        assert!(submit(&root, &mut c, Role::Fixer, "{}").err.contains("isn't waiting for the fixer's report"));
        // Not the shape.
        let r = submit(&root, &mut c, Role::Reviewer, r#"{"findings": [{"id": "F1"}]}"#);
        assert!(r.code == 1 && r.err.contains("Fix the report and submit it again"), "{}", r.err);
        let twice = r#"{"findings": [{"id": "F1", "file": "a", "severity": "minor", "summary": "s", "failure": "f"}, {"id": "F1", "file": "b", "severity": "minor", "summary": "s", "failure": "f"}]}"#;
        assert!(submit(&root, &mut c, Role::Reviewer, twice).err.contains("used twice"));
        c.stage = Stage::Fixing;
        c.open = vec!["F1".into(), "F2".into()];
        let short = r#"{"commits": ["abcdef1"], "tests": "ok", "findings": [{"id": "F1", "outcome": "fixed"}]}"#;
        assert!(submit(&root, &mut c, Role::Fixer, short).err.contains("no answer for F2"));
        let unexplained = r#"{"commits": [], "tests": "ok", "findings": [{"id": "F1", "outcome": "not_a_bug"}, {"id": "F2", "outcome": "wont_fix", "note": "out of scope"}]}"#;
        assert!(submit(&root, &mut c, Role::Fixer, unexplained).err.contains("say why in `note`"));
        c.stage = Stage::Verifying;
        let lenient = r#"{"verdict": "pass", "findings": [{"id": "F1", "ok": true}, {"id": "F2", "ok": false, "reason": "still broken"}], "ci": "green"}"#;
        assert!(submit(&root, &mut c, Role::Gatekeeper, lenient).err.contains("pass needs every finding ok"));
        let red = r#"{"verdict": "pass", "findings": [{"id": "F1", "ok": true}, {"id": "F2", "ok": true}], "ci": "red"}"#;
        assert!(submit(&root, &mut c, Role::Gatekeeper, red).err.contains("CI not red"));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn the_case_asks_you_when_nothing_is_found_and_when_the_budget_is_spent() {
        let (root, mut c) = fixture("asks");
        let mut env = Fake { now: 1_795_000_000, ci: runs(&[]), ..Fake::default() };
        c.ci_expected = false;
        step(&mut c, &root, &mut env);
        submit(&root, &mut c, Role::Reviewer, r#"{"findings": []}"#);
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::NothingFound));
        // A key that isn't an answer does nothing; m takes it to the gate.
        env.keys = vec!['y', 'm'];
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::NothingFound));
        step(&mut c, &root, &mut env);
        assert_eq!((c.ask, c.stage), (None, Stage::Gate));
        step(&mut c, &root, &mut env);
        assert!(c.ask == Some(Ask::Merge) && c.why.contains("no CI checks"), "{}", c.why);
        // Merged pinned to what the reviewer read, not the branch as it is now.
        env.keys.push('y');
        step(&mut c, &root, &mut env);
        assert_eq!(env.merged.last().unwrap()[4..], ["--match-head-commit".to_string(), "prsha".to_string()]);
        // The budget: $10 by default.
        let (root, mut c) = fixture("budget");
        let mut env = Fake { now: 1_795_000_000, ..Fake::default() };
        step(&mut c, &root, &mut env);
        env.seen[0].cost = 10.5;
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::Budget));
        env.keys.push('b');
        step(&mut c, &root, &mut env);
        assert_eq!((c.ask, c.budget), (None, 20.0));
        // And a stopped agent gets one reminder, then you're asked.
        set_state(&mut env, "triage-12-review", "done");
        step(&mut c, &root, &mut env);
        assert!(env.sent.last().unwrap().1.contains("waiting for your report"));
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::NoReport));
        // The report comes after all: on it goes, unasked.
        submit(&root, &mut c, Role::Reviewer, FINDINGS);
        step(&mut c, &root, &mut env);
        assert_eq!((c.ask, c.stage), (None, Stage::Fixing));
        step(&mut c, &root, &mut env);
        set_state(&mut env, "triage-12-fix", "done");
        step(&mut c, &root, &mut env);
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::NoReport));
        env.keys.push('a');
        step(&mut c, &root, &mut env);
        assert_eq!(c.stage, Stage::Aborted);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn red_ci_at_the_gate_is_a_finding_for_the_fixer() {
        let (root, mut c) = fixture("ci");
        let mut env = Fake { now: 1_795_000_000, ci: runs(&[("clippy", "completed", Some("failure")), ("test", "completed", Some("success"))]), ..Fake::default() };
        c.stage = Stage::Gate;
        c.findings = serde_json::from_str::<Findings>(FINDINGS).unwrap().findings;
        env.seen.push(Seen { name: "triage-12-fix".into(), state: "done".into(), cost: 0.0 });
        step(&mut c, &root, &mut env);
        assert_eq!((c.stage, c.round, c.open.clone()), (Stage::Fixing, 2, vec!["CI1".to_string()]));
        assert!(c.findings.iter().any(|f| f.id == "CI1" && f.summary.contains("clippy")));
        assert!(env.sent.last().unwrap().1.contains("CI fails on the PR: clippy"));
        // Out of rounds: it asks.
        c.stage = Stage::Gate;
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::CiFailed));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn only_the_roles_own_agent_hands_in_its_report() {
        let (root, mut c) = fixture("who");
        c.stage = Stage::Verifying;
        c.open = vec!["F1".into()];
        let pass = r#"{"verdict": "pass", "findings": [{"id": "F1", "ok": true}], "ci": "green"}"#;
        // The fixer, still open, can't pass its own work.
        let r = submit_as(&root, &mut c, Some("triage-12-fix"), Role::Gatekeeper, pass);
        assert!(r.err.contains("only triage-12-gate-1 hands in the gatekeeper's report"), "{}", r.err);
        assert!(!report_path(&root, &c.id, Role::Gatekeeper, 1).exists());
        assert_eq!(submit_as(&root, &mut c, Some("triage-12-gate-1"), Role::Gatekeeper, pass).code, 0);
        // You, from a terminal, may hand one in for it.
        assert_eq!(submit_as(&root, &mut c, None, Role::Gatekeeper, pass).code, 0);
        // A commit that isn't a commit id never reaches git.
        c.stage = Stage::Fixing;
        let opt = r#"{"commits": ["--output=/tmp/x"], "tests": "ok", "findings": [{"id": "F1", "outcome": "fixed"}]}"#;
        assert!(submit(&root, &mut c, Role::Fixer, opt).err.contains("isn't a commit id"));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn checks_not_registered_yet_are_waited_for() {
        let (root, mut c) = fixture("pending");
        let mut env = Fake { now: 1_795_000_000, ci: runs(&[]), ..Fake::default() };
        c.stage = Stage::Gate;
        c.checked = Some("fixsha".into());
        // The PR had CI: none yet on the pushed commit is waiting.
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, None);
        assert!(env.said.last().unwrap().1.contains("waiting for CI on fixsha"), "{:?}", env.said);
        env.ci = runs(&[("test", "queued", None)]);
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, None);
        env.ci = runs(&[("test", "completed", Some("success")), ("lint", "completed", Some("skipped"))]);
        step(&mut c, &root, &mut env);
        assert_eq!(c.ask, Some(Ask::Merge));
        // Never merged without a checked commit to pin it to.
        c.checked = None;
        env.keys.push('y');
        step(&mut c, &root, &mut env);
        assert!(env.merged.is_empty() && c.ask == Some(Ask::MergeFailed), "{:?}", env.merged);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn ids_go_on_from_a_finished_case_and_never_share_a_running_one() {
        let (root, mut c) = fixture("ids");
        assert!(new_id(&root, 12).unwrap_err().contains("already running"));
        c.stage = Stage::Merged;
        store(&root, &c).unwrap();
        assert_eq!(new_id(&root, 12).unwrap(), "triage-12-2");
        assert_eq!(new_id(&root, 13).unwrap(), "triage-13");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
