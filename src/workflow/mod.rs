//! Trix Workflows: a workflow prompts the agents, re-prompts them and
//! checks their work, so you don't. Triage (`case.rs`) is the first one.

pub mod cli;
pub mod env;
pub mod model;
pub mod runner;
pub mod triage;
pub mod view;

#[cfg(test)]
pub mod replay;
