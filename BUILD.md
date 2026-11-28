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
| 3 | Generic runner extracted, the triage tests ported (three green commits) | 1d928ee, aba1465, (this) | pass |

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
- **D5. `Env::open_case` still takes a triage `Case`.** It's what triage's
  "a builder starts a fresh PR" uses, and the PRD lists it as part of the
  existing seam. Workflow runs start their sub-runs through `Env::open_run`
  (step 10), added beside it.
