# Integration test catalog

| Test | Focus |
|------|-------|
| `workflow_demo_prepend_and_disjoint_merge` | Prepend comment, no move cascade, disjoint merge |
| `identity_demo_payload_edit_disjoint_merge_and_conflict` | EditPayload, sibling merge, rename conflict |
| `cli_merge_resolve_conflict` | `--resolve path:ours\|theirs` |
| `record_respects_gitignore` | Ignore rules during scan |
| `multi_language_repo_roundtrip` | Mixed AST and text files in one repo |
| `history_walk_and_log_order` | Timeline ordering |
| `blob_deduplication_across_states` | Content-addressed storage |
| `rust_unparse_roundtrip_via_repo` | Parse, record, checkout round-trip |
| `parse_all_supported_languages` | Every supported extension parses |
| `edit_roundtrip_preserves_structure_across_languages` | Parse, trivial edit, unparse, re-parse: structural and textual stability (Rust, Python, JS, JSON, TS, Go) |
| `same_file_demo_disjoint_merge` | Same-file disjoint AST merge (alignment stress test) |
| `branch_merge_with_merge_base` | LCA and branch merge |
| `merge_demo_add_add_and_deletion` | Add/add and deletion cases |
| `merge_demo_deletion_when_other_branch_unchanged` | Modify vs delete |
| `checkout_state_and_empty_record` | Detached HEAD, no-op record |
| `config_files_use_ast_frontend` | TOML/YAML/JSON AST path |
| `merge_conflict_diagnostics_without_side_effects` | Atomic rollback on conflict |
| `transparency_scan_and_parse_notices` | `-v` notice output |
| `notices_suppressed_without_verbose` | Default stderr verbosity |
