---
name: astvcs-add-tree-sitter-language
description: Adds or extends tree-sitter language support in astvcs. Use when adding a file extension, wiring a tree-sitter grammar, editing src/frontend, or when the user mentions a new language, parser, or AST translation for a source file type.
paths:
  - "src/frontend/**"
  - "Cargo.toml"
  - "docs/architecture.md"
metadata:
  project: astvcs
---

# Add tree-sitter language support

## Gather

1. Confirm the extension is not already listed in [docs/architecture.md](../../../docs/architecture.md).
2. Find a maintained `tree-sitter-*` crate on crates.io compatible with `tree-sitter = "0.26.9"`.
3. Read `src/frontend/languages.rs`, `src/frontend/treesitter.rs`, and `supported_extensions()` in `src/lib.rs`.

## Act

### 1. Cargo.toml

Add the `tree-sitter-<lang>` dependency. Pin a crates.io version that builds with the repo's tree-sitter.

### 2. SourceLanguage enum (`languages.rs`)

- Add a variant.
- Map extensions in `from_path` (case-sensitive; use substring after the last `.`).
- For basename-only manifests (for example `go.mod`), match `file_name()` before the extension rule and list the path in `supported_special_paths()`.
- Return the grammar in `tree_sitter_language()`.
- Extend unit tests in the same file for extension detection.

### 3. Translator (`treesitter.rs`)

Wire the grammar into the existing visitor. Reuse `NodeKind` mappings where the AST shape matches an existing language. Add kind mappings only when tree-sitter node types differ. Leading trivia gaps use the previous sibling's rightmost leaf end byte (see `last_leaf_end_byte` in `treesitter.rs`).

### 4. Public API (`lib.rs`)

Add the extension string to `supported_extensions()`, or the basename to `supported_special_paths()` when there is no stable extension.

### 5. Docs

Add the extension and language to the table in [docs/architecture.md](../../../docs/architecture.md).

### 6. Integration test (`tests/integration.rs`)

Add a `(path, source)` sample to `parse_all_supported_languages`. Every extension from `supported_extensions()` and every path from `supported_special_paths()` must have a sample or the test fails.

Example entry:

```rust
("main.rb", "def main\nend\n"),
```

### Extension rules

- `types.d.ts` maps to `.ts`, not a separate extension.
- Unsupported extensions and parse failures fall back to text with `warning:` on stderr.
- Only UTF-8 text paths are tracked.

## Verify

```powershell
cargo test parse_all_supported_languages
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Round-trip check when unparser coverage exists:

```powershell
cargo test rust_unparse_roundtrip go_unparse_roundtrip
```
