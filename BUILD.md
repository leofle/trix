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
| 1 | Baseline | (this commit) | pass |

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
