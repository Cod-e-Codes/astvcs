# Fixture and test map

| Fixture | Integration test | Exercises |
|---------|------------------|-----------|
| `examples/workflow-demo/` | `workflow_demo_prepend_and_disjoint_merge` | Staging (`add .`); prepend without move cascade; disjoint file merge |
| `examples/same-file-demo/` | `same_file_demo_disjoint_merge` | Same-file rename + insert merge |
| `examples/merge-demo/` | `merge_demo_add_add_and_deletion`, `merge_demo_deletion_when_other_branch_unchanged` | Add/add, modify vs delete |
| `examples/identity-demo/` | `identity_demo_payload_edit_disjoint_merge_and_conflict` | EditPayload, sibling literal merge, rename conflict |
| `examples/network-demo/` | `network_file_remote_fetch_push_and_clone` | File remote clone, push |
| `examples/lifecycle-demo/` | `rebase_linear_success`, `cherry_pick_clean_commit`, `stash_*`, `tag_create_and_list`, `blame_linear_two_commits` | Rebase, cherry-pick, stash, tags, blame |
| `examples/shallow-demo/` | `shallow_clone_has_fewer_timeline_entries_than_full_clone` | `clone --depth`, `shallow.json` |
| `examples/import-git-demo/` | `import_git_snapshot_from_subprocess` | `import-git` (requires `git` on PATH) |
| `examples/serve-demo/` | `serve_requires_token_for_mutations`, `http_transport_sends_bearer_token` | HTTP serve and bearer clone |

CLI-only scenarios (no fixture directory): staging (`partial_commit_only_stages_paths`), `reset --mixed` (`reset_mixed_unstages_and_keeps_disk`). See [examples/README.md](../../../../examples/README.md).

Reset fixtures before manual CLI walks:

```bash
./examples/reset.sh
```

Run all nine fixture walkthroughs non-interactively:

```bash
./examples/run-demos.sh
```
