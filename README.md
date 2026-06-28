# astvcs

Version control for a working tree of source files. Where tree-sitter can parse a file, astvcs stores an abstract syntax tree and diffs and merges structural edits. Everything else is stored as UTF-8 text with a line-oriented diff.

The CLI follows familiar names (`init`, `status`, `record`, `branch`, `merge`, `checkout`, `reset`, `revert`, `log`, `fetch`, `push`, `clone`). astvcs is a local-first tool with optional network sync over file paths or HTTP. There is no index staging area and no conflict markers written into files.

## Why structure matters

A line diff often rewrites every line below an insertion. Where tree-sitter parses a file, astvcs diffs structure: it aligns nodes between versions with heuristics and classifies the resulting edits. Prepending a doc comment to `lib.rs` can produce one intent instead of a line cascade:

```
--- lib.rs
+++ lib.rs
intents:
  [0] prepend comment
```

Three-way merge applies both sides when alignment finds disjoint structural edits. Overlapping edits on the same node (for example, two renames of one identifier) are reported as structural conflicts; the repository stays unchanged until you pass `merge --resolve path:ours|theirs` to pick a whole-file side for each conflicted path.

## Quick start

```powershell
cargo build --release

.\target\release\astvcs.exe init
# edit tracked files (AST extensions: see docs/architecture.md) or other UTF-8 text
.\target\release\astvcs.exe status
.\target\release\astvcs.exe record -m "describe the change"
.\target\release\astvcs.exe diff
.\target\release\astvcs.exe log
```

Use `--repo <path>` to target a repository outside the current directory. Pass `-v` / `--verbose` for operational detail (`notice:`) on stderr.

Fixture walkthroughs live under [`examples/`](examples/README.md). Design and CLI reference: [`docs/architecture.md`](docs/architecture.md), [`docs/commands.md`](docs/commands.md). Cursor [Agent Skills](https://cursor.com/docs/skills) for contributors live in [`.cursor/skills/`](.cursor/skills/) (`/astvcs-develop`, `/astvcs-structural-diff-merge`, `/astvcs-add-tree-sitter-language`, `/astvcs-integration-tests`).

## Build

Requires Rust 1.96+ (edition 2024).

```powershell
cargo build --release
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

CI runs the same checks on `ubuntu-latest` and `windows-latest` for every push to `main` and every pull request.

Binary: `target\release\astvcs.exe`

## Scope

| In scope | Out of scope (today) |
|----------|----------------------|
| Content-addressed states and branches | Git interoperability |
| AST diff and three-way merge for supported languages | Interactive conflict resolution in the working tree |
| Network sync (`fetch`, `push`, `clone`, `serve`) over file or HTTP | Hosting service or authentication |
| Per-path merge resolution (`--resolve path:ours\|theirs`) | Conflict markers in files |
| `reset`, `revert`, detached checkout (refs include remote-tracking) | Binary or non-UTF-8 file tracking |
| `.gitignore` / `.astvcsignore` scanning | |

Unsupported extensions and parse failures fall back to text blobs; astvcs prints `warning:` to stderr when that happens.

## License

This project is licensed under the [MIT License](LICENSE).
