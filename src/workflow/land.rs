//! Landing: the built-in step that brings the commits of writers who
//! worked in worktrees of their own onto the run's branch.

use super::engine::WfRun;
use super::env::Env;

/// The land step, at this look.
pub fn land_node(run: &mut WfRun, env: &mut dyn Env, id: &str) {
    run.land_unbuilt(env, id);
}
