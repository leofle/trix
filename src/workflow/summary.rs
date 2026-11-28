//! The outcome summary: what leaves the machine, in the end-of-run report
//! and the PR's body. Nodes, verdicts, cycles, check results, landings and
//! approvals, and nothing else: never a prompt, inbox or issue text, a
//! local path or a command's log, because the PR may be public.

use super::engine::WfRun;

/// The summary of a run, as Markdown.
pub fn outcome(r: &WfRun) -> String {
    let mut s = format!(
        "**trix workflow `{}`**, run `{}`: {}.\n\nCycles: {} ({} wasted), of at most {}.\n\n| node | kind | backend | visits | result |\n|---|---|---|---|---|\n",
        r.workflow.name,
        r.id,
        r.state.label(),
        r.cycles,
        r.wasted,
        r.ceiling
    );
    for (id, n) in &r.workflow.nodes {
        let st = &r.nodes[id];
        let backend = match n.kind.as_str() {
            "check" => "check".to_string(),
            _ if n.is_agent() => st.backend.clone().or(n.agent.clone()).unwrap_or_default(),
            _ => "trix".to_string(),
        };
        // A verdict is one of the words the workflow declares: never free text.
        let mut result = st.verdict.clone().unwrap_or_else(|| "-".into());
        if n.kind == "check" {
            let code = st.outputs.get("exit_code").and_then(|c| c.as_i64());
            let sha = st
                .outputs
                .get("commit")
                .and_then(|c| c.as_str())
                .unwrap_or_default();
            if let Some(code) = code {
                result = format!("{result} (exit {code}) on {}", &sha[..sha.len().min(7)]);
            }
        }
        s.push_str(&format!(
            "| {id} | {} | {backend} | {} | {result} |\n",
            n.kind, st.visits
        ));
    }
    if let Some(sha) = &r.pinned {
        s.push_str(&format!(
            "\nChecked commit: `{}`.\n",
            &sha[..sha.len().min(12)]
        ));
    }
    if let Some(who) = &r.gate.approved {
        let how = if who == "you" {
            "the user".to_string()
        } else {
            format!("the approver node `{who}` (rule `approvals.merge`)")
        };
        s.push_str(&format!("Merge approved by {how}.\n"));
    }
    for floor in &r.gate.let_through {
        s.push_str(&format!(
            "The user let this run past the floor `{floor}`.\n"
        ));
    }
    s
}
