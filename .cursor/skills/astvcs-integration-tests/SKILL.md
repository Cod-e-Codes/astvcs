---
name: astvcs-integration-tests
description: Writes or extends astvcs integration tests and CLI fixtures. Use when editing tests/integration.rs, examples/, or when the user asks for end-to-end tests, fixture walkthroughs, merge scenarios, or CLI regression coverage.
paths:
  - "tests/**"
  - "examples/**"
metadata:
  project: astvcs
---

# Integration tests and fixtures

## Gather

1. Read existing helpers at the top of `tests/integration.rs`: `astvcs_bin()`, `run_astvcs()`, `copy_fixture()`.
2. Pick or create a fixture directory under `examples/`.
3. Map the scenario to an existing test in [references/test-catalog.md](references/test-catalog.md) before adding a duplicate.

## Act

### Test patterns

**Library API** (diff/merge internals). Use `Repo::init_with_identity` in tests that call `commit`, `merge`, or `revert` (or call `set_identity` after `Repo::init`).

```rust
let old_graph = parse_source("lib.rs", old_src).unwrap();
let new_graph = parse_source("lib.rs", new_src).unwrap();
let diff = diff_graphs(&old_graph, &new_graph);
```

**CLI subprocess** (end-to-end):

```rust
let out = run_astvcs(Some(repo.path()), &["merge", "feature", "-m", "msg"]);
assert!(out.status.success());
```

**Fixture copy**: use `copy_fixture(&dir, &fixture_root())` and skip `.astvcs` so each test starts clean.

### Fixture conventions

- One directory per scenario (`workflow-demo`, `merge-demo`, `identity-demo`, `same-file-demo`).
- Add `.gitignore` in the fixture if generated artifacts appear during manual runs.
- Document CLI steps in [examples/README.md](../../../examples/README.md) with a table row linking fixture, test name, and what it shows.
- Use `.\examples\reset.ps1` in docs to restore baseline files.

### Assertions worth including

- `repo.working_tree_is_clean()` after successful merge.
- No `MoveNode` mutations on prepend-only edits (see `workflow_demo_prepend_and_disjoint_merge`).
- Merge conflicts leave repo unchanged (see `merge_conflict_diagnostics_without_side_effects`, `merge_conflict_still_leaves_refs_and_disk_unchanged_under_lock`).
- Revert conflicts and failed materialize commands on dirty trees leave refs and disk unchanged (see `cli_materialize_refuses_dirty_tree_and_force_overrides`, `cli_revert_and_dry_run`, `reset_hard_refuses_dirty_tree_without_force` in `src/store/repo.rs`).
- Repository lock contention fails fast with `repository is locked by another process; cannot acquire …/repo.lock` (see `cli_reports_repository_lock_contention`, `concurrent_repo_lock_fails_fast_with_actionable_error`).
- Reentrant lock guards may drop in any order; `outer_guard_may_drop_before_inner_without_releasing_lock` guards thread-local lock depth (see `src/store/lock.rs`).
- `sequential_acquire_after_release_on_same_thread` guards back-to-back in-process repo calls on Linux (see `src/store/lock.rs`).
- Stray `.astvcs-tmp` files from a prior crash are removed when the canonical file exists (see `stray_temp_file_cleaned_on_next_locked_command`).
- `gc` defaults to dry-run; `--prune` deletes only blobs unreachable from ref tips; timeline entries are kept (see `gc_*` tests in `store/integrity.rs`, `cli_gc_dry_run_and_prune`).
- `fsck` is report-only with no `--repair`; clean repos print `fsck: repository ok` (see `cli_fsck_clean_repository`, `cli_fsck_detects_corruption`).
- `gc` and `fsck` fail fast under external lock with the same `repository is locked by another process` message (see `cli_gc_and_fsck_fail_under_external_lock`).
- `commit`, `merge`, and `revert` require configured author identity (`identity set` or env vars); see `commit_without_identity_fails_with_actionable_error`.
- `identity set` / `identity get` use locked atomic writes to `config.json` (repository) or `~/.astvcs/config.json` (global); see `identity_set_and_read_roundtrip_via_repo_open`.
- Author metadata is stored on timeline entries but not in state id hashes; see `identity_does_not_change_content_addressed_state_id` and `identity_recorded_on_commit_merge_and_revert`.
- CLI `--json` prints structured `RepoError` JSON on stderr; message text matches plain `error:` output (see `structured_errors_match_plain_messages_and_kinds`).
- `merge`, `checkout`, and `revert` refuse by default when the working tree is dirty; `--force` emits `warning: <command> --force: discarded uncommitted changes in <path>` per clobbered path (same contract as hard `reset`).
- Merge planning reads committed states only; `merge_force_on_dirty_overlapping_path_applies_committed_plan` guards against uncommitted edits affecting the merge result.
- No-op reverts skip the dirty-tree guard even when the working tree is dirty (`revert_noop_with_dirty_working_tree_skips_materialize_guard`).
- `parse_all_supported_languages` covers every `supported_extensions()` entry and `supported_special_paths()` basename (for example `go.mod`).
- `edit_roundtrip_preserves_structure_across_languages` checks parse → apply trivial edit → unparse → re-parse; roundtrip text must match edited source bytes (includes HTML and CSS).
- `same_file_demo_disjoint_merge` is the main stress test for same-file alignment heuristics; watch overlapping cases when changing diff/merge.
- Path rename tests: `path_rename_status_and_diff_integration`, `path_rename_merges_with_independent_content_edit`, `path_rename_conflicts_with_independent_add_at_destination`, `conflicting_path_renames_report_conflict`.
- Move tests: `move_subtree_and_sibling_payload_edit_merge`, `moved_function_reports_move_not_delete_insert`.
- Binary tests: `binary_commit_status_and_diff`, `binary_roundtrip_checkout_on_branch`, `binary_merge_add_add_conflict`, `binary_fsck_clean_after_commit`, `binary_push_clone_roundtrip`, `binary_reset_hard_roundtrip`, `binary_diff_state`.
- Symlink/mode tests: `symlink_commit_and_checkout`, `executable_mode_commit_and_checkout`, `symlink_vs_file_merge_conflict` (all platforms; CI enables Windows symlinks); unit coverage in `store/manifest.rs`, `store/working.rs`, `merge::tracked_symlink_vs_regular_file_conflicts`.
- Parse fallback visibility: `parse_fallback_status_annotation`, `parse_fallback_diff_annotation`, `parse_fallback_md_commit_stays_silent`, `parse_fallback_broken_rs_stderr_warning`, `parse_fallback_verbose_notice_detail`; unit coverage in `frontend/textblob.rs` (`syntax_error_emits_warning`, `syntax_error_verbose_emits_notice`).
- Incremental scan cache: `incremental_status_reuses_unchanged_file_reads` (unit, `store/repo.rs`); `incremental_scan_reuses_unchanged_paths`, `incremental_scan_finds_new_file_in_changed_dir` (unit, `store/walk.rs`); `verified_detects_content_change_with_unchanged_stat` (unit, `store/scan_cache.rs`); `--full-scan` on `status` and `commit`; `-v` forces full scan.

### What to avoid

- Tests that depend on a pre-existing `.astvcs` directory in fixtures.
- Asserting exact state ids unless the test controls the full history.
- Trivial tests that only restate type signatures or obvious parser success without behavioral claims.

## Verify

```powershell
cargo test <new_test_name>
cargo test
```

For CLI-heavy tests, also run the documented fixture commands from [examples/README.md](../../../examples/README.md).
