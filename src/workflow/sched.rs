//! When runs start and stop on their own: cron starts, a deadline (stop
//! new work and park) and a kill time.

use super::engine::WfRun;
use super::env::Env;
use super::policy::Stop;

/// A deadline or a kill time that has come.
pub fn time_of_day(_run: &mut WfRun, _env: &mut dyn Env) -> Option<Stop> {
    None
}
