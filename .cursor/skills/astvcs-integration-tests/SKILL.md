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
4. Cross-check [docs/architecture.md](../../../docs/architecture.md) Testing table when adding integration tests that guard new CLI behavior.

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

- One directory per scenario (`workflow-demo`, `merge-demo`, `identity-demo`, `same-file-demo`, `network-demo`, `lifecycle-demo`, `shallow-demo`, `import-git-demo`, `serve-demo`).
- Add `.gitignore` in the fixture if generated artifacts appear during manual runs.
- Document CLI steps in [examples/README.md](../../../examples/README.md) with a table row linking fixture, test name, and what it shows.
- Use `./examples/reset.sh` in docs to restore baseline files (Windows: `.\examples\reset.ps1`).
- After changing fixture walkthroughs, run `./examples/run-demos.sh` to replay all ten fixtures (includes `identity set`, staging, `reset --mixed`, file remote sync, lifecycle commands, shallow clone, optional `import-git`, and HTTP serve).

### Assertions worth including

- `repo.working_tree_is_clean()` after successful merge.
- No `MoveNode` mutations on prepend-only edits (see `workflow_demo_prepend_and_disjoint_merge`).
- Merge conflicts leave repo unchanged (see `merge_conflict_diagnostics_without_side_effects`, `merge_conflict_still_leaves_refs_and_disk_unchanged_under_lock`).
- Revert conflicts and failed materialize commands on dirty trees leave refs and disk unchanged (see `cli_materialize_refuses_dirty_tree_and_force_overrides`, `cli_revert_and_dry_run`, `reset_hard_refuses_dirty_tree_without_force` in `src/store/repo.rs`).
- Repository lock contention fails fast with `repository is locked by another process; cannot acquire …/repo.lock` (see `cli_reports_repository_lock_contention`, `concurrent_repo_lock_fails_fast_with_actionable_error`).
- Reentrant lock guards may drop in any order; `outer_guard_may_drop_before_inner_without_releasing_lock` guards thread-local lock depth (see `src/store/lock.rs`).
- `sequential_acquire_after_release_on_same_thread` guards back-to-back in-process repo calls on Linux (see `src/store/lock.rs`).
- Stray `.astvcs-tmp` files from a prior crash are removed when the canonical file exists (see `stray_temp_file_cleaned_on_next_locked_command`).
- `gc` defaults to dry-run for blobs and history; `--prune` deletes only blobs unreachable from ref tips; `--prune-history` deletes unreachable timeline and state manifests (see `gc_*` tests in `store/integrity.rs`, `cli_gc_dry_run_and_prune`).
- `repack` packs loose blobs into `.astvcs/packs/` under the repo lock; reads fall back to pack index after loose files are removed (see `repack_roundtrip_and_fsck`, `gc_preserves_packed_blobs`, `repack_fetch_push_roundtrip`).
- `fsck` defaults to report-only; `--repair` rewrites a valid-HEAD index and removes unambiguous stray temps; `--prune-refs` deletes dangling ref files; repair refuses ambiguous HEAD when other branches exist; warns on `config.json` `format_version` newer than the binary (see `fsck_warns_on_unknown_format_version`, `cli_fsck_warns_on_unknown_format_version`).
- On-disk format versioning: legacy repos (`format_version` absent or `0`) migrate to the current version on first outermost `repo_lock`; new repos write `format_version: 1` on init (see `legacy_repo_without_format_version_migrates_on_lock`, `format_version_migrates_on_open_and_lock`).
- `gc` and `fsck` (including `--repair` and `--prune-refs`) fail fast under external lock with the same `repository is locked by another process` message (see `cli_gc_and_fsck_fail_under_external_lock`).
- `commit`, `merge`, and `revert` require configured author identity (`identity set` or env vars); see `commit_without_identity_fails_with_actionable_error`.
- `identity set` / `identity get` use locked atomic writes to `config.json` (repository) or `~/.astvcs/config.json` (global); see `identity_set_and_read_roundtrip_via_repo_open`.
- Manifest ids remain `hash_manifest`; commit ids hash parents and metadata. See `identity_does_not_change_content_addressed_state_id`, `parallel_branches_identical_content_keep_distinct_log_messages`, and `identity_recorded_on_commit_merge_and_revert`.
- CLI `--json` prints the full structured `RepoError` JSON on stderr. Focused plain errors may use `RepoError.concise`; `--details` restores the full `message` (see `structured_errors_match_plain_messages_and_kinds`).
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
- Change-first HTML viewer: `cli_diff_view_writes_html_with_alignment` and `cli_diff_view_large_file_keeps_change_first_controls` cover controls, embedded alignment, and large generated input. `viewer_javascript_indexes_all_file_modes_and_targets_insertions` executes shipped navigation and targeting logic with Node when available. Browser open is skipped when `CI` is set.
- Git drivers: `merge_driver_resolves_disjoint_structural_edits`, `merge_driver_conflicts_on_overlapping_literal_edits`, `diff_driver_prints_structural_intents`, `diff_driver_omits_binary_content`, `git_invokes_merge_driver_on_disjoint_edits`, `git_invokes_merge_driver_writes_markers_on_conflict` (`tests/git_drivers.rs`; Git e2e tests require `git` on PATH).
- Incremental scan cache: `incremental_status_reuses_unchanged_file_reads` (unit, `store/repo.rs`); `incremental_scan_reuses_unchanged_paths`, `incremental_scan_finds_new_file_in_changed_dir`, `incremental_scan_finds_new_file_in_deep_nested_dir` (unit, `store/walk.rs`); `add_all_from_deep_subdirectory_stages_new_file` (integration); `verified_detects_content_change_with_unchanged_stat` (unit, `store/scan_cache.rs`); `--full-scan` on `status` and `commit`; `-v` forces full scan.
- `default_branch` config: removing the default branch promotes `main` when present else lexicographically first remaining name (`remove_default_branch_updates_config` in `store/repo.rs` and `tests/integration.rs`); `branch create` fixes dangling default refs; `clone_uses_remote_default_branch` (unit, `network/sync.rs`) checks out upstream non-`main` default.
- Staging index: `partial_commit_only_stages_paths`, `status_shows_staged_and_unstaged_columns`, `merge_refuses_with_staged_changes`, `checkout_force_with_staged_changes`, `reset_mixed_unstages_and_keeps_disk`, `reset_modes_soft_mixed_hard_comparison`, `cli_commit_empty_staging_errors`; legacy whole-tree `commit` when `staging.json` is empty and `active` is false.
- `pull`: `pull_merges_upstream_changes`, `pull_detached_head_requires_branch`, `pull_merge_conflict_after_fetch`; `merge_remote_tracking_ref` (unit, `src/store/repo.rs`) for `merge origin/main` after fetch.
- `stash`: `stash_before_checkout`, `stash_pop_restores_files`, `stash_pop_preserves_unstashed_tracked_files`, `stash_pop_conflict_keeps_entry`, `stash_drop_discards_without_applying`, `stash_clear_removes_all_entries`; unit coverage in `store/stash.rs` (`stash_stack_save_load`, `stash_entry_roundtrip`, `stash_drop_and_clear_remove_entries_without_apply`).
- Lightweight tags: `tag_create_and_list`, `checkout_tag_detached`, `tag_fetch_push_between_repos`; unit coverage in `store/tags.rs` (`tag_create_list_remove`, `resolve_tag_ref`, `tag_name_validation`).
- Client hooks: `hook_pre_commit_aborts_commit`, `hook_commit_msg_edits_message`, `hook_nested_astvcs_status_in_pre_commit`, `hook_no_verify_skips_pre_commit`, `hook_pre_merge_aborts`; lock suspend/resume in `store/lock.rs` (`suspend_and_resume_releases_for_subprocess`).
- Rebase: `rebase_linear_success`, `rebase_conflict_abort_restores`, `rebase_conflict_continue`; unit coverage in `store/rebase.rs` (`collect_linear_commits_orders_oldest_first`, `collect_linear_commits_rejects_merge_commit`).
- Cherry-pick: `cherry_pick_clean_commit`, `cherry_pick_conflict_leaves_head_unchanged`, `cherry_pick_from_remote_tracking_ref`; unit coverage in `store/cherry_pick.rs` (`cherry_pick_rejects_merge_commit`, `linear_timeline_parent_rejects_merge_commit`).
- Blame: `blame_linear_two_commits`, `blame_reorder_preserves_attribution_for_moved_lines`; unit coverage in `store/blame.rs` (`child_to_parent_map_tracks_equal_lines`, `lines_changed_in_child_detects_insert_and_modify`, `reorder_does_not_mark_moved_lines_as_changed`).
- Bisect: `bisect_linear_four_commits`, `bisect_run_releases_lock_for_nested_astvcs`; unit coverage in `store/bisect.rs` (`collect_bisect_candidates_orders_oldest_first`, `collect_bisect_candidates_rejects_non_ancestor`, `collect_bisect_candidates_rejects_merge_commit`). `bisect run` suspends the repository lock like hooks so nested `astvcs` calls succeed.
- HTTP serve auth, TLS, and concurrency: unit coverage in `network/serve.rs` (`serve_requires_token_for_mutations`, `serve_read_requires_token_by_default`, `serve_public_read_allows_anonymous_get`, `serve_put_returns_503_when_advisory_lock_held`, `serve_concurrent_reads_during_writes`, `validate_tls_config_requires_both_or_neither`), `network/transport.rs` (`parse_remote_url_accepts_https`, `http_transport_sends_bearer_token`, `insecure_client_accepts_self_signed_cert`), `network/remote.rs` (`remote_token_roundtrip`); `network_file_remote_fetch_push_and_clone` confirms file remotes stay unrestricted and push targets pass `fsck`; `push_advances_index_on_head_branch_without_materializing` (unit, `network/sync.rs`) guards index sync on network ref advance.
- Shallow fetch/clone: `shallow_clone_has_fewer_timeline_entries_than_full_clone`, `full_fetch_deepens_shallow_clone`, `merge_base_fails_on_shallow_clone_with_incomplete_history` (integration); `shallow_clone_fetches_fewer_timeline_entries_than_full_clone`, `full_fetch_deepens_shallow_clone`, `merge_base_fails_on_shallow_repo_when_lca_missing` (unit, `network/sync.rs`); ancestry API `GET /v1/timeline/{tip}/ancestry?depth=N` (omit `depth` for full ancestry).
- SSH remotes: unit coverage in `network/ssh.rs` (URL parsing, `ssh_session_sends_bearer_token`), `network/remote_serve.rs` (newline JSON protocol roundtrip and token auth), `network/transport.rs` (`parse_remote_url_accepts_ssh`); optional `#[cfg(unix)]` localhost SSH test in `network/ssh.rs` skips when `ssh` is unavailable.
- `import-git`: `import_git_snapshot_from_subprocess` (integration) imports git HEAD via subprocess when `git` is on PATH; `import_git_ignores_stray_untracked_files` and `import_git_does_not_commit_skipped_binary_stray` guard against whole-tree stray commits; unit coverage in `store/git_import.rs` (`parse_ls_tree_line_*`).

### What to avoid

- Tests that depend on a pre-existing `.astvcs` directory in fixtures.
- Asserting exact state ids unless the test controls the full history.
- Trivial tests that only restate type signatures or obvious parser success without behavioral claims.

## Verify

```bash
cargo test <new_test_name>
cargo test
cargo test --test props --test history_smoke --test diff_git
```

For property tests, `PROPTEST_CASES` overrides the default 64 cases. For the long history driver: `cargo test history_long -- --ignored` with optional `HISTORY_SEED` and `HISTORY_OPS`.

For CLI-heavy tests, also run the documented fixture commands from [examples/README.md](../../../examples/README.md).
