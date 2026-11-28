//! Who may do what to a run, and what trix itself never does: the floors,
//! the hooks fingerprint, the control file a run's pane reads.

use super::engine::{Op, WfRun};
use super::env::Env;
use super::settings::{AgentPolicy, May};
use super::view::RunState;
use std::path::Path;

/// Why trix stopped before something of its own.
pub enum Stop {
    /// Parked: it says why, and resumes when you say.
    Park(String),
    /// A floor: for you to let through, for this run only. The floor, then why.
    You(String, String),
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
    }
}

fn control_file(root: &Path, id: &str) -> std::path::PathBuf {
    super::engine::dir_of(root, id).join("control")
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
    let path = control_file(root, id);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{word}\n").as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
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
            "abort" => {
                super::runner::Run::abort(run, env, &[]);
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

/// Before trix's own push: the floors, and the hooks fingerprint.
pub fn before_push(_run: &mut WfRun, _env: &mut dyn Env, _sha: &str) -> Option<Stop> {
    None
}

/// Before trix's own merge.
pub fn before_merge(_run: &mut WfRun, _env: &mut dyn Env, _sha: &str) -> Option<Stop> {
    None
}

/// A deadline or a kill time that has come.
pub fn time_of_day(_run: &mut WfRun, _env: &mut dyn Env) -> Option<Stop> {
    None
}
