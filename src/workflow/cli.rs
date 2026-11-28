//! `trix workflow …` and `trix wf …`.

use std::collections::BTreeMap;

pub const HELP: &str = "\
usage:
  trix workflow run <name> [--input k=v]…   start a run of a workflow
                                            (triage: --input target=<PR or issue>)
  trix workflow ls [--json]                 every run and where it is
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
        ["approve", id] => Ok(Cmd::Approve(id.to_string())),
        ["abort", id] => Ok(Cmd::Abort(id.to_string())),
        _ => Err(HELP.to_string()),
    }
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
        Cmd::Run { name, .. } => {
            eprintln!("trix: workflow: no workflow named '{name}'");
            1
        }
        Cmd::Approve(id) | Cmd::Abort(id) => {
            eprintln!("trix: workflow: no run named '{id}' (trix workflow ls)");
            1
        }
    }
}
