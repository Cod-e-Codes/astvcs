# Release notes template

Copy this outline when drafting a GitHub Release for a new tag. Project overview and install summary: [README.md](../README.md). Git driver setup: [git-integration.md](git-integration.md).

## Version

`v0.1.2` (matches `Cargo.toml` `version` and `astvcs --version`)

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

Verify: `astvcs --version` should print `0.1.2`.

## Changelog

### v0.1.2

- On structural merge-driver conflict, write standard `<<<<<<<` / `=======` / `>>>>>>>` markers into `%A` for text/AST paths (optional `%L` marker size)
- Document the conflict-marker contract accurately in [git-integration.md](git-integration.md)
- Binary conflicts still leave `%A` unchanged and exit nonzero

### v0.1.1

- Add `astvcs-merge-driver` and `astvcs-diff-driver` for optional Git merge and external-diff wiring (see [git-integration.md](git-integration.md))
- Release archives now include all three binaries (Linux x86_64 and Windows x86_64)
- Drivers call the existing `merge_files` / `diff_graphs` paths; they do not read or write `.astvcs/`
- Same-kind insertions at one site (for example both sides appending different functions at EOF) still conflict under the node-level overlap rules; that is unchanged from the standalone merge engine
- Note: `v0.1.1` left `%A` unchanged on structural conflict and claimed Git had already written markers; use `v0.1.2` for marker files in the working tree
