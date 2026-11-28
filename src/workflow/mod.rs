//! Trix Workflows: a workflow prompts the agents, re-prompts them and
//! checks their work, so you don't. Triage (`case.rs`) is the first one.

pub mod env;
pub mod runner;

#[cfg(test)]
mod replay;
