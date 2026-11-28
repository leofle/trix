//! Trix Workflows: a workflow prompts the agents, re-prompts them and
//! checks their work, so you don't. Triage (`case.rs`) is the first one.

// Parts are built ahead of the steps that use them: this comes off in the
// docs step, once every step is in.
#![allow(dead_code)]

pub mod caps;
pub mod check;
pub mod cli;
pub mod compose;
pub mod engine;
pub mod env;
pub mod guard;
pub mod land;
pub mod launch;
pub mod model;
pub mod policy;
pub mod probe;
pub mod prompt;
pub mod runner;
pub mod settings;
pub mod store;
pub mod summary;
pub mod triage;
pub mod trigger;
pub mod view;

#[cfg(test)]
mod check_tests;
#[cfg(test)]
pub mod engine_tests;
#[cfg(test)]
pub mod replay;

/// How many ways a workflow gets checked, each through the one checker.
#[cfg(test)]
pub const SURFACES: usize = 2;

/// What each of those ways says of a workflow: its name, and the text.
#[cfg(test)]
pub fn surfaces(
    w: &model::Workflow,
    m: &caps::Matrix,
    ctx: &check::Ctx,
) -> Vec<(&'static str, String)> {
    let start = match launch::preflight(w, m, ctx) {
        Ok(report) => report.render(),
        Err(refused) => refused,
    };
    vec![
        ("trix workflow check", cli::check_text(w, m, ctx)),
        ("run start", start),
    ]
}
