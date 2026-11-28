//! Dry run and rehearsal. Both start with the one checker and refuse on
//! any error.
//!
//! A dry run then drives the real engine through a stub `Env`: no agent,
//! no git, no GitHub. Stub agents hand in canned verdicts, so what comes
//! out is every prompt each node would get, the path a run takes, what
//! would stop it and what would be approved without you.
//!
//! A rehearsal runs real agents in a throwaway clone whose `origin` is a
//! local bare mirror, with the guard on every pane and check and trix's
//! own push, PR and merge stubbed. That stops an accidental push or `gh`
//! call from reaching the real repository. It's guarded, not enforced: an
//! agent that reads your credentials from disk and names the repository
//! itself could still reach it.

use super::caps::Matrix;
use super::check::{self, Ctx};
use super::engine::{self, WfRun};
use super::env::{AgentSpec, CheckOut, Env, Seen};
use super::model::Workflow;
use super::runner;
use super::settings::Settings;
use super::view::RunState;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What the stub agents say.
#[derive(Clone, Debug, PartialEq)]
pub enum Scenario {
    /// Every node gives its first verdict, every check passes.
    Happy,
    /// Each node that can fail does, once, then passes.
    EachFailure,
    /// Every node that can fail keeps failing: what stops the run?
    Worst,
    /// A node goes quiet: it never hands in.
    Stall(String),
    /// A node's backend runs out of quota.
    Quota(String),
    /// The land step conflicts, once.
    Conflict,
    /// Verdicts you script, by node, one per visit.
    Script(BTreeMap<String, Vec<String>>),
}

impl Scenario {
    pub fn parse(s: &str) -> Result<Scenario, String> {
        match s.split_once('=') {
            None => match s {
                "happy" => Ok(Scenario::Happy),
                "failures" => Ok(Scenario::EachFailure),
                "worst" => Ok(Scenario::Worst),
                "conflict" => Ok(Scenario::Conflict),
                _ => Err(format!(
                    "'{s}' isn't a scenario: happy, failures, worst, conflict, stall=<node>, quota=<node>, or <node>=<verdict>,<verdict>…"
                )),
            },
            Some(("stall", node)) => Ok(Scenario::Stall(node.into())),
            Some(("quota", node)) => Ok(Scenario::Quota(node.into())),
            Some((node, verdicts)) => Ok(Scenario::Script(BTreeMap::from([(
                node.to_string(),
                verdicts.split(',').map(String::from).collect(),
            )]))),
        }
    }
}

/// The world a dry run plays in.
struct Stub {
    root: PathBuf,
    run: String,
    workflow: Workflow,
    scenario: Scenario,
    now: u64,
    head: u32,
    seen: Vec<Seen>,
    visits: BTreeMap<String, u32>,
    conflicted: bool,
    /// How many checks ran.
    checked: u32,
    prs: u32,
    /// Every prompt, in order: the node, its visit, the text.
    prompts: Vec<(String, u32, String)>,
    notes: Vec<String>,
}

impl Stub {
    fn sha(&self) -> String {
        format!("{:040x}", 0xd00d_0000u64 + self.head as u64)
    }

    /// The node a pane's name is for.
    fn node_of(&self, pane: &str) -> Option<String> {
        let rest = pane.strip_prefix(&format!("{}-", self.run))?;
        self.workflow
            .nodes
            .keys()
            .filter(|id| rest == id.as_str() || rest.starts_with(&format!("{id}-")))
            .max_by_key(|id| id.len())
            .cloned()
    }

    /// The verdict a node's stub agent gives on this visit.
    fn verdict(&self, node: &str, visit: u32) -> Option<String> {
        let n = &self.workflow.nodes[node];
        let (first, second) = (n.verdicts.first().cloned(), n.verdicts.get(1).cloned());
        match &self.scenario {
            Scenario::Stall(s) | Scenario::Quota(s) if s == node => None,
            Scenario::EachFailure if visit == 1 => second.or(first),
            Scenario::Worst => second.or(first),
            Scenario::Script(by) => by
                .get(node)
                .and_then(|v| v.get(visit as usize - 1).or(v.last()))
                .cloned()
                .or(first),
            _ => first,
        }
    }

    /// The stub agent's turn: its prompt is kept, and its report is handed in.
    fn play(&mut self, pane: &str, prompt: &str) {
        let Some(node) = self.node_of(pane) else {
            return;
        };
        let visit = self.visits.entry(node.clone()).or_insert(0);
        *visit += 1;
        let visit = *visit;
        self.prompts.push((node.clone(), visit, prompt.to_string()));
        let Some(verdict) = self.verdict(&node, visit) else {
            // Quiet: it stops without its report.
            if let Some(a) = self.seen.iter_mut().find(|a| a.name == pane) {
                a.state = "idle".into();
            }
            return;
        };
        let n = &self.workflow.nodes[&node];
        let mut report = serde_json::json!({"verdict": verdict, "summary": "(simulated)"});
        for (name, kind) in &n.output {
            report[name] = if kind.starts_with("list") {
                serde_json::json!(["(simulated)"])
            } else {
                serde_json::json!("(simulated)")
            };
        }
        if n.kind == "supervisor" {
            report["answer"] = "(simulated answer)".into();
        }
        // A writer that hands in has moved the head, unless it's failing for good.
        if n.writes() {
            self.head += 1;
        }
        let dir = engine::dir_of(&self.root, &self.run).join("reports");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("{node}.json")), report.to_string());
        if let Some(a) = self.seen.iter_mut().find(|a| a.name == pane) {
            a.state = "done".into();
        }
    }
}

impl Env for Stub {
    fn now(&self) -> u64 {
        self.now
    }
    fn git(&mut self, _repo: &Path, args: &[&str]) -> Result<String, String> {
        match args {
            ["worktree", "add", rest @ ..] => {
                if let Some(p) = rest.iter().find(|a| a.contains(".trix/worktrees")) {
                    let _ = std::fs::create_dir_all(p);
                }
                Ok(String::new())
            }
            ["worktree", "remove", rest @ ..] => {
                if let Some(p) = rest.last() {
                    let _ = std::fs::remove_dir_all(p);
                }
                Ok(String::new())
            }
            ["rev-parse", "HEAD"] => Ok(self.sha()),
            ["rev-parse", ..] | ["config", ..] => Err("(simulated)".into()),
            ["rev-list", "--count", _] => Ok("1".into()),
            ["rebase", "--abort"] => Ok(String::new()),
            ["rebase", ..] if self.scenario == Scenario::Conflict && !self.conflicted => {
                self.conflicted = true;
                Err("CONFLICT (simulated)".into())
            }
            ["diff", "--name-only", "--diff-filter=U"] => {
                Ok("src/conflicted-in-simulation\n".into())
            }
            _ => Ok(String::new()),
        }
    }
    fn gh(&mut self, _repo: &Path, args: &[&str]) -> Result<String, String> {
        match args {
            ["pr", "list", ..] if self.prs == 0 => Ok("[]".into()),
            ["pr", "list", ..] => Ok(r#"[{"number": 1, "url": "(simulated PR)"}]"#.into()),
            ["pr", "create", ..] => {
                self.prs = 1;
                Ok(String::new())
            }
            ["api", path] if path.contains("/check-runs") => Ok(r#"{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]}"#.into()),
            ["api", path] if path.contains("/branches/") => Ok(r#"{"protected": true}"#.into()),
            _ => Ok(String::new()),
        }
    }
    fn spawn(
        &mut self,
        name: &str,
        _agent: &AgentSpec,
        _cwd: &Path,
        prompt: &str,
    ) -> Result<(), String> {
        self.seen.push(Seen {
            name: name.into(),
            state: "working".into(),
            cost: 0.0,
        });
        self.play(name, prompt);
        Ok(())
    }
    fn open_case(&mut self, _c: &crate::case::Case) -> Result<(), String> {
        Ok(())
    }
    fn send(&mut self, name: &str, text: &str) -> Result<(), String> {
        // A nudge isn't a visit.
        if text.contains("is waiting for your report")
            || text.starts_with("Continue where you left off")
        {
            return Ok(());
        }
        self.play(name, text);
        Ok(())
    }
    fn close(&mut self, name: &str, _clean: bool) -> Result<(), String> {
        self.seen.retain(|a| a.name != name);
        Ok(())
    }
    fn agents(&mut self) -> Result<Vec<Seen>, String> {
        Ok(self.seen.clone())
    }
    fn answer(&mut self) -> Option<char> {
        None
    }
    fn say(&mut self, _event: &str, _detail: &str) {}
    fn print(&mut self, _line: &str) {}
    fn check(&mut self, _cwd: &Path, _argv: &[String], _guard: bool) -> Result<CheckOut, String> {
        // A check passes on what a writer changed since it last failed.
        let fails = matches!(self.scenario, Scenario::Worst)
            || (self.scenario == Scenario::EachFailure && self.checked == 0);
        self.checked += 1;
        Ok(CheckOut {
            exit_code: i32::from(fails),
            log_tail: if fails {
                "(simulated failure)".into()
            } else {
                "(simulated pass)".into()
            },
        })
    }
    fn screen(&mut self, name: &str) -> Option<String> {
        match &self.scenario {
            Scenario::Quota(node) if self.node_of(name).as_ref() == Some(node) => {
                Some("You've hit your usage limit.".into())
            }
            _ => None,
        }
    }
    fn notify(&mut self, title: &str, body: &str) {
        self.notes.push(format!("{title}: {body}"));
    }
    fn open_run(&mut self, _id: &str, _repo: &Path) -> Result<(), String> {
        Err("a dry run doesn't start the runs its workflow nodes name".into())
    }
}

/// What a dry run found.
#[derive(Clone, Debug, PartialEq)]
pub struct DryRun {
    /// What the checker says: the same text as everywhere.
    pub check: String,
    /// False: the checker refused, and nothing was simulated.
    pub ran: bool,
    pub state: RunState,
    pub why: String,
    pub cycles: u32,
    pub wasted: u32,
    /// Every prompt each node would get, per visit.
    pub prompts: Vec<(String, u32, String)>,
    /// The path the run took.
    pub log: Vec<String>,
    /// Every approval that would be made without you, with its rule.
    pub auto: Vec<String>,
    pub estimate: check::Estimate,
    pub looks: u32,
    /// What this simulation leaves out, said before the path.
    pub note: Option<String>,
}

/// The most looks a simulated run gets: past this it's stuck.
const LOOKS: u32 = 400;

/// Simulate a run of `w`. `inputs`: canned values for its inputs.
pub fn dry_run(
    w: &Workflow,
    m: &Matrix,
    ctx: &Ctx,
    settings: &Settings,
    scenario: Scenario,
    inputs: BTreeMap<String, serde_json::Value>,
) -> DryRun {
    let report = check::check(w, m, ctx);
    let mut out = DryRun {
        check: report.render(),
        ran: false,
        state: RunState::Scheduled,
        why: String::new(),
        cycles: 0,
        wasted: 0,
        prompts: Vec::new(),
        log: Vec::new(),
        auto: Vec::new(),
        estimate: report.estimate.clone(),
        looks: 0,
        note: None,
    };
    if !report.runnable() {
        return out;
    }
    // A folder of its own: two dry runs at once never share one.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!("trix-dry-run-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, repo) = (base.join("runs"), base.join("repo"));
    let _ = std::fs::create_dir_all(&repo);
    let id = format!("{}-dry", w.name);
    let opts = super::launch::opts(
        w,
        m,
        ctx,
        settings,
        ctx.trusted != Some(false),
        "(this repository)",
    );
    let mut run = WfRun::new(&id, w.clone(), repo, "main", inputs, opts, 1_795_000_000);
    run.trusted_input = true;
    let mut env = Stub {
        root: root.clone(),
        run: id.clone(),
        workflow: w.clone(),
        scenario,
        now: 1_795_000_000,
        head: 1,
        seen: Vec::new(),
        visits: BTreeMap::new(),
        conflicted: false,
        checked: 0,
        prs: 0,
        prompts: Vec::new(),
        notes: Vec::new(),
    };
    let _ = engine::store(&root, &run);
    while !run.state.over()
        && run.ask.is_none()
        && run.state == RunState::Running
        && out.looks < LOOKS
    {
        out.looks += 1;
        env.now += 30;
        // A fan-out's instances are nodes of the run: the stub plays them too.
        env.workflow = run.workflow.clone();
        runner::step(&mut run, &root, &mut env);
        let _ = engine::store(&root, &run);
    }
    out.ran = true;
    if w.is_triage() {
        // Triage runs as a case, on its own stages: this is its template's path.
        out.note = Some("triage's stages are its own: this follows the template's edges with stub verdicts. Its prompts aren't shown here, and what a case asks you on the way (nothing found, the round limit, a conflict, …) isn't simulated.".into());
    }
    out.state = run.state;
    out.why = if out.looks >= LOOKS {
        format!("still going after {LOOKS} looks")
    } else {
        run.why.clone()
    };
    out.cycles = run.cycles;
    out.wasted = run.wasted;
    out.prompts = env.prompts;
    out.log = run.log.iter().map(|(_, l)| l.clone()).collect();
    if w.is_triage() {
        // The prompts are triage's own, written by the case: none to show.
        out.prompts.clear();
        // A stage with no agent is trix's own step.
        for l in &mut out.log {
            *l = l.replace(" (, ", " (trix, ");
        }
    }
    out.auto = out
        .log
        .iter()
        .filter(|l| l.starts_with('⚡'))
        .cloned()
        .collect();
    let _ = std::fs::remove_dir_all(&base);
    out
}

impl DryRun {
    /// The dry run, for you to read.
    pub fn render(&self, prompts: bool) -> String {
        let mut s = self.check.clone();
        if !self.ran {
            s.push_str("\nnot simulated: the checker found errors\n");
            return s;
        }
        if let Some(note) = &self.note {
            s.push_str(&format!("\n{note}\n"));
        }
        s.push_str("\nsimulated path:\n");
        for l in &self.log {
            s.push_str(&format!("  {l}\n"));
        }
        s.push_str(&format!(
            "\nends {}{} · cycles {} ({} wasted) · peak {} at once · cost {}\n",
            self.state.label(),
            if self.why.is_empty() {
                String::new()
            } else {
                format!(": {}", self.why)
            },
            self.cycles,
            self.wasted,
            self.estimate.peak_parallel,
            self.estimate.cost()
        ));
        if self.auto.is_empty() {
            s.push_str("no approval would be made without you\n");
        }
        for a in &self.auto {
            s.push_str(&format!("{a}\n"));
        }
        if prompts {
            s.push_str(&self.prompts_text());
        }
        s
    }

    /// Every prompt each node would get, per visit.
    pub fn prompts_text(&self) -> String {
        let mut s = String::new();
        for (node, visit, text) in &self.prompts {
            s.push_str(&format!("\n── {node}, visit {visit} ──\n{text}\n"));
        }
        s
    }
}

// ───────────────────────────── rehearsal ─────────────────────────────

/// A throwaway clone to rehearse in, and the local bare mirror its
/// `origin` points at.
#[derive(Clone, Debug, PartialEq)]
pub struct Rehearsal {
    pub base: PathBuf,
    pub clone: PathBuf,
    pub mirror: PathBuf,
}

/// Make the rehearsal's clone: a bare mirror of `repo`, and a clone of the
/// mirror. A push from the clone can only reach the mirror, and `gh` finds
/// no GitHub remote there.
pub fn prepare(
    repo: &Path,
    run: impl Fn(&Path, &[&str]) -> Result<String, String>,
) -> Result<Rehearsal, String> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "trix-rehearsal-{}-{n}-{}",
        std::process::id(),
        crate::prices::now()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let (mirror, clone) = (base.join("mirror.git"), base.join("clone"));
    run(
        &base,
        &[
            "clone",
            "--quiet",
            "--mirror",
            &repo.display().to_string(),
            "mirror.git",
        ],
    )?;
    run(
        &base,
        &["clone", "--quiet", &mirror.display().to_string(), "clone"],
    )?;
    Ok(Rehearsal {
        base,
        clone,
        mirror,
    })
}

impl Rehearsal {
    /// Delete the clone and the mirror: nothing is left behind.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// `Env` for a rehearsal: real agents and real checks, with the guard on
/// every one of them whatever the workflow's push policy, and trix's own
/// push, PR and merge stubbed.
pub struct Rehearse<'a> {
    pub inner: &'a mut dyn Env,
    pub pr: bool,
}

impl Env for Rehearse<'_> {
    fn now(&self) -> u64 {
        self.inner.now()
    }
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, String> {
        // Trix's own push is stubbed: nothing leaves the clone.
        if args.first() == Some(&"push") {
            return Ok(String::new());
        }
        self.inner.git(repo, args)
    }
    fn gh(&mut self, _repo: &Path, args: &[&str]) -> Result<String, String> {
        match args {
            ["pr", "list", ..] if !self.pr => Ok("[]".into()),
            ["pr", "list", ..] => Ok(r#"[{"number": 0, "url": "(rehearsal: no PR was opened)"}]"#.into()),
            ["pr", "create", ..] => {
                self.pr = true;
                Ok(String::new())
            }
            ["api", path] if path.contains("/check-runs") => Ok(r#"{"check_runs": [{"name": "rehearsal", "status": "completed", "conclusion": "success"}]}"#.into()),
            ["api", path] if path.contains("/branches/") => Ok(r#"{"protected": false}"#.into()),
            _ => Ok(String::new()),
        }
    }
    fn spawn(
        &mut self,
        name: &str,
        agent: &AgentSpec,
        cwd: &Path,
        prompt: &str,
    ) -> Result<(), String> {
        self.inner.spawn_in(name, agent, cwd, prompt, true)
    }
    fn spawn_in(
        &mut self,
        name: &str,
        agent: &AgentSpec,
        cwd: &Path,
        prompt: &str,
        _guard: bool,
    ) -> Result<(), String> {
        self.inner.spawn_in(name, agent, cwd, prompt, true)
    }
    fn check(&mut self, cwd: &Path, argv: &[String], _guard: bool) -> Result<CheckOut, String> {
        self.inner.check(cwd, argv, true)
    }
    fn open_case(&mut self, c: &crate::case::Case) -> Result<(), String> {
        self.inner.open_case(c)
    }
    fn send(&mut self, name: &str, text: &str) -> Result<(), String> {
        self.inner.send(name, text)
    }
    fn close(&mut self, name: &str, clean: bool) -> Result<(), String> {
        self.inner.close(name, clean)
    }
    fn agents(&mut self) -> Result<Vec<Seen>, String> {
        self.inner.agents()
    }
    fn answer(&mut self) -> Option<char> {
        self.inner.answer()
    }
    fn say(&mut self, event: &str, detail: &str) {
        self.inner.say(event, detail)
    }
    fn print(&mut self, line: &str) {
        self.inner.print(line)
    }
    fn screen(&mut self, name: &str) -> Option<String> {
        self.inner.screen(name)
    }
    fn notify(&mut self, title: &str, body: &str) {
        self.inner.notify(title, body)
    }
    fn open_run(&mut self, id: &str, repo: &Path) -> Result<(), String> {
        self.inner.open_run(id, repo)
    }
}
