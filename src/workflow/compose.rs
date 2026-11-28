//! Composition: a workflow as a node of another, a fan-out over a list,
//! and the join that collects it.

use super::engine::WfRun;
use super::env::{Env, Seen};
use std::path::Path;

pub fn for_each_node(run: &mut WfRun, _root: &Path, env: &mut dyn Env, _seen: &[Seen], id: &str) {
    run.land_unbuilt(env, id);
}

pub fn join_node(run: &mut WfRun, env: &mut dyn Env, id: &str) {
    run.land_unbuilt(env, id);
}

pub fn workflow_node(run: &mut WfRun, _root: &Path, env: &mut dyn Env, id: &str) {
    run.land_unbuilt(env, id);
}
