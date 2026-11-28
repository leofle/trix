//! Trix Workflows: a workflow prompts the agents, re-prompts them and
//! checks their work, so you don't. Triage (`case.rs`) is the first one.

pub mod builder;
pub mod canvas;
pub mod caps;
pub mod check;
pub mod cli;
pub mod compose;
pub mod dryrun;
pub mod engine;
pub mod env;
pub mod guard;
pub mod land;
pub mod launch;
pub mod model;
pub mod overview;
pub mod policy;
pub mod probe;
pub mod prompt;
pub mod runlog;
pub mod runner;
pub mod sched;
pub mod settings;
pub mod store;
pub mod summary;
pub mod triage;
pub mod trigger;
pub mod view;
pub mod yaml;

#[cfg(test)]
mod builder_tests;
#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod dryrun_tests;
#[cfg(test)]
pub mod engine_tests;
#[cfg(test)]
mod ops_tests;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
pub mod replay;

/// How many ways a workflow gets checked, each through the one checker.
#[cfg(test)]
pub const SURFACES: usize = 5;

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
    let dry = dryrun::dry_run(
        w,
        m,
        ctx,
        &Default::default(),
        dryrun::Scenario::Happy,
        Default::default(),
    );
    vec![
        ("trix workflow check", cli::check_text(w, m, ctx).0),
        ("run start", start),
        ("dry run and rehearsal", dry.check),
        (
            "the builder",
            builder::Builder::new(w.clone(), m.clone(), ctx.clone())
                .problems()
                .render(),
        ),
        // Import adds one line of its own: imported, it isn't trusted yet.
        ("import", {
            let yaml = yaml::export(w, m, ctx).unwrap_or_default();
            let said = yaml::import(&yaml, m, ctx)
                .map(|(_, r)| r.render())
                .unwrap_or_default();
            let (first, rest) = said.split_once('\n').unwrap_or_default();
            let rest: Vec<&str> = rest
                .lines()
                .filter(|l| !l.contains("untrusted, or changed since trust was given"))
                .collect();
            format!("{first}\n{}\n", rest.join("\n"))
        }),
    ]
}
