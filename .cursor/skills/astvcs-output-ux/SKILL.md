---
name: astvcs-output-ux
description: Keeps astvcs CLI and diff viewer output focused, actionable, accessible, and compatible. Use when changing command output, errors, conflict reports, --details, -v, --json, or src/diff/view.
paths:
  - "src/main.rs"
  - "src/intent/**"
  - "src/merge/**"
  - "src/diff/view/**"
  - "src/diff/view.rs"
  - "src/store/error.rs"
metadata:
  project: astvcs
---

# astvcs output UX

## Contract

- Default output shows every semantic change without internal node or state IDs.
- Coalesce repeated formatting-only intents. Never truncate semantic edits.
- `--details` adds node IDs, state IDs, raw mutations, and complete diagnostics.
- `-v` includes the same details plus operational `notice:` lines.
- Keep `RepoError.message`, `Display`, `Deref`, and JSON output compatible. Plain CLI errors may use `RepoError.concise`.
- Focused conflicts name each path, both sides' intents, and the overlap reason. Show exact `--resolve path:ours|theirs` syntax only for commands that support it, and use command-specific side labels. State when examples are omitted.

## Viewer

- Lead with the intent summary and next or previous change controls.
- Expand changed nodes and their ancestors. Represent unrelated branches with count-labelled controls and create their children on request.
- Keep alignment methods, node IDs, mutations, and pipeline data in collapsed details.
- Support `n` and `p` for changes, `j` and `k` for files, visible focus, semantic controls, ARIA labels and state, keyboard help, reduced motion, and a single-column narrow layout.
- Use real alignment data. Do not invent confidence scores.

## Verify

- Add unit and CLI regression coverage for compact and detailed output.
- Generate a large-file viewer case and test the shipped HTML.
- Run `cargo fmt`, all tests, clippy with `-D warnings`, release build, and every demo.
- See [references/sources.md](references/sources.md) for the primary guidance behind this contract.
