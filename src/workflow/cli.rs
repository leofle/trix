//! `trix workflow …` and `trix wf …`.

use super::caps::{HandIn, Installed, Matrix};
use super::check::{self, Ctx};
use super::engine::{self, Op, WfRun};
use super::env::{Env, Live, control};
use super::model::Workflow;
use super::settings::Settings;
use super::store::{Stores, runs_root};
use crate::control::Request;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
usage:
  trix workflow new                         the wizard: a template, the goal, a backend per role
  trix workflow edit <name>                 the builder (alt+p in the workspace)

  trix workflow run <name> [--input k=v]…   start a run of a workflow
                                            (triage: --input target=<PR or issue>)
  trix workflow ls [--json]                 every run and where it is
  trix workflow show <run> [--json]         one run: its nodes, its log
  trix workflow overview <run>              the run at a glance, and what in it needs you
  trix workflow log <run> [--verify]        every prompt, report, edge and nudge; --verify
                                            reports a line that was edited, deleted or cut off
  trix workflow audit <run>                 every approval: who, by which rule, what exactly
  trix workflow schedule <name> --cron '0 2 * * *' [--deadline HH:MM] [--kill-at HH:MM]
  trix workflow schedule ls | rm <name>
  trix workflow scheduler                   start what's scheduled and triggered, until stopped
  trix workflow units systemd|launchd [--yes]
                                            a unit that runs the scheduler with no terminal:
                                            shown, and written only with --yes
  trix workflow check <name> [--fix]        the one checker: the builder, import, dry run and
                                            run start use it too. --fix applies what it can
  trix workflow dry-run <name> [--scenario s] [--prompts]
                                            simulate a run with stub agents: the path, every
                                            prompt, what would stop it, what's approved without
                                            you. s: happy, failures, worst, conflict,
                                            stall=<node>, quota=<node>, <node>=<verdict>,…
  trix workflow rehearse <name> [--input k=v]…
                                            real agents in a throwaway clone whose origin is a
                                            local mirror, the guard on every pane and check
  trix workflow capabilities [--kind k] [--verify]
                                            what each backend can do, and how well trix knows
                                            it. --verify runs real probes (opt-in: it spends a
                                            little quota)
  trix workflow pause|resume|abort <run>    from an agent pane: as [workflows.agents] allows
  trix workflow approve <run>               yours only: refused from every agent pane and check
  trix workflow answer <run> <key>          answer what a run asks, as in its pane
  trix workflow revoke <run>                mid-run: every approval drops back to ask
  trix workflow export <name> > wf.yaml     a self-contained YAML file, to share
  trix workflow import <file>               runs check, converts to native, asks for trust
  trix workflow trust <name>                yours only: trust a workflow file as it is now
  trix workflow autopilot <name> [--yes]    show what each node will do unattended, then (with
                                            --yes) switch the workflow to autopilot
  trix wf submit <node> [--file F]          for the agents: hand in a report, JSON on stdin
  trix wf ask [--file F]                    for the agents: a question for the supervisor node;
                                            @node queues it to that node's inbox
";

/// A `trix workflow …` command line, read.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Run {
        name: String,
        inputs: BTreeMap<String, String>,
    },
    New,
    Edit(String),
    /// alt+p: the run view when a run is going, else the builder.
    Open,
    /// The run's pane: what a saved layout starts again.
    Pane(String),
    Ls {
        json: bool,
    },
    Show {
        id: String,
        json: bool,
    },
    Overview(String),
    Log {
        id: String,
        verify: bool,
    },
    Audit(String),
    Schedule {
        name: String,
        cron: String,
        deadline: Option<String>,
        kill_at: Option<String>,
        inputs: BTreeMap<String, String>,
    },
    ScheduleLs,
    ScheduleRm(String),
    Scheduler,
    Units {
        kind: String,
        yes: bool,
    },
    Check {
        name: String,
        fix: bool,
    },
    DryRun {
        name: String,
        scenario: String,
        prompts: bool,
    },
    Rehearse {
        name: String,
        inputs: BTreeMap<String, String>,
    },
    Capabilities {
        kind: Option<String>,
        verify: bool,
    },
    Approve(String),
    Abort(String),
    Pause(String),
    Resume(String),
    Revoke(String),
    Export(String),
    Import(String),
    Trust(String),
    Autopilot {
        name: String,
        yes: bool,
    },
    Answer {
        id: String,
        key: char,
    },
}

/// What `trix workflow <args>` asks for, or why it isn't a command.
pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let id = |s: &&str| s.to_string();
    match words.as_slice() {
        ["run", name, rest @ ..] => {
            let mut inputs = BTreeMap::new();
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                let pair = match (*flag, flag.strip_prefix("--input=")) {
                    (_, Some(p)) => p,
                    ("--input", _) => rest.next().ok_or("--input takes name=value")?,
                    _ => {
                        return Err(format!(
                            "run: '{flag}' isn't an option (--input name=value)"
                        ));
                    }
                };
                let (k, v) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--input {pair}: name=value"))?;
                inputs.insert(k.to_string(), v.to_string());
            }
            Ok(Cmd::Run {
                name: name.to_string(),
                inputs,
            })
        }
        ["dry-run", name, rest @ ..] => {
            let (mut scenario, mut prompts) = ("happy".to_string(), false);
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                match *flag {
                    "--prompts" => prompts = true,
                    "--scenario" => {
                        scenario = rest.next().ok_or("--scenario takes one")?.to_string()
                    }
                    f => {
                        return Err(format!(
                            "dry-run: '{f}' isn't an option (--scenario s, --prompts)"
                        ));
                    }
                }
            }
            Ok(Cmd::DryRun {
                name: name.to_string(),
                scenario,
                prompts,
            })
        }
        ["rehearse", rest @ ..] => {
            let as_run: Vec<String> = std::iter::once("run")
                .chain(rest.iter().copied())
                .map(String::from)
                .collect();
            match parse(&as_run)? {
                Cmd::Run { name, inputs } => Ok(Cmd::Rehearse { name, inputs }),
                _ => Err(HELP.to_string()),
            }
        }
        ["overview", x] => Ok(Cmd::Overview(id(x))),
        ["log", x] => Ok(Cmd::Log {
            id: id(x),
            verify: false,
        }),
        ["log", x, "--verify"] | ["log", "--verify", x] => Ok(Cmd::Log {
            id: id(x),
            verify: true,
        }),
        ["audit", x] => Ok(Cmd::Audit(id(x))),
        ["schedule", "ls"] => Ok(Cmd::ScheduleLs),
        ["schedule", "rm", x] => Ok(Cmd::ScheduleRm(id(x))),
        ["schedule", name, rest @ ..] => {
            let (mut cron, mut deadline, mut kill_at, mut inputs) =
                (None, None, None, BTreeMap::new());
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                let mut value = || {
                    rest.next()
                        .map(|v| v.to_string())
                        .ok_or(format!("{flag} takes a value"))
                };
                match *flag {
                    "--cron" => cron = Some(value()?),
                    "--deadline" => deadline = Some(value()?),
                    "--kill-at" => kill_at = Some(value()?),
                    "--input" => {
                        let pair = value()?;
                        let (k, v) = pair
                            .split_once('=')
                            .ok_or_else(|| format!("--input {pair}: name=value"))?;
                        inputs.insert(k.to_string(), v.to_string());
                    }
                    f => {
                        return Err(format!(
                            "schedule: '{f}' isn't an option (--cron, --deadline, --kill-at, --input)"
                        ));
                    }
                }
            }
            let cron = cron.ok_or("schedule needs --cron \"minute hour day month weekday\"")?;
            super::sched::matches(&cron, super::sched::Moment::local(0))?;
            for t in deadline.iter().chain(&kill_at) {
                super::settings::clock(t)?;
            }
            Ok(Cmd::Schedule {
                name: name.to_string(),
                cron,
                deadline,
                kill_at,
                inputs,
            })
        }
        ["scheduler"] => Ok(Cmd::Scheduler),
        ["units", kind] => Ok(Cmd::Units {
            kind: kind.to_string(),
            yes: false,
        }),
        ["units", kind, "--yes"] => Ok(Cmd::Units {
            kind: kind.to_string(),
            yes: true,
        }),
        ["new"] => Ok(Cmd::New),
        ["edit", x] => Ok(Cmd::Edit(id(x))),
        ["open"] => Ok(Cmd::Open),
        ["pane", x] => Ok(Cmd::Pane(id(x))),
        ["ls"] => Ok(Cmd::Ls { json }),
        ["show", x] => Ok(Cmd::Show { id: id(x), json }),
        ["check", name] => Ok(Cmd::Check {
            name: name.to_string(),
            fix: false,
        }),
        ["check", "--fix", name] | ["check", name, "--fix"] => Ok(Cmd::Check {
            name: name.to_string(),
            fix: true,
        }),
        ["capabilities", rest @ ..] => {
            let (mut kind, mut verify) = (None, false);
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                match *flag {
                    "--verify" => verify = true,
                    "--kind" => {
                        kind = Some(rest.next().ok_or("--kind takes a backend")?.to_string())
                    }
                    f => {
                        return Err(format!(
                            "capabilities: '{f}' isn't an option (--kind k, --verify)"
                        ));
                    }
                }
            }
            Ok(Cmd::Capabilities { kind, verify })
        }
        ["approve", x] => Ok(Cmd::Approve(id(x))),
        ["abort", x] => Ok(Cmd::Abort(id(x))),
        ["pause", x] => Ok(Cmd::Pause(id(x))),
        ["resume", x] => Ok(Cmd::Resume(id(x))),
        ["revoke", x] => Ok(Cmd::Revoke(id(x))),
        ["export", x] => Ok(Cmd::Export(id(x))),
        ["import", x] => Ok(Cmd::Import(id(x))),
        ["trust", x] => Ok(Cmd::Trust(id(x))),
        ["autopilot", x] => Ok(Cmd::Autopilot {
            name: id(x),
            yes: false,
        }),
        ["autopilot", x, "--yes"] | ["autopilot", "--yes", x] => Ok(Cmd::Autopilot {
            name: id(x),
            yes: true,
        }),
        ["answer", x, key] if key.chars().count() == 1 => Ok(Cmd::Answer {
            id: id(x),
            key: key.chars().next().unwrap_or(' ').to_ascii_lowercase(),
        }),
        _ => Err(HELP.to_string()),
    }
}

/// A `trix wf …` command line: what an agent hands in or asks.
#[derive(Debug, Clone, PartialEq)]
pub enum WfCmd {
    Submit { node: String, file: Option<PathBuf> },
    Ask { file: Option<PathBuf> },
}

pub fn parse_wf(args: &[String]) -> Result<WfCmd, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["submit", node] => Ok(WfCmd::Submit {
            node: node.to_string(),
            file: None,
        }),
        ["submit", node, "--file", f] => Ok(WfCmd::Submit {
            node: node.to_string(),
            file: Some(PathBuf::from(f)),
        }),
        ["ask"] => Ok(WfCmd::Ask { file: None }),
        ["ask", "--file", f] => Ok(WfCmd::Ask {
            file: Some(PathBuf::from(f)),
        }),
        _ => Err("usage: trix wf submit <node> [--file F] | trix wf ask [--file F]".into()),
    }
}

/// `trix workflow check`: what the one checker says, as text, and
/// whether the workflow can run.
pub fn check_text(w: &Workflow, m: &Matrix, ctx: &Ctx) -> (String, bool) {
    let report = check::check(w, m, ctx);
    (report.render(), report.runnable())
}

/// The workflow as a run would have it: triage is bound to `[triage]`,
/// which is how a case runs it. Any other is as its file has it.
fn as_it_runs(w: Workflow) -> Result<Workflow, String> {
    if w.is_triage() {
        Ok(super::triage::bound(&crate::config::load()?.triage))
    } else {
        Ok(w)
    }
}

/// The main checkout of the repository this runs in, if it's in one.
pub fn main_checkout() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let common = super::env::run_cmd(
        "git",
        &cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    Path::new(common.trim()).parent().map(Path::to_path_buf)
}

/// The matrix and what the checker knows of this machine: your config's
/// overrides, what local probes verified, and the backends installed.
pub fn live() -> Result<(Matrix, Ctx, Settings), String> {
    let settings = crate::config::load()?.workflows;
    let m = Matrix::load(&settings.capabilities, &super::caps::read_local())?;
    let ctx = Ctx {
        installed: Installed::detect(&m),
        codex_hooks_trusted: crate::worktree::state_dir()
            .join("codex-hooks-seen")
            .exists(),
        ci_paths: settings.ci_paths.clone(),
        confirm_over: settings.confirm_over,
        approvals_allow: settings.approvals.allow.clone(),
        trusted: None,
    };
    Ok((m, ctx, settings))
}

/// `trix workflow capabilities`: the matrix as a table, each entry with
/// the level it counts at on this machine.
pub fn capabilities_text(m: &Matrix, installed: &Installed, only: Option<&str>) -> String {
    let mut s = format!(
        "{:<12} {:<10} {:<11} {:<9} {:<9} {:<8} {:<11} {:<7} {}\n",
        "kind", "installed", "level", "hands-in", "read-only", "push", "prompts", "priced", "quota"
    );
    for (kind, e) in
        m.0.iter()
            .filter(|(k, _)| only.is_none_or(|o| o == k.as_str()))
    {
        let hands = match &e.hand_in {
            HandIn::All(true) => "yes".to_string(),
            HandIn::All(false) => "no".to_string(),
            HandIn::ByMode(by) => by
                .iter()
                .filter(|(_, ok)| **ok)
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(","),
        };
        s.push_str(&format!(
            "{:<12} {:<10} {:<11} {:<9} {:<9} {:<8} {:<11} {:<7} {}{}\n",
            kind,
            installed
                .0
                .get(kind)
                .map_or("-", |v| if v.is_empty() { "?" } else { v.as_str() }),
            m.level(kind, installed).label(),
            hands.chars().take(9).collect::<String>(),
            if e.read_only_enforced {
                "enforced"
            } else {
                "by word"
            },
            e.push,
            m.decision(kind, installed).label(),
            if e.priced { "yes" } else { "no" },
            e.quota_signal,
            if e.overridden {
                "  (overridden in your config)"
            } else {
                ""
            },
        ));
    }
    s.push_str(
        "\nlevel: verified (a probe passed on the installed version), documented, or unverified (counts as no).\n\
         prompts: whether trix may answer a permission prompt through this kind's hook: only when verified.\n",
    );
    s
}

/// The `trix case` command a workflow command is for a triage run: the two
/// are the same thing under two names.
pub fn as_case(cmd: &Cmd, is_case: impl Fn(&str) -> bool) -> Option<Vec<String>> {
    let words = |w: &[&str]| Some(w.iter().map(|s| s.to_string()).collect());
    match cmd {
        Cmd::Approve(id) if is_case(id) => words(&["approve", id]),
        Cmd::Abort(id) if is_case(id) => words(&["abort", id]),
        Cmd::Show { id, json: false } if is_case(id) => words(&["show", id]),
        Cmd::Show { id, json: true } if is_case(id) => words(&["show", id, "--json"]),
        Cmd::Answer { id, key } if is_case(id) => words(&["answer", id, &key.to_string()]),
        _ => None,
    }
}

/// The workflows a workflow's workflow nodes name, all the way down.
pub fn subs(stores: &Stores, w: &Workflow) -> Result<Vec<Workflow>, String> {
    let mut names: Vec<String> = w
        .nodes
        .values()
        .filter_map(|n| n.workflow.clone().filter(|_| n.kind == "workflow"))
        .collect();
    let (mut seen, mut out) = (std::collections::BTreeSet::new(), Vec::new());
    while let Some(name) = names.pop() {
        if seen.insert(name.clone()) {
            let (sub, _) = stores.load(&name)?;
            names.extend(
                sub.nodes
                    .values()
                    .filter_map(|n| n.workflow.clone().filter(|_| n.kind == "workflow")),
            );
            out.push(sub);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Whether a workflow is trusted as it is now: a built-in is, a file is
/// when its content is what you trusted.
pub fn trusted(stores: &Stores, w: &Workflow, builtin: bool) -> Result<bool, String> {
    let hash = super::policy::content_hash(w, &subs(stores, w)?);
    Ok(super::policy::is_trusted(
        &super::policy::trust_file(),
        w,
        &hash,
        builtin,
    ))
}

/// Stand-in values for a workflow's inputs, for a dry run.
pub fn sample_inputs(w: &Workflow) -> BTreeMap<String, serde_json::Value> {
    w.inputs
        .iter()
        .map(|(name, i)| {
            let v = match i.resolve.as_deref() {
                Some(_) => serde_json::json!({"number": 1, "title": "(a sample issue)", "body": "(the issue's text)", "url": "(its URL)", "outsiders": []}),
                None => serde_json::json!("(sample)"),
            };
            (name.clone(), v)
        })
        .collect()
}

/// Start a run of the workflow named `name` in `repo`: checked, trusted as
/// it is, its pane opened as the server allows. `deadline` and `kill_at`:
/// a schedule's, over the workflow's own.
pub fn start_named(
    repo: &Path,
    name: &str,
    inputs: &BTreeMap<String, String>,
    deadline: Option<&str>,
    kill_at: Option<&str>,
) -> Result<String, String> {
    let root = runs_root();
    let stores = Stores::of(Some(repo));
    let (mut w, origin) = stores.load(name)?;
    let (m, mut ctx, settings) = live()?;
    // A file runs only as you trusted it: a change since then needs your trust again.
    let trusted = trusted(&stores, &w, origin == super::store::Origin::Builtin)?;
    ctx.trusted = Some(trusted);
    if let Some(d) = deadline {
        w.limits.deadline = Some(d.to_string());
    }
    if let Some(k) = kill_at {
        w.limits.kill_at = Some(k.to_string());
    }
    let mut env = Live::looking("workflow");
    let l = super::launch::Launch {
        stores: &stores,
        repo,
        m: &m,
        ctx: &ctx,
        settings: &settings,
        trusted,
    };
    let run = super::launch::start(&mut env, &root, &l, &w, inputs)?;
    // The server says whether this pane may start it: an agent's only as
    // [workflows.agents] allows.
    let allowed = control(Request::Wf {
        run: Some(run.id.clone()),
        op: Op::Start,
    });
    if let Err(e) = allowed.and_then(|_| env.open_run(&run.id, repo)) {
        let _ = std::fs::remove_dir_all(engine::dir_of(&root, &run.id));
        return Err(e);
    }
    Ok(format!("{}\n", run.id))
}

/// `trix workflow scheduler`: start what's scheduled and what's triggered,
/// looking every half minute, until it's stopped. It needs a trix session
/// to open the runs' panes in.
fn scheduler() -> i32 {
    println!("trix workflow scheduler: looking every 30 s (ctrl+c stops it)");
    let fired_file = crate::worktree::state_dir().join("triggers-fired.json");
    let mut digest_day = 0u32;
    loop {
        let now = crate::prices::now();
        let at = super::sched::Moment::local(now);
        // Cron.
        let file = super::sched::file();
        let mut all = super::sched::load(&file);
        let due = super::sched::due(&mut all, now, at);
        if !due.is_empty() {
            let _ = super::sched::save(&file, &all);
        }
        for s in due {
            match start_named(
                &s.repo,
                &s.workflow,
                &s.inputs,
                s.deadline.as_deref(),
                s.kill_at.as_deref(),
            ) {
                Ok(id) => println!(
                    "started {} ({}, scheduled {})",
                    id.trim(),
                    s.workflow,
                    s.cron
                ),
                Err(e) => println!(
                    "couldn't start {}: {}",
                    s.workflow,
                    e.lines().next().unwrap_or_default()
                ),
            }
        }
        // Triggers, for the repository this runs in.
        if let Some(repo) = main_checkout() {
            let stores = Stores::of(Some(&repo));
            let flows: Vec<(String, Vec<super::model::Trigger>)> = stores
                .list()
                .iter()
                .filter_map(|(n, _)| stores.load(n).ok())
                .map(|(w, _)| (w.name, w.triggers))
                .filter(|(_, t)| !t.is_empty())
                .collect();
            if !flows.is_empty() {
                let mut facts: Vec<super::trigger::Fact> = engine::all(&runs_root())
                    .iter()
                    .filter(|r| r.state.over())
                    .map(|r| super::trigger::Fact::RunEnded {
                        run: r.id.clone(),
                        workflow: r.workflow.name.clone(),
                        state: r.state.label().into(),
                    })
                    .collect();
                for t in flows
                    .iter()
                    .flat_map(|(_, t)| t)
                    .filter(|t| t.on == "github")
                {
                    let kind = t.kind.as_deref().unwrap_or("issue");
                    let mut args = vec![
                        kind,
                        "list",
                        "--state",
                        "open",
                        "--json",
                        "number,labels",
                        "--limit",
                        "50",
                    ];
                    if let Some(l) = &t.label {
                        args.extend(["--label", l.as_str()]);
                    }
                    if let Ok(json) = super::env::run_cmd("gh", &repo, &args) {
                        facts.extend(super::trigger::github_facts(kind, &json));
                    }
                }
                let head = [
                    "log",
                    "-1",
                    "--format=%H",
                    "--name-only",
                    "refs/remotes/origin/HEAD",
                ];
                if let Ok(out) = super::env::run_cmd("git", &repo, &head) {
                    let mut lines = out.lines().filter(|l| !l.is_empty());
                    if let Some(sha) = lines.next() {
                        facts.push(super::trigger::Fact::Files {
                            sha: sha.to_string(),
                            paths: lines.map(String::from).collect(),
                        });
                    }
                }
                let mut fired = super::trigger::Fired::load(&fired_file);
                for s in super::trigger::due(&flows, &facts, &mut fired) {
                    match start_named(&repo, &s.workflow, &s.inputs, None, None) {
                        Ok(id) => println!(
                            "started {} ({}, because {})",
                            id.trim(),
                            s.workflow,
                            s.because
                        ),
                        Err(e) => println!(
                            "couldn't start {}: {}",
                            s.workflow,
                            e.lines().next().unwrap_or_default()
                        ),
                    }
                }
                let _ = fired.save(&fired_file);
            }
        }
        // The digest, once a day at its time.
        if let Ok(settings) = crate::config::load().map(|c| c.workflows)
            && let Some(when) = settings
                .digest
                .as_deref()
                .and_then(|t| super::settings::clock(t).ok())
            && at.hour * 60 + at.minute == when
            && digest_day != at.day
        {
            digest_day = at.day;
            let runs = engine::all(&runs_root());
            let ended: BTreeMap<String, u64> = runs
                .iter()
                .map(|r| (r.id.clone(), r.log.last().map_or(r.created, |l| l.0)))
                .collect();
            let views: Vec<_> = runs.iter().map(|r| r.view(&[])).collect();
            if let Some(text) = super::sched::digest(&views, &ended, now.saturating_sub(86_400)) {
                crate::platform::notify("trix workflows: the last day", &text, false);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}

/// A line you type.
fn ask(what: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{what}: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

/// The run view: the overview, drawn again every couple of seconds. A key
/// then enter: n the next item that needs you, y approve, p pause, a
/// abort, R revoke, q leave.
fn watch(root: &Path, id: &str) -> i32 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
            let _ = tx.send(line.trim().to_string());
            line.clear();
        }
    });
    let mut at = None;
    loop {
        let Ok(run) = engine::load(root, id) else {
            return fail(format!("no run named '{id}'"));
        };
        let seen = Live::looking("workflow").agents().unwrap_or_default();
        let needs = super::overview::needs(&run, &seen);
        let width = crossterm::terminal::size().map_or(100, |(w, _)| w as usize);
        print!(
            "\x1b[2J\x1b[H{}",
            super::overview::render(&run, &seen, at, width)
        );
        if let Ok(key) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            let op = match key.as_str() {
                "n" => {
                    at = super::overview::next(&needs, at);
                    None
                }
                "y" => Some(Op::Approve),
                "p" => Some(Op::Pause),
                "a" => Some(Op::Abort),
                "R" => Some(Op::Revoke),
                "l" => {
                    print!("{}", super::runlog::render(&engine::dir_of(root, id)));
                    let _ = rx.recv();
                    None
                }
                "q" => return 0,
                _ => None,
            };
            if let Some(op) = op
                && let Err(e) = control(Request::Wf {
                    run: Some(id.to_string()),
                    op,
                })
            {
                println!("{e}");
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
}

/// `trix workflow new`: the wizard. It asks, then opens what it made in
/// the builder.
fn new_workflow() -> Result<String, String> {
    let (m, ctx, _) = live()?;
    let template = ask("template (issue-to-pr, bug-repro, nightly-deps, blank) [issue-to-pr]")?;
    let template = if template.is_empty() {
        "issue-to-pr".to_string()
    } else {
        template
    };
    let name = ask("its name")?;
    let goal = ask("the goal, in a sentence (enter keeps the template's)")?;
    let mut answers = super::builder::Answers {
        template: template.clone(),
        name,
        goal: Some(goal),
        ..Default::default()
    };
    // A backend per role, with each kind's fit for it.
    if let Some(w) = super::store::builtin(&template) {
        let b = super::builder::Builder::new(w.clone(), m.clone(), ctx.clone());
        for (id, n) in w
            .nodes
            .iter()
            .filter(|(_, n)| n.is_agent() && n.agent.as_ref().is_some_and(|a| !a.starts_with("{{")))
        {
            let fits: Vec<String> = b
                .backends(id)
                .into_iter()
                .map(|(k, fit, _)| format!("{} {k}", fit.mark()))
                .collect();
            let kind = ask(&format!(
                "{id} ({}): {} [{}]",
                n.role().map_or("agent", |r| r.name()),
                fits.join("  "),
                n.agent.clone().unwrap_or_default()
            ))?;
            if !kind.is_empty() {
                answers.backends.insert(id.clone(), kind);
            }
        }
    }
    answers.autopilot = ask("supervised or autopilot [supervised]")?.starts_with("auto");
    let w = super::builder::wizard(&answers, &m, &ctx)?;
    let stores = Stores::of(main_checkout().as_deref());
    stores.save(&w, super::store::Origin::Personal)?;
    edit(&w.name)
}

/// `trix workflow edit`: the builder, in this terminal.
fn edit(name: &str) -> Result<String, String> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    let repo = main_checkout();
    let stores = Stores::of(repo.as_deref());
    let (mut w, mut origin) = stores.load(name)?;
    // A built-in is read-only: what's edited is a copy under a name of its own.
    if origin == super::store::Origin::Builtin {
        w.name = format!("my-{}", w.name);
        w.builtin = false;
        origin = super::store::Origin::Personal;
    }
    let (m, ctx, _) = live()?;
    let mut b = super::builder::Builder::new(w, m, ctx);
    crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let _ = crossterm::execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture);
    let mut term =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
            .map_err(|e| e.to_string())?;
    let save = |b: &mut super::builder::Builder| -> Result<String, String> {
        let path = stores.save(&b.w, origin)?;
        // Your own edits are trusted as you make them.
        let hash = super::policy::content_hash(&b.w, &subs(&stores, &b.w)?);
        let trusted = control(Request::Wf {
            run: None,
            op: Op::Trust {
                name: b.w.name.clone(),
                hash,
            },
        });
        b.dirty = false;
        Ok(format!(
            "saved {}{}",
            crate::app::tilde(&path),
            if trusted.is_ok() {
                ""
            } else {
                " (not trusted yet: trix workflow trust)"
            }
        ))
    };
    let out = loop {
        let _ = term.draw(|f| b.draw(f));
        match crossterm::event::read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match b.key(k) {
                super::builder::Outcome::Stay => {}
                super::builder::Outcome::Quit => break Ok(String::new()),
                super::builder::Outcome::Save => b.message = save(&mut b).unwrap_or_else(|e| e),
                super::builder::Outcome::DryRun => break Ok("dry-run".to_string()),
                super::builder::Outcome::Run => break save(&mut b).map(|_| "run".to_string()),
            },
            Ok(Event::Mouse(ev)) => b.mouse(ev),
            Ok(_) => {}
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    let _ = crossterm::terminal::disable_raw_mode();
    match out?.as_str() {
        "dry-run" => {
            let (m, ctx, settings) = live()?;
            let dry = super::dryrun::dry_run(
                &b.w,
                &m,
                &ctx,
                &settings,
                super::dryrun::Scenario::Happy,
                sample_inputs(&b.w),
            );
            Ok(dry.render(false))
        }
        "run" => {
            println!("{}", b.launch_text());
            let repo = repo.ok_or("not in a git repository")?;
            let inputs: BTreeMap<String, String> =
                b.w.inputs
                    .iter()
                    .filter(|(_, i)| i.required)
                    .map(|(k, i)| ask(&format!("input {k} ({})", i.kind)).map(|v| (k.clone(), v)))
                    .collect::<Result<_, _>>()?;
            start_named(&repo, &b.w.name, &inputs, None, None)
        }
        _ => Ok(String::new()),
    }
}

fn fail(e: impl std::fmt::Display) -> i32 {
    eprintln!("trix: workflow: {e}");
    1
}

fn say(r: Result<String, String>) -> i32 {
    match r {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => fail(e),
    }
}

/// One run, for you to read: its nodes and its log.
pub fn show(r: &WfRun, seen: &[super::env::Seen]) -> String {
    let v = r.view(seen);
    let spent = if v.unpriced {
        format!("{} + ?", crate::cost::usd(v.spent_usd))
    } else {
        crate::cost::usd(v.spent_usd)
    };
    let mut s = format!(
        "{} · {} · {}\n{}\n{} of {} done · {} blocked · cycles {} ({} wasted) of at most {} · {spent} · {}m of {}m\n",
        v.id,
        v.workflow,
        v.state.label(),
        v.goal,
        v.done(),
        v.nodes.len(),
        v.blocked(),
        v.cycles,
        v.wasted,
        r.ceiling,
        r.inflight / 60,
        r.wall_clock / 60
    );
    if !v.why.is_empty() {
        s.push_str(&format!("{}: {}\n", v.state.label(), v.why));
    }
    s.push('\n');
    for n in &v.nodes {
        let owns = if n.owns.is_empty() {
            "–".to_string()
        } else {
            n.owns.join(",")
        };
        s.push_str(&format!(
            "  {:<14} {:<10} {:<9} {:<18} {}\n",
            n.id,
            n.state.label(),
            n.backend,
            owns,
            n.last
        ));
    }
    if !r.log.is_empty() {
        s.push_str("\nlog:\n");
        for (at, text) in &r.log {
            let (_, day, secs) = crate::ledger::local(*at);
            s.push_str(&format!(
                "  {day:>2}d {:02}:{:02}  {text}\n",
                secs / 3600,
                secs / 60 % 60
            ));
        }
    }
    s
}

/// `trix workflow …`.
pub fn run(args: &[String]) -> i32 {
    let cmd = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprint!(
                "{}",
                if e == HELP {
                    e
                } else {
                    format!("trix: workflow: {e}\n")
                }
            );
            return 2;
        }
    };
    let cases = crate::case::root();
    if let Some(case) = as_case(&cmd, |id| crate::case::load(&cases, id).is_ok()) {
        return crate::case::run(&case);
    }
    let root = runs_root();
    let wf = |run: String, op: Op| say(control(Request::Wf { run: Some(run), op }));
    match cmd {
        // `trix triage 123` is this, under its older name.
        Cmd::Run { name, inputs } if name == "triage" => match inputs.get("target") {
            Some(target) if inputs.len() == 1 => crate::case::triage(std::slice::from_ref(target)),
            _ => {
                eprintln!("trix: workflow: triage takes one input: --input target=<PR or issue>");
                2
            }
        },
        Cmd::Run { name, inputs } => {
            let Some(repo) = main_checkout() else {
                return fail("not in a git repository");
            };
            match start_named(&repo, &name, &inputs, None, None) {
                Ok(out) => say(Ok(out)),
                // A refusal is the checker's own text: nothing to put before it.
                Err(e) if e.contains(" error") && e.contains(" warning") => {
                    eprint!("{e}");
                    1
                }
                Err(e) => fail(e),
            }
        }
        Cmd::Pane(id) => match engine::load(&root, &id) {
            // A rehearsal: the guard on everything, trix's own push, PR and
            // merge stubbed, and the clone and mirror deleted once it's over.
            Ok(mut run) if run.rehearsal.is_some() => {
                let base = run.rehearsal.clone().unwrap_or_default();
                let mut live = Live::in_pane("workflow");
                let mut env = super::dryrun::Rehearse {
                    inner: &mut live,
                    pr: false,
                };
                super::runner::drive(&mut run, &root, &mut env, || {
                    let _ = std::fs::remove_dir_all(&base);
                    println!(
                        "the rehearsal's clone and mirror are deleted: nothing was pushed, opened or merged"
                    );
                })
            }
            _ => super::runner::run_pane::<WfRun>(&root, &id),
        },
        Cmd::DryRun {
            name,
            scenario,
            prompts,
        } => {
            let stores = Stores::of(main_checkout().as_deref());
            say(stores.load(&name).and_then(|(w, _)| {
                let (m, ctx, settings) = live()?;
                let w = as_it_runs(w)?;
                let scenario = super::dryrun::Scenario::parse(&scenario)?;
                Ok(
                    super::dryrun::dry_run(&w, &m, &ctx, &settings, scenario, sample_inputs(&w))
                        .render(prompts),
                )
            }))
        }
        Cmd::Rehearse { name, inputs } => {
            let Some(repo) = main_checkout() else {
                return fail("not in a git repository");
            };
            let stores = Stores::of(Some(&repo));
            say(stores.load(&name).and_then(|(w, _)| {
                let (m, ctx, settings) = live()?;
                let r = super::dryrun::prepare(&repo, |dir, args| super::env::run_cmd("git", dir, args))?;
                let mut env = Live::looking("workflow");
                let l = super::launch::Launch { stores: &stores, repo: &r.clone, m: &m, ctx: &ctx, settings: &settings, trusted: false };
                let started = super::launch::start(&mut env, &root, &l, &w, &inputs).and_then(|mut run| {
                    run.rehearsal = Some(r.base.clone());
                    engine::store(&root, &run)?;
                    env.open_run(&run.id, &r.clone).map(|_| run.id)
                });
                match started {
                    Ok(id) => Ok(format!("{id}\nrehearsing in {}: its origin is a local mirror, and it's deleted afterwards\n", crate::app::tilde(&r.clone))),
                    Err(e) => {
                        r.cleanup();
                        Err(e)
                    }
                }
            }))
        }
        Cmd::Ls { json } => {
            let seen = Live::looking("workflow").agents().unwrap_or_default();
            let mut runs: Vec<_> = crate::case::all(&cases)
                .iter()
                .map(crate::case::Case::view)
                .collect();
            runs.extend(engine::all(&root).iter().map(|r| r.view(&seen)));
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&runs).unwrap_or_default()
                );
            } else if runs.is_empty() {
                println!("no runs yet: trix workflow run <name> starts one");
            } else {
                let w = runs.iter().map(|r| r.id.len()).max().unwrap_or(0);
                for r in &runs {
                    let why = if r.why.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", r.why)
                    };
                    println!(
                        "{:<w$}  {:<12}  {}{why}  ·  {}",
                        r.id,
                        r.workflow,
                        r.state.label(),
                        r.goal
                    );
                }
            }
            0
        }
        Cmd::Show { id, json } => say(engine::load(&root, &id).and_then(|r| {
            if json {
                return serde_json::to_string_pretty(&r)
                    .map(|s| s + "\n")
                    .map_err(|e| e.to_string());
            }
            Ok(show(
                &r,
                &Live::looking("workflow").agents().unwrap_or_default(),
            ))
        })),
        Cmd::Check { name, fix } => {
            let stores = Stores::of(main_checkout().as_deref());
            let found = stores.load(&name).and_then(|(w, origin)| {
                let w = as_it_runs(w)?;
                live().map(|(m, ctx, _)| (w, origin, m, ctx))
            });
            let (mut w, origin, m, ctx) = match found {
                Ok(x) => x,
                Err(e) => return fail(e),
            };
            if fix {
                let did = check::fix_all(&mut w, &m, &ctx);
                for line in &did {
                    println!("fixed: {line}");
                }
                if !did.is_empty() {
                    match stores.save(&w, origin) {
                        Ok(p) => println!("saved {}", crate::app::tilde(&p)),
                        Err(e) => println!("not saved: {e}"),
                    }
                }
            }
            let (text, runnable) = check_text(&w, &m, &ctx);
            print!("{text}");
            i32::from(!runnable)
        }
        Cmd::Capabilities {
            kind,
            verify: false,
        } => match live() {
            Ok((m, ctx, _)) => {
                print!("{}", capabilities_text(&m, &ctx.installed, kind.as_deref()));
                0
            }
            Err(e) => fail(e),
        },
        Cmd::Capabilities { kind, verify: true } => say(live()
            .and_then(|(m, ctx, _)| super::probe::verify(&m, &ctx.installed, kind.as_deref()))),
        Cmd::Overview(id) => say(engine::load(&root, &id).map(|r| {
            let seen = Live::looking("workflow").agents().unwrap_or_default();
            let at = super::overview::next(&super::overview::needs(&r, &seen), None);
            super::overview::render(&r, &seen, at, 100)
        })),
        Cmd::Log { id, verify } => {
            let dir = engine::dir_of(&root, &id);
            if !verify {
                return say(engine::load(&root, &id).map(|_| super::runlog::render(&dir)));
            }
            match super::runlog::verify(&dir) {
                Ok(n) => say(Ok(format!("{id}: {n} lines, each as it was written\n"))),
                Err(wrong) => fail(format!("{id}'s log was changed:\n  {}", wrong.join("\n  "))),
            }
        }
        Cmd::Audit(id) => say(engine::load(&root, &id).map(|_| {
            let text = std::fs::read_to_string(engine::dir_of(&root, &id).join("approvals.jsonl"))
                .unwrap_or_default();
            if text.is_empty() {
                "no approval was made in this run\n".to_string()
            } else {
                text
            }
        })),
        Cmd::Schedule {
            name,
            cron,
            deadline,
            kill_at,
            inputs,
        } => {
            let Some(repo) = main_checkout() else {
                return fail("not in a git repository");
            };
            say(Stores::of(Some(&repo)).load(&name).and_then(|_| {
                let file = super::sched::file();
                let mut all = super::sched::load(&file);
                all.retain(|s| !(s.workflow == name && s.repo == repo));
                all.push(super::sched::Schedule { workflow: name.clone(), cron: cron.clone(), deadline, kill_at, inputs, repo, last: 0 });
                super::sched::save(&file, &all)?;
                Ok(format!("{name} is scheduled: {cron}. It starts while trix workflow scheduler runs (trix workflow units writes a unit for that)\n"))
            }))
        }
        Cmd::ScheduleLs => {
            let all = super::sched::load(&super::sched::file());
            for s in &all {
                println!(
                    "{:<16} {:<14} {}{}",
                    s.workflow,
                    s.cron,
                    crate::app::tilde(&s.repo),
                    s.deadline
                        .as_ref()
                        .map_or(String::new(), |d| format!("  deadline {d}"))
                );
            }
            if all.is_empty() {
                println!(
                    "nothing is scheduled: trix workflow schedule <name> --cron \"0 2 * * *\""
                );
            }
            0
        }
        Cmd::ScheduleRm(name) => {
            let file = super::sched::file();
            let mut all = super::sched::load(&file);
            let before = all.len();
            all.retain(|s| s.workflow != name);
            if all.len() == before {
                return fail(format!("{name} isn't scheduled"));
            }
            say(super::sched::save(&file, &all).map(|_| format!("{name} is no longer scheduled\n")))
        }
        Cmd::Scheduler => scheduler(),
        Cmd::Units { kind, yes } => say(super::sched::Unit::parse(&kind).and_then(|unit| {
            let home =
                std::path::PathBuf::from(std::env::var("HOME").map_err(|_| "no home folder")?);
            let exe = crate::platform::trix_exe().map_err(|e| e.to_string())?;
            unit.install(&home, &exe, yes)
        })),
        Cmd::Edit(name) => say(edit(&name)),
        Cmd::New => say(new_workflow()),
        Cmd::Open => {
            // A run that's going: its overview, refreshed. Else the builder.
            let going = engine::all(&root).into_iter().find(|r| !r.state.over());
            match going {
                Some(run) => watch(&root, &run.id),
                None => {
                    let stores = Stores::of(main_checkout().as_deref());
                    let names: Vec<String> = stores
                        .list()
                        .into_iter()
                        .map(|(n, o)| format!("{n} ({})", o.label()))
                        .collect();
                    println!("no run is going. Workflows: {}", names.join(", "));
                    say(ask("which one to open in the builder")
                        .and_then(|name| edit(name.split_whitespace().next().unwrap_or_default())))
                }
            }
        }
        Cmd::Export(name) => {
            let stores = Stores::of(main_checkout().as_deref());
            say(stores.load(&name).and_then(|(w, _)| {
                live().and_then(|(m, ctx, _)| super::yaml::export(&w, &m, &ctx))
            }))
        }
        Cmd::Import(file) => {
            let stores = Stores::of(main_checkout().as_deref());
            say(std::fs::read_to_string(&file)
                .map_err(|e| format!("{file}: {e}"))
                .and_then(|text| {
                    let (m, ctx, _) = live()?;
                    let (w, report, path) = super::yaml::import_into(&stores, &text, &m, &ctx)?;
                    // What it is, before you're asked anything about it.
                    println!(
                        "{}saved {}",
                        super::yaml::import_text(&report),
                        crate::app::tilde(&path)
                    );
                    // Trust is yours to give, and asked for: never taken.
                    if ask(&format!("trust {} as it is now? [y/N]", w.name))?
                        .eq_ignore_ascii_case("y")
                    {
                        let hash = super::policy::content_hash(&w, &subs(&stores, &w)?);
                        return control(Request::Wf {
                            run: None,
                            op: Op::Trust {
                                name: w.name.clone(),
                                hash,
                            },
                        });
                    }
                    Ok(String::new())
                }))
        }
        Cmd::Approve(id) => wf(id, Op::Approve),
        Cmd::Abort(id) => wf(id, Op::Abort),
        Cmd::Pause(id) => wf(id, Op::Pause),
        Cmd::Resume(id) => wf(id, Op::Resume),
        Cmd::Revoke(id) => wf(id, Op::Revoke),
        Cmd::Answer { id, key } => wf(id, Op::Answer { key }),
        Cmd::Trust(name) => {
            let stores = Stores::of(main_checkout().as_deref());
            say(stores.load(&name).and_then(|(w, _)| {
                let hash = super::policy::content_hash(&w, &subs(&stores, &w)?);
                control(Request::Wf {
                    run: None,
                    op: Op::Trust { name, hash },
                })
            }))
        }
        Cmd::Autopilot { name, yes } => {
            let stores = Stores::of(main_checkout().as_deref());
            say(stores.load(&name).and_then(|(mut w, origin)| {
                let (m, ctx, _) = live()?;
                let before = check::check(&w, &m, &ctx);
                // Autopilot can't be switched on while the checker shows an error.
                if before.errors() > 0 {
                    return Err(format!("fix these first:\n{}", before.render()));
                }
                let nodes = super::policy::autopilot(&mut w, &m, &ctx, "", "main");
                let mut out = super::policy::preview_text(&w, &nodes);
                out.push_str(&check::check(&w, &m, &ctx).render());
                if yes {
                    let path = stores.save(&w, origin)?;
                    let hash = super::policy::content_hash(&w, &subs(&stores, &w)?);
                    control(Request::Wf { run: None, op: Op::Trust { name: w.name.clone(), hash } })?;
                    out.push_str(&format!("saved {}: add \"{}\" to [approvals] allow in your config to switch its approvals on\n", crate::app::tilde(&path), w.name));
                } else {
                    out.push_str("nothing was changed: --yes switches it on\n");
                }
                Ok(out)
            }))
        }
    }
}

/// `trix wf …`: what an agent hands in or asks, from its pane. The server
/// knows which run and which node by the pane it comes from.
pub fn wf(args: &[String]) -> i32 {
    let cmd = match parse_wf(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let read = |file: &Option<PathBuf>| -> Result<String, String> {
        let mut text = String::new();
        match file {
            // For an agent whose permissions refuse a heredoc.
            Some(f) => {
                text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?
            }
            None => {
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text);
            }
        }
        if text.trim().is_empty() {
            return Err("pipe it in on stdin, or name a file with --file".into());
        }
        Ok(text)
    };
    let (file, sent) = match cmd {
        WfCmd::Submit { node, file } => {
            let r = read(&file).and_then(|report| {
                control(Request::Wf {
                    run: None,
                    op: Op::Submit { node, report },
                })
            });
            (file, r)
        }
        WfCmd::Ask { file } => {
            let r = read(&file).and_then(|text| {
                control(Request::Wf {
                    run: None,
                    op: Op::Ask { text },
                })
            });
            (file, r)
        }
    };
    // Taken: the file is gone, so it can't be committed. One sent back
    // stays, to be fixed and handed in again.
    if let (Ok(_), Some(f)) = (&sent, &file) {
        let _ = std::fs::remove_file(f);
    }
    match sent {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("trix: wf: {e}");
            1
        }
    }
}
