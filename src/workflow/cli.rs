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
  trix workflow run <name> [--input k=v]…   start a run of a workflow
                                            (triage: --input target=<PR or issue>)
  trix workflow ls [--json]                 every run and where it is
  trix workflow show <run> [--json]         one run: its nodes, its log
  trix workflow check <name> [--fix]        the one checker: the builder, import, dry run and
                                            run start use it too. --fix applies what it can
  trix workflow capabilities [--kind k] [--verify]
                                            what each backend can do, and how well trix knows
                                            it. --verify runs real probes (opt-in: it spends a
                                            little quota)
  trix workflow pause|resume|abort <run>    from an agent pane: as [workflows.agents] allows
  trix workflow approve <run>               yours only: refused from every agent pane and check
  trix workflow answer <run> <key>          answer what a run asks, as in its pane
  trix workflow revoke <run>                mid-run: every approval drops back to ask
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
    /// The run's pane: what a saved layout starts again.
    Pane(String),
    Ls {
        json: bool,
    },
    Show {
        id: String,
        json: bool,
    },
    Check {
        name: String,
        fix: bool,
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

/// `trix workflow check`: what the one checker says, as text.
pub fn check_text(w: &Workflow, m: &Matrix, ctx: &Ctx) -> String {
    check::check(w, m, ctx).render()
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
            let stores = Stores::of(Some(&repo));
            let started = stores.load(&name).and_then(|(w, _)| {
                let (m, ctx, settings) = live()?;
                let mut env = Live::looking("workflow");
                let l = super::launch::Launch {
                    stores: &stores,
                    repo: &repo,
                    m: &m,
                    ctx: &ctx,
                    settings: &settings,
                    trusted: true,
                };
                let run = super::launch::start(&mut env, &root, &l, &w, &inputs)?;
                if let Err(e) = env.open_run(&run.id, &repo) {
                    let _ = std::fs::remove_dir_all(engine::dir_of(&root, &run.id));
                    return Err(e);
                }
                Ok(format!("{}\n", run.id))
            });
            // A refusal is the checker's own text: nothing to put before it.
            match started {
                Ok(out) => say(Ok(out)),
                Err(e) if e.contains(" error") && e.contains(" warning") => {
                    eprint!("{e}");
                    1
                }
                Err(e) => fail(e),
            }
        }
        Cmd::Pane(id) => super::runner::run_pane::<WfRun>(&root, &id),
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
            let found = stores
                .load(&name)
                .and_then(|(w, origin)| live().map(|(m, ctx, _)| (w, origin, m, ctx)));
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
            let report = check::check(&w, &m, &ctx);
            print!("{}", report.render());
            i32::from(!report.runnable())
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
        Cmd::Approve(id) => wf(id, Op::Approve),
        Cmd::Abort(id) => wf(id, Op::Abort),
        Cmd::Pause(id) => wf(id, Op::Pause),
        Cmd::Resume(id) => wf(id, Op::Resume),
        Cmd::Revoke(id) => wf(id, Op::Revoke),
        Cmd::Answer { id, key } => wf(id, Op::Answer { key }),
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
