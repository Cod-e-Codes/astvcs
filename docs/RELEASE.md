# Release notes template

Copy this outline when drafting a GitHub Release for a new tag. Project overview and install summary: [README.md](../README.md).

## Version

`v0.1.0` (matches `Cargo.toml` `version` and `astvcs --version`)

## Requirements

- **MSRV:** Rust 1.96+ (edition 2024)
- **Build:** A working C toolchain for tree-sitter native dependencies (same as CI and local `cargo build --release`)

## Install

Download the platform archive from [GitHub Releases](https://github.com/Cod-e-Codes/astvcs/releases):

| Platform | Asset |
|----------|-------|
| Linux x86_64 | `astvcs-linux-x86_64.tar.gz` |
| Windows x86_64 | `astvcs-windows-x86_64.zip` |

Verify: `astvcs --version` should print `0.1.0`.

## Changelog

First public release of astvcs: local-first structural version control with tree-sitter AST diff and merge where parsing succeeds, and text or binary fallback otherwise.

- AST diff and three-way merge for supported languages; change-first `diff --view` HTML alignment viewer
- Staging index (`add`, `diff --staged`), branches, lightweight tags, author identity
- `reset`, `revert`, `rebase`, `cherry-pick`, `stash`, `blame`, `bisect` on linear first-parent history
- Remotes over local path, HTTP, HTTPS, or SSH; optional bearer auth; TLS on `serve`; shallow `clone` / `fetch` with `--depth`
- Per-path merge resolution; client hooks under `.astvcs/hooks/` (`--no-verify` on selected commands)
- `gc`, `fsck`, `repack`; symlink and executable modes; path rename detection
- Repository advisory locking; one-way `import-git` snapshot aid (not git-compatible)
