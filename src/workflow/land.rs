//! Workspaces and landing. Writers that edges put in order work one after
//! another in the run's worktree. Writers that can run at the same time
//! each get a worktree of their own, branched from the run's branch with
//! its uncommitted changes copied in, so no two agents ever share an index.
//! The land step rebases each one's commits onto the run's branch, in the
//! order they finished, and nothing is removed before it has landed.

use super::check;
use super::engine::{WfRun, first_line, plural};
use super::env::Env;
use super::view::NodeState;
use std::path::{Path, PathBuf};

/// The branch a writer with its own worktree works on.
pub fn branch_of(run: &WfRun, node: &str) -> String {
    format!("{}-{node}", run.branch)
}

/// A writer's own worktree, made if it isn't there: from the run's branch,
/// with what's uncommitted in the run's worktree copied in. Staged files
/// stay staged, and untracked files come too.
pub fn own_tree(run: &mut WfRun, env: &mut dyn Env, node: &str) -> Result<PathBuf, String> {
    let path = run
        .repo
        .join(".trix/worktrees")
        .join(format!("{}-{node}", run.id));
    if path.is_dir() {
        return Ok(path);
    }
    let shared = run.ensure_tree(env)?;
    let p = path.display().to_string();
    env.git(
        &run.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &branch_of(run, node),
            &p,
            &run.branch,
        ],
    )?;
    // A commit of what's staged and changed there, made without touching
    // that worktree or anyone's stash.
    let dirty = env.git(&shared, &["stash", "create"])?.trim().to_string();
    if !dirty.is_empty() {
        env.git(&path, &["stash", "apply", "--quiet", "--index", &dirty])?;
    }
    let untracked = env.git(&shared, &["ls-files", "--others", "--exclude-standard"])?;
    for file in untracked.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (from, to) = (shared.join(file), path.join(file));
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::copy(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
    }
    Ok(path)
}

/// Take trix's own lock off a worktree, so git will remove it. Someone
/// else's lock stays, and so does the worktree.
pub fn unlock_ours(run: &WfRun, env: &mut dyn Env, tree: &Path) {
    let listing = env
        .git(
            &run.repo,
            &[
                "-c",
                "core.quotePath=false",
                "worktree",
                "list",
                "--porcelain",
            ],
        )
        .unwrap_or_default();
    if crate::worktree::lock_in(&listing, tree) == crate::worktree::Lock::Trix {
        let _ = env.git(
            &run.repo,
            &["worktree", "unlock", &tree.display().to_string()],
        );
    }
}

/// Remove a writer's own worktree and its branch: only once its commits
/// are on the run's branch and nothing in it is uncommitted. The same care
/// `trix worktrees rm` takes: never forced.
pub fn remove_tree(run: &mut WfRun, env: &mut dyn Env, node: &str) -> Result<(), String> {
    let Some(tree) = run.nodes[node]
        .cwd
        .clone()
        .filter(|t| *t != run.tree() && t.is_dir())
    else {
        return Ok(());
    };
    let branch = branch_of(run, node);
    let ahead = env.git(
        &run.repo,
        &["rev-list", "--count", &format!("{}..{branch}", run.branch)],
    )?;
    let ahead: usize = ahead.trim().parse().unwrap_or(1);
    if ahead > 0 {
        return Err(format!(
            "{node}'s worktree isn't removed: {} on {branch} {} landed on {}",
            plural(ahead, "commit"),
            if ahead == 1 { "hasn't" } else { "haven't" },
            run.branch
        ));
    }
    if !env
        .git(&tree, &["status", "--porcelain"])?
        .trim()
        .is_empty()
    {
        return Err(format!(
            "{node}'s worktree isn't removed: it has uncommitted changes"
        ));
    }
    unlock_ours(run, env, &tree);
    env.git(
        &run.repo,
        &["worktree", "remove", &tree.display().to_string()],
    )?;
    crate::worktree::forget(&tree);
    // Every commit on it is on the run's branch: only the name goes.
    let _ = env.git(&run.repo, &["branch", "--quiet", "-D", &branch]);
    Ok(())
}

/// One writer's commits onto the run's branch: rebased there, then the
/// run's branch fast-forwards. `Err`: the files that conflict.
fn land_one(run: &mut WfRun, env: &mut dyn Env, node: &str) -> Result<usize, Vec<String>> {
    let tree = run.nodes[node].cwd.clone().unwrap_or_default();
    let branch = branch_of(run, node);
    let count = |env: &mut dyn Env, range: String| -> usize {
        env.git(&run.repo, &["rev-list", "--count", &range])
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    let commits = count(env, format!("{}..{branch}", run.branch));
    if env.git(&tree, &["rebase", "--quiet", &run.branch]).is_err() {
        let files = env
            .git(&tree, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap_or_default();
        let _ = env.git(&tree, &["rebase", "--abort"]);
        return Err(files
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect());
    }
    let shared = run.tree();
    if env
        .git(&shared, &["merge", "--quiet", "--ff-only", &branch])
        .is_err()
    {
        return Err(Vec::new());
    }
    Ok(commits)
}

/// The land step, at this look: once every writer that works in a worktree
/// of its own has handed in, their commits land in the order they finished.
/// A conflict goes back to its author (`on_conflict: author`, a cycle of
/// cost) or ends the run (`fail`).
pub fn land_node(run: &mut WfRun, env: &mut dyn Env, id: &str) {
    let writers: Vec<String> = run
        .workflow
        .nodes
        .iter()
        .filter(|(w, n)| n.writes() && check::isolated(&run.workflow, w))
        .map(|(w, _)| w.clone())
        .collect();
    // A writer still at work, or about to start: the landing waits for it.
    if writers.iter().any(|w| run.active.contains(w)) {
        return;
    }
    // Trix's own git work: not past a hook or a setting that changed mid-run.
    if run.workflow.policy.push != super::model::Push::Allow
        && let Some(why) = super::policy::fingerprint_changed(run, env)
    {
        return run.park(env, why);
    }
    let order: Vec<String> = run
        .finished
        .iter()
        .filter(|w| !run.nodes[*w].landed)
        .cloned()
        .collect();
    for w in order {
        match land_one(run, env, &w) {
            Ok(commits) => {
                run.nodes.get_mut(&w).unwrap().landed = true;
                run.note(
                    env,
                    format!(
                        "landed {w}: {} onto {}",
                        plural(commits, "commit"),
                        run.branch
                    ),
                );
            }
            Err(files) => {
                let what = if files.is_empty() {
                    "it couldn't be rebased: commit or drop what's uncommitted in your worktree"
                        .to_string()
                } else {
                    format!("these files conflict: {}", files.join(", "))
                };
                run.note(
                    env,
                    format!(
                        "landing {w} conflicted: {}",
                        if files.is_empty() {
                            "not rebased".into()
                        } else {
                            files.join(", ")
                        }
                    ),
                );
                if run.workflow.nodes[id].on_conflict.as_deref() == Some("fail") {
                    return run.fail(
                        env,
                        format!("landing {w} onto {} conflicted: {what}", run.branch),
                    );
                }
                // Back to its author, with what conflicts: a cycle of cost.
                run.cycles += 1;
                let st = run.nodes.get_mut(&w).unwrap();
                st.inbox = vec![format!(
                    "Your commits couldn't land on the run's branch ({}): {what}. Another writer's work landed there first. \
                     Bring that branch in (git rebase {} in your worktree), resolve the conflicts keeping what both sides \
                     meant, commit, and hand in again.",
                    run.branch, run.branch
                )];
                st.state = NodeState::Waiting;
                st.sent_back = true;
                run.active.retain(|a| a != id);
                run.active.push(w.clone());
                if run.cycles > run.ceiling {
                    let ceiling = run.ceiling as usize;
                    run.limit(
                        env,
                        "cycles",
                        format!(
                            "the ceiling of {} is reached, and {w} still conflicts",
                            plural(ceiling, "cycle")
                        ),
                    );
                }
                return;
            }
        }
    }
    let head = match run.head(env) {
        Ok(h) => h,
        Err(e) => {
            return run.fail(
                env,
                format!(
                    "couldn't read the run's head after landing: {}",
                    first_line(&e)
                ),
            );
        }
    };
    let st = run.nodes.get_mut(id).unwrap();
    st.visits += 1;
    st.verdict = Some("landed".into());
    st.outputs.insert("head".into(), head.into());
    st.state = NodeState::Done;
    run.route(env, id, "landed");
}
