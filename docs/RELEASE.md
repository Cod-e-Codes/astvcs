# Release notes template

Copy this outline when drafting a GitHub Release for a new tag. Project overview and install summary: [README.md](../README.md). Git driver setup: [git-integration.md](git-integration.md).

## Version

`v0.1.5` (matches `Cargo.toml` `version` and `astvcs --version`)

## Requirements

- **MSRV:** Rust 1.96+ (edition 2024)
- **Build:** A working C toolchain for tree-sitter native dependencies (same as CI and local `cargo build --release`)

## Install

Download the platform archive from [GitHub Releases](https://github.com/Cod-e-Codes/astvcs/releases):

| Platform | Asset |
|----------|-------|
| Linux x86_64 | `astvcs-linux-x86_64.tar.gz` |
| Windows x86_64 | `astvcs-windows-x86_64.zip` |

Each archive contains three binaries: `astvcs`, `astvcs-merge-driver`, and `astvcs-diff-driver` (`.exe` on Windows). The `v0.1.0` archives shipped only the main `astvcs` binary.

Verify: `astvcs --version` should print `0.1.5`.

## Changelog

### v0.1.5

- Fix wide-list alignment: uniquely pair content-addressed NodeIds before structural LCS so several empty-payload Function siblings plus an EOF append no longer invent false overlaps
- Add `examples/go-eof-insert-demo/` and language-merge coverage for that case
### v0.1.4

- Fix same-site insert unparse: apply shared anchor `SetTrivia` once and synthesize separator trivia between multiple inserts at one site (avoids Python `@a@b@x` matmul corruption)
- Strengthen decorator/attribute/annotation merge tests to assert separators and reparsed node counts/names

### v0.1.3

- Narrow same-site insert overlap: distinct substantive sibling inserts (EOF functions, Python decorators, Rust attributes, Java annotations on an already-wrapped target) merge in ours-then-theirs order
- First-time wrapper inserts (Python `decorated_definition`, Java `modifiers`) and competing literal/punctuation inserts at one site still conflict
- Docs and skills updated to match the content-aware overlap rule

### v0.1.2

- On structural merge-driver conflict, write standard `<<<<<<<` / `=======` / `>>>>>>>` markers into `%A` for text/AST paths (optional `%L` marker size)
- Document the conflict-marker contract accurately in [git-integration.md](git-integration.md)
- Binary conflicts still leave `%A` unchanged and exit nonzero

### v0.1.1

- Add `astvcs-merge-driver` and `astvcs-diff-driver` for optional Git merge and external-diff wiring (see [git-integration.md](git-integration.md))
- Release archives now include all three binaries (Linux x86_64 and Windows x86_64)
- Drivers call the existing `merge_files` / `diff_graphs` paths; they do not read or write `.astvcs/`
- Note: `v0.1.1` left `%A` unchanged on structural conflict and claimed Git had already written markers; use `v0.1.2` for marker files in the working tree
