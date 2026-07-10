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
| `remove_default_branch_updates_config` | Removing default branch promotes `develop` over `feature` in config |
| `merge_demo_add_add_and_deletion` | Add/add and deletion cases |
| `merge_demo_deletion_when_other_branch_unchanged` | Modify vs delete |
| `checkout_state_and_empty_commit` | Detached HEAD, no-op commit |
| `config_files_use_ast_frontend` | TOML/YAML/JSON AST path |
| `merge_conflict_diagnostics_without_side_effects` | Atomic rollback on conflict |
| `rename_vs_parent_delete_reports_overlap` | Rename vs parent delete overlap report |
| `transparency_scan_and_parse_notices` | `-v` notice output |
| `notices_suppressed_without_verbose` | Default stderr verbosity |
| `network_file_remote_fetch_push_and_clone` | File remote sync |
| `shallow_clone_has_fewer_timeline_entries_than_full_clone` | `--depth` on clone limits timeline entries |
| `merge_base_fails_on_shallow_clone_with_incomplete_history` | Shallow history blocks merge-base and merge |
| `shallow_clone_fetches_fewer_timeline_entries_than_full_clone` | Unit shallow vs full clone (unit, `network/sync.rs`) |
| `merge_base_fails_on_shallow_repo_when_lca_missing` | Shallow merge-base error (unit, `network/sync.rs`) |
| `serve_requires_token_for_mutations`, `serve_read_requires_token_by_default`, `serve_public_read_allows_anonymous_get`, `serve_put_returns_503_when_advisory_lock_held`, `serve_concurrent_reads_during_writes` | HTTP serve bearer auth and concurrent reads (unit, `network/serve.rs`) |
| `http_transport_sends_bearer_token`, `remote_token_roundtrip` | Client bearer token and remotes.json storage (unit, `network/transport.rs`, `network/remote.rs`) |
| `clone_uses_remote_default_branch` | Clone checks out upstream `default_branch` (unit, `network/sync.rs`) |
| `cli_reset_hard_soft_and_force` | Reset modes, force clobber warnings |
| `cli_materialize_refuses_dirty_tree_and_force_overrides` | Merge, checkout, revert refuse dirty tree; `--force` clobber warnings |
| `merge_force_on_dirty_overlapping_path_applies_committed_plan` | Merge `--force` on dirty path in merge plan uses committed three-way result (unit, `src/store/repo.rs`) |
| `merge_refuses_dirty_tree_when_merge_is_clean` | Merge refuses dirty tree before materialize (unit, `src/store/repo.rs`) |
| `revert_noop_with_dirty_working_tree_skips_materialize_guard` | No-op revert succeeds with dirty tree, no guard (unit, `src/store/repo.rs`) |
| `cli_status_clean_tree_summary` | Clean-tree status summary line |
| `cli_revert_and_dry_run` | Revert conflict and success paths |
| `cli_revert_conflict_labels_sides_without_merge_resolution_syntax` | Revert-specific side labels and no unsupported `--resolve` guidance |
| `cli_revert_of_revert_restores_content` | Revert then revert the revert commit (parent state reuse) |
| `resolve_remote_ref_for_diff_merge_base_and_checkout` | Remote-tracking ref resolution |
| `pull_merges_upstream_changes` | `pull` fetches and merges upstream commits |
| `pull_detached_head_requires_branch` | `pull` on detached HEAD requires `--branch` |
| `pull_merge_conflict_after_fetch` | Fetch succeeds; merge conflict leaves local branch unchanged |
| `stash_before_checkout` | `stash push` cleans tree so checkout succeeds without `--force` |
| `stash_pop_restores_files` | `stash pop` restores stashed file content to disk |
| `stash_pop_preserves_unstashed_tracked_files` | `stash pop` leaves tracked files outside the stash manifest on disk |
| `stash_pop_conflict_keeps_entry` | Conflicting `stash pop` aborts and keeps the stash entry |
| `rebase_linear_success` | Feature branch commits replayed onto updated main |
| `rebase_conflict_abort_restores` | Replay conflict then `rebase --abort` restores tip and disk |
| `rebase_conflict_continue` | `--resolve` on `rebase --continue` finishes replay |
| `tag_create_and_list` | `tag create`, `tag list`, `tag remove` |
| `checkout_tag_detached` | `checkout --state <tagname>` detached at tagged state |
| `tag_fetch_push_between_repos` | Tags sync on fetch/push between file remotes |
| `hook_pre_commit_aborts_commit` | `pre-commit` exit 1 aborts commit |
| `hook_commit_msg_edits_message` | `commit-msg` edits message file |
| `hook_nested_astvcs_status_in_pre_commit` | Nested `astvcs status` in `pre-commit` |
| `hook_no_verify_skips_pre_commit` | `--no-verify` skips failing hook |
| `hook_pre_merge_aborts` | `pre-merge` exit 1 aborts merge |
| `cherry_pick_clean_commit` | Cherry-pick feature commit onto diverged main |
| `cherry_pick_conflict_leaves_head_unchanged` | Conflicting cherry-pick aborts without side effects |
| `cherry_pick_from_remote_tracking_ref` | Cherry-pick `origin/feature` after fetch |
| `blame_linear_two_commits` | Line blame attributes edits to correct commits in linear history |
| `blame_reorder_preserves_attribution_for_moved_lines` | Reordered unchanged lines stay attributed to the introducing commit |
| `bisect_linear_four_commits` | Bisect finds first bad commit via script in linear history |
| `cli_version_prints_crate_version` | `astvcs --version` prints `CARGO_PKG_VERSION` (integration) |
| `bisect_run_releases_lock_for_nested_astvcs` | Bisect script runs nested `astvcs status` without lock error |
| `merge_remote_tracking_ref` | `merge origin/main` after remote ref update (unit, `src/store/repo.rs`) |
| `cli_reports_repository_lock_contention` | External lock held: CLI fails fast naming `repo.lock` |
| `concurrent_repo_lock_fails_fast_with_actionable_error` | Concurrent commit blocked; succeeds after lock release (unit, `src/store/repo.rs`) |
| `sequential_acquire_after_release_on_same_thread` | Back-to-back lock acquire on same thread (unit, `src/store/lock.rs`) |
| `stray_temp_file_cleaned_on_next_locked_command` | Crash leftover `.astvcs-tmp` cleaned on next command (unit) |
| `merge_conflict_still_leaves_refs_and_disk_unchanged_under_lock` | Merge conflict rollback with locking (unit) |
| `go_sum_and_ps1_status_are_quiet` | Known text-only paths on scan |
| `parse_fallback_status_annotation` | Broken `.rs` shows ` (text fallback)` in status |
| `parse_fallback_diff_annotation` | Diff banner and `parse mode:` intent for fallback |
| `cli_diff_view_writes_html_with_alignment` | Change-first `diff --view` controls, accessibility hooks, path, intents, and alignment export |
| `cli_diff_view_large_file_keeps_change_first_controls` | Generated large AST file retains lazy unchanged-tree and change navigation controls |
| `viewer_javascript_indexes_all_file_modes_and_targets_insertions` | Executed viewer JavaScript indexes AST, text, binary, added, and deleted changes and targets inserted nodes (unit, `src/diff/view.rs`) |
| `cli_diff_defaults_to_compact_intents_and_details_restores_mutations` | Compact default intent output and detailed raw mutation output |
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
| `import_git_snapshot_from_subprocess` | `import-git` reads local git HEAD; file content and log message match |
| `identity_set_and_read_roundtrip_via_repo_open` | Repository identity config round-trip |
| `identity_recorded_on_commit_merge_and_revert` | Author metadata on commit, merge, and revert states |
| `identity_does_not_change_content_addressed_state_id` | Manifest-only state ids unchanged by identity |
| `structured_errors_match_plain_messages_and_kinds` | `RepoError.kind`, `--json` stderr, plain string parity |
| `incremental_status_reuses_unchanged_file_reads` | Incremental scan skips unchanged content reads; touched file alone re-read (unit) |
| `incremental_scan_reuses_unchanged_paths` | Incremental walk reuses cached path stats (unit) |
| `partial_commit_only_stages_paths` | Staged commit snapshots only added paths |
| `status_shows_staged_and_unstaged_columns` | Git-style `MM` / `M ` status columns |
| `merge_refuses_with_staged_changes` | Merge blocked when staging non-empty |
| `checkout_force_with_staged_changes` | Checkout `--force` with staged edits warns |
| `reset_mixed_unstages_and_keeps_disk` | `reset --mixed` clears staging, keeps disk |
| `reset_modes_soft_mixed_hard_comparison` | Soft/mixed/hard reset behavior with dirty tree and staging |
| `reset_mixed_syncs_index_clears_staging_preserves_disk` | Mixed reset unit test (unit, `store/repo.rs`) |
| `cli_commit_empty_staging_errors` | Active staging with no staged paths errors on commit |
| `parse_remote_url_accepts_https` | `https://` remotes accepted by URL parser (unit, `network/transport.rs`) |
| `parse_remote_url_accepts_ssh` | scp-style SSH remotes accepted by URL parser (unit, `network/transport.rs`) |
| `parse_ssh_scheme_url`, `parse_scp_style_url`, `reject_scp_style_without_user` | SSH URL parsing (unit, `network/ssh.rs`) |
| `remote_serve_io_get_config_put_blob_head_404`, `remote_request_requires_token_when_configured` | remote-serve JSON protocol (unit, `network/remote_serve.rs`) |
| `ssh_session_sends_bearer_token` | SSH transport sends bearer token in protocol headers (unit, `network/ssh.rs`) |
| `insecure_client_accepts_self_signed_cert` | HTTPS transport with `--insecure` accepts self-signed serve cert (unit, `network/transport.rs`) |
| `validate_tls_config_requires_both_or_neither` | `--tls-cert` and `--tls-key` must be paired (unit, `network/serve.rs`) |
