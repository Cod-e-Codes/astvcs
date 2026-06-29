# Integration test catalog

| Test | Focus |
|------|-------|
| `workflow_demo_prepend_and_disjoint_merge` | Prepend comment, no move cascade, disjoint merge |
| `identity_demo_payload_edit_disjoint_merge_and_conflict` | EditPayload, sibling merge, rename conflict |
| `trailing_comment_and_literal_edit_merge` | Trailing comment trivia survives merge with sibling literal edit |
| `cli_trivia_only_commit` | Whitespace-only commit via CLI |
| `cli_merge_resolve_conflict` | `--resolve path:ours\|theirs` |
| `commit_respects_gitignore` | Ignore rules during scan |
| `multi_language_repo_roundtrip` | Mixed AST and text files in one repo |
| `history_walk_and_log_order` | Timeline ordering |
| `blob_deduplication_across_states` | Content-addressed storage |
| `rust_unparse_roundtrip_via_repo` | Parse, commit, checkout round-trip |
| `go_unparse_roundtrip_via_repo` | Go parse, commit, checkout round-trip (block closing newlines) |
| `parse_all_supported_languages` | Every supported extension and special path parses |
| `edit_roundtrip_preserves_structure_across_languages` | Parse, trivial edit, unparse, re-parse: structural stability and text matches edited source (Rust, Python, JS, JSON, TS, Go, HTML, CSS) |
| `same_file_demo_disjoint_merge` | Same-file disjoint AST merge (alignment stress test) |
| `branch_merge_with_merge_base` | LCA and branch merge |
| `cli_branch_remove_guardrails` | Branch remove: checked-out, last branch, not found, recreate name |
| `merge_demo_add_add_and_deletion` | Add/add and deletion cases |
| `merge_demo_deletion_when_other_branch_unchanged` | Modify vs delete |
| `checkout_state_and_empty_commit` | Detached HEAD, no-op commit |
| `config_files_use_ast_frontend` | TOML/YAML/JSON AST path |
| `merge_conflict_diagnostics_without_side_effects` | Atomic rollback on conflict |
| `rename_vs_parent_delete_reports_overlap` | Rename vs parent delete overlap report |
| `transparency_scan_and_parse_notices` | `-v` notice output |
| `notices_suppressed_without_verbose` | Default stderr verbosity |
| `network_file_remote_fetch_push_and_clone` | File remote sync |
| `cli_reset_hard_soft_and_force` | Reset modes, force clobber warnings |
| `cli_materialize_refuses_dirty_tree_and_force_overrides` | Merge, checkout, revert refuse dirty tree; `--force` clobber warnings |
| `merge_force_on_dirty_overlapping_path_applies_committed_plan` | Merge `--force` on dirty path in merge plan uses committed three-way result (unit, `src/store/repo.rs`) |
| `merge_refuses_dirty_tree_when_merge_is_clean` | Merge refuses dirty tree before materialize (unit, `src/store/repo.rs`) |
| `revert_noop_with_dirty_working_tree_skips_materialize_guard` | No-op revert succeeds with dirty tree, no guard (unit, `src/store/repo.rs`) |
| `cli_status_clean_tree_summary` | Clean-tree status summary line |
| `cli_revert_and_dry_run` | Revert conflict and success paths |
| `cli_revert_of_revert_restores_content` | Revert then revert the revert commit (parent state reuse) |
| `resolve_remote_ref_for_diff_merge_base_and_checkout` | Remote-tracking ref resolution |
| `cli_reports_repository_lock_contention` | External lock held: CLI fails fast naming `repo.lock` |
| `concurrent_repo_lock_fails_fast_with_actionable_error` | Concurrent commit blocked; succeeds after lock release (unit, `src/store/repo.rs`) |
| `sequential_acquire_after_release_on_same_thread` | Back-to-back lock acquire on same thread (unit, `src/store/lock.rs`) |
| `stray_temp_file_cleaned_on_next_locked_command` | Crash leftover `.astvcs-tmp` cleaned on next command (unit) |
| `merge_conflict_still_leaves_refs_and_disk_unchanged_under_lock` | Merge conflict rollback with locking (unit) |
| `go_sum_and_ps1_status_are_quiet` | Known text-only paths on scan |
| `parse_fallback_status_annotation` | Broken `.rs` shows ` (text fallback)` in status |
| `parse_fallback_diff_annotation` | Diff banner and `parse mode:` intent for fallback |
| `parse_fallback_md_commit_stays_silent` | `.md` commit emits no warnings |
| `parse_fallback_broken_rs_stderr_warning` | Broken `.rs` commit warns on stderr |
| `parse_fallback_verbose_notice_detail` | `-v` adds text fallback `notice:` on commit |
| `gc_no_unreachable_is_noop`, `gc_preserves_remote_tracking_blobs`, `gc_twice_is_idempotent`, `gc_preserves_packed_blobs`, `gc_prune_history_idempotent`, `gc_preserves_unreachable_states_until_prune_history`, `gc_prune_history_does_not_remove_reachable_states`, `fsck_clean_repository`, `fsck_clean_after_repack`, `fsck_repair_fixes_index_inconsistency`, `fsck_repair_refuses_ambiguous_head`, `fsck_prune_refs_removes_dangling_ref`, `fsck_warns_on_unknown_format_version`, `legacy_repo_without_format_version_migrates_on_lock`, `format_version_zero_migrates_idempotently` | Reachability GC, prune-history, repack, fsck, and format migration unit tests (`store/integrity.rs`, `store/format.rs`) |
| `cli_fsck_clean_repository`, `cli_fsck_detects_corruption`, `cli_fsck_repair_fixes_index_inconsistency`, `cli_fsck_repair_refuses_ambiguous_head`, `cli_fsck_repair_leaves_missing_blob`, `cli_fsck_prune_refs_removes_dangling_ref`, `cli_fsck_warns_on_unknown_format_version` | fsck clean, corruption, repair, prune-refs, unknown format version |
| `format_version_migrates_on_open_and_lock` | Legacy `format_version: 0` migrates on first locked command |
| `cli_gc_dry_run_and_prune` | gc dry-run reports blobs and history; `--prune` removes unreachable blobs |
| `repack_roundtrip_and_fsck` | repack loose blobs; fsck clean; working tree unchanged |
| `gc_preserves_packed_blobs` | gc `--prune` keeps reachable packed blobs |
| `repack_fetch_push_roundtrip` | fetch/push/clone after upstream repack |
| `cli_gc_and_fsck_fail_under_external_lock` | gc/fsck lock contention |
| `path_rename_status_and_diff_integration` | Path rename in status (`R old -> new`) and diff (`RenamePath` intent) |
| `path_rename_merges_with_independent_content_edit` | Rename on one branch + edit on other merges at renamed path (unit) |
| `path_rename_conflicts_with_independent_add_at_destination` | Rename vs independent add at destination conflicts (unit) |
| `conflicting_path_renames_report_conflict` | Both branches rename same path to different destinations (unit) |
| `move_subtree_and_sibling_payload_edit_merge` | Move + payload edit merge cleanly (unit) |
| `moved_function_reports_move_not_delete_insert` | Intra-file reposition avoids delete+insert (unit) |
| `commit_without_identity_fails_with_actionable_error` | Commit without identity configured |
| `identity_set_and_read_roundtrip_via_repo_open` | Repository identity config round-trip |
| `identity_recorded_on_commit_merge_and_revert` | Author metadata on commit, merge, and revert states |
| `identity_does_not_change_content_addressed_state_id` | Manifest-only state ids unchanged by identity |
| `structured_errors_match_plain_messages_and_kinds` | `RepoError.kind`, `--json` stderr, plain string parity |
| `incremental_status_reuses_unchanged_file_reads` | Incremental scan skips unchanged content reads; touched file alone re-read (unit) |
| `incremental_scan_reuses_unchanged_paths` | Incremental walk reuses cached path stats (unit) |
