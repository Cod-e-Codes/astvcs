# Release notes template

Copy this outline when drafting a GitHub Release for a new tag. Project overview and install summary: [README.md](../README.md).

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

Verify: `astvcs --version` should print `X.Y.Z`.

## Changelog

- <!-- bullet -->
- <!-- bullet -->
