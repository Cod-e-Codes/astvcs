# Release notes template

Copy this outline when drafting a GitHub Release for a new tag. Project overview and install summary: [README.md](../README.md). Git driver setup: [git-integration.md](git-integration.md).

## Version

`vX.Y.Z` (matches `Cargo.toml` `version` and `astvcs --version`)

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

Verify: `astvcs --version` should print the release version.

## Changelog

Summarize user-facing changes since the previous tag. When drivers or packaging change, call out:

- Git merge/diff driver binaries and [git-integration.md](git-integration.md) setup
- Archive contents (all three binaries vs main CLI only)
