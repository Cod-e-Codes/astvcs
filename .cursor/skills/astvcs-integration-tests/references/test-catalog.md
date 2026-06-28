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
| `edit_roundtrip_preserves_structure_across_languages` | Parse, trivial edit, unparse, re-parse: structural stability and text matches edited source (Rust, Python, JS, JSON, TS, Go) |
| `same_file_demo_disjoint_merge` | Same-file disjoint AST merge (alignment stress test) |
| `branch_merge_with_merge_base` | LCA and branch merge |
| `cli_branch_remove_guardrails` | Branch remove: checked-out, last branch, not found, recreate name |
| `merge_demo_add_add_and_deletion` | Add/add and deletion cases |
| `merge_demo_deletion_when_other_branch_unchanged` | Modify vs delete |
| `checkout_state_and_empty_commit` | Detached HEAD, no-op commit |
| `config_files_use_ast_frontend` | TOML/YAML/JSON AST path |
| `merge_conflict_diagnostics_without_side_effects` | Atomic rollback on conflict |
| `transparency_scan_and_parse_notices` | `-v` notice output |
| `notices_suppressed_without_verbose` | Default stderr verbosity |
| `network_file_remote_fetch_push_and_clone` | File remote sync |
| `cli_reset_hard_soft_and_force` | Reset modes, force clobber warnings |
| `cli_revert_and_dry_run` | Revert conflict and success paths |
| `cli_revert_of_revert_restores_content` | Revert then revert the revert commit (parent state reuse) |
| `resolve_remote_ref_for_diff_merge_base_and_checkout` | Remote-tracking ref resolution |
| `go_sum_and_ps1_status_are_quiet` | Known text-only paths on scan |
