# Fixture and test map

| Fixture | Integration test | Exercises |
|---------|------------------|-----------|
| `examples/workflow-demo/` | `workflow_demo_prepend_and_disjoint_merge` | Prepend without move cascade; disjoint file merge |
| `examples/same-file-demo/` | `same_file_demo_disjoint_merge` | Same-file rename + insert merge |
| `examples/merge-demo/` | `merge_demo_add_add_and_deletion`, `merge_demo_deletion_when_other_branch_unchanged` | Add/add, modify vs delete |
| `examples/identity-demo/` | `identity_demo_payload_edit_disjoint_merge_and_conflict` | EditPayload, sibling literal merge, rename conflict |

Reset fixtures before manual CLI walks:

```powershell
.\examples\reset.ps1
```
