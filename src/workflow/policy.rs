//! Who may do what to a run, and what trix itself never does.
//!
//! Floors and policy bind what trix does itself (the gate's push, PR and
//! merge, the land step) and the permission requests routed to it. A
//! command an agent runs without a prompt is limited only by its profile
//! and the guard: nothing here claims otherwise.

use super::caps::{Level, Matrix};
use super::check::{self, Ctx, ci_globs, overlap};
use super::engine::{self, Op, WfRun, short};
use super::env::Env;
use super::model::{Allow, MergeApproval, Mode, Notify, OnLimit, PermRule, Push, Workflow};
use super::settings::{AgentPolicy, May};
use super::view::RunState;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Why trix stopped before something of its own.
pub enum Stop {
    /// Parked: it says why, and resumes when you say.
    Park(String),
    /// A floor: for you to let through, for this run only. The floor, then why.
    You(String, String),
    /// The kill time came: the run is aborted.
    Kill(String),
}

pub fn op_name(op: &Op) -> &'static str {
    match op {
        Op::Abort => "aborting",
        Op::Pause => "pausing",
        Op::Resume => "resuming",
        Op::Revoke => "approvals revoked: every one is back to ask",
        Op::Approve => "approved",
        Op::Answer { .. } => "answered",
        Op::Submit { .. } => "report taken",
        Op::Ask { .. } => "asked",
        Op::Decide { .. } => "decided",
        Op::Start => "started",
        Op::Trust { .. } => "trusted",
    }
}

fn control_file(root: &Path, id: &str) -> PathBuf {
    engine::dir_of(root, id).join("control")
}

fn append(path: &Path, line: &str) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Leave a word for the run's pane: pause, resume, abort, revoke. The
/// runner reads it at its next look.
pub fn write_control(root: &Path, id: &str, op: &Op) -> Result<(), String> {
    let word = match op {
        Op::Abort => "abort",
        Op::Pause => "pause",
        Op::Resume => "resume",
        Op::Revoke => "revoke",
        _ => return Err("not something a run is told this way".into()),
    };
    append(&control_file(root, id), word)
}

/// What was left for the run since its last look.
pub fn control(run: &mut WfRun, root: &Path, env: &mut dyn Env) {
    let path = control_file(root, &run.id);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    for word in text.lines() {
        match word.trim() {
            "pause" if run.state == RunState::Running || run.state == RunState::NeedsYou => {
                run.state = RunState::Paused;
                run.log.push((env.now(), "paused".into()));
                env.print("paused: trix workflow resume goes on");
                env.say("Question", &format!("{} · paused", run.id));
            }
            "resume" if matches!(run.state, RunState::Paused | RunState::Parked) => {
                run.state = if run.ask.is_some() {
                    RunState::NeedsYou
                } else {
                    RunState::Running
                };
                run.why.clear();
                run.looked = env.now();
                run.log.push((env.now(), "resumed".into()));
                env.print("resumed");
            }
            "revoke" => {
                run.revoked = true;
                run.gate.approved = None;
                run.log.push((
                    env.now(),
                    "approvals revoked: every one is back to ask".into(),
                ));
                env.print("approvals revoked: every one is back to ask");
            }
            "abort" => super::runner::Run::abort(run, env, &[]),
            // An agent started it: whose it is and how deep, and, when
            // `[workflows.agents] run = "ask"`, your say first.
            w if w.starts_with("ask-launch:") || w.starts_with("started-by:") => {
                let mut parts = w.splitn(3, ':');
                let (kind, depth, who) = (
                    parts.next(),
                    parts.next(),
                    parts.next().unwrap_or_default().to_string(),
                );
                run.depth = depth.and_then(|d| d.parse().ok()).unwrap_or(1);
                run.started_by = Some(who.clone());
                if kind == Some("ask-launch") {
                    let name = run.workflow.name.clone();
                    run.ask(
                        env,
                        engine::Ask::Launch,
                        None,
                        format!("{who} wants to start this run of {name}"),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Whether an agent pane may pause, resume or abort this run, as
/// `[workflows.agents]` says. Approving a gate is never an agent's.
pub fn agent_may(
    policy: &AgentPolicy,
    run: &WfRun,
    asker: Option<&crate::control::Asker>,
    op: &Op,
) -> Result<(), String> {
    let Some(who) = asker else { return Ok(()) };
    let (may, what) = match op {
        Op::Resume => (policy.resume, "resume"),
        _ => (policy.stop, "stop"),
    };
    if policy.own_only && run.started_by.as_deref() != Some(who.name()) {
        return Err(format!(
            "{} can only {what} a run it started ([workflows.agents] scope = \"own\"), and {} isn't one",
            who.said(),
            run.id
        ));
    }
    match may {
        May::Allow => Ok(()),
        May::Deny => Err(format!(
            "{} may not {what} a run: [workflows.agents] {what} = \"deny\"",
            who.said()
        )),
        May::Ask => Err(format!(
            "{} may not {what} a run without the user: [workflows.agents] {what} = \"ask\". The user does it, with trix workflow {} {}",
            who.said(),
            if what == "resume" {
                "resume"
            } else {
                "pause|abort"
            },
            run.id
        )),
    }
}

/// An agent pane starts a run: allowed, refused, or yours to confirm in
/// the run's pane, as `[workflows.agents] run` says. A run started by a
/// run counts its depth.
pub fn agent_start(
    policy: &AgentPolicy,
    root: &Path,
    run: &WfRun,
    asker: Option<&crate::control::Asker>,
) -> Result<(), String> {
    let Some(who) = asker else { return Ok(()) };
    // A run an agent starts is one level deep; one a node's agent starts
    // is one deeper than its own run.
    let depth = engine::run_of(root, who.name()).map_or(1, |(parent, _)| parent.depth + 1);
    if depth > policy.max_depth {
        return Err(format!(
            "{} would start a run {depth} deep, and [workflows.agents] max_depth is {}",
            who.said(),
            policy.max_depth
        ));
    }
    match policy.run {
        May::Deny => Err(format!(
            "{} may not start a run: [workflows.agents] run = \"deny\"",
            who.said()
        )),
        May::Allow => append(
            &control_file(root, &run.id),
            &format!("started-by:{depth}:{}", who.name()),
        ),
        May::Ask => append(
            &control_file(root, &run.id),
            &format!("ask-launch:{depth}:{}", who.name()),
        ),
    }
}

// ───────────────────────────── floors ─────────────────────────────

/// A fingerprint of what could make git run a command behind trix's back:
/// the repository's hooks, and the settings that name a program
/// (`core.hooksPath`, `core.fsmonitor`, `core.sshCommand`,
/// `credential.helper`). Taken when a run begins and held against before
/// trix's own push, land and merge.
pub fn fingerprint(env: &mut dyn Env, repo: &Path) -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    for key in [
        "core.hooksPath",
        "core.fsmonitor",
        "core.sshCommand",
        "credential.helper",
    ] {
        f.insert(
            key.to_string(),
            env.git(repo, &["config", "--get-all", key])
                .unwrap_or_default()
                .trim()
                .to_string(),
        );
    }
    let hooks = env
        .git(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
        )
        .map(|p| PathBuf::from(p.trim()))
        .unwrap_or_else(|_| repo.join(".git/hooks"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(&hooks)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    for file in files {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // The samples git ships do nothing.
        if name.ends_with(".sample") {
            continue;
        }
        // SHA-256: a hook an agent rewrote can't be made to look the same.
        let bytes = std::fs::read(&file).unwrap_or_default();
        f.insert(format!("hook {name}"), super::runlog::sha256(&bytes));
    }
    f
}

/// What changed in the hooks or the settings since the run began, if
/// anything did.
pub fn fingerprint_changed(run: &WfRun, env: &mut dyn Env) -> Option<String> {
    let before = run.fingerprint.as_ref()?;
    let now = fingerprint(env, &run.repo);
    let mut changed: Vec<String> = now
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .collect();
    changed.extend(before.keys().filter(|k| !now.contains_key(*k)).cloned());
    (!changed.is_empty()).then(|| {
        format!(
            "git hooks or settings that run commands changed since the run began ({}): trix won't push, land or merge past that. Look at them, put them back, then trix workflow resume {}",
            changed.join(", "),
            run.id
        )
    })
}

/// Before trix's own push. Under `push: guarded` and `gate_only`: the
/// hooks fingerprint, then the CI-config floor: a branch trix pushes runs
/// its own CI files on GitHub with the repository's secrets, so a change
/// to them is yours to let through, for that run only. No approver node
/// and no rule can lift it.
pub fn before_push(run: &mut WfRun, env: &mut dyn Env, sha: &str) -> Option<Stop> {
    if run.workflow.policy.push == Push::Allow {
        return None;
    }
    if let Some(why) = fingerprint_changed(run, env) {
        return Some(Stop::Park(why));
    }
    let floor = "push_ci_config_without_you";
    if run.opts.floors.iter().any(|f| f == floor)
        && !run.gate.let_through.iter().any(|f| f == floor)
    {
        let base = format!("refs/remotes/origin/{}", run.base);
        let names = env
            .git(
                &run.repo,
                &["diff", "--name-only", &format!("{base}...{sha}")],
            )
            .unwrap_or_default();
        let ci = ci_globs(&run.opts.ci_paths);
        let touched: Vec<&str> = names
            .lines()
            .map(str::trim)
            .filter(|f| !f.is_empty() && ci.iter().any(|g| overlap(g, f)))
            .collect();
        if !touched.is_empty() {
            return Some(Stop::You(
                floor.into(),
                format!(
                    "{} changes CI configuration ({}), which would run on GitHub with the repository's secrets once pushed: trix won't push it without you",
                    short(sha),
                    touched.join(", ")
                ),
            ));
        }
    }
    let floor = "push_without_you";
    if run.opts.floors.iter().any(|f| f == floor)
        && !run.gate.let_through.iter().any(|f| f == floor)
    {
        return Some(Stop::You(
            floor.into(),
            format!(
                "your floors say every push is yours: {} is ready to push",
                short(sha)
            ),
        ));
    }
    None
}

/// Before trix's own merge: the fingerprint again, and no merge into a
/// protected branch without green CI on the checked commit, whoever
/// approved.
pub fn before_merge(run: &mut WfRun, env: &mut dyn Env, sha: &str) -> Option<Stop> {
    if run.workflow.policy.push != Push::Allow
        && let Some(why) = fingerprint_changed(run, env)
    {
        return Some(Stop::Park(why));
    }
    let floor = "merge_without_you";
    if run.opts.floors.iter().any(|f| f == floor) && run.gate.approved.as_deref() != Some("you") {
        run.gate.approved = None;
        return Some(Stop::Park(format!(
            "your floors say every merge is yours: trix workflow approve {} once the gate asks",
            run.id
        )));
    }
    let protected = env
        .gh(
            &run.repo,
            &[
                "api",
                &format!("repos/{{owner}}/{{repo}}/branches/{}", run.base),
            ],
        )
        .ok()
        .and_then(|o| serde_json::from_str::<serde_json::Value>(&o).ok())
        // What can't be asked counts as protected.
        .is_none_or(|v| v["protected"].as_bool().unwrap_or(true));
    if protected {
        let runs =
            crate::case::check_runs(&run.repo, sha, &mut |r, a| env.gh(r, a)).unwrap_or_default();
        let green = !runs.is_empty()
            && runs.iter().all(|v| {
                v["status"] == "completed"
                    && matches!(
                        v["conclusion"].as_str(),
                        Some("success" | "neutral" | "skipped")
                    )
            });
        if !green {
            return Some(Stop::Park(format!(
                "{} is protected, and CI isn't green on {}: trix never merges that, whoever approved",
                run.base,
                short(sha)
            )));
        }
    }
    None
}

/// A deadline or a kill time that has come.
pub fn time_of_day(run: &mut WfRun, env: &mut dyn Env) -> Option<Stop> {
    super::sched::time_of_day(run, env)
}

// ───────────────────────────── permission rules ─────────────────────────────

/// Whether a rule's tool pattern covers a request: `Bash(npm install:*)`
/// covers a Bash command that starts with `npm install`; a bare tool name
/// covers that tool.
fn tool_matches(pattern: &str, tool: &str, command: &str) -> bool {
    match pattern.split_once('(') {
        None => pattern == tool,
        Some((name, rest)) => {
            let inner = rest.trim_end_matches(')');
            let prefix = inner.trim_end_matches(":*").trim_end_matches('*');
            name == tool
                && (command == prefix
                    || command.starts_with(&format!("{prefix} "))
                    || (inner.ends_with('*')
                        && !inner.ends_with(":*")
                        && command.starts_with(prefix)))
        }
    }
}

/// What a routed permission request may never be answered with allow,
/// whatever the rules say: a push (trix pushes, in the gate), a merge, or
/// a forced anything.
fn routed_floor(tool: &str, command: &str) -> Option<&'static str> {
    if tool != "Bash" {
        return None;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    let has = |a: &str, b: &str| words.windows(2).any(|w| w[0] == a && w[1] == b);
    if has("git", "push") {
        return Some("a push is the gate's, never a rule's");
    }
    if has("pr", "merge") || has("gh", "api") {
        return Some("a merge or a GitHub API call is never a rule's");
    }
    None
}

/// The decision for one permission request from a node's agent: allow only
/// when approvals are on for this run and not revoked, the backend's hook
/// decision is verified for the installed version, no floor is in the way,
/// and a rule of this workflow covers exactly this tool, command and
/// place. Anything else is `Err(why)`: the prompt goes to you.
pub fn decide(
    run: &WfRun,
    node: &str,
    tool: &str,
    command: &str,
    cwd: &str,
) -> Result<usize, String> {
    if !run.opts.approvals {
        return Err("approvals are off for this workflow".into());
    }
    if run.revoked {
        return Err("approvals were revoked for this run".into());
    }
    let kind = run.nodes[node]
        .backend
        .clone()
        .or(run.workflow.nodes[node].agent.clone())
        .unwrap_or_default();
    if !run.opts.decision.contains(&kind) {
        return Err(format!(
            "{kind}'s hook decision isn't verified for the installed version: the rule is suspended"
        ));
    }
    if let Some(why) = routed_floor(tool, command) {
        return Err(why.into());
    }
    let tree = run.nodes[node].cwd.clone().unwrap_or_else(|| run.tree());
    for (i, r) in run.workflow.approvals.permissions.iter().enumerate() {
        if r.node != "*" && r.node != node {
            continue;
        }
        if !r.tools.is_empty() && !r.tools.iter().any(|t| tool_matches(t, tool, command)) {
            continue;
        }
        if !r.paths.is_empty() {
            let inside = r.paths.iter().any(|p| {
                let p = p.replace("{{worktree}}", &tree.display().to_string());
                overlap(&p, cwd) || Path::new(cwd).starts_with(p.trim_end_matches("/**"))
            });
            if !inside {
                continue;
            }
        }
        // The first rule that covers it decides.
        return match (r.mode, r.approver.as_deref()) {
            (Allow::Allow, None | Some("rule")) => Ok(i),
            (Allow::Allow, Some(a)) => Err(format!(
                "'{a}' can't approve a permission prompt: agents approve merges only"
            )),
            (Allow::Ask, _) => Err(format!("permission rule {} says ask", i + 1)),
        };
    }
    Err("no rule covers it".into())
}

/// One line of `approvals.jsonl`.
pub fn audit(root: &Path, run: &WfRun, mut entry: serde_json::Value, now: u64) {
    entry["at"] = now.into();
    entry["run"] = run.id.clone().into();
    entry["branch"] = run.branch.clone().into();
    let _ = append(
        &engine::dir_of(root, &run.id).join("approvals.jsonl"),
        &serde_json::to_string(&entry).unwrap_or_default(),
    );
}

/// What Claude's PermissionRequest hook prints to allow one request.
pub const ALLOW: &str = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#;

/// The request a PermissionRequest hook makes of the server: which tool,
/// which command, where. None: not a permission request, or a backend
/// whose hook can't decide.
pub fn hook_request(source: &str, v: &serde_json::Value) -> Option<Op> {
    if !matches!(source, "claude" | "codex") || v["hook_event_name"] != "PermissionRequest" {
        return None;
    }
    let input = &v["tool_input"];
    Some(Op::Decide {
        tool: v["tool_name"].as_str().unwrap_or_default().to_string(),
        command: input["command"]
            .as_str()
            .or(input["file_path"].as_str())
            .unwrap_or_default()
            .to_string(),
        cwd: v["cwd"].as_str().unwrap_or_default().to_string(),
    })
}

/// What the hook prints for the server's reply: allow, for that request,
/// or nothing. No reply in time, a refusal, no server: nothing, and the
/// prompt shows as it always did. A timeout falls back to ask.
pub fn hook_answer(reply: Result<String, String>) -> Option<&'static str> {
    match reply {
        Ok(out) if out.trim() == "allow" => Some(ALLOW),
        _ => None,
    }
}

/// The hook's part of a rule approval. A PermissionRequest from an agent
/// in a trix pane is asked of the server, which answers for that one
/// request, inside the hook's timeout. Nothing is ever typed into the pane.
pub fn hook_decision(source: &str, v: &serde_json::Value) -> Option<&'static str> {
    let op = hook_request(source, v)?;
    let msg = crate::session::ClientMsg::Control {
        protocol: crate::control::CONTROL,
        from: crate::control::caller(),
        req: crate::control::Request::Wf { run: None, op },
    };
    // Well inside the 5 s (Codex: 3 s) the backend waits for its hook.
    hook_answer(match crate::session::control(&msg, Some(2)) {
        Ok(r) if r.code == 0 => Ok(r.out),
        Ok(r) => Err(r.err),
        Err(e) => Err(e),
    })
}

// ───────────────────────────── trust ─────────────────────────────

/// Where trust is kept: a workflow's name, and the hash of its content
/// when you trusted it.
pub fn trust_file() -> PathBuf {
    crate::worktree::state_dir().join("trusted-workflows.json")
}

/// A hash of everything a run of `w` would act on: the workflow, the
/// workflows its workflow nodes name, and the arguments each node's
/// profile resolves to. A change to any of them is a change of trust.
pub fn content_hash(w: &Workflow, subs: &[Workflow]) -> String {
    let profiles: Vec<_> = w
        .nodes
        .iter()
        .map(|(id, n)| (id, check::profile_of(w, n).map(|p| p.args)))
        .collect();
    // SHA-256: what you trusted can't be swapped for a file made to hash the same.
    let text = serde_json::to_string(&(w, subs, profiles)).unwrap_or_default();
    super::runlog::sha256(text.as_bytes())
}

/// Whether the workflow is trusted as it is now. One from the binary is
/// (`builtin`: where it was found, never what the file says of itself). A
/// file is when its content is what you trusted.
pub fn is_trusted(file: &Path, w: &Workflow, hash: &str, builtin: bool) -> bool {
    if builtin {
        return true;
    }
    let trusted: BTreeMap<String, String> = std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    trusted.get(&w.name).map(String::as_str) == Some(hash)
}

/// Trust a workflow as it is now. Yours only: the server refuses it from
/// every agent pane and check.
pub fn trust(file: &Path, name: &str, hash: &str) -> Result<(), String> {
    let mut trusted: BTreeMap<String, String> = std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    trusted.insert(name.to_string(), hash.to_string());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    super::store::write_atomic(
        file,
        &serde_json::to_vec_pretty(&trusted).map_err(|e| e.to_string())?,
    )
}

// ───────────────────────────── autopilot ─────────────────────────────

/// What a node will do unattended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unattended {
    /// Its profile never prompts: what isn't allowed is refused.
    Profile,
    /// Its prompts are answered by rules, through a verified hook.
    Rules,
    /// A prompt parks the run and tells you.
    Parks,
    /// It isn't an agent: nothing prompts.
    Quiet,
}

impl Unattended {
    pub fn label(self) -> &'static str {
        match self {
            Unattended::Profile => "covered by its profile",
            Unattended::Rules => "covered by rules",
            Unattended::Parks => "will park on prompts",
            Unattended::Quiet => "no prompts",
        }
    }
}

/// What each node will do unattended, as the workflow stands.
pub fn preview(w: &Workflow, m: &Matrix, ctx: &Ctx) -> Vec<(String, Unattended)> {
    w.nodes
        .iter()
        .map(|(id, n)| {
            let Some(profile) = check::profile_of(w, n).filter(|_| n.is_agent()) else {
                return (id.clone(), Unattended::Quiet);
            };
            let ruled = w
                .approvals
                .permissions
                .iter()
                .any(|r| r.mode == Allow::Allow && (r.node == "*" || r.node == *id));
            let how = if profile.never_prompts() {
                Unattended::Profile
            } else if ruled && m.decision(&profile.kind, &ctx.installed) == Level::Verified {
                Unattended::Rules
            } else {
                Unattended::Parks
            };
            (id.clone(), how)
        })
        .collect()
}

/// The autopilot preset: what the toggle writes. Limits that park instead
/// of asking, a merge approver that didn't write the change (a judge, else
/// a verifier, else a gatekeeper) when there's one, and a catch-all rule
/// that sends any prompt to you. It writes no rule that allows a tool:
/// profiles stay the main way, since a prompt that never appears needs no
/// approval. `repo` and `branch`: where the approval holds.
pub fn autopilot(
    w: &mut Workflow,
    m: &Matrix,
    ctx: &Ctx,
    repo: &str,
    branch: &str,
) -> Vec<(String, Unattended)> {
    w.mode = Mode::Autopilot;
    w.limits.on_limit = Some(OnLimit::Park);
    w.limits.retries = Some(2);
    w.limits.flaky_check_reruns = Some(1);
    // The approver the workflow names, if it's a valid one; else the one
    // the checker would suggest: a judge, else a verifier, else a gatekeeper.
    let named = w.approvals.merge.as_ref().and_then(|a| a.approver.clone());
    let valid = named.is_some() && !check::check(w, m, ctx).has("merge-approver");
    let approver = if valid {
        named
    } else {
        let mut probe = w.clone();
        probe.policy.merge = super::model::Merge::Approver;
        probe.approvals.merge = Some(MergeApproval {
            mode: Allow::Allow,
            ..MergeApproval::default()
        });
        check::check(&probe, m, ctx)
            .find("merge-approver")
            .and_then(|p| match &p.fixes.first()?.op {
                check::FixOp::SetApprover(a) => a.clone(),
                _ => None,
            })
    };
    if w.nodes.values().any(|n| n.kind == "gh.gate") {
        match approver {
            Some(a) => {
                w.policy.merge = super::model::Merge::Approver;
                w.approvals.merge = Some(MergeApproval {
                    mode: Allow::Allow,
                    approver: Some(a),
                    repos: if repo.is_empty() {
                        Vec::new()
                    } else {
                        vec![repo.to_string()]
                    },
                    branches: vec![branch.to_string()],
                    require: vec!["done_when".into(), "ci_green_on_checked_commit".into()],
                });
            }
            // Nobody but an author could approve: the merge stays yours.
            None => {
                w.policy.merge = super::model::Merge::You;
                w.approvals.merge = None;
            }
        }
    }
    // Anything a profile doesn't settle parks the run and tells you.
    w.approvals.permissions.retain(|r| r.mode == Allow::Allow);
    w.approvals.permissions.push(PermRule {
        node: "*".into(),
        mode: Allow::Ask,
        ..PermRule::default()
    });
    w.notify = Notify {
        on: vec!["succeeded".into(), "failed".into(), "stuck".into()],
        summary: true,
    };
    preview(w, m, ctx)
}

/// The preview as text, for the confirm.
pub fn preview_text(w: &Workflow, nodes: &[(String, Unattended)]) -> String {
    let mut s = format!("{} on autopilot:\n", w.name);
    for (id, how) in nodes.iter().filter(|(_, h)| *h != Unattended::Quiet) {
        s.push_str(&format!("  {id:<14} {}\n", how.label()));
    }
    match w.approvals.merge.as_ref().and_then(|a| a.approver.as_ref()) {
        Some(a) => s.push_str(&format!("  ⚡ merge: approved by {a}, once done_when holds and CI is green on the checked commit\n")),
        None if w.nodes.values().any(|n| n.kind == "gh.gate") => s.push_str("  merge: yours (no node could approve without having written the change)\n"),
        None => {}
    }
    s.push_str(
        "  a limit, a stall, a floor or a prompt outside the rules parks the run and tells you\n",
    );
    s
}
