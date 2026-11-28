//! `trix workflow …` and `trix wf …`.

use super::caps::{HandIn, Installed, Matrix};
use super::check::{self, Ctx};
use super::model::Workflow;
use super::settings::Settings;
use super::store::Stores;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
usage:
  trix workflow run <name> [--input k=v]…   start a run of a workflow
                                            (triage: --input target=<PR or issue>)
  trix workflow ls [--json]                 every run and where it is
  trix workflow check <name> [--fix]        the one checker: the builder, import, dry run and
                                            run start use it too. --fix applies what it can
  trix workflow capabilities [--kind k] [--verify]
                                            what each backend can do, and how well trix knows
                                            it. --verify runs real probes (opt-in: it spends a
                                            little quota)
  trix workflow approve <run>               yours only: let a run's gate through
  trix workflow abort <run>                 stop a run
";

/// A `trix workflow …` command line, read.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Run {
        name: String,
        inputs: BTreeMap<String, String>,
    },
    Ls {
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
}

/// What `trix workflow <args>` asks for, or why it isn't a command.
pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
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
        ["ls"] => Ok(Cmd::Ls { json }),
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
        ["approve", id] => Ok(Cmd::Approve(id.to_string())),
        ["abort", id] => Ok(Cmd::Abort(id.to_string())),
        _ => Err(HELP.to_string()),
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
        _ => None,
    }
}

fn fail(e: impl std::fmt::Display) -> i32 {
    eprintln!("trix: workflow: {e}");
    1
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
    let root = crate::case::root();
    if let Some(case) = as_case(&cmd, |id| crate::case::load(&root, id).is_ok()) {
        return crate::case::run(&case);
    }
    match cmd {
        // `trix triage 123` is this, under its older name.
        Cmd::Run { name, inputs } if name == "triage" => match inputs.get("target") {
            Some(target) if inputs.len() == 1 => crate::case::triage(std::slice::from_ref(target)),
            _ => {
                eprintln!("trix: workflow: triage takes one input: --input target=<PR or issue>");
                2
            }
        },
        Cmd::Ls { json } => {
            let runs: Vec<_> = crate::case::all(&root)
                .iter()
                .map(crate::case::Case::view)
                .collect();
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
                        "{:<w$}  {:<8}  {}{why}  ·  {}",
                        r.id,
                        r.workflow,
                        r.state.label(),
                        r.goal
                    );
                }
            }
            0
        }
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
        Cmd::Capabilities { verify: true, .. } => fail("probes aren't built into this trix yet"),
        Cmd::Run { name, .. } => fail(format!("no workflow named '{name}'")),
        Cmd::Approve(id) | Cmd::Abort(id) => {
            fail(format!("no run named '{id}' (trix workflow ls)"))
        }
    }
}
