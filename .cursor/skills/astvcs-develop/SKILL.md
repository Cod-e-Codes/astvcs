---
name: astvcs-develop
description: Builds, tests, and validates the astvcs Rust CLI. Use when changing astvcs source, fixing CI, running cargo test or clippy, or when the user asks about project layout, build requirements, or release binaries.
compatibility: Requires Rust 1.96+ (edition 2024), cargo, and a working C toolchain for tree-sitter native deps. CI runs on push/PR via `.github/workflows/ci.yml`.
metadata:
  project: astvcs
---

# Develop astvcs

## Gather

1. Read [docs/architecture.md](../../../docs/architecture.md) for repository model, AST diff, and merge behavior.
2. Read [docs/commands.md](../../../docs/commands.md) for CLI flags and subcommands.
3. Identify which layer changed: `frontend/`, `graph/`, `diff/`, `intent/`, `merge/`, `store/`, `network/`, or CLI (`main.rs`, `store/repo.rs`).

## Act

### Build and verify

Run the verify script after substantive changes:

```powershell
powershell -File .cursor/skills/astvcs-develop/scripts/verify.ps1
```

Or run steps manually:

```powershell
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

### Code change rules

- Minimize scope. Match existing module layout and naming in `src/`.
- `NodeId` is per-snapshot only. Cross-version continuity comes from `diff_graphs` alignment, not persistent ids.
- Parse failures and unsupported extensions fall back to text blobs with `warning:` on stderr; AST-capable text fallback paths also show ` (text fallback)` in `status` and a banner plus `parse mode:` intent in `diff`.
- NUL-containing or non-UTF-8 file content stores as `FileContent::Binary` (byte-for-byte round-trip).
- Operational detail uses `trace::notice` (gated by `-v`); user-facing problems use `trace::warning`.

### Source layout

| Module | Role |
|--------|------|
| `graph/` | `AstGraph`, `NodeId`, `Mutation`, apply_batch |
| `frontend/` | tree-sitter parse, extension map, text fallback |
| `diff/` | structural alignment (LCS), text Myers diff |
| `intent/` | human-readable edit intents, overlap checks |
| `merge/` | three-way merge, conflict detection |
| `store/` | blobs, pack storage (`pack.rs`), timeline, repo CLI backend, manifest metadata (`manifest.rs`, `tracked.rs`, `working.rs`), incremental working-tree scan (`walk.rs`, `scan_cache.rs`); `error.rs`, `identity.rs`, `lock.rs`, `hooks.rs`, `rebase.rs`, `cherry_pick.rs`, `blame.rs`, `bisect.rs`, `atomic.rs`, `format.rs`, `reachability.rs`, `integrity.rs` (`gc`, `fsck`, `repack`) |
| `network/` | remotes, fetch/pull/push/clone, HTTP serve (optional bearer auth) |
| `unparser.rs` | AST back to source text (leading trivia between siblings) |

## Verify

- All tests pass (`cargo test`).
- Clippy is clean with `-D warnings`.
- If CLI behavior changed (including `identity`, `--json`, `reset`, `revert`, `merge`, `rebase`, `cherry-pick`, `blame`, `bisect`, `pull`, `checkout`, `stash`, `tag`, `branch remove`, `branch create`, `default_branch` config sync, `add`, staging index, client hooks (`--no-verify`, `.astvcs/hooks/`), `gc` (`--prune`, `--prune-history`), `fsck` (`--repair`, `--prune-refs`), `repack`, binary file tracking, symlink and executable file modes, materialize dirty-tree guard, remote ref resolution, parse fallback visibility in `status`/`diff`, incremental scan cache / `--full-scan`, on-disk format versioning / migrations, or repository lock errors), update [docs/commands.md](../../../docs/commands.md) and add or extend a test in `tests/integration.rs`; update [docs/architecture.md](../../../docs/architecture.md) when network, locking, atomicity, reachability, gc/fsck/repack (including two-tier history retention and fsck repair), author identity, structured errors, binary blobs, symlink/mode metadata, parse fallback policy, incremental scan cache, on-disk format versioning, staging index, default branch config, lightweight tags, client hooks, rebase, cherry-pick, blame, bisect, or repository model semantics change.
- If merge, revert, or diff semantics changed, update the matching fixture under `examples/` and its row in [examples/README.md](../../../examples/README.md).
- If network sync behavior changed, update [docs/architecture.md](../../../docs/architecture.md) network section and extend `tests/integration.rs` or `src/network/` tests (serve auth, transport bearer token, file remote unchanged).
