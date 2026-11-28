# Trix Workflows: build log

One checkpoint per step of the PRD's 17-step build plan (PRD v6, section 19).
Each entry says what was done, the decisions made, the tests added and
anything deferred. "Decisions to review" at the end collects every place the
build had to choose.

How every commit on this branch is checked:

```
GIT_CONFIG_GLOBAL=/dev/null cargo test
cargo clippy --all-targets -- -D warnings
rustfmt --check --edition 2024 src/workflow/mod.rs   # all new code, see D2
```

## Checkpoints

| Step | What | Commit | Result |
|---|---|---|---|
| 1 | Baseline | 5843b50 | pass |
| 2 | Seeded replay and goldens of today's triage | a39b7a0 | pass |
| 3 | Generic runner extracted, the triage tests ported (three green commits) | 1d928ee, aba1465, 8277530 | pass |
| 4 | Built-ins and the triage template (**hard parity gate**) | f590813 | **pass** |
| 5 | Workflow model and native TOML | 75e1cbc | pass |
| 6 | Capability matrix and the shared checker | 81d1bc5 | pass |
| 7 | Matrix probes | 77e9fd5 (with 8) | pass |
| 8 | Generic nodes and prompting | 77e9fd5 (with 7) | pass |
| 9 | Workspaces and landing | 871528d | pass |
| 10 | Composition | a899ab0 | pass |
| 11 | Policy, approvals and autopilot | 7e1cc86 | pass |
| 12 | Dry run and rehearsal | (this) | pass |

## Step 1: baseline

- Branch `trix/workflows` from `origin/main` at `fe0cbe3` (1.7.8).
- Full suite: **709 passed, 0 failed, 1 ignored**. Clippy clean with
  `--all-targets -- -D warnings`.
- The triage tests in `src/case.rs`: **45 tests, 375 assertions**, counted
  by `docs/workflows/assert-counts.sh src/case.rs`. The PRD says 46: `grep -c
  '#\[test\]'` gives 46 because one fixture string (`R1`, a proof patch)
  contains the text `#[test]`. There are 45 test functions.

| Test | Assertions |
|---|---|
| a_case_goes_from_findings_through_two_rounds_to_a_merge | 38 |
| reports_that_dont_fit_go_back_to_their_agent | 7 |
| the_case_asks_you_when_nothing_is_found_and_when_the_budget_is_spent | 18 |
| red_ci_at_the_gate_is_a_finding_for_the_fixer | 4 |
| a_conflict_at_the_gate_goes_to_the_fixer_then_the_gatekeeper_then_the_gate | 11 |
| a_round_for_a_conflict_the_gatekeeper_still_sees_has_the_merge_steps | 7 |
| a_conflict_with_the_rounds_used_up_asks | 5 |
| unknown_mergeability_is_waited_for | 5 |
| a_base_that_moved_as_it_merged_is_tried_again | 4 |
| a_merge_the_branch_rules_refuse_is_told_from_other_failures | 2 |
| a_merge_that_needs_another_reviewer_waits_for_an_approval | 9 |
| an_admin_may_merge_past_the_branch_rules_only_when_they_say_so | 6 |
| auto_merge_finishes_the_case_once_github_merges_it_and_is_turned_off_on_abort | 8 |
| approve_while_awaiting_approval_tries_the_merge_again | 1 |
| a_failed_merge_tried_again_goes_to_the_fixer_if_it_conflicts_now | 3 |
| an_issue_case_closes_its_issue_once_merged | 6 |
| an_issue_its_pr_closes_is_left_to_github | 3 |
| an_issue_not_said_solved_by_both_stays_open | 3 |
| a_merge_removes_the_read_only_checkouts_trix_locked | 7 |
| only_the_roles_own_agent_hands_in_its_report | 5 |
| checks_not_registered_yet_are_waited_for | 5 |
| ids_go_on_from_a_finished_case_and_never_share_a_running_one | 6 |
| an_issue_is_triaged_through_its_pr_or_you_choose_one | 10 |
| the_kind_of_change_comes_from_the_title_then_the_labels | 13 |
| the_prefix_is_spelled_as_the_repository_spells_it | 7 |
| an_issues_branch_follows_the_repositorys_naming | 3 |
| prs_that_mention_the_issue_or_refer_to_it_are_found_too | 3 |
| the_reviewer_checks_its_own_findings_and_whether_the_issue_is_solved | 15 |
| a_pr_that_doesnt_solve_the_issue_asks_how_to_go_on | 7 |
| what_the_fixer_leaves_to_you_is_told_at_the_gate_and_the_issue_must_be_solved | 12 |
| an_outsiders_comment_asks_before_the_reviewer_of_a_linked_pr_starts | 6 |
| only_a_formal_link_is_taken_without_asking | 8 |
| what_triage_is_given_is_checked_first | 8 |
| an_issue_with_no_pr_is_built_then_triaged | 18 |
| an_issue_from_outside_the_project_asks_before_the_builder_starts | 5 |
| two_reviewers_start_side_by_side_and_the_case_waits_for_both | 22 |
| the_same_defect_from_both_reviewers_is_one_finding_tagged_with_both | 9 |
| the_gatekeeper_sees_both_reviews_side_by_side | 6 |
| the_issue_closes_only_when_both_reviewers_and_the_gatekeeper_say_solved | 5 |
| a_case_restarted_mid_review_goes_on_with_each_reviewer_where_it_was | 11 |
| a_second_reviewer_that_cant_start_asks_while_the_first_goes_on | 9 |
| while_the_case_asks_about_one_reviewer_the_other_is_started_and_watched | 12 |
| a_second_reviewer_that_cant_start_either_waits_for_your_answer | 5 |
| a_finding_needs_its_proof_to_reach_the_fixer | 14 |
| a_case_from_before_two_reviewers_goes_on_with_its_one | 4 |
| **Total: 45 tests** | **375** |

Tests added: none. Deferred: nothing.

## Step 2: seeded replay and goldens of today's triage

- `src/workflow/replay.rs` (test-only): `World`, a recording `Env`.
  - **git is real**, in a temporary repository with a bare `origin`, run
    with no personal or system config, one author and dates that depend
    only on how many commits came before, so every SHA is the same on every
    machine. Worktrees, branches and pushes really happen.
  - **GitHub is fake** (`Hub`): PRs, check runs, mergeability, merge
    errors, branch rules, auto-merge, the issue API. No network, no login.
  - **Agents are scripted**: the scenario sets their states, makes their
    commits and pushes, and hands in their reports through the server's own
    check (`case::handle`), as `trix case submit` does.
- A golden (`tests/goldens/triage/*.golden`, 14 files, 311 KB) holds, in
  order: every `Env` call (`agents`, `git`, `gh`, `spawn`, `send`, `close`,
  `answer`, `say`, `print`, `open_case`), every prompt and message word for
  word, the case folder (`case.json` and each report file), the
  repository's worktrees and refs, and origin's refs.
- Scenarios, and what each covers:

| Golden | Covers |
|---|---|
| two-rounds | Reviewing, Fixing, Verifying, Gate, Merged; the gatekeeper's changes and a second round sent to the kept fixer; CI pending then green; Merge by `trix case approve` |
| two-reviewers | reviewers 2: side by side, the same defect merged, a retraction, the comparison the gatekeeper gets |
| nothing-found | NothingFound; `answer y` refused (a merge is only ever approve); the gate with no gatekeeper |
| round-limit | RoundLimit, a key that isn't an answer, Aborted from the gate |
| ci-failed | red CI as a finding with a round left; CiFailed with none |
| budget-no-report | the reminder, NoReport (stopped, then exited and started again), Budget, `trix case abort` |
| merge-failed | MergeFailed, a base that moved as it merged, mergeability unknown |
| conflict | Conflict with the rounds used up, the base merged in, a real upstream commit |
| setup | Setup: an agent that can't start, then can |
| approval | branch rules: Approval, auto-merge (`w`), approved, merged on GitHub |
| restore | reviewers 2, the case read back from its folder three times mid-run |
| choose | Choosing and Choose; a PR from a fork refused, and an issue whose only PR is one |
| build | Outsider, Building, commits not on origin sent back, the PR opened by trix, Unsolved (`f`), the issue closed after the merge |
| rebuild | Unsolved (`b`): handed to a fresh case |

  All 13 questions (NothingFound, RoundLimit, CiFailed, Budget, NoReport,
  Merge, MergeFailed, Conflict, Setup, Choose, Unsolved, Outsider,
  Approval) and all 8 stages appear in at least one golden.
- **Accept:** goldens committed; replay against the current runner
  (`case::step`, untouched) is identical, twice in a row and under another
  timezone.
- **Decisions:** what git prints on a failure isn't recorded (it differs
  between git versions), only that it failed. The hour in a printed log
  line isn't recorded (it's the machine's local time); the log's own
  timestamps are, in `case.json`.
- **Frozen:** the goldens are written once (`TRIX_BLESS=1` writes a missing
  golden and refuses to overwrite one). From here on `git diff <step 2
  commit> -- tests/goldens/triage` must stay empty.
- Tests added: 14 replay scenarios. Suite: 723 passed. Deferred: nothing.

## Step 3: the generic runner, and the triage tests ported

Three commits, each green (suite, clippy, new code rustfmt-clean):

- **A (`1d928ee`): the generic runner, beside triage's own.**
  - `src/workflow/env.rs`: the `Env` trait (unchanged: `now`, `git`, `gh`,
    `spawn`, `open_case`, `send`, `close`, `agents`, `answer`, `say`,
    `print`), `Seen`, `AgentSpec` (triage's `RoleAgent` is this type), and
    `Live`, the real world.
  - `src/workflow/runner.rs`: the `Run` trait and the generic `step` and
    `run_pane`. The runner owns what every run shares: looking at the
    agents, closing the panes whose node handed in (pane lifetime), taking
    your answers and `a` to abort, the limits, and saying what the pane is
    doing. What the nodes do is the run's own `visit`.
  - `src/case.rs`: triage's `step` was first split into its parts
    (`meanwhile`, `answered`, `over_budget`, `visit`) with no change in
    behaviour, then `impl Run for Case` puts the case on the generic
    runner. Triage's own `step` still drove `trix case run` in this commit.
  - Every replay scenario ran on **both** runners against the one golden
    (28 replays), so the two were shown equal before anything switched.
- **B (`aba1465`): the 45 tests ported.** They drive the case through
  `workflow::runner::step` and no longer through triage's own. Same names,
  every assertion kept: `docs/workflows/assert-counts.sh src/case.rs` gives
  the same 45 lines as at baseline, 375 assertions. The port is its own
  commit and it's small, because the generic `step` takes the same three
  arguments: the diff is the import.
- **C (this commit): switched over, the old runner deleted.** `trix case
  run <id>` is `runner::run_pane::<Case>`. Triage's own `step` and
  `run_pane` are gone: no second runner remains. The replay runs on the
  generic runner only.
- **Accept:** every commit green; assertion counts equal (45 tests, 375
  assertions, before and after); the port is its own commit; replay is
  identical after C (`git diff a39b7a0 -- tests/goldens/triage` is empty).
- Tests added: none (14 replays temporarily doubled in A). Suite: 723.
  Deferred: nothing.

## Step 4: built-ins and the triage template (hard parity gate)

**Built**

- `src/workflow/model.rs`: the workflow model (inputs, goal and
  `done_when`, limits, policy, approvals, profiles, nodes, edges, layout),
  read from native TOML with a schema version. Step 5 completes its
  storage, round trip and snapshot.
- `src/workflow/templates/triage.toml`: the built-in triage template,
  compiled into the binary and read-only. `push = "allow"`, `merge =
  "you"`, `no_progress = "off"`, `wall_clock = "12h"` (in-flight),
  `max_iterations` and `budget_usd` from `[triage]`, a brief on every agent
  node (`owns = ["**"]` for build and fix, nothing for review and gatekeep).
- **The `triage.*` and `gh.*` kinds are `src/case.rs`**: `triage.build`,
  `triage.review`, `triage.fix`, `triage.gatekeep`, `gh.resolve_target`
  (the Choosing stage) and `gh.gate` (which also merges). They keep today's
  prompts, report schemas and validators word for word, and run no scope
  check. What changed is who decides:
  - **What's next is the template's edges.** Every stage change in
    `case.rs` now goes through `go(case, verdict)`, which follows the
    template's edge from the stage's node. No built-in names its successor
    any more (the only `c.stage = …` left outside `go` are abort and the
    hand-off to a fresh case, which are your actions, not verdicts).
    A verdict with no edge asks you instead of guessing.
  - **Pane lifetime is the node's `revisit`.** `release()` closes a
    role's pane after its report when its node is `fresh` (builder,
    reviewers, gatekeeper) and keeps it when it's `reuse` (the fixer, whose
    next round is sent to the kept pane).
  - **Who a role's agent is, and the limits, come from the bound
    template.** `trix triage` binds `[triage]` into the template
    (`{{config.triage.…}}`) and makes the case from the result.
- **Aliases:** `trix workflow run triage --input target=<n>` is `trix
  triage <n>`; `trix workflow approve|abort <id>` is `trix case
  approve|abort <id>` for a triage run; `trix workflow ls` lists cases as
  runs. Every `trix case` subcommand and flag is kept (`ls`, `show`, `run`,
  `approve`, `abort`, `answer`, `submit`, `--json`, `--file`), and so is
  every `[triage]` key.
- **The `case.json` converter:** `Case::view()` turns any case, one written
  by an older trix too, into the run view the overview and `trix workflow
  ls` use. `case.json` itself keeps its format, so a case in flight across
  the upgrade just goes on.

**Accept (all pass)**

- **Replay is identical on the template path.** The replay now makes its
  cases from the bound template and runs them through the generic runner
  with graph routing and `revisit` lifetimes. All 14 goldens match, byte
  for byte: `git diff a39b7a0 -- tests/goldens/triage` is empty. That
  covers the prompts and messages word for word, the pane lifetimes (the
  `close` calls: fixer kept between rounds, builder, reviewers and
  gatekeeper closed after their report) and the pushes (origin's refs).
- **Every alias and flag is tested**
  (`every_trix_case_command_and_flag_still_reads_the_same`,
  `the_workflow_commands_are_the_case_commands_under_another_name`), and
  the `[triage]` mapping both ways
  (`the_triage_config_maps_onto_the_template_and_back`).
- **A converter test per stage**
  (`a_case_converts_to_a_run_at_every_stage`: Choosing, Building,
  Reviewing, Fixing, Verifying, Gate, Merged, Aborted, plus a `case.json`
  from before workflows).
- **A saved layout with `trix case run <id>` restores**
  (`a_saved_layouts_case_pane_restores_and_goes_on`).
- The 45 triage tests still pass with their 375 assertions.

Tests added: 8 (`workflow::triage`). Suite: 731 passed.

**Deferred to step 13:** enforcing the template's `wall_clock = "12h"` on a
triage run. Today's triage has no time limit; the in-flight clock needs
state that can't go into `case.json` without changing the folder the
goldens freeze, so it's kept in the run's own folder, which step 13 adds.

## Step 5: the workflow model and native TOML

- `src/workflow/model.rs` (from step 4) is the format: `schema = 1`,
  inputs, goal and `done_when` (mechanical and attested), limits, policy
  (`push`: `gate_only`, `guarded` or `allow`), approvals, profiles, notify,
  nodes with briefs, edges, and the builder's layout positions. Every
  struct refuses a key it doesn't know (`deny_unknown_fields`), and nothing
  unset is written out.
- `src/workflow/store.rs`: where workflows live. Built-ins are compiled in
  and read-only; `.trix/workflows/<name>.toml` in the main checkout is the
  team's and shadows `~/.config/trix/workflows/<name>.toml`, yours. A
  built-in's name can't be saved over: a fork gets its own.
- **Snapshot at start:** `store::snapshot` writes the workflow into the
  run's folder (`<state>/runs/<id>/workflow.toml`), and a run reads only
  that.
- Templates: `triage`, `issue-to-pr`, `bug-repro`, `nightly-deps`
  (`src/workflow/templates/`).
- **Accept:** round-trip and schema tests pass
  (`every_template_round_trips_through_the_native_format`,
  `the_schema_version_and_the_keys_are_checked`,
  `the_teams_workflow_shadows_yours_and_built_ins_are_read_only`,
  `a_run_works_from_the_snapshot_taken_at_its_start`).
- **Decisions:** `{{config.…}}` settings (triage's round ceiling, budget and
  agents) are kept as text in the file and bound when a run starts, so the
  shipped template round-trips unchanged. Durations are text (`3h`,
  `1h30m`). The content hash that trust is pinned to is FNV-1a over the
  workflow's canonical JSON: it detects edits, it isn't a signature.
- Tests added: 4. Suite: 735 passed. Deferred: nothing.

## Step 6: the capability matrix and the shared checker

**Built**

- `src/workflow/capabilities.toml`, shipped in the binary: one entry per
  backend (claude, codex, opencode, copilot, muse, grok, gemini, cursor,
  antigravity, amp, custom) with its **level** (`verified`, `documented`,
  `unverified`), version range, hand-in, read-only, best push protection,
  state source, resume, `quota_signal`, `permission_decision`, merge
  approver, priced and hook timeout. **Every entry ships as `documented`**;
  nothing is `verified` until a probe passes (step 7).
- `src/workflow/caps.rs`: the matrix, `[capabilities.<kind>]` overrides
  (labelled), local probe results, version detection (`<program>
  --version`) and the rule that a version outside the entry's range drops
  it to `documented`. Also the **safe list** (13 commands, exactly the
  PRD's), the five shipped profiles, and `Profile::protection`: a Claude
  profile is `enforced` only if it names its tools, every one is on the safe
  list, and it refuses the rest; anything else is `guarded`.
- `src/workflow/check.rs`: **`check(workflow, matrix, ctx)`, the only
  checker.** Each problem has a level (error, blocks-run, warning, info),
  a stable code, where it is (workflow, node, edge, rule, approvals) and
  its fixes; `apply` makes a fix, `fix_all` is `check --fix`. It also gives
  each agent node its badges (`Label`: hand-in, push protection,
  read-only enforced or by instruction, decision level, priced) and the
  cost estimate (peak parallel agents, cycles, priced cost, and whether
  some of it is unknown).
- `src/workflow/settings.rs` and `src/config.rs`: `[workflows]`
  (`ci_paths`, `on_finish`, `digest`), `[workflows.agents]`, `[approvals]`
  (`allow`, `allow_sandboxed`, `notify`, `floors`), `[capabilities.<kind>]`
  and `[cost] confirm_over`, with the PRD's defaults: agents ask, own runs
  only, depth 2; approvals off; the three floors on and not removable.
- `trix workflow check <name> [--fix]` and `trix workflow capabilities
  [--kind k]`.

**Accept**

- One test per row of the predictions and graph-check tables
  (`src/workflow/check_tests.rs`, 25 tests): Codex read-only hand-in;
  gate_only with Codex, Claude with `Bash(npm test:*)`, with `git:*`, with
  bare `Bash`, with `git push`, and a kind with no enforced profile; the
  workflow-level push-guarded warning; rules on unverified kinds (error) and
  on documented-only kinds (warning, suspended); an agent approver on a
  permission rule; a missing or invalid approver and the default order
  (judge, verifier, gatekeeper, you); dead rules; overlapping scopes on
  writers that can run at the same time; a missing brief on an agent node
  and none needed on a check; a verifier that is the author; a missing or
  unreachable land step; an expensive topology; a kind with no quota
  signal; an upgraded backend version; a writer whose scope covers CI
  configuration; plus every graph rule.
- **issue-to-pr checks to exactly 0 errors and 2 warnings** on a machine
  with its three backends installed in range and Codex's hooks trusted:
  the workflow-level push-guarded warning (implementer, judge, tests) and
  the judge's read-only by instruction.
- Every template passes check with 0 errors, triage bound to `[triage]`
  included.
- **Identical output on every fixture:** `every_way_in_gives_the_same_report`
  holds each way in against the API's text. At this step the ways in are
  the API and `trix workflow check`; import, dry run, the builder and run
  start are added to that same test as they're built (steps 8, 12, 14, 15),
  and its `SURFACES` count ends at 5.

**Decisions**

- A run starts where `entry` says, or at the nodes no edge leads to.
  issue-to-pr and bug-repro name theirs, because their first node is also
  the target of their back edges.
- "Can run at the same time" is: neither reaches the other going forward
  (an edge that leads back orders nothing).
- Path-glob overlap is decided segment by segment (`**` any folders, `*`
  within one). It can say "overlap" for two globs that share no real file
  when both have a `*` in the same segment; it's used for warnings and for
  the must-not-touch error, where erring that way is the safe side.
- The cost estimate before there's history is $0.50 per priced agent visit,
  doubled for a node on a loop. It's labelled an estimate, and unpriced
  kinds make the total `+ ?`.
- An info line says approvals stay off until the workflow's name is in
  `[approvals] allow`. It's info, not a warning, so it doesn't change the
  counts above.
- Codex's version range was set from the build machine (0.167.2 installed).
- Codex hook trust can't be read from Codex, so the checker counts the
  hooks as trusted once trix has seen a Codex hook event (step 8 records
  it); until then the warning shows.

Tests added: 31 (25 checker, 4 matrix, 2 settings). Suite: 766 passed.
Deferred: nothing.

## Step 7: matrix probes

**Built**

- `src/workflow/probe.rs`: the harness. Each probe runs the backend
  headless in a scratch clone (a bare `origin` beside it, a `trix` on PATH
  that only records a hand-in) and looks at what happened:
  `hand-in:<profile>` for each shipped profile the kind has (and, for
  Codex, the read-only sandbox the matrix says can't), `push-blocked` (a
  push to a remote the guard disables), `permission-decision` (was the hook
  asked, was its allow taken) and `hook-timeout` (an allow that comes after
  the timeout must not be taken). Outcomes: `verified`, `failed`,
  `inconclusive` (the event couldn't be triggered in 3 tries: doesn't
  count), `skipped`.
- `trix workflow capabilities --verify [--kind k]` runs them for the
  installed backends and records `capabilities.json` in trix's state
  folder. It's opt-in and spends a little quota.
- `src/workflow/guard.rs`: the guard's environment (git config overrides
  that send every push nowhere and turn repository hooks off, a `gh` with
  no login), needed here for the blocked-push probe and used from step 8 on.
- `tests/fixtures/backends/{claude,codex,gemini}`: shell stand-ins that
  take each backend's headless command line, make its hook calls and print
  its screens. `FAKE_MODE` makes them misbehave on purpose.

**Accept**

- **CI, fake backends only** (`workflow::probe::tests`, 6 tests): the
  harness records `verified` (a backend that does what the matrix says),
  `inconclusive` (the hook event never fires: "no permission request
  reached the hook in 3 tries"), and `failed` (the hook's allow isn't
  taken; a late allow is taken; it can't hand in; code that undoes the
  guard pushes). Covered: hand-in per profile, the blocked push, the
  PermissionRequest decision, the hook timeout. No test needs a real
  backend or a login.
- **Real probes on the build machine** (2026-11-28, macOS):

| Backend | Version | Result |
|---|---|---|
| claude | 2.1.332 | **verified**: hand-in under edit-only, edit-and-test, read-only and review-and-test; push blocked; the PermissionRequest hook's decision is taken; an allow after the 5 s timeout is not. Ships as `verified` for 2.1.332, `permission_decision = "verified"`. |
| codex | 0.167.2 | **inconclusive, ships as `documented`**: a managed policy on this machine refuses `approval_policy = never` ("falling back to required value UnlessTrusted"), so headless `codex exec` couldn't run the probe's command at all. Read-only not handing in matched the matrix. Nothing about Codex itself was shown either way. `permission_decision` stays `unverified`. |
| gemini | not installed | not probed: `documented` |
| amp, antigravity, copilot, cursor, grok, muse, opencode | installed | not probed: trix knows no headless mode for them. `documented` |

- **Codex's decision result is recorded** (inconclusive), so it isn't
  verified, and **Codex prompt approvals stay out**: a permission rule on a
  Codex node is the check error `rule-no-decision`
  (`codex_hands_in_from_workspace_write_only_and_its_decision_is_recorded`).
- Claude's entry is `verified` for exactly 2.1.332. On any other version it
  counts as `documented` (rules suspended, a warning) until the probe runs
  there.

**Decisions**

- The first two real Codex runs said "failed: it didn't hand in" because of
  the harness, not Codex: `codex exec` doesn't take `-a`. That was fixed
  (the flag is dropped for headless Codex) and the probe rerun; the result
  above is from the rerun plus a by-hand run that showed the managed policy.
- The fake backends are POSIX shell, so tests need `sh`, `sed` and `git`,
  nothing else.

Tests added: 7 (6 probe, 1 guard). Deferred: nothing.

## Step 8: generic nodes and prompting

**Built**

- `src/workflow/engine.rs`: `WfRun`, a run of any workflow, on the generic
  runner (`impl Run`). Its state is `runs/<id>/run.json` (versioned), with
  the snapshot, `reports/`, `asks/` and `approvals.jsonl` beside it.
  - **Agent nodes** (agent, judge, supervisor): a writer works in the run's
    worktree on `trix/wf-<id>`; a reader gets a detached checkout of the
    checked commit, its own each visit. `revisit: reuse` keeps the pane and
    sends the next visit to it; `fresh` closes it once the report is in.
  - **Checks**: a clean, detached checkout of the run branch's head, which
    trix resolves itself; the exit code decides; the commit it ran on is
    **pinned**, and everything after judges that commit. A flaky rerun
    isn't a cycle.
  - **The gate** (`gh.gate`): `done_when` on the pinned commit, trix's own
    plain push of exactly that commit, the PR (its body is the outcome
    summary), CI on that commit ("no checks yet" is never green), the
    approval (you, or the approver node when approvals are on for this
    workflow and it voted pass on that commit and didn't write it), a fresh
    CI look, then `gh pr merge --match-head-commit`.
  - **Edges** pick what's next from the verdict. An edge that leads back is
    a cycle of cost; the ceiling (`max_iterations`), `max_visits`, the
    budget (priced kinds only: an unpriced agent's spend is unknown, never
    $0) and the in-flight wall clock stop it, by `on_limit`.
  - **Wasted cycles**: a writer that comes back and leaves the head where it
    was, a check that fails the same way on the same commit, a reader that
    gives the same verdict on the same commit. `no_progress: 1` fails the
    run at the first.
  - **Scope checks** at each hand-in of a generic writer: files its own
    commits changed and that still differ, plus what it left uncommitted;
    not what a merge of the base brought in. Outside `owns`, or inside
    `must_not_touch`, goes back to the node as a cycle of cost.
  - **Questions** (`trix wf ask`) go to the supervisor node, whose answer
    comes back as the asker's next prompt, or to you (no supervisor, or
    ESCALATE). **Mentions** are queued to an agent node's inbox and
    delivered on its next visit along a declared edge; a mention of a
    check, gate or unknown node is refused when it's submitted.
  - **Watching**: a stopped agent is nudged once after its kind's settle
    time, then it's yours (autopilot: parked). Only an agent that *exited*
    is started again (twice by default), then the `exited` event fires.
  - **Quota**: a stopped agent whose screen shows a shipped quota pattern
    is `blocked: quota`, retried with backoff (5 min, doubling; 6 tries or
    2 h), on `on_quota.fallback` if set, then escalated. Blocked time
    isn't in-flight time.
  - **The stall monitor**: when nothing is pending and the goal isn't met,
    `on_stall` applies (ask when supervised, park and notify on autopilot).
- `src/workflow/prompt.rs`: templates (`{{name}}`, `{{fence name}}`,
  `{{#if}}`), the brief and how to hand in, in every generic prompt.
- `src/workflow/launch.rs`: starting a run: the checker first (a refusal is
  the checker's own text), input resolvers before any prompt (`gh.issue`),
  the snapshot, the run's options from your config.
- Server and CLI: `Request::Wf` (reports and questions are taken only from
  the node's own pane; approve, answer and revoke only from you),
  `Request::SpawnGuarded`, `trix wf submit|ask`, `trix workflow
  run|pane|ls|show|approve|abort|pause|resume|revoke|answer`.

**Accept** (`src/workflow/engine_tests.rs`, 14 tests, all on a real
temporary repository with scripted agents and a fake GitHub through `Env`)

- issue-to-pr runs end to end on a fixture repository: Codex implements,
  the check fails then passes, Claude reviews, Gemini judges and approves,
  trix pushes, opens the PR and merges the checked commit. Zero human
  actions, 2 cycles, 0 wasted.
- A linear flow finishes in one pass (1 cycle, ceiling 1).
- The run stops at the first wasted cycle; with `no_progress: off` it goes on.
- Reuse nodes keep their pane between visits; fresh nodes close theirs
  after the hand-in.
- A mention never wakes a node and arrives on its next edge visit; a
  mention of a check or gate is refused.
- An out-of-scope change goes back to its node; a base-branch merge
  doesn't count against scope.
- The stall monitor fires only when nothing is pending.
- A simulated quota stop shows blocked, backs off without using the wall
  clock, and escalates; a fallback backend takes over when set.
- Idle agents are nudged, never restarted; exited ones are restarted.
- Injected failures recover: an agent that can't start, a flaky check, a
  failed push, red CI, a failed merge, pause and resume, abort.

**Decisions**

- A check blocks the run's pane while it runs. The pane is a process of its
  own, so nothing else waits on it, but a run doesn't look at its other
  nodes during a long check.
- Quota is looked for only on an agent that has stopped without its report,
  in the last 15 lines of its screen: an agent *talking about* quotas while
  it works isn't taken for one that's out of them.
- The gate parks on red CI unless the workflow has an edge for `ci_failed`.
- A second run for the same issue is named `<name>-<n>-2`.
- Steps 7 and 8 are one commit: the real probes (step 7) ran in the
  background while step 8 was written, and both touch the same files.
  Each was green on its own tests before the other was finished.

Tests added: 17 (14 engine, 3 prompt). Suite: 790 passed. Deferred to
their own steps: `land`, `for_each`, `join` and `workflow` nodes (9, 10);
floors, the hooks fingerprint, rule approvals and the agent-start policy
(11); the run log (13).

## Step 9: workspaces and landing

**Built** (`src/workflow/land.rs`, and the engine's `begin`)

- **Shared:** writers that edges put in order work one after another in
  the run's worktree (`trix/wf-<run>`).
- **Isolated:** a writer that can run at the same time as another (no edge
  orders them), that fans out, or that asks for it gets a worktree of its
  own on `trix/wf-<run>-<node>`, branched from the run's branch. What's
  uncommitted in the run's worktree is copied in: staged files stay staged
  (`git stash create`, applied with `--index`; nobody's stash is touched)
  and untracked files come too.
- **The land step:** once every isolated writer at work has handed in,
  their commits are rebased onto the run's branch in the order they
  finished, and the run's branch fast-forwards. A conflict takes the rebase
  back, then goes to its author with the conflicting files in its inbox
  (`on_conflict: author`, a cycle of cost) or ends the run (`fail`). After
  landing, the check runs on the landed commit, which trix pins.
- **Nothing is removed before it lands.** `land::remove_tree` refuses a
  worktree whose branch has commits not on the run's branch, or that has
  uncommitted changes, and never forces. At the end of a run, landed
  worktrees and their branches go; the run's own stays.
- A verdict can **fan out** to several nodes (two edges on the same
  trigger).

**Accept** (`workflow::engine_tests`, 3 tests)

- Writers in order share one worktree (`writers_in_order_share_one_worktree`).
- Writers that can run at the same time get their own, with scopes that
  don't overlap too, and never share an index
  (`writers_that_can_run_at_the_same_time_get_their_own_worktrees_and_land`:
  three different index files, the dirty state copied, landing in finish
  order, the check and a Gemini verifier on the landed commit, the gate,
  the outcome summary as the PR's body).
- A forced conflict goes back to its author with the conflicting file, and
  lands once resolved; `on_conflict: fail` ends the run
  (`a_land_conflict_goes_back_to_its_author`).
- No worktree is removed before landing: the attempted removal is refused
  in the test ("1 commit on trix/wf-pair-42-ui hasn't landed").

**Decisions**

- The cycle ceiling is `max_iterations` whenever it's set, 1 otherwise, and
  it bounds every way work goes back: an edge, a scope send-back, a land
  conflict. So the checker now also wants `max_iterations` when a land step
  sends conflicts back to their author (`loop-no-ceiling` on the land node).
- A landed writer's branch is deleted by name (`git branch -D`) only after
  trix has verified it has no commit that isn't on the run's branch.

Tests added: 3, and one checker case. Suite: 793 passed. Deferred: nothing.

## Step 10: composition

**Built**

- `src/workflow/compose.rs`:
  - **`for_each`**: a fan-out declared in the workflow and checked before
    the run. At run time it's one agent visit per item of the list another
    node handed in, each in a worktree of its own, at most `parallel` at
    once. An item that fails (a verdict, or a runtime event past its
    restarts) is a failed item, never a failed run.
  - **`join`**: waits for what leads into it by its rule (`all`, `any`, a
    number, or triage's `both_or_ask`), then gives its first verdict if
    enough succeeded and its second if not. `both_or_ask` asks you when
    only some parts came.
  - **`workflow`**: a workflow as a node. Its run is a run of its own
    (`<parent>.<node>`), one level deeper, started with the parent's
    inputs; the parent waits and takes its outcome as the verdict
    (`succeeded` or `failed`). The sub-workflow is checked and snapshotted
    when the parent starts.
- **Blackboard:** a report's `board` object is merged into the run's
  shared state, and any later prompt can read `{{board.key}}`.
- **Triggers** (`src/workflow/trigger.rs`, and `triggers` in the model):
  another workflow's run ending, an issue or PR on GitHub with a label
  (polled through `gh`), files changed on the default branch. Each fires
  once for each thing it sees; what fired is kept across restarts. The
  scheduler that polls them is step 13.

**Accept** (4 tests)

- **Nested runs**, the **depth limit** and **spend roll-up**:
  `a_workflow_node_runs_a_nested_run_rolls_its_spend_up_and_keeps_to_the_depth_limit`
  (the inner run's live and closed spend shows in the outer's total; with
  `max_depth = 2` a third level isn't started; a failed inner run fails
  the outer's node; an inner workflow that doesn't check keeps the outer
  from starting).
- **Join partial failure**:
  `a_fan_out_runs_an_agent_per_item_and_the_join_takes_partial_failure`
  (three packages: one upgraded, one skipped, one exited three times; the
  join says `some`, what upgraded lands and is tested; `parallel = 1`;
  `none`; `both_or_ask`).
- **Triggers that fire once**: `a_trigger_fires_once_for_each_thing_it_sees`.
- The blackboard: `what_a_node_writes_on_the_blackboard_every_later_node_can_read`.

**Decisions**

- A fan-out's instances are added to the run's own copy of the workflow as
  agent nodes (`upgrade-1`, `upgrade-2`, …) when the list is known. That's
  the declared fan-out unfolding, not a graph an LLM rewrote: the instance
  is the for_each node's agent, brief and profile, and nothing else can add
  a node.
- A fan-out's first declared verdict is the one that counts as success for
  a join.
- A nested run works on its own branch and, if it has a gate, opens its own
  PR. The parent reads its outcome; it doesn't merge the child's branch
  into its own. (Known limit.)

Tests added: 4. Suite: 797 passed. Deferred: the polling loop for triggers
(step 13).

## Step 11: policy, approvals and autopilot

**Built** (`src/workflow/policy.rs`, and the engine, server and hook client)

- **Floors on trix's own actions** (on by default, not removable; a
  workflow or your config can only add): trix's push is a plain one of the
  checked commit, never forced; no merge into a protected branch without
  green CI on the checked commit, whoever approved (a branch whose
  protection can't be asked counts as protected); no push of a change to CI
  configuration without you, released for that run only by `trix workflow
  approve`. Two opt-in floors: `push_without_you`, `merge_without_you`.
- **The hooks fingerprint**: the repository's hooks and `core.hooksPath`,
  `core.fsmonitor`, `core.sshCommand`, `credential.helper`, taken when the
  run begins (under `push: guarded` and `gate_only`) and held against
  before trix's own push, land and merge. A change parks the run and says
  what changed.
- **The guard** on every agent pane (`Request::SpawnGuarded`, a new request
  so an old server hangs up instead of starting the pane unguarded) and on
  every check. A check runs in a process of its own with trix's variables
  removed, and its pid is registered with the server (`Request::CheckPid`):
  the server's ancestry walk finds a registered check before any pane, so a
  request from it, or from anything it starts, is `Asker::Check`, refused
  for every operation. `push: allow` gets no guard, as today.
- **Rule approvers through the hook.** `trix hook claude` (and codex), on a
  PermissionRequest, asks the server whether a rule of the run covers this
  one request. Allow is printed for the backend; anything else (no rule, a
  floor, revoked, approvals off, the hook not verified for the installed
  version, a timeout, no server) prints nothing, and the prompt is yours as
  before. Nothing is typed into a pane. A push or a merge is never a
  rule's. **Codex's decision isn't verified** (step 7), so no rule fires on
  Codex.
- **Merge approver nodes** (from step 8) with the two switches: the
  workflow's `approvals` block, and its name in `[approvals] allow` plus a
  trusted file.
- **Trust by hash** (`trix workflow trust`): a file runs only as you
  trusted it; the hash covers the workflow, the workflows its nodes run and
  the arguments its profiles resolve to. A built-in is trusted by where it
  was found, never by what a file says of itself. Only you trust: the
  server refuses it from agent panes and checks.
- **`[workflows.agents]`**: an agent pane starts a run only as `run` says
  (`ask`: the run asks you first, in its pane), stops or resumes only as
  `stop` and `resume` say, only its own runs under `scope = "own"`, and no
  deeper than `max_depth`. A gate is never an agent's.
- **Audit** (`approvals.jsonl`): every approval with time, run, node, kind,
  approver, rule, the exact tool and command, cwd, branch and SHA.
  **Revoke** drops every approval back to ask at once.
- **The autopilot preset** (`trix workflow autopilot <name> [--yes]`):
  refused while the checker shows an error; shows what each node will do
  unattended (covered by its profile, covered by rules, will park on
  prompts); writes limits that park, the merge approver (a judge, else a
  verifier, else a gatekeeper; yours when only an author could), and a
  catch-all rule that asks. It writes no rule that allows a tool.

**Accept** (`workflow::policy_tests`, 9 tests, and one in `control.rs`)

- The decision is tied to the request (one synchronous request and reply
  for one tool, command and place), and a hook timeout falls back to ask:
  `a_rule_answers_one_request_through_the_hook_and_everything_else_asks`.
- No code path types approval keys: an approver node's and a rule's
  approvals return no key (asserted), and the end-to-end run types none.
- Outside agent panes can't approve; self-approval and missing approvers
  are refused (checker, and `approver()` at the gate).
- **A planted test in a check** that runs `git push --force`, `gh pr merge`
  and `trix workflow approve` fails on all three, with a fake `gh` on PATH
  that succeeds only when it finds a login; the fixture remote and PR are
  unchanged: `a_planted_test_cant_push_merge_or_approve`. The same test
  under `push: allow` pushes and merges, which shows the guard is what
  stopped it.
- A planted pre-push hook makes the gate park before its push:
  `a_planted_pre_push_hook_parks_the_gate_before_its_push`.
- A commit that changes `.github/workflows/` parks the gate for you before
  its push, an approver node can't release it, and `trix workflow approve`
  releases it for that run only:
  `a_ci_config_change_parks_the_gate_for_you_and_no_approver_can_release_it`.
- A Claude profile with `Bash(npm test:*)` is labelled guarded and one with
  only safe-list commands enforced (step 6's `only_a_safe_list_profile_is_enforced`).
- `push: allow` panes get no guard (asserted in the planted test).
- The preset on issue-to-pr passes check with zero errors and has no dead
  rules: `the_autopilot_preset_passes_check_and_has_no_dead_rules`.
- An audit entry exists for every approval: by the approver node, by you,
  for a floor, for a permission rule (asserted in each test).

**What the planted-test proof is, exactly.** In the test the check's
`trix` is a stand-in on PATH that records the call and fails; the server's
refusal is proven separately, on the real code: `engine::handle` refuses
`Asker::Check` for every operation, and
`control::tests::a_check_and_what_it_starts_are_nobodys_whatever_pane_runs_them`
shows with real process ancestry that a registered check's child resolves
to the check before the pane above it. There is no end-to-end test with a
live server and a real `trix` binary in a check.

**Decisions**

- A sandboxed agent can hand in and ask. Starting and stopping runs is
  refused for it unless `[approvals] allow_sandboxed = true`; approving is
  refused always.
- The CI-config floor compares the checked commit with the base as origin
  has it (`origin/<base>...<sha>`).
- The preset writes no tool-allowing rule (the PRD's `npm install` example
  is one a user adds): a rule trix invents would be an approval nobody chose.

Tests added: 10. Suite: 807 passed. Deferred: nothing.

## Step 12: dry run and rehearsal

**Built** (`src/workflow/dryrun.rs`)

- **Dry run** (`trix workflow dry-run <name> [--scenario s] [--prompts]`):
  the one checker first (a refusal is its text), then the **real engine**
  driven through a stub `Env`: no agent, no git, no GitHub. Stub agents hand
  in canned verdicts by scenario: `happy`, `failures` (each failure edge
  once), `worst`, `conflict`, `stall=<node>`, `quota=<node>`, or verdicts
  you script per visit (`<node>=<v>,<v>`). It reports the path, every
  prompt each node would get per visit with its inbox filled in, how the
  run ends and why, cycles and waste, peak parallelism, the cost estimate
  (unknown for unpriced kinds) and every ⚡ approval that would be made
  without you.
- **Rehearsal** (`trix workflow rehearse <name>`): `prepare` makes a bare
  mirror of the repository and a clone of the mirror, so a push can only
  reach the mirror and `gh` finds no GitHub remote. The run's pane wraps
  its `Env` in `Rehearse`: the guard on every pane and check whatever the
  push policy, and trix's own push, PR and merge stubbed. The clone and
  mirror are deleted when the run is over.

**Accept** (`workflow::dryrun_tests`, 3 tests)

- **Seeded defects are reported**
  (`seeded_defects_are_reported_before_any_agent_runs`): a checker error
  (not simulated, said why); the worst case (what stops it); a defect only
  a run shows (`done_when` names a node that never votes pass: the checker
  passes it, the simulation fails it); a node that goes quiet; a quota
  stop; a land conflict; and autopilot parking where supervised asks.
- **Prompts match the goldens**: `tests/goldens/dryrun/issue-to-pr-happy.golden`
  and `issue-to-pr-failures.golden` hold every prompt of those two dry
  runs; `a_dry_run_shows_every_prompt_and_what_would_be_approved_without_you`
  compares them.
- **In rehearsal** an agent's `git push`, `gh pr create`, `gh pr merge` and
  `gh api` all fail (with a `gh` that works whenever it finds a login), the
  fixture's remote is unchanged, trix's own push and PR never reach git or
  gh, and after cleanup nothing is left behind:
  `in_a_rehearsal_nothing_an_agent_does_reaches_the_real_remote`.
- The "same report" test now covers three ways in (check, run start, dry
  run and rehearsal).

**Decisions**

- A dry run doesn't start the runs its workflow nodes name: it says so at
  that node. Dry-run each workflow on its own.
- The stub's check passes unless the scenario says otherwise; it can't know
  what your tests would say.

Tests added: 3. Suite: 810 passed. Deferred: nothing.

## Decisions to review

- **D1. BUILD.md is at the repository root.** The PRD says
  `docs/workflows/BUILD.md`; the build instructions say the root. The
  instructions are the later word, so it's here. Supporting files are in
  `docs/workflows/`.
- **D2. `cargo fmt --check` is applied to new code only.** `origin/main`
  isn't rustfmt-formatted (`cargo fmt --check` reports 2,746 diffs at
  `fe0cbe3`, and CI doesn't run it). Making the whole-repo check pass would
  mean reformatting every existing file, which is an unrelated change that
  would bury this PR's diff. So all new code lives under `src/workflow/` and
  is rustfmt-clean, checked with `rustfmt --check --edition 2024
  src/workflow/mod.rs` (which follows every submodule). Existing files that
  this branch edits (`src/case.rs`, `src/control.rs`, `src/main.rs`, …) keep
  their current style so their diffs stay readable.
- **D3. Tests run with `GIT_CONFIG_GLOBAL=/dev/null` on the build machine.**
  One existing test
  (`review::tests::a_worktree_with_copied_ignored_files_is_still_cleaned_up_unforced`)
  fails here on `origin/main` because this machine's personal git config sets
  `init.templateDir` to a template without `info/`. It passes with the
  personal config out of the way, and in CI. The test wasn't changed.
- **D4. The triage tests number 45, not 46** (see step 1).
- **D6. The built-in kinds stay in `src/case.rs`.** The PRD asks for
  "`triage.*` and `gh.*` kinds"; they are the functions of `case.rs`,
  driven by the generic runner and routed by the template, not a rewrite
  into new files. Moving 2,000 lines would have made the parity diff
  unreadable and bought nothing. `gh.merge` is part of `gh.gate` here, as
  the merge is today. A triage **fork** (your own copy with other edges or
  an approver node) runs on the generic engine's `gh.gate`, not on these
  built-ins: see the known limits.
- **D5. `Env::open_case` still takes a triage `Case`.** It's what triage's
  "a builder starts a fresh PR" uses, and the PRD lists it as part of the
  existing seam. Workflow runs start their sub-runs through `Env::open_run`
  (step 10), added beside it.
