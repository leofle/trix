# Changelog

## Unreleased

### Workflows

trix can now run any job the way it runs triage: agents with one task each, the checks that judge their work and the edges between them, prompted and re-prompted by trix itself. See [Workflows](README.md#workflows).

- **Templates**: `triage`, `issue-to-pr`, `bug-repro` and `nightly-deps`, shipped in the binary and read-only. Triage's stages run only in the built-in triage: it can't be copied, scheduled or used as a node yet, and the checker says so.
- **The builder** (`trix workflow edit <name>`, `alt+p`) and the **wizard** (`trix workflow new`): a workflow as boxes and edges, checked on every edit, with each problem's fixes one key away.
- **Any backend**: a capability matrix says what each supported agent can do in each role and how well trix knows it (`verified`, `documented`, `unverified`). `trix workflow capabilities [--verify]`.
- **One checker** behind `trix workflow check`, the builder, dry runs, imports and the start of a run.
- **Dry run and rehearsal**: `trix workflow dry-run` simulates a run with stub agents; `trix workflow rehearse` runs real agents against a throwaway clone and a local mirror.
- **Nodes**: agents with briefs and write scopes, checks that pin the commit they ran on, a gate that pushes, opens the PR and merges the checked commit, fan-out (`for_each`), `join`, and workflows as nodes.
- **Limits that mean cost**: a wall clock that counts only work, a cycle ceiling, wasted-cycle detection, a budget, quota backoff and a stall monitor.
- **Approvals, opt-in**: merge approver nodes and permission rules, off unless the workflow has an `[approvals]` block *and* is named in `[approvals] allow` with a trusted file. Three floors on trix's own push and merge can't be removed. Every approval is in `trix workflow audit`; `trix workflow revoke` takes them all back.
- **Run log**: every prompt, report and edge, hash-chained (`trix workflow log --verify`), kept on your machine. Only an outcome summary goes into a PR.
- **Scheduling**: `trix workflow schedule`, `trix workflow scheduler`, systemd and launchd units, `on_finish`, a daily digest.
- **Sharing**: `trix workflow export` and `import` (YAML). An import is checked, converted to native TOML and never trusted by being imported.
- New settings: `[workflows]`, `[workflows.agents]`, `[approvals]`, `[capabilities.<kind>]`, `[cost] confirm_over`, and the `workflows` key (`alt+p`).

### Changed

- **The Security promise.** "trix never answers a prompt for you" is now "never by default": a workflow permission rule you opt into twice lets trix answer the one prompt it covers, through the backend's own hook, and only where that hook's decision is verified for your installed version (Claude Code today). Nothing is typed into a pane. The README also says plainly that a workflow's checks and test commands run code the agents wrote, as you.
- **Triage runs on the workflow engine.** Its behavior is the same: the stages, the questions, the prompts, the case folder and every `trix triage` and `trix case` command are as they were, shown by replaying recorded cases against the old and the new code. Every `[triage]` setting is kept, and cases in flight go on. `trix workflow check triage` and `trix workflow dry-run triage` show it as a workflow.

### Migrating

- Nothing to do for triage. `trix triage`, `trix case …` and `[triage]` work as before.
- **One break: an agent trix can't price counts as unknown, not $0.** trix prices Claude and Codex agents. A triage case with an agent of another kind used to show that agent's spend as $0; it now shows `spent $1.20 + unknown of $10.00`. The budget still asks on what's priced, and can't count the rest. Workflows say the same (`+ ?`).
- `alt+p` now opens workflows. A key you bound yourself stays yours: bind `workflows` under `[keys]` to move it.
- A server from before this version can't start guarded panes. Restart the session (`trix kill`, then `trix`) before running a workflow.
