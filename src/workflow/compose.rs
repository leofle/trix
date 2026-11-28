//! Composition: a fan-out over a list (`for_each`), the join that collects
//! it, and a workflow as a node of another.
//!
//! A fan-out is declared in the workflow and checked before the run. At
//! run time it becomes one agent visit per item, each in a worktree of its
//! own, at most `parallel` at once. An item that fails is a failed item,
//! not a failed run: the join decides what the rest is worth.

use super::engine::{self, Ask, WfRun, first_line, plural};
use super::env::{Env, Seen};
use super::model::{Node, Workspace};
use super::view::{NodeState, RunState};
use std::path::Path;

/// The instances a fan-out made, by id.
fn instances(run: &WfRun, id: &str) -> Vec<String> {
    run.nodes[id]
        .outputs
        .get("instances")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}

/// A fan-out: one visit of its agent per item of the list it's over.
pub fn for_each_node(run: &mut WfRun, _root: &Path, env: &mut dyn Env, _seen: &[Seen], id: &str) {
    let n = run.workflow.nodes[id].clone();
    if run.nodes[id].state == NodeState::Waiting {
        // The list: another node's output, as it handed it in.
        let over = n.over.clone().unwrap_or_default();
        let (from, field) = over.split_once('.').unwrap_or((over.as_str(), ""));
        let items: Vec<String> = run
            .nodes
            .get(from)
            .and_then(|st| st.outputs.get(field))
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string())
            })
            .collect();
        let mut made = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let child = format!("{id}-{}", i + 1);
            // The same agent, brief and profile; the prompt names its item.
            let prompt = n
                .prompt
                .as_deref()
                .unwrap_or("Goal: {{goal.text}}\nYour item: {{item}}");
            let node = Node {
                kind: "agent".into(),
                over: None,
                parallel: None,
                workspace: Some(Workspace::Isolated),
                prompt: Some(prompt.replace("{{item}}", item).replace("{{node}}", &child)),
                ..n.clone()
            };
            run.workflow.nodes.insert(child.clone(), node);
            let st = engine::NodeRun {
                parent: Some(id.to_string()),
                ..engine::NodeRun::default()
            };
            run.nodes.insert(child.clone(), st);
            run.nodes
                .get_mut(&child)
                .unwrap()
                .outputs
                .insert("item".into(), item.clone().into());
            made.push(child);
        }
        let st = run.nodes.get_mut(id).unwrap();
        st.visits += 1;
        st.outputs.insert("instances".into(), made.clone().into());
        st.state = NodeState::Working;
        run.note(
            env,
            format!(
                "{id} fans out over {}: {}",
                plural(items.len(), "item"),
                items.join(", ")
            ),
        );
        if made.is_empty() {
            return finish(run, env, id);
        }
    }
    // At most `parallel` at once: the next starts when one hands in.
    let all = instances(run, id);
    let at_work = all.iter().filter(|c| run.active.contains(c)).count();
    let room = (n.parallel.unwrap_or(all.len().max(1) as u32) as usize).saturating_sub(at_work);
    let next: Vec<String> = all
        .iter()
        .filter(|c| run.nodes[*c].visits == 0 && !run.active.contains(c))
        .take(room)
        .cloned()
        .collect();
    for child in next {
        run.active.push(child);
    }
    if all.iter().all(|c| {
        matches!(run.nodes[c].state, NodeState::Done | NodeState::Failed) && !run.active.contains(c)
    }) {
        finish(run, env, id);
    }
}

/// Every item is in: the fan-out is done, and what it led to goes on.
fn finish(run: &mut WfRun, env: &mut dyn Env, id: &str) {
    let all = instances(run, id);
    let results: Vec<serde_json::Value> = all
        .iter()
        .map(|c| serde_json::json!({"item": run.nodes[c].outputs.get("item"), "verdict": run.nodes[c].verdict, "node": c}))
        .collect();
    let st = run.nodes.get_mut(id).unwrap();
    st.outputs.insert("results".into(), results.into());
    st.state = NodeState::Done;
    run.active.retain(|a| a != id);
    // No item at all: its first edge still leads on, so the join can say none.
    if all.is_empty() {
        let first = run.workflow.nodes[id]
            .verdicts
            .first()
            .cloned()
            .unwrap_or_default();
        for e in run
            .workflow
            .follow_all(id, &first)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            wake(run, &e.to);
        }
        run.note(env, format!("{id} had nothing to fan out over"));
    }
}

fn wake(run: &mut WfRun, to: &str) {
    if let Some(st) = run.nodes.get_mut(to) {
        if st.state != NodeState::Working {
            st.state = NodeState::Waiting;
        }
        if !run.active.iter().any(|a| a == to) {
            run.active.push(to.to_string());
        }
    }
}

/// An instance handed in, or ended with a runtime event: it's counted,
/// and what the fan-out's edge on that verdict leads to is woken. An event
/// is a failed item, never a failed run.
pub fn child_done(run: &mut WfRun, env: &mut dyn Env, parent: &str, child: &str, trigger: &str) {
    run.active.retain(|a| a != child);
    let st = run.nodes.get_mut(child).unwrap();
    if st.verdict.is_none() {
        st.verdict = Some(trigger.to_string());
        st.state = NodeState::Failed;
    }
    run.note(env, format!("{child} ({}): {trigger}", st_item(run, child)));
    for e in run
        .workflow
        .follow_all(parent, trigger)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>()
    {
        wake(run, &e.to);
    }
}

fn st_item(run: &WfRun, child: &str) -> String {
    run.nodes[child]
        .outputs
        .get("item")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A join: it waits for what leads into it by its rule (`all`, `any`, a
/// number, or `both_or_ask`), then says whether enough of it succeeded.
pub fn join_node(run: &mut WfRun, env: &mut dyn Env, id: &str) {
    let n = run.workflow.nodes[id].clone();
    // What it collects: each node with an edge into it, a fan-out counted
    // by its instances. An item succeeded if it gave its fan-out's first verdict.
    let mut members: Vec<(String, bool, bool)> = Vec::new();
    let mut sources: Vec<String> = run
        .workflow
        .edges
        .iter()
        .filter(|e| e.to == id)
        .map(|e| e.from.clone())
        .collect();
    sources.sort();
    sources.dedup();
    let mut fanning = false;
    for src in &sources {
        let Some(node) = run.workflow.nodes.get(src) else {
            continue;
        };
        if node.kind == "for_each" {
            fanning |= run.nodes[src].state != NodeState::Done;
            let good = node.verdicts.first().cloned().unwrap_or_default();
            for c in instances(run, src) {
                let st = &run.nodes[&c];
                let over = matches!(st.state, NodeState::Done | NodeState::Failed)
                    && !run.active.contains(&c);
                members.push((
                    c,
                    over,
                    over && st.verdict.as_deref() == Some(good.as_str()),
                ));
            }
        } else {
            let st = &run.nodes[src];
            let over = matches!(st.state, NodeState::Done | NodeState::Failed);
            let good = over
                && st.verdict.as_ref().is_some_and(|v| {
                    run.workflow
                        .edges
                        .iter()
                        .any(|e| e.from == *src && e.to == id && e.on.list().contains(&v.as_str()))
                });
            members.push((src.clone(), over, good));
        }
    }
    let (total, over, good) = (
        members.len(),
        members.iter().filter(|m| m.1).count(),
        members.iter().filter(|m| m.2).count(),
    );
    let rule = n.wait.as_deref().unwrap_or("all");
    let ready = match rule {
        "any" => good >= 1 || (over == total && !fanning),
        "all" | "both_or_ask" => over == total && !fanning,
        k => k.parse::<usize>().is_ok_and(|k| good >= k) || (over == total && !fanning),
    };
    if !ready {
        return;
    }
    let enough = match rule {
        "all" | "any" => good >= 1,
        "both_or_ask" => good == total,
        k => good >= k.parse::<usize>().unwrap_or(1),
    };
    let results: Vec<serde_json::Value> = members
        .iter()
        .map(|(m, _, ok)| serde_json::json!({"node": m, "ok": ok}))
        .collect();
    let st = run.nodes.get_mut(id).unwrap();
    st.visits += 1;
    st.outputs.insert("results".into(), results.into());
    // Partial failure, where every part was wanted: yours to say.
    if rule == "both_or_ask"
        && !enough
        && good > 0
        && !run.gate.let_through.contains(&format!("join:{id}"))
    {
        run.limit = Some(format!("join:{id}"));
        return run.ask(
            env,
            Ask::Partial,
            Some(id),
            format!("{good} of {total} parts of {id} succeeded: go on with those?"),
        );
    }
    let enough = enough || (rule == "both_or_ask" && good > 0);
    let verdict = if enough {
        n.verdicts.first()
    } else {
        n.verdicts.get(1).or(n.verdicts.first())
    }
    .cloned()
    .unwrap_or_else(|| "done".into());
    let st = run.nodes.get_mut(id).unwrap();
    st.verdict = Some(verdict.clone());
    st.state = NodeState::Done;
    run.note(env, format!("{id}: {good} of {total} succeeded: {verdict}"));
    run.route(env, id, &verdict);
}

/// The id a workflow node's run gets: the parent's, then the node.
pub fn child_id(run: &WfRun, node: &str) -> String {
    format!("{}.{node}", run.id)
}

/// A workflow as a node: a run of its own, started with this run's inputs,
/// one level deeper. This run waits for it and takes its outcome as the
/// node's verdict: `succeeded` or `failed`.
pub fn workflow_node(run: &mut WfRun, root: &Path, env: &mut dyn Env, id: &str) {
    let child = child_id(run, id);
    if run.nodes[id].state == NodeState::Waiting {
        let name = run.workflow.nodes[id].workflow.clone().unwrap_or_default();
        if run.depth + 1 > run.opts.max_depth {
            run.nodes.get_mut(id).unwrap().state = NodeState::Failed;
            run.note(
                env,
                format!(
                    "{id} would start {name} {} deep, and max_depth is {}: not started",
                    plural((run.depth + 1) as usize, "level"),
                    run.opts.max_depth
                ),
            );
            return run.route(env, id, "failed");
        }
        let started =
            super::store::snapshot_of(&engine::dir_of(root, &run.id).join("sub").join(&name))
                .and_then(|w| {
                    let inputs = run
                        .inputs
                        .iter()
                        .filter(|(k, _)| w.inputs.contains_key(*k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    if let Some(missing) = w
                        .inputs
                        .iter()
                        .find(|(k, i)| i.required && !run.inputs.contains_key(*k))
                    {
                        return Err(format!(
                            "{name} needs the input {}, which this run doesn't have",
                            missing.0
                        ));
                    }
                    let mut sub = WfRun::new(
                        &child,
                        w.clone(),
                        run.repo.clone(),
                        &run.base,
                        inputs,
                        run.opts.clone(),
                        env.now(),
                    );
                    sub.parent = Some(run.id.clone());
                    sub.depth = run.depth + 1;
                    sub.started_by = run.started_by.clone();
                    sub.trusted_input = run.trusted_input;
                    // Its own sub-workflows were snapshotted with this run's.
                    let from = engine::dir_of(root, &run.id).join("sub");
                    let to = engine::dir_of(root, &child).join("sub");
                    for e in std::fs::read_dir(&from).into_iter().flatten().flatten() {
                        let _ = std::fs::create_dir_all(to.join(e.file_name()));
                        let _ = std::fs::copy(
                            e.path().join("workflow.toml"),
                            to.join(e.file_name()).join("workflow.toml"),
                        );
                    }
                    super::store::snapshot(&engine::dir_of(root, &child), &w)?;
                    engine::store(root, &sub)?;
                    env.open_run(&child, &run.repo)
                });
        return match started {
            Ok(()) => {
                let st = run.nodes.get_mut(id).unwrap();
                st.visits += 1;
                st.state = NodeState::Working;
                st.outputs.insert("run".into(), child.clone().into());
                run.note(env, format!("{id} started the run {child} ({name})"));
            }
            Err(e) => {
                run.nodes.get_mut(id).unwrap().state = NodeState::Failed;
                run.note(
                    env,
                    format!("{id} couldn't start {name}: {}", first_line(&e)),
                );
                run.route(env, id, "failed")
            }
        };
    }
    // Its run, as it stands: what it spent rolls up into this one.
    let Ok(sub) = engine::load(root, &child) else {
        return;
    };
    let (spent, _) = sub.spend(&[]);
    let st = run.nodes.get_mut(id).unwrap();
    st.outputs.insert("spent".into(), spent.into());
    st.outputs.insert("state".into(), sub.state.label().into());
    let verdict = match sub.state {
        RunState::Succeeded => "succeeded",
        RunState::Failed | RunState::Aborted => "failed",
        _ => return,
    };
    if let Some(sha) = &sub.pinned {
        st.outputs.insert("commit".into(), sha.clone().into());
    }
    st.verdict = Some(verdict.into());
    st.state = if verdict == "succeeded" {
        NodeState::Done
    } else {
        NodeState::Failed
    };
    run.note(env, format!("{id}'s run {child} {}", sub.state.label()));
    run.route(env, id, verdict);
}
