//! Trix Workflows: a workflow prompts the agents, re-prompts them and
//! checks their work, so you don't. Triage (`case.rs`) is the first one.

// Parts are built ahead of the steps that use them: this comes off in the
// docs step, once every step is in.
#![allow(dead_code)]

pub mod cli;
pub mod env;
pub mod model;
pub mod runner;
pub mod store;
pub mod triage;
pub mod view;

#[cfg(test)]
pub mod replay;
