//! Starting a run: the one checker first, then the inputs' resolvers
//! (before any prompt is written), the snapshot, and the run's pane.

use super::caps::Matrix;
use super::check::{self, Ctx, Report};
use super::engine::{self, Opts, WfRun};
use super::env::Env;
use super::model::Workflow;
use super::settings::Settings;
use std::collections::BTreeMap;
use std::path::Path;

/// Who may write what an agent reads without you looking first.
const TRUSTED: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

/// The check a run starts with: the same checker as everywhere, and a run
/// never starts with an error or untrusted. `Err` is the report as text.
pub fn preflight(w: &Workflow, m: &Matrix, ctx: &Ctx) -> Result<Report, String> {
    let report = check::check(w, m, ctx);
    if report.runnable() {
        Ok(report)
    } else {
        Err(report.render())
    }
}

/// `gh.issue`: the issue an input names, with who wrote it from outside.
fn resolve_issue(env: &mut dyn Env, repo: &Path, given: &str) -> Result<serde_json::Value, String> {
    let n: u64 = given
        .trim_start_matches('#')
        .parse()
        .map_err(|_| format!("'{given}' isn't an issue number"))?;
    let out = env
        .gh(
            repo,
            &["api", &format!("repos/{{owner}}/{{repo}}/issues/{n}")],
        )
        .map_err(|e| format!("no issue #{n}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    if v.get("pull_request").is_some() {
        return Err(format!("#{n} is a pull request, not an issue"));
    }
    let who = |v: &serde_json::Value| {
        (
            v["user"]["login"].as_str().unwrap_or("?").to_string(),
            v["author_association"]
                .as_str()
                .unwrap_or("NONE")
                .to_string(),
        )
    };
    let (author, association) = who(&v);
    let outsiders: Vec<String> = if TRUSTED.contains(&association.as_str()) {
        Vec::new()
    } else {
        vec![format!(
            "@{} ({})",
            crate::case::clean(&author),
            crate::case::clean(&association)
        )]
    };
    Ok(serde_json::json!({
        "number": n,
        "title": v["title"].as_str().unwrap_or_default(),
        "body": v["body"].as_str().unwrap_or_default(),
        "url": v["html_url"].as_str().unwrap_or_default(),
        "author": author,
        "outsiders": outsiders,
    }))
}

/// Each input the workflow declares, resolved: a resolver fetches outside
/// data now, so templates stay pure.
fn resolve(
    env: &mut dyn Env,
    repo: &Path,
    w: &Workflow,
    given: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    if let Some(extra) = given.keys().find(|k| !w.inputs.contains_key(*k)) {
        let names: Vec<&str> = w.inputs.keys().map(String::as_str).collect();
        return Err(format!(
            "'{extra}' isn't an input of {} ({})",
            w.name,
            if names.is_empty() {
                "it takes none".into()
            } else {
                names.join(", ")
            }
        ));
    }
    let mut out = BTreeMap::new();
    for (name, input) in &w.inputs {
        let Some(value) = given.get(name) else {
            if input.required {
                return Err(format!("{} needs --input {name}=<{}>", w.name, input.kind));
            }
            continue;
        };
        let v = match input.resolve.as_deref() {
            Some("gh.issue") => resolve_issue(env, repo, value)?,
            Some(other) => return Err(format!("the resolver '{other}' isn't one this trix has")),
            None => match input.kind.as_str() {
                "int" => serde_json::Value::from(
                    value
                        .parse::<i64>()
                        .map_err(|_| format!("--input {name}: '{value}' isn't a number"))?,
                ),
                _ => serde_json::Value::from(value.clone()),
            },
        };
        out.insert(name.clone(), v);
    }
    Ok(out)
}

/// What your config and the matrix say now: a run keeps to it.
pub fn opts(w: &Workflow, m: &Matrix, settings: &Settings, trusted: bool, repo_name: &str) -> Opts {
    Opts {
        approvals: trusted && settings.approvals.allow.contains(&w.name),
        ci_paths: settings.ci_paths.clone(),
        floors: super::settings::floors(
            &[settings.approvals.floors.clone(), w.policy.floors.clone()].concat(),
        )
        .unwrap_or_default(),
        notify_each: settings.approvals.notify_each,
        on_finish: settings.on_finish.clone(),
        repo_name: repo_name.into(),
        max_depth: settings.agents.max_depth,
        state_from: m
            .0
            .iter()
            .map(|(k, e)| (k.clone(), e.state.clone()))
            .collect(),
        priced: m
            .0
            .iter()
            .filter(|(_, e)| e.priced)
            .map(|(k, _)| k.clone())
            .collect(),
    }
}

/// What a start needs to know.
pub struct Launch<'a> {
    /// Where the workflows a workflow node names are found.
    pub stores: &'a super::store::Stores,
    pub repo: &'a Path,
    pub m: &'a Matrix,
    pub ctx: &'a Ctx,
    pub settings: &'a Settings,
    /// The file is trusted as it is: approvals may be on.
    pub trusted: bool,
}

/// Start a run of `w`: checked, its inputs resolved, its snapshot taken,
/// its state stored. The run is made, not yet looked at: its pane does that.
pub fn start(
    env: &mut dyn Env,
    root: &Path,
    l: &Launch,
    w: &Workflow,
    given: &BTreeMap<String, String>,
) -> Result<WfRun, String> {
    preflight(w, l.m, l.ctx)?;
    let inputs = resolve(env, l.repo, w, given)?;
    // The default branch, and the repository's name, as GitHub has them.
    let info = env
        .gh(l.repo, &["api", "repos/{owner}/{repo}"])
        .ok()
        .and_then(|o| serde_json::from_str::<serde_json::Value>(&o).ok());
    let base = match info.as_ref().and_then(|v| v["default_branch"].as_str()) {
        Some(b) => b.to_string(),
        None => env
            .git(l.repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string(),
    };
    let repo_name = info
        .as_ref()
        .and_then(|v| v["full_name"].as_str())
        .unwrap_or_default()
        .to_string();
    let hint = inputs
        .values()
        .find_map(|v| v["number"].as_u64())
        .map(|n| n.to_string());
    let id = engine::new_id(root, &w.name, hint.as_deref());
    let mut run = WfRun::new(
        &id,
        w.clone(),
        l.repo.to_path_buf(),
        &base,
        inputs,
        opts(w, l.m, l.settings, l.trusted, &repo_name),
        env.now(),
    );
    // Whether checks will come for a commit: they're on the base's head.
    if w.nodes.values().any(|n| n.kind == "gh.gate") {
        let sha = env
            .git(
                l.repo,
                &["rev-parse", &format!("refs/remotes/origin/{base}")],
            )
            .unwrap_or_default();
        run.gate.ci_expected =
            crate::case::check_runs(l.repo, sha.trim(), &mut |r, a| env.gh(r, a))
                .is_ok_and(|runs| !runs.is_empty());
    }
    // The workflows its workflow nodes name are checked and snapshotted
    // with it: a sub-run reads the snapshot, like any run.
    let mut subs: Vec<String> = w
        .nodes
        .values()
        .filter_map(|n| n.workflow.clone().filter(|_| n.kind == "workflow"))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = subs.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let (sub, _) = l.stores.load(&name)?;
        preflight(&sub, l.m, l.ctx)
            .map_err(|e| format!("the workflow node's workflow doesn't check:\n{e}"))?;
        subs.extend(
            sub.nodes
                .values()
                .filter_map(|n| n.workflow.clone().filter(|_| n.kind == "workflow")),
        );
        super::store::snapshot(&engine::dir_of(root, &id).join("sub").join(&name), &sub)?;
    }
    super::store::snapshot(&engine::dir_of(root, &id), w)?;
    engine::store(root, &run)?;
    Ok(run)
}
