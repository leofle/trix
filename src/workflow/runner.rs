//! The generic runner: one deterministic loop that drives any run through
//! `Env`. It isn't an agent and never asks one what to do next.
//!
//! A run is its state, kept on disk, and what its nodes do when they're
//! looked at. The runner owns what every run shares: looking at the agents,
//! closing the panes whose node handed in, taking your answers, the limits,
//! and saying what the pane is doing. What a node does is the run's own
//! (`visit`): triage's built-in kinds, or a workflow's generic nodes.

use super::env::{Env, Live, Seen};
use std::path::Path;

/// A run, as the runner drives it.
pub trait Run: Clone + PartialEq + Sized {
    /// Who its pane says it is to the server: `triage`, `workflow`.
    const SOURCE: &'static str;

    fn load(root: &Path, id: &str) -> Result<Self, String>;
    /// Store the run, and write what it did since to its log.
    fn store(&mut self, root: &Path) -> Result<(), String>;

    /// Finished, failed or aborted: nothing more is started.
    fn over(&self) -> bool;
    /// Panes whose node is done with them: closed once they stop working.
    fn closing(&mut self) -> &mut Vec<String>;
    /// A pane was closed: what it had spent.
    fn spent(&mut self, cost: f64);

    /// What came in while the run asks you something. True: the run
    /// finished here, and the look ends.
    fn meanwhile(&mut self, root: &Path, env: &mut dyn Env, seen: &[Seen]) -> bool;
    /// The keys that answer what the run asks you now, `a` (abort) aside.
    fn asks(&self) -> Option<Vec<char>>;
    /// Your answer to it.
    fn answered(&mut self, root: &Path, env: &mut dyn Env, seen: &[Seen], key: char);
    fn abort(&mut self, env: &mut dyn Env, seen: &[Seen]);
    /// A limit was hit, and the run asked, parked or failed: the look ends.
    fn limited(&mut self, env: &mut dyn Env, seen: &[Seen]) -> bool;
    /// The nodes at work: started, their reports taken, the next ones
    /// chosen. True: the look ends here.
    fn visit(&mut self, root: &Path, env: &mut dyn Env, seen: &[Seen]) -> bool;
    /// What the pane says it's working on, when the run doesn't say itself.
    fn working(&self) -> Option<String>;

    /// What the pane shows as it starts: the run, and its log so far.
    fn banner(&self) -> String;
    /// What it asks you, for the pane's state.
    fn question(&self) -> Option<String>;
    /// It ran before (the workspace was restored): its agents come back first.
    fn resumed(&self) -> bool;
    /// Once it's over: the line the pane ends on, and what it says it is.
    fn ended(&self) -> (String, String);
}

/// How often the pane looks at its run.
const LOOK: std::time::Duration = std::time::Duration::from_secs(2);

/// One look at a run: take what came in, start what's next, ask you what
/// only you decide. Returns whether the run changed.
pub fn step<R: Run>(run: &mut R, root: &Path, env: &mut dyn Env) -> bool {
    let before = run.clone();
    let mut seen = env.agents().unwrap_or_default();
    reap(run, env, &mut seen);
    if run.over() {
        return *run != before;
    }
    if run.meanwhile(root, env, &seen) {
        return true;
    }
    if let Some(keys) = run.asks() {
        let Some(key) = env.answer() else {
            return *run != before;
        };
        if key == 'a' {
            run.abort(env, &seen);
            return true;
        }
        if !keys.contains(&key) {
            return *run != before;
        }
        run.answered(root, env, &seen, key);
        return true;
    }
    if run.limited(env, &seen) || run.visit(root, env, &seen) {
        return true;
    }
    if let Some(what) = run.working() {
        env.say("Working", &what);
    }
    *run != before
}

/// Close the panes whose node is done with them, once they've stopped
/// working. What they spent stays counted; they're no longer seen.
fn reap<R: Run>(run: &mut R, env: &mut dyn Env, seen: &mut Vec<Seen>) {
    let mut closed = Vec::new();
    for name in run.closing().clone() {
        match seen.iter().find(|a| a.name == name) {
            None => closed.push(name),
            Some(a) if a.state != "working" && a.state != "starting" => {
                if env.close(&name, false).is_ok() {
                    run.spent(a.cost);
                    closed.push(name);
                }
            }
            Some(_) => {}
        }
    }
    run.closing().retain(|n| !closed.contains(n));
    seen.retain(|a| !closed.contains(&a.name));
}

/// The run's pane: runs until the run is over, then stays, so its log is
/// there to read.
pub fn run_pane<R: Run>(root: &Path, id: &str) -> i32 {
    let mut run = match R::load(root, id) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("trix: {e}");
            return 1;
        }
    };
    let mut env = Live::in_pane(R::SOURCE);
    drive(&mut run, root, &mut env, || {})
}

/// Drive a run in its pane, through `env`, for good. `over`: called once,
/// when the run is over and its panes are closed.
pub fn drive<R: Run>(run: &mut R, root: &Path, env: &mut dyn Env, mut over: impl FnMut()) -> ! {
    print!("{}", run.banner());
    if let Some(q) = run.question() {
        env.say("Question", &q);
    }
    // Restored with the workspace: let restore bring the run's agents back
    // before looking for them, or they'd be started twice.
    if run.resumed() {
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    let mut said_over = false;
    loop {
        if step(run, root, env) {
            if let Err(e) = run.store(root) {
                eprintln!("trix: {e}");
            }
            if run.asks().is_some() {
                println!("(type the key, then enter)");
            }
        }
        if run.over() && run.closing().is_empty() && !said_over {
            said_over = true;
            let (line, what) = run.ended();
            println!("{line}");
            env.say("Finished", &what);
            over();
        }
        std::thread::sleep(LOOK);
    }
}
