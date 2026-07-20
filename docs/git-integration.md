# Using astvcs as a Git merge and diff driver

astvcs's structural three-way merge and AST-aware diff can run inside an
existing Git repository, with no migration. Git keeps handling history,
branches, remotes, and hosting; astvcs only replaces how conflicts are
resolved and how diffs are displayed for the paths you opt in.

This is a different mode of use than the rest of astvcs's documentation
([README.md](../README.md), [commands.md](commands.md), [architecture.md](architecture.md)),
which describes astvcs as a standalone local-first VCS with its own `.astvcs/`
repository. The two modes do not conflict: nothing here touches `.astvcs/`, and
a directory can use the Git integration, the standalone astvcs CLI, both, or
neither.

## What this gets you

**Merge driver (`astvcs-merge-driver`):** when `git merge`, `git rebase`,
`git cherry-pick`, or `git pull` hits a conflict on a configured path, Git
runs astvcs's structural three-way merge first. Edits that touch different
parts of the syntax tree (a renamed parameter here, a changed operator
there, a comment added above a function) merge cleanly even when they land
on the same or adjacent lines and would conflict under Git's line-based
ort/recursive strategies. Edits that genuinely overlap (both sides
change the same literal) fail the driver with a nonzero exit so Git marks
the path unmerged. For text/AST paths the driver overwrites `%A` with a
standard `<<<<<<< ours` / `=======` / `>>>>>>> theirs` file so the working
tree matches normal Git conflict UX. Resolve by editing the markers (or
`git checkout --ours|--theirs -- <path>`), then `git add`.

**Diff driver (`astvcs-diff-driver`):** `git diff`, `git show`, and
`git log -p` on a configured path print astvcs's compact structural edit
intents (rename identifier, insert function, edit literal, and so
on) instead of a raw line diff.

Both binaries are stateless: they take file paths on the command line, do
not read or write `.astvcs/`, `.git/`, or any repository metadata beyond the
files Git hands them, and exit with a status Git understands.

## Install

Build or install `astvcs`, `astvcs-merge-driver`, and `astvcs-diff-driver`
together; they ship from the same crate.

```bash
cargo build --release
```

This produces three binaries in `target/release/`:

- `target/release/astvcs`
- `target/release/astvcs-merge-driver`
- `target/release/astvcs-diff-driver`

Put all three on your `PATH` (or reference them by absolute path in the Git
config below). Prebuilt platform archives on
[GitHub Releases](https://github.com/Cod-e-Codes/astvcs/releases) include
all three binaries starting with `v0.1.1`. The `v0.1.0` archives contain only
the main CLI. Conflict-marker writes on structural failure require `v0.1.2` or
newer (or a current build from `main`).

## Set up the merge driver

1. Register the driver, either per-repository (`.git/config`, not
   committed) or globally (`~/.gitconfig`, applies to every repository on the
   machine):

```bash
git config --global merge.astvcs.name "astvcs structural merge driver"
git config --global merge.astvcs.driver "astvcs-merge-driver %O %A %B %P"
```

`%O`, `%A`, `%B`, and `%P` are Git's placeholders for the common ancestor,
current branch's version, other branch's version, and the file's repository
path, substituted by Git before it runs the command. `%A` is the path Git
expects the merge result written back to. You may also pass `%L` before `%P`
(`astvcs-merge-driver %O %A %B %L %P`) to set the conflict marker length
(default 7).

If the binaries are not on `PATH`, use an absolute path instead. On Windows,
prefer forward slashes (or quote the path) so Git's shell does not treat
backslashes as escapes:

```bash
git config --global merge.astvcs.driver "/path/to/astvcs-merge-driver %O %A %B %P"
# Windows example:
# git config --global merge.astvcs.driver "\"C:/Tools/astvcs-merge-driver.exe\" %O %A %B %P"
```

2. Opt in specific paths by adding a `.gitattributes` file (commit this
   one; it is what makes the setup apply for every contributor who has the
   driver registered, and a harmless no-op for anyone who does not):

```gitattributes
# .gitattributes
*.rs merge=astvcs
*.py merge=astvcs
*.go merge=astvcs
*.ts merge=astvcs
*.tsx merge=astvcs
```

List any extension astvcs has an AST frontend for (see the table in
[architecture.md](architecture.md), "Parsing and storage"). Paths astvcs cannot
parse fall back to a plain text three-way merge inside the driver itself, which
behaves like Git's own line merge, so it is safe to be generous with the
`.gitattributes` patterns.

3. Merge as usual. No new commands: `git merge`, `git rebase`,
   `git cherry-pick`, and `git pull` all invoke registered merge drivers
   automatically for conflicting paths that match a `.gitattributes` pattern.

## Set up the diff driver

1. Register the driver:

```bash
git config --global diff.astvcs.command astvcs-diff-driver
```

2. Opt in the same paths, extending the `.gitattributes` from above:

```gitattributes
*.rs diff=astvcs merge=astvcs
*.py diff=astvcs merge=astvcs
```

3. Diff as usual: `git diff`, `git show <ref>`, and `git log -p` print
   astvcs's structural summary for matched paths.

## Verifying the setup

Confirm Git resolves the drivers on a matched path:

```bash
git check-attr merge diff -- src/lib.rs
```

Expect `merge: astvcs` and `diff: astvcs` (or whichever paths you configured).

To see the merge driver run, create a conflicting pair of branches that edit
different parts of the same function and merge them; `git merge --no-edit`
should succeed where a `git config --unset merge.astvcs.driver` rerun of the
same merge would conflict. Driver notices use the same `trace::notice` path as
the main CLI (`-v`); conflict reports always print on stderr because Git only
surfaces driver stderr on failure or with `-v` on the merge/diff invocation
itself.

## Known limitations

**Add/add and one-sided changes never reach the merge driver.** Git only
invokes a configured merge driver when both branches modified the same
path relative to the merge base and neither side is a clean fast-forward
of the other for that path. Renames, deletions, and new files follow
Git's normal rename-detection and add/add handling first.

**Symlink targets cannot be merge-driver output.** If a structural merge
resolves to a changed symlink target (rare; this generally only happens
if a path's Git attributes are misconfigured across a type change), the
driver reports an error and exits nonzero without rewriting `%A`, rather
than silently mismatching the file type Git expects at `%A`.

**Binary conflicts leave `%A` unchanged.** Text and AST conflicts get
marker files; binary (or symlink) conflicts exit nonzero without writing
`<<<<<<<` markers.

**The diff driver does not implement `git diff --stat` byte/line counts**
the way textconv would; it prints a summary block per file. For scripts
that parse `--stat`/`--numstat` output, keep using Git's default diff
(leave the path's `diff` attribute unset, or override per-invocation with
`git diff --no-ext-diff`).

**Binary and non-UTF-8 files** always fall back to
`binary file - content diff omitted` / whole-file replace on conflict,
matching the standalone astvcs CLI's handling of the same content kinds.

**Same-kind insertions at one site still conflict.** Both sides appending
different functions at end-of-file, or both adding different decorators to
the same method, are treated as overlapping same-intent edits (same class of
case as Mergiraf today). Entity-level matching (Weave-style) is out of scope
for these drivers; they reuse the existing node-level merge engine unchanged.
