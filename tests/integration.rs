use astvcs::diff::diff_graphs;
use astvcs::frontend::parse_source;
use astvcs::graph::Mutation;
use astvcs::store::{FileStatus, Repo, RepoErrorKind, StateId, configured_identity, set_identity};
use astvcs::trace;
use astvcs::unparse;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn astvcs_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_astvcs"))
}

fn run_astvcs(repo: Option<&Path>, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(astvcs_bin());
    if let Some(root) = repo {
        cmd.arg("--repo").arg(root);
    }
    cmd.args(args).output().expect("spawn astvcs")
}

fn assert_astvcs_ok(out: &std::process::Output, step: &str) {
    assert!(
        out.status.success(),
        "{step} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn workflow_demo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/workflow-demo")
}

fn merge_demo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/merge-demo")
}

fn identity_demo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/identity-demo")
}

fn copy_fixture(dir: &TempDir, src: &PathBuf) -> std::io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".astvcs" {
            continue;
        }
        let dest = dir.path().join(name);
        if entry.path().is_dir() {
            fs::create_dir_all(&dest)?;
            for file in fs::read_dir(entry.path())? {
                let file = file?;
                fs::copy(file.path(), dest.join(file.file_name()))?;
            }
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn copy_workflow_demo(dir: &TempDir) -> std::io::Result<()> {
    copy_fixture(dir, &workflow_demo_root())
}

fn copy_merge_demo(dir: &TempDir) -> std::io::Result<()> {
    copy_fixture(dir, &merge_demo_root())
}

fn copy_identity_demo(dir: &TempDir) -> std::io::Result<()> {
    copy_fixture(dir, &identity_demo_root())
}

#[test]
fn cli_version_prints_crate_version() {
    let out = Command::new(astvcs_bin())
        .arg("--version")
        .output()
        .expect("spawn astvcs");
    assert_astvcs_ok(&out, "astvcs --version");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "expected version in stdout: {stdout}"
    );
}

fn create_test_symlink(target: &Path, link: &Path) {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).expect("create symlink");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_file;
        symlink_file(target, link)
            .expect("create symlink (Windows requires Developer Mode or CI symlink enable step)");
    }
}

#[test]
fn workflow_demo_prepend_and_disjoint_merge() {
    let dir = TempDir::new().unwrap();
    copy_workflow_demo(&dir).unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();

    fs::write(dir.path().join("lib.rs"), "pub mod core;\npub mod util;\n").unwrap();
    repo.commit("baseline").unwrap();

    let with_doc = "//! workflow demo crate\npub mod core;\npub mod util;\n";
    fs::write(dir.path().join("lib.rs"), with_doc).unwrap();
    let head = repo.head_state().unwrap();
    let base_files = repo.load_state_files(&head).unwrap();
    let old_graph = match &base_files["lib.rs"].content {
        astvcs::FileContent::Ast(g) => g,
        _ => panic!("expected ast"),
    };
    let new_graph = parse_source("lib.rs", with_doc).unwrap();
    let diff = diff_graphs(old_graph, &new_graph);
    assert!(
        !diff
            .mutations
            .iter()
            .any(|m| matches!(m, Mutation::MoveNode { .. })),
        "prepend should not cascade moves: {:?}",
        diff.mutations
    );
    repo.commit("prepend doc comment").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("util.rs"),
        "pub fn label() -> &'static str {\n    \"feature-branch\"\n}\n",
    )
    .unwrap();
    repo.commit("feature util label").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(
        dir.path().join("core.rs"),
        "pub fn answer() -> i32 {\n    43\n}\n",
    )
    .unwrap();
    repo.commit("main core answer").unwrap();

    let merged = repo
        .merge_branch("feature", "merge feature into main")
        .unwrap();
    assert!(
        repo.working_tree_is_clean().unwrap(),
        "working tree dirty after merge"
    );

    let util_disk = fs::read_to_string(dir.path().join("util.rs")).unwrap();
    assert!(
        util_disk.contains("feature-branch"),
        "util.rs on disk after merge: {util_disk}"
    );

    let files = repo.load_state_files(&merged).unwrap();
    let lib_text = match &files["lib.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    assert!(lib_text.contains("workflow demo crate"));
    let core_text = match &files["core.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    assert!(core_text.contains('3'));
    let util_text = match &files["util.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    assert!(util_text.contains("feature-branch"));
}

#[test]
fn identity_demo_payload_edit_disjoint_merge_and_conflict() {
    let dir = TempDir::new().unwrap();
    copy_identity_demo(&dir).unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();

    fs::write(
        dir.path().join("core.rs"),
        "pub fn answer() -> i32 {\n    42\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("labels.rs"),
        "pub fn pair() -> (&'static str, &'static str) {\n    (\"alpha\", \"beta\")\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("conflict.rs"),
        "fn sample() {\n    let value = 1;\n}\n",
    )
    .unwrap();
    repo.commit("baseline").unwrap();

    let head = repo.head_state().unwrap();
    let base_files = repo.load_state_files(&head).unwrap();
    let old_core = match &base_files["core.rs"].content {
        astvcs::FileContent::Ast(g) => g,
        _ => panic!("expected ast"),
    };
    let new_core = parse_source("core.rs", "pub fn answer() -> i32 {\n    43\n}\n").unwrap();
    let core_diff = diff_graphs(old_core, &new_core);
    assert!(
        core_diff
            .mutations
            .iter()
            .any(|m| matches!(m, Mutation::EditPayload { .. })),
        "literal change should be EditPayload: {:?}",
        core_diff.mutations
    );

    fs::write(
        dir.path().join("core.rs"),
        "pub fn answer() -> i32 {\n    43\n}\n",
    )
    .unwrap();
    repo.commit("edit literal on main").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("labels.rs"),
        "pub fn pair() -> (&'static str, &'static str) {\n    (\"alpha\", \"BETA\")\n}\n",
    )
    .unwrap();
    repo.commit("edit second string literal").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(
        dir.path().join("labels.rs"),
        "pub fn pair() -> (&'static str, &'static str) {\n    (\"ALPHA\", \"beta\")\n}\n",
    )
    .unwrap();
    repo.commit("edit first string literal").unwrap();

    let merged = repo
        .merge_branch("feature", "merge sibling literal edits")
        .unwrap();
    let files = repo.load_state_files(&merged).unwrap();
    let labels = match &files["labels.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    assert!(labels.contains("ALPHA"), "merged labels: {labels}");
    assert!(labels.contains("BETA"), "merged labels: {labels}");

    repo.create_branch("conflict", None).unwrap();
    repo.checkout_branch("conflict").unwrap();
    fs::write(
        dir.path().join("conflict.rs"),
        "fn sample() {\n    let renamed = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename to renamed").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(
        dir.path().join("conflict.rs"),
        "fn sample() {\n    let alternate = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename to alternate").unwrap();

    let plan = repo.plan_merge("conflict").unwrap();
    assert!(!plan.is_clean());
    let report = plan.format_conflicts();
    assert!(report.contains("intents from base"), "{report}");
    assert!(report.contains("rename"), "{report}");
}

#[test]
fn cli_merge_resolve_conflict() {
    let dir = TempDir::new().unwrap();
    copy_identity_demo(&dir).unwrap();
    let root = dir.path();
    fs::write(
        root.join("conflict.rs"),
        "fn sample() {\n    let value = 1;\n}\n",
    )
    .unwrap();

    assert!(
        run_astvcs(None, &["init", root.to_str().unwrap()])
            .status
            .success()
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(root),
            &[
                "identity",
                "set",
                "--name",
                "Test User",
                "--email",
                "test@example.com",
            ],
        ),
        "identity set",
    );
    assert!(
        run_astvcs(Some(root), &["commit", "-m", "baseline"])
            .status
            .success()
    );
    assert!(
        run_astvcs(Some(root), &["branch", "create", "conflict"])
            .status
            .success()
    );
    assert!(
        run_astvcs(Some(root), &["checkout", "--branch", "conflict"])
            .status
            .success()
    );
    fs::write(
        root.join("conflict.rs"),
        "fn sample() {\n    let renamed = 1;\n}\n",
    )
    .unwrap();
    assert!(
        run_astvcs(Some(root), &["commit", "-m", "rename to renamed"])
            .status
            .success()
    );
    assert!(
        run_astvcs(Some(root), &["checkout", "--branch", "main"])
            .status
            .success()
    );
    fs::write(
        root.join("conflict.rs"),
        "fn sample() {\n    let alternate = 1;\n}\n",
    )
    .unwrap();
    assert!(
        run_astvcs(Some(root), &["commit", "-m", "rename to alternate"])
            .status
            .success()
    );

    let dry = run_astvcs(Some(root), &["merge", "conflict", "--dry-run"]);
    assert!(!dry.status.success());
    let focused = String::from_utf8_lossy(&dry.stdout);
    assert!(focused.contains("conflict: conflict.rs"), "{focused}");
    assert!(focused.contains("ours: rename"), "{focused}");
    assert!(focused.contains("theirs: rename"), "{focused}");
    assert!(focused.contains("--resolve conflict.rs:ours"), "{focused}");
    assert!(!focused.contains("mutations from base"), "{focused}");
    assert!(!focused.contains("base:"), "{focused}");

    let detailed = run_astvcs(Some(root), &["--details", "merge", "conflict", "--dry-run"]);
    assert!(!detailed.status.success());
    let detailed_stdout = String::from_utf8_lossy(&detailed.stdout);
    assert!(detailed_stdout.contains("base:"), "{detailed_stdout}");
    assert!(
        detailed_stdout.contains("mutations from base"),
        "{detailed_stdout}"
    );

    let head_before = Repo::open(root).unwrap().head_state().unwrap();
    let merge = run_astvcs(
        Some(root),
        &[
            "merge",
            "conflict",
            "-m",
            "resolved via cli",
            "--resolve",
            "conflict.rs:theirs",
        ],
    );
    assert!(
        merge.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    assert_ne!(Repo::open(root).unwrap().head_state().unwrap(), head_before);

    let disk = fs::read_to_string(root.join("conflict.rs")).unwrap();
    assert!(
        disk.contains("renamed"),
        "conflict.rs should keep theirs: {disk}"
    );
    assert!(
        !disk.contains("alternate"),
        "conflict.rs should not keep ours: {disk}"
    );
}

#[test]
fn commit_respects_gitignore() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join(".gitignore"), "build/\nsecret.txt\n").unwrap();
    fs::create_dir_all(dir.path().join("build")).unwrap();
    fs::write(dir.path().join("build").join("out.rs"), "fn out() {}\n").unwrap();
    fs::write(dir.path().join("secret.txt"), "hidden\n").unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

    let id = repo.commit("init").unwrap().state_id;
    let files = repo.load_state_files(&id).unwrap();
    assert!(files.contains_key("main.rs"));
    assert!(!files.contains_key("build/out.rs"));
    assert!(!files.contains_key("secret.txt"));
}

#[test]
fn multi_language_repo_roundtrip() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("app.py"), "def main():\n    pass\n").unwrap();
    fs::write(dir.path().join("data.json"), "{\"a\": 1}\n").unwrap();
    fs::write(dir.path().join("app.ts"), "function main(): void {}\n").unwrap();
    fs::write(
        dir.path().join("view.tsx"),
        "export function View() { return null; }\n",
    )
    .unwrap();
    fs::write(dir.path().join("main.cpp"), "int main() { return 0; }\n").unwrap();
    fs::write(
        dir.path().join("Main.java"),
        "class Main { public static void main(String[] args) {} }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("Program.cs"),
        "class Program { static void Main() {} }\n",
    )
    .unwrap();
    fs::write(dir.path().join("main.swift"), "func main() {}\n").unwrap();
    fs::write(dir.path().join("main.kt"), "fun main() {}\n").unwrap();
    fs::write(dir.path().join("build.kts"), "fun main() {}\n").unwrap();
    fs::write(dir.path().join("main.zig"), "pub fn main() void {}\n").unwrap();
    fs::write(dir.path().join("query.sql"), "SELECT 1;\n").unwrap();
    fs::write(dir.path().join("script.sh"), "#!/bin/sh\necho hi\n").unwrap();
    fs::write(dir.path().join("script.bash"), "echo hi\n").unwrap();
    fs::write(dir.path().join("notes.txt"), "plain text\n").unwrap();
    let id = repo.commit("multi-lang").unwrap().state_id;
    let files = repo.load_state_files(&id).unwrap();
    assert_eq!(files.len(), 16);
    for path in [
        "main.rs",
        "app.py",
        "data.json",
        "app.ts",
        "view.tsx",
        "main.cpp",
        "Main.java",
        "Program.cs",
        "main.swift",
        "main.kt",
        "build.kts",
        "main.zig",
        "query.sql",
        "script.sh",
        "script.bash",
    ] {
        assert!(files[path].content.is_ast(), "{path} should be AST");
    }
    assert!(!files["notes.txt"].content.is_ast());
}

#[test]
fn history_walk_and_log_order() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("first").unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
    repo.commit("second").unwrap();
    let history = repo.history(10).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].message, "second");
    assert_eq!(history[1].message, "first");
}

#[test]
fn blob_deduplication_across_states() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let id1 = repo.commit("v1").unwrap().state_id;
    fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
    let id2 = repo.commit("v2").unwrap().state_id;
    let m1 = repo.load_manifest(&id1).unwrap();
    let m2 = repo.load_manifest(&id2).unwrap();
    assert_eq!(m1["main.rs"].blob, m2["main.rs"].blob);
}

#[test]
fn go_unparse_roundtrip_via_repo() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    let src = "package main\n\nimport \"fmt\"\n\nfunc greet(name string) string {\n    return fmt.Sprintf(\"Hi, %s!\", name)\n}\n\nfunc main() {\n    fmt.Println(greet(\"world\"))\n}\n";
    fs::write(dir.path().join("hello.go"), src).unwrap();
    let id = repo.commit("hello").unwrap().state_id;
    let files = repo.load_state_files(&id).unwrap();
    if let astvcs::FileContent::Ast(graph) = &files["hello.go"].content {
        assert_eq!(unparse(graph).as_bytes(), src.as_bytes());
    } else {
        panic!("expected ast");
    }
    repo.checkout_branch("main").unwrap();
    let disk = fs::read_to_string(dir.path().join("hello.go")).unwrap();
    assert_eq!(normalize_newlines(&disk), normalize_newlines(src));
}

#[test]
fn rust_unparse_roundtrip_via_repo() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    fs::write(dir.path().join("lib.rs"), src).unwrap();
    let id = repo.commit("add fn").unwrap().state_id;
    let files = repo.load_state_files(&id).unwrap();
    if let astvcs::FileContent::Ast(graph) = &files["lib.rs"].content {
        assert_eq!(unparse(graph).as_bytes(), src.as_bytes());
    } else {
        panic!("expected ast");
    }
}

#[test]
fn parse_all_supported_languages() {
    let samples: &[(&str, &str)] = &[
        ("main.rs", "fn main() {}\n"),
        ("app.py", "def main():\n    pass\n"),
        ("app.pyw", "def main():\n    pass\n"),
        ("index.js", "function main() {}\n"),
        ("index.mjs", "export function main() {}\n"),
        ("index.cjs", "module.exports = {};\n"),
        ("main.go", "package main\nfunc main() {}\n"),
        ("go.mod", "module example.com/demo\n\ngo 1.22\n"),
        ("main.c", "int main() { return 0; }\n"),
        ("main.h", "int x;\n"),
        ("data.json", "{\"k\": 1}\n"),
        (
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        ),
        ("config.yaml", "key: value\n"),
        ("config.yml", "key: value\n"),
        ("app.ts", "function main(): void {}\n"),
        ("types.d.ts", "declare const x: number;\n"),
        ("view.tsx", "export function View() { return null; }\n"),
        ("main.cpp", "int main() { return 0; }\n"),
        ("util.cc", "int main() { return 0; }\n"),
        ("util.cxx", "int main() { return 0; }\n"),
        ("util.hpp", "int main() { return 0; }\n"),
        ("util.hh", "int main() { return 0; }\n"),
        (
            "Main.java",
            "class Main { public static void main(String[] args) {} }\n",
        ),
        ("Program.cs", "class Program { static void Main() {} }\n"),
        ("main.swift", "func main() {}\n"),
        ("main.kt", "fun main() {}\n"),
        ("build.kts", "fun main() {}\n"),
        ("main.zig", "pub fn main() void {}\n"),
        ("query.sql", "SELECT 1;\n"),
        ("script.sh", "#!/bin/sh\necho hi\n"),
        ("script.bash", "echo hi\n"),
        (
            "index.html",
            "<!DOCTYPE html><html><body><p>1</p></body></html>\n",
        ),
        ("page.htm", "<html><body>ok</body></html>\n"),
        ("style.css", "body { color: red; }\n"),
    ];
    let mut covered = std::collections::HashSet::new();
    for (path, src) in samples {
        let graph = parse_source(path, src).expect(path);
        graph.validate().expect(path);
        covered.insert(path.rsplit('.').next().unwrap());
    }
    for ext in astvcs::supported_extensions() {
        assert!(
            covered.contains(ext),
            "parse sample missing extension: {ext}"
        );
    }
    for path in astvcs::supported_special_paths() {
        let ext = path.rsplit('.').next().unwrap_or(path);
        assert!(
            covered.contains(ext),
            "parse sample missing special path: {path}"
        );
    }
}

#[test]
fn same_file_demo_disjoint_merge() {
    let dir = TempDir::new().unwrap();
    copy_fixture(
        &dir,
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/same-file-demo"),
    )
    .unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("sample.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    repo.commit("baseline").unwrap();
    repo.create_branch("feature", None).unwrap();

    fs::write(
        dir.path().join("sample.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename on main").unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("sample.rs"),
        "fn foo() {\n    let x = 1;\n    let z = 2;\n}\n",
    )
    .unwrap();
    repo.commit("insert on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    let merged = repo.merge_branch("feature", "merge feature").unwrap();
    let files = repo.load_state_files(&merged).unwrap();
    let text = match &files["sample.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    let expected = "fn foo() {\n    let y = 1;\n    let z = 2;\n}\n";
    assert_eq!(
        normalize_newlines(&text),
        expected,
        "merged text formatting"
    );
    let disk = fs::read_to_string(dir.path().join("sample.rs")).unwrap();
    assert_eq!(
        normalize_newlines(&disk),
        expected,
        "disk sample.rs formatting"
    );
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Parse `before`, diff to `after`, apply mutations, unparse, re-parse, and assert stability.
fn assert_edit_roundtrip(path: &str, before: &str, after: &str) {
    let base = parse_source(path, before).unwrap_or_else(|e| panic!("{path}: parse before: {e}"));
    base.validate()
        .unwrap_or_else(|e| panic!("{path}: validate before: {e}"));

    let target = parse_source(path, after).unwrap_or_else(|e| panic!("{path}: parse after: {e}"));
    target
        .validate()
        .unwrap_or_else(|e| panic!("{path}: validate after: {e}"));

    let diff = diff_graphs(&base, &target);
    assert!(
        !diff.mutations.is_empty(),
        "{path}: expected a non-empty edit from before to after"
    );

    let mut applied = base;
    applied
        .apply_batch(&diff.mutations)
        .unwrap_or_else(|e| panic!("{path}: apply edit: {e}"));
    applied
        .validate()
        .unwrap_or_else(|e| panic!("{path}: validate applied: {e}"));

    let text = unparse(&applied);
    let reparsed = parse_source(path, &text).unwrap_or_else(|e| panic!("{path}: re-parse: {e}"));
    reparsed
        .validate()
        .unwrap_or_else(|e| panic!("{path}: validate reparsed: {e}"));

    let structural = diff_graphs(&target, &reparsed);
    assert!(
        structural.mutations.is_empty(),
        "{path}: structural drift after roundtrip: {:?}",
        structural.mutations
    );

    let text_after_reparse = unparse(&reparsed);
    assert_eq!(
        normalize_newlines(&text),
        normalize_newlines(&text_after_reparse),
        "{path}: textual drift across re-parse"
    );
    assert_eq!(
        normalize_newlines(&text),
        normalize_newlines(after),
        "{path}: roundtrip text should match edited source"
    );
}

#[test]
fn edit_roundtrip_preserves_structure_across_languages() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "main.rs",
            "fn foo() {\n    let x = 1;\n}\n",
            "fn foo() {\n    let x = 2;\n}\n",
        ),
        (
            "app.py",
            "def foo():\n    x = 1\n    return x\n",
            "def foo():\n    x = 2\n    return x\n",
        ),
        (
            "index.js",
            "function foo() {\n    return 1;\n}\n",
            "function foo() {\n    return 2;\n}\n",
        ),
        ("data.json", "{\"count\": 1}\n", "{\"count\": 2}\n"),
        (
            "app.ts",
            "function foo(): number {\n    return 1;\n}\n",
            "function foo(): number {\n    return 2;\n}\n",
        ),
        (
            "main.go",
            "package main\n\nimport \"fmt\"\n\nfunc foo() string {\n    return fmt.Sprintf(\"%d\", 1)\n}\n",
            "package main\n\nimport \"fmt\"\n\nfunc foo() string {\n    return fmt.Sprintf(\"%d\", 2)\n}\n",
        ),
        (
            "index.html",
            "<!DOCTYPE html><html><body><p>1</p></body></html>\n",
            "<!DOCTYPE html><html><body><p>2</p></body></html>\n",
        ),
        (
            "style.css",
            "body { color: red; }\n",
            "body { color: blue; }\n",
        ),
    ];
    for (path, before, after) in cases {
        assert_edit_roundtrip(path, before, after);
    }
}

#[test]
fn branch_merge_with_merge_base() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename on main").unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n    let z = 2;\n}\n",
    )
    .unwrap();
    repo.commit("insert on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    let merged = repo.merge_branch("feature", "merge feature").unwrap();
    let files = repo.load_state_files(&merged).unwrap();
    let text = match &files["main.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    let expected = "fn foo() {\n    let y = 1;\n    let z = 2;\n}\n";
    assert_eq!(text, expected);
}

#[test]
fn merge_demo_add_add_and_deletion() {
    let dir = TempDir::new().unwrap();
    copy_merge_demo(&dir).unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();

    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "pub fn label() -> &'static str { \"base\" }\n",
    )
    .unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    fs::write(dir.path().join("util.rs"), "pub fn util() {}\n").unwrap();
    repo.commit("main adds util.rs").unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("util.rs"), "pub fn util() {}\n").unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "pub fn label() -> &'static str { \"feature\" }\n",
    )
    .unwrap();
    repo.commit("feature adds util and edits lib").unwrap();

    repo.checkout_branch("main").unwrap();
    let merged = repo.merge_branch("feature", "merge add/add").unwrap();
    let files = repo.load_state_files(&merged).unwrap();
    assert!(files.contains_key("util.rs"));
    let lib = match &files["lib.rs"].content {
        astvcs::FileContent::Ast(g) => unparse(g),
        _ => panic!("expected ast"),
    };
    assert!(
        lib.contains("feature"),
        "lib.rs should keep feature edit: {lib}"
    );
}

#[test]
fn merge_demo_deletion_when_other_branch_unchanged() {
    let dir = TempDir::new().unwrap();
    copy_merge_demo(&dir).unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();

    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.path().join("lib.rs"),
        "pub fn label() -> &'static str { \"base\" }\n",
    )
    .unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    fs::remove_file(dir.path().join("lib.rs")).unwrap();
    repo.commit("main deletes lib.rs").unwrap();

    repo.checkout_branch("feature").unwrap();
    repo.commit("feature noop").unwrap();

    repo.checkout_branch("main").unwrap();
    let merged = repo.merge_branch("feature", "merge deletion").unwrap();
    let files = repo.load_state_files(&merged).unwrap();
    assert!(!files.contains_key("lib.rs"));
    assert!(!dir.path().join("lib.rs").exists());
}

#[test]
fn checkout_state_and_empty_commit() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let v1 = repo.commit("v1").unwrap().state_id;
    fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
    repo.commit("v2").unwrap();

    repo.checkout_state(&v1).unwrap();
    assert!(repo.is_detached().unwrap());
    assert!(repo.working_tree_is_clean().unwrap());

    let again = repo.commit("noop").unwrap();
    assert!(!again.created);
    assert_eq!(again.state_id, v1);
    let entry = repo.load_timeline_entry(&v1).unwrap();
    assert_eq!(entry.message, "v1");
}

#[test]
fn config_files_use_ast_frontend() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("config.yaml"), "key: value\nlist:\n  - a\n").unwrap();
    let id = repo.commit("config").unwrap().state_id;
    let files = repo.load_state_files(&id).unwrap();
    assert!(files["Cargo.toml"].content.is_ast());
    assert!(files["config.yaml"].content.is_ast());
}

#[test]
fn merge_conflict_diagnostics_without_side_effects() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename to y on main").unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let z = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename to z on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    let head_before = repo.head_state().unwrap();
    let base = repo.merge_base_refs("main", "feature").unwrap();
    let main_id = repo.head_state().unwrap();
    let feature_id = repo.branch_state("feature").unwrap();
    let three_way = repo
        .diff_three_way(&base, &main_id, &feature_id, Some("main.rs"))
        .unwrap();
    assert!(three_way.contains("base -> left:"), "{three_way}");
    assert!(three_way.contains("base -> right:"), "{three_way}");

    let plan = repo.plan_merge("feature").unwrap();
    assert!(!plan.is_clean());
    let report = plan.format_conflicts();
    assert!(report.contains("overlapping edit pairs"), "{report}");
    assert!(report.contains("intents from base"), "{report}");
    assert!(report.contains("rename"), "{report}");

    let err = repo.merge_branch("feature", "try merge").unwrap_err();
    assert!(err.contains("overlapping edit pairs"), "{err}");
    assert_eq!(repo.head_state().unwrap(), head_before);
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn rename_vs_parent_delete_reports_overlap() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n    let z = 2;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n    let z = 2;\n}\n",
    )
    .unwrap();
    repo.commit("rename binding on main").unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let z = 2;\n}\n",
    )
    .unwrap();
    repo.commit("delete first statement on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    let head_before = repo.head_state().unwrap();
    let plan = repo.plan_merge("feature").unwrap();
    assert!(!plan.is_clean());
    let report = plan.format_conflicts();
    assert!(report.contains("overlapping edit pairs"), "{report}");
    assert!(report.contains("delete"), "{report}");
    assert!(report.contains("rename"), "{report}");
    assert!(
        report.contains("covers edit"),
        "expected ancestry overlap detail: {report}"
    );

    let err = repo
        .merge_branch("feature", "merge rename vs delete")
        .unwrap_err();
    assert!(err.contains("overlapping edit pairs"), "{err}");
    assert_eq!(repo.head_state().unwrap(), head_before);
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn transparency_scan_and_parse_notices() {
    trace::clear_log();
    trace::set_verbose(true);
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("app.ts"), "function main(): void {}\n").unwrap();
    fs::write(dir.path().join("notes.md"), "# doc\n").unwrap();
    fs::write(dir.path().join("image.png"), [0x89, 0x50, 0x4E, 0x47, 0, 0]).unwrap();

    repo.status().unwrap();
    let log = trace::take_log();
    assert!(log.iter().any(|l| l.contains("image.png")));
    assert!(log.iter().any(|l| l.contains("notes.md")));

    trace::clear_log();
    let outcome = repo.commit("baseline").unwrap();
    assert!(outcome.created);
    let log = trace::take_log();
    assert!(log.iter().any(|l| l.contains("main.rs: parsed as AST")));
    assert!(log.iter().any(|l| l.contains("app.ts: parsed as AST")));
    assert!(log.iter().any(|l| l.contains("notes.md")));
    assert!(
        log.iter()
            .any(|l| l.contains("image.png: stored as binary blob"))
    );

    trace::clear_log();
    let noop = repo.commit("noop").unwrap();
    assert!(!noop.created);
    assert!(trace::take_log().iter().any(|l| l.contains("no changes")));
    trace::set_verbose(false);
}

#[test]
fn notices_suppressed_without_verbose() {
    trace::clear_log();
    trace::set_verbose(false);
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    assert!(
        !trace::take_log().iter().any(|l| l.contains("notice:")),
        "init notices should be gated"
    );

    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("widget.foo"), "data\n").unwrap();
    trace::clear_warned();
    repo.commit("baseline").unwrap();
    let log = trace::take_log();
    assert!(
        log.iter()
            .any(|l| l.contains("warning:") && l.contains("widget.foo")),
        "unexpected extensions should warn: {log:?}"
    );
    assert!(
        !log.iter()
            .any(|l| l.contains("warning:") && l.contains("main.rs")),
        "parsed files should not warn: {log:?}"
    );
    assert!(
        !log.iter().any(|l| l.contains("parsed as AST")),
        "notices should be gated: {log:?}"
    );
}

#[test]
fn network_file_remote_fetch_push_and_clone() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "v1\n").unwrap();
    upstream_repo.commit("v1").unwrap();

    let clone_dir = TempDir::new().unwrap();
    let out = run_astvcs(
        None,
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(clone_dir.path().join("note.txt")).unwrap(),
        "v1\n"
    );

    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &[
                "identity",
                "set",
                "--name",
                "Test User",
                "--email",
                "test@example.com",
            ],
        ),
        "identity set",
    );
    fs::write(clone_dir.path().join("note.txt"), "v2\n").unwrap();
    let out = run_astvcs(Some(clone_dir.path()), &["commit", "-m", "v2"]);
    assert!(out.status.success());
    let out = run_astvcs(
        Some(clone_dir.path()),
        &["push", "origin", "--branch", "main"],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        upstream_repo.head_state().unwrap(),
        Repo::open(clone_dir.path()).unwrap().head_state().unwrap()
    );

    let fsck = run_astvcs(Some(upstream.path()), &["fsck"]);
    assert_astvcs_ok(&fsck, "fsck upstream after push");
    assert!(
        String::from_utf8_lossy(&fsck.stdout).contains("repository ok"),
        "push target should pass fsck: {}",
        String::from_utf8_lossy(&fsck.stdout)
    );
}

#[test]
fn cli_reset_hard_soft_and_force() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    let v1 = repo.commit("v1").unwrap().state_id;
    fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
    repo.commit("v2").unwrap();

    let out = run_astvcs(Some(dir.path()), &["reset", &v1]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("Reset to state {v1}")));
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "v1\n"
    );

    fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
    repo.commit("v2 again").unwrap();
    fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();

    let out = run_astvcs(Some(dir.path()), &["reset", &v1]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("uncommitted changes"));

    let out = run_astvcs(Some(dir.path()), &["reset", &v1, "--soft"]);
    assert!(out.status.success());
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "dirty\n"
    );

    let out = run_astvcs(Some(dir.path()), &["reset", &v1, "--force"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning: reset --force: discarded uncommitted changes in note.txt"));
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "v1\n"
    );
}

#[test]
fn cli_revert_and_dry_run() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("keep.txt"), "stay\n").unwrap();
    fs::write(dir.path().join("notes.txt"), "seed\n").unwrap();
    repo.commit("seed").unwrap();
    fs::remove_file(dir.path().join("notes.txt")).unwrap();
    repo.commit("remove").unwrap();
    fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
    let target = repo.commit("add notes").unwrap().state_id;
    fs::write(dir.path().join("notes.txt"), "added later\n").unwrap();
    let tip = repo.commit("modify notes").unwrap().state_id;

    let out = run_astvcs(
        Some(dir.path()),
        &["revert", &target, "-m", "revert add", "--dry-run"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("path modified after the reverted state")
    );
    assert_eq!(repo.head_state().unwrap(), tip);

    fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
    repo.reset(&target, true, false, false).unwrap();
    assert_eq!(repo.head_state().unwrap(), target);
    let out = run_astvcs(Some(dir.path()), &["revert", &target, "-m", "revert add"]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.path().join("notes.txt").exists());
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn cli_revert_conflict_labels_sides_without_merge_resolution_syntax() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn sample() {\n    let value = 1;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn sample() {\n    let renamed = 1;\n}\n",
    )
    .unwrap();
    let target = repo.commit("rename value").unwrap().state_id;
    fs::write(
        dir.path().join("main.rs"),
        "fn sample() {\n    let alternate = 1;\n}\n",
    )
    .unwrap();
    repo.commit("rename again").unwrap();

    let out = run_astvcs(
        Some(dir.path()),
        &["revert", &target, "-m", "revert rename", "--dry-run"],
    );
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("reverted parent: rename"), "{stdout}");
    assert!(stdout.contains("current HEAD: rename"), "{stdout}");
    assert!(!stdout.contains("--resolve"), "{stdout}");
}

#[test]
fn cli_revert_of_revert_restores_content() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    let v1 = repo.commit("v1").unwrap().state_id;
    fs::write(dir.path().join("extra.txt"), "extra\n").unwrap();
    let v2 = repo.commit("v2 add extra").unwrap().state_id;
    fs::write(dir.path().join("note.txt"), "v3\n").unwrap();
    let v3 = repo.commit("v3").unwrap().state_id;

    let out = run_astvcs(Some(dir.path()), &["revert", &v2, "-m", "revert extra add"]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let revert_id = repo.head_state().unwrap();
    assert_ne!(revert_id, v1);
    assert_ne!(revert_id, v2);
    assert_ne!(revert_id, v3);
    assert!(!dir.path().join("extra.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "v3\n"
    );

    let out = run_astvcs(
        Some(dir.path()),
        &["revert", &revert_id, "-m", "revert the revert"],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(repo.head_state().unwrap(), v3);
    assert!(dir.path().join("extra.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "v3\n"
    );
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn resolve_remote_ref_for_diff_merge_base_and_checkout() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "v1\n").unwrap();
    upstream_repo.commit("v1").unwrap();
    fs::write(upstream.path().join("note.txt"), "v2\n").unwrap();
    let v2 = upstream_repo.commit("v2").unwrap().state_id;

    let clone_dir = TempDir::new().unwrap();
    run_astvcs(
        None,
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ],
    );
    let clone_repo = Repo::open(clone_dir.path()).unwrap();
    set_identity(&clone_repo, "Test User", "test@example.com", false).unwrap();
    fs::write(clone_dir.path().join("note.txt"), "v3\n").unwrap();
    clone_repo.commit("v3").unwrap();

    let out = run_astvcs(
        Some(clone_dir.path()),
        &["merge-base", "origin/main", "main"],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(base, v2);

    let out = run_astvcs(Some(clone_dir.path()), &["diff", "--state", "origin/main"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("v2"));

    let out = run_astvcs(
        Some(clone_dir.path()),
        &["checkout", "--state", "origin/main"],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let detached = Repo::open(clone_dir.path()).unwrap();
    assert!(detached.is_detached().unwrap());
    assert_eq!(
        fs::read_to_string(clone_dir.path().join("note.txt")).unwrap(),
        "v2\n"
    );

    let remote_tip = detached.read_remote_ref("origin", "main").unwrap().unwrap();
    let out = run_astvcs(Some(clone_dir.path()), &["reset", "origin/main"]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after_reset = Repo::open(clone_dir.path()).unwrap();
    assert_eq!(after_reset.head_state().unwrap(), remote_tip);
}

#[test]
fn pull_merges_upstream_changes() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "v1\n").unwrap();
    upstream_repo.commit("v1").unwrap();

    let clone_dir = TempDir::new().unwrap();
    assert_astvcs_ok(
        &run_astvcs(
            None,
            &[
                "clone",
                upstream.path().to_str().unwrap(),
                clone_dir.path().to_str().unwrap(),
            ],
        ),
        "clone",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &[
                "identity",
                "set",
                "--name",
                "Test User",
                "--email",
                "test@example.com",
            ],
        ),
        "identity set",
    );

    fs::write(upstream.path().join("note.txt"), "v2\n").unwrap();
    upstream_repo.commit("v2").unwrap();

    let out = run_astvcs(Some(clone_dir.path()), &["pull", "origin"]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Fetched origin/main"));
    assert!(stdout.contains("Merged origin/main"));
    assert_eq!(
        fs::read_to_string(clone_dir.path().join("note.txt")).unwrap(),
        "v2\n"
    );
    let clone_repo = Repo::open(clone_dir.path()).unwrap();
    assert!(clone_repo.working_tree_is_clean().unwrap());
    assert_eq!(
        clone_repo.head_state().unwrap(),
        upstream_repo.head_state().unwrap()
    );
}

#[test]
fn pull_detached_head_requires_branch() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "v1\n").unwrap();
    upstream_repo.commit("v1").unwrap();

    let clone_dir = TempDir::new().unwrap();
    run_astvcs(
        None,
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ],
    );
    let v1 = Repo::open(clone_dir.path()).unwrap().head_state().unwrap();
    run_astvcs(Some(clone_dir.path()), &["checkout", "--state", &v1]);

    let out = run_astvcs(Some(clone_dir.path()), &["pull", "origin"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("detached HEAD"));
    assert!(stderr.contains("--branch"));
}

#[test]
fn pull_merge_conflict_after_fetch() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(
        upstream.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    upstream_repo.commit("base").unwrap();

    let clone_dir = TempDir::new().unwrap();
    run_astvcs(
        None,
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ],
    );
    let clone_repo = Repo::open(clone_dir.path()).unwrap();
    set_identity(&clone_repo, "Test User", "test@example.com", false).unwrap();

    fs::write(
        clone_dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    clone_repo.commit("local edit").unwrap();
    let head_before = clone_repo.head_state().unwrap();

    fs::write(
        upstream.path().join("main.rs"),
        "fn foo() {\n    let z = 1;\n}\n",
    )
    .unwrap();
    upstream_repo.commit("upstream edit").unwrap();
    let upstream_tip = upstream_repo.head_state().unwrap();

    let out = run_astvcs(Some(clone_dir.path()), &["pull", "origin"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("merge failed after successful fetch"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Fetched origin/main"));

    let after = Repo::open(clone_dir.path()).unwrap();
    assert_eq!(after.head_state().unwrap(), head_before);
    assert_eq!(
        after.read_remote_ref("origin", "main").unwrap().unwrap(),
        upstream_tip
    );
    assert!(after.working_tree_is_clean().unwrap());
}

#[test]
fn go_sum_and_ps1_status_are_quiet() {
    trace::clear_log();
    trace::clear_warned();
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("go.sum"), "hash example\n").unwrap();
    fs::write(dir.path().join("run.ps1"), "Write-Host hi\n").unwrap();
    repo.commit("deps and script").unwrap();

    trace::clear_log();
    repo.status().unwrap();
    let log = trace::take_log();
    assert!(
        !log.iter().any(|l| l.contains("warning:")),
        "text-only paths should not warn on status: {log:?}"
    );
}

#[test]
fn cli_status_clean_tree_summary() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    repo.commit("v1").unwrap();

    let out = run_astvcs(Some(dir.path()), &["status"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nothing to commit, working tree clean"),
        "{stdout}"
    );
    assert!(
        !stdout.contains(" M "),
        "clean tree should not list unchanged paths"
    );
}

#[test]
fn trailing_comment_and_literal_edit_merge() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"Hello, World!\");\n}\n",
    )
    .unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "baseline"]),
        "baseline",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "create", "test"]),
        "branch",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["checkout", "--branch", "test"]),
        "checkout test",
    );
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"Hello, World!\"); // waddup fool\n}\n",
    )
    .unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "test: add comment"]),
        "test commit",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["checkout", "--branch", "main"]),
        "checkout main",
    );
    fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    println!(\"sup?\");\n}\n",
    )
    .unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "main: change greeting"]),
        "main commit",
    );
    let merge = run_astvcs(Some(root), &["merge", "test", "-m", "merge test into main"]);
    assert!(
        merge.status.success(),
        "merge failed: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let merged = fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(
        merged.contains("sup?") && merged.contains("// waddup fool"),
        "merged main.rs: {merged}"
    );
}

#[test]
fn cli_trivia_only_commit() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main(){\n    let x=1;\n}\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "baseline"]),
        "baseline",
    );
    fs::write(root.join("src/main.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "format whitespace"]),
        "trivia commit",
    );
    let text = fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert_eq!(text, "fn main() {\n    let x = 1;\n}\n");
}

#[test]
fn cli_branch_remove_guardrails() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    fs::write(root.join("note.txt"), "v1\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "baseline"]),
        "baseline",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "create", "feature"]),
        "create feature",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "create", "archive"]),
        "create archive",
    );

    let on_main = run_astvcs(Some(root), &["branch", "remove", "main"]);
    assert!(!on_main.status.success());
    assert!(String::from_utf8_lossy(&on_main.stderr).contains("checked-out branch"));

    let remove_feature = run_astvcs(Some(root), &["branch", "remove", "feature"]);
    assert!(remove_feature.status.success());
    assert!(String::from_utf8_lossy(&remove_feature.stdout).contains("Removed branch feature"));

    let list = run_astvcs(Some(root), &["branch", "list"]);
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(!listed.contains("feature"));
    assert!(listed.contains("archive"));

    assert_astvcs_ok(
        &run_astvcs(Some(root), &["checkout", "--branch", "archive"]),
        "checkout archive",
    );
    let remove_main = run_astvcs(Some(root), &["branch", "remove", "main"]);
    assert!(remove_main.status.success());

    let only = run_astvcs(Some(root), &["branch", "remove", "archive"]);
    assert!(!only.status.success());
    assert!(String::from_utf8_lossy(&only.stderr).contains("checked-out branch"));

    let dir2 = TempDir::new().unwrap();
    let root2 = dir2.path();
    let solo = Repo::init_with_identity(root2).unwrap();
    fs::write(root2.join("note.txt"), "solo\n").unwrap();
    solo.commit("solo").unwrap();
    let head = solo.head_state().unwrap();
    solo.checkout_state(&head).unwrap();
    let last = run_astvcs(Some(root2), &["branch", "remove", "main"]);
    assert!(!last.status.success());
    assert!(String::from_utf8_lossy(&last.stderr).contains("last branch"));

    let missing = run_astvcs(Some(root), &["branch", "remove", "no-such-branch"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("branch not found"));

    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "create", "feature"]),
        "recreate feature",
    );
    let list2 = run_astvcs(Some(root), &["branch", "list"]);
    assert!(String::from_utf8_lossy(&list2.stdout).contains("feature"));
}

#[test]
fn branch_create_from_remote_tracking_ref() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let repo = Repo::init_with_identity(root).unwrap();
    fs::write(root.join("note.txt"), "v1\n").unwrap();
    let v1 = repo.commit("v1").unwrap().state_id;
    repo.write_remote_ref("origin", "upstream", &v1).unwrap();

    assert_astvcs_ok(
        &run_astvcs(
            Some(root),
            &[
                "branch",
                "create",
                "local-track",
                "--from",
                "origin/upstream",
            ],
        ),
        "branch create from remote-tracking ref",
    );
    assert_eq!(repo.branch_state("local-track").unwrap(), v1);

    let bad = run_astvcs(
        Some(root),
        &["branch", "create", "bad", "--from", "origin/missing"],
    );
    assert!(!bad.status.success());
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("unknown branch or state"),
        "expected actionable ref error: {stderr}"
    );
}

#[test]
fn remove_default_branch_updates_config() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    fs::write(root.join("note.txt"), "v1\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "baseline"]),
        "baseline",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "create", "feature"]),
        "create feature",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "create", "develop"]),
        "create develop",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["checkout", "--branch", "feature"]),
        "checkout feature",
    );

    let config_before: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(".astvcs/config.json")).unwrap())
            .unwrap();
    assert_eq!(config_before["default_branch"], "main");

    assert_astvcs_ok(
        &run_astvcs(Some(root), &["branch", "remove", "main"]),
        "remove main",
    );

    let config_after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(".astvcs/config.json")).unwrap())
            .unwrap();
    assert_eq!(config_after["default_branch"], "develop");
}

#[test]
fn cli_materialize_refuses_dirty_tree_and_force_overrides() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    repo.commit("feature file").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("dirty.txt"), "dirty\n").unwrap();
    let main_tip = repo.head_state().unwrap();

    let merge_clean = run_astvcs(
        Some(dir.path()),
        &["merge", "feature", "-m", "merge feature"],
    );
    assert!(!merge_clean.status.success());
    assert!(String::from_utf8_lossy(&merge_clean.stderr).contains("uncommitted changes"));
    assert_eq!(repo.head_state().unwrap(), main_tip);
    assert_eq!(
        fs::read_to_string(dir.path().join("dirty.txt")).unwrap(),
        "dirty\n"
    );

    let merge_force = run_astvcs(
        Some(dir.path()),
        &["merge", "feature", "-m", "merge feature", "--force"],
    );
    assert_astvcs_ok(&merge_force, "merge --force");
    let stderr = String::from_utf8_lossy(&merge_force.stderr);
    assert!(stderr.contains("warning: merge --force: discarded uncommitted changes in dirty.txt"));
    assert!(dir.path().join("feature.txt").exists());

    fs::write(dir.path().join("dirty.txt"), "dirty again\n").unwrap();
    let merged_tip = repo.head_state().unwrap();

    let checkout_branch = run_astvcs(Some(dir.path()), &["checkout", "--branch", "feature"]);
    assert!(!checkout_branch.status.success());
    assert!(String::from_utf8_lossy(&checkout_branch.stderr).contains("uncommitted changes"));
    assert_eq!(repo.head_state().unwrap(), merged_tip);

    let checkout_branch_force = run_astvcs(
        Some(dir.path()),
        &["checkout", "--branch", "feature", "--force"],
    );
    assert_astvcs_ok(&checkout_branch_force, "checkout branch --force");
    let stderr = String::from_utf8_lossy(&checkout_branch_force.stderr);
    assert!(
        stderr.contains("warning: checkout --force: discarded uncommitted changes in dirty.txt")
    );

    fs::write(dir.path().join("dirty.txt"), "dirty again\n").unwrap();
    let feature_tip = repo.head_state().unwrap();
    let base_id = repo
        .history(10)
        .unwrap()
        .into_iter()
        .find(|e| e.message == "base")
        .unwrap()
        .id;

    let checkout_state = run_astvcs(Some(dir.path()), &["checkout", "--state", &base_id]);
    assert!(!checkout_state.status.success());
    assert!(String::from_utf8_lossy(&checkout_state.stderr).contains("uncommitted changes"));
    assert_eq!(repo.head_state().unwrap(), feature_tip);

    let checkout_state_force = run_astvcs(
        Some(dir.path()),
        &["checkout", "--state", &base_id, "--force"],
    );
    assert_astvcs_ok(&checkout_state_force, "checkout state --force");
    let stderr = String::from_utf8_lossy(&checkout_state_force.stderr);
    assert!(
        stderr.contains("warning: checkout --force: discarded uncommitted changes in dirty.txt")
    );

    repo.checkout_branch_with_force("main", true).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    repo.commit("v1 note").unwrap();
    fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
    let v2 = repo.commit("v2 note").unwrap().state_id;
    fs::write(dir.path().join("note.txt"), "dirty revert\n").unwrap();
    let before_revert = repo.head_state().unwrap();

    let revert = run_astvcs(Some(dir.path()), &["revert", &v2, "-m", "undo v2"]);
    assert!(!revert.status.success());
    assert!(String::from_utf8_lossy(&revert.stderr).contains("uncommitted changes"));
    assert_eq!(repo.head_state().unwrap(), before_revert);
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "dirty revert\n"
    );

    let revert_force = run_astvcs(
        Some(dir.path()),
        &["revert", &v2, "-m", "undo v2", "--force"],
    );
    assert_astvcs_ok(&revert_force, "revert --force");
    let stderr = String::from_utf8_lossy(&revert_force.stderr);
    assert!(stderr.contains("warning: revert --force: discarded uncommitted changes in note.txt"));
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "v1\n"
    );
}

#[test]
fn cli_reports_repository_lock_contention() {
    use astvcs::store::RepoLockGuard;

    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    let astvcs = dir.path().join(".astvcs");
    let _guard = RepoLockGuard::acquire(&astvcs).unwrap();

    let out = run_astvcs(Some(dir.path()), &["status"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("repository is locked by another process"),
        "{err}"
    );
    assert!(err.contains("repo.lock"), "{err}");
}

#[test]
fn cli_fsck_clean_repository() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let out = run_astvcs(Some(dir.path()), &["fsck"]);
    assert_astvcs_ok(&out, "fsck clean");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fsck: repository ok"), "{stdout}");
}

#[test]
fn cli_fsck_detects_corruption() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("note.txt"), "data\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("init").unwrap();
    let head = repo.head_state().unwrap();
    let manifest = repo.load_manifest(&head).unwrap();
    let note_blob = manifest.get("note.txt").unwrap().blob.clone();
    let shard = &note_blob[..2];
    fs::remove_file(
        dir.path()
            .join(".astvcs/blobs")
            .join(shard)
            .join(format!("{note_blob}.json")),
    )
    .unwrap();

    fs::write(
        dir.path().join(".astvcs/refs/heads/dangling"),
        format!("{}\n", "f".repeat(64)),
    )
    .unwrap();
    fs::write(dir.path().join(".astvcs/HEAD"), "ghost\n").unwrap();
    fs::write(
        dir.path().join(".astvcs/index.json"),
        r#"{
  "main.rs": {
    "state_id": "0000000000000000000000000000000000000000000000000000000000000000",
    "content_kind": "text"
  }
}"#,
    )
    .unwrap();
    fs::write(dir.path().join("orphan.txt.astvcs-tmp"), "partial").unwrap();

    let out = run_astvcs(Some(dir.path()), &["fsck"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fsck:"), "{stdout}");
    assert!(stdout.contains("issue(s) found"), "{stdout}");
    assert!(stdout.contains("missing blob"), "{stdout}");
    assert!(stdout.contains("dangling ref"), "{stdout}");
    assert!(stdout.contains("HEAD branch missing"), "{stdout}");
    assert!(stdout.contains("index inconsistent"), "{stdout}");
    assert!(stdout.contains("orphan temp file"), "{stdout}");
}

#[test]
fn cli_fsck_repair_fixes_index_inconsistency() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("note.txt"), "data\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("init").unwrap();
    fs::write(
        dir.path().join(".astvcs/index.json"),
        r#"{
  "main.rs": {
    "state_id": "0000000000000000000000000000000000000000000000000000000000000000",
    "content_kind": "text"
  }
}"#,
    )
    .unwrap();
    fs::write(
        dir.path().join(".astvcs/refs/heads/dangling"),
        format!("{}\n", "f".repeat(64)),
    )
    .unwrap();
    fs::write(dir.path().join("note.txt"), "data\n").unwrap();
    fs::write(dir.path().join("note.txt.astvcs-tmp"), "partial").unwrap();

    let repair = run_astvcs(Some(dir.path()), &["fsck", "--repair", "--prune-refs"]);
    assert_astvcs_ok(&repair, "fsck repair");
    let repair_out = String::from_utf8_lossy(&repair.stdout);
    assert!(repair_out.contains("repair(s) applied"), "{repair_out}");
    assert!(repair_out.contains("index rewritten"), "{repair_out}");
    assert!(repair_out.contains("dangling ref pruned"), "{repair_out}");
    assert!(repair_out.contains("repository ok"), "{repair_out}");
    assert!(!dir.path().join("note.txt.astvcs-tmp").exists());

    let again = run_astvcs(Some(dir.path()), &["fsck"]);
    assert_astvcs_ok(&again, "fsck after repair");
}

#[test]
fn cli_fsck_repair_refuses_ambiguous_head() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("init").unwrap();
    fs::write(dir.path().join(".astvcs/HEAD"), "ghost\n").unwrap();

    let out = run_astvcs(Some(dir.path()), &["fsck", "--repair"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("fsck --repair refused"), "{stderr}");
}

#[test]
fn cli_fsck_repair_leaves_missing_blob() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("note.txt"), "data\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("init").unwrap();
    let head = repo.head_state().unwrap();
    let manifest = repo.load_manifest(&head).unwrap();
    let note_blob = manifest.get("note.txt").unwrap().blob.clone();
    let shard = &note_blob[..2];
    fs::remove_file(
        dir.path()
            .join(".astvcs/blobs")
            .join(shard)
            .join(format!("{note_blob}.json")),
    )
    .unwrap();
    fs::write(
        dir.path().join(".astvcs/index.json"),
        r#"{
  "main.rs": {
    "state_id": "0000000000000000000000000000000000000000000000000000000000000000",
    "content_kind": "text"
  }
}"#,
    )
    .unwrap();

    let out = run_astvcs(Some(dir.path()), &["fsck", "--repair"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("missing blob"), "{stdout}");
    assert!(stdout.contains("repair(s) applied"), "{stdout}");
    assert!(stdout.contains("index rewritten"), "{stdout}");
}

#[test]
fn cli_fsck_prune_refs_removes_dangling_ref() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("init").unwrap();
    fs::write(
        dir.path().join(".astvcs/refs/heads/dangling"),
        format!("{}\n", "f".repeat(64)),
    )
    .unwrap();

    let out = run_astvcs(Some(dir.path()), &["fsck", "--prune-refs"]);
    assert_astvcs_ok(&out, "fsck prune-refs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dangling ref pruned"), "{stdout}");
    assert!(stdout.contains("repository ok"), "{stdout}");
    assert!(!dir.path().join(".astvcs/refs/heads/dangling").exists());
    assert!(dir.path().join(".astvcs/refs/heads/main").is_file());
}

#[test]
fn cli_gc_dry_run_and_prune() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("temp", None).unwrap();
    repo.checkout_branch("temp").unwrap();
    fs::write(dir.path().join("temp.txt"), "gone\n").unwrap();
    repo.commit("temp").unwrap();
    repo.checkout_branch("main").unwrap();
    repo.remove_branch("temp").unwrap();

    let dry = run_astvcs(Some(dir.path()), &["gc"]);
    assert_astvcs_ok(&dry, "gc dry-run");
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(dry_out.contains("dry-run"), "{dry_out}");
    assert!(dry_out.contains("unreachable"), "{dry_out}");
    assert!(dry_out.contains("state(s)"), "{dry_out}");

    let prune = run_astvcs(Some(dir.path()), &["gc", "--prune"]);
    assert_astvcs_ok(&prune, "gc --prune");
    let prune_out = String::from_utf8_lossy(&prune.stdout);
    assert!(prune_out.contains("removed"), "{prune_out}");

    let again = run_astvcs(Some(dir.path()), &["gc", "--prune"]);
    assert_astvcs_ok(&again, "gc second prune");
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("nothing to prune"),
        "{}",
        String::from_utf8_lossy(&again.stdout)
    );
}

#[test]
fn cli_gc_and_fsck_fail_under_external_lock() {
    use astvcs::store::RepoLockGuard;

    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    let astvcs = dir.path().join(".astvcs");
    let _guard = RepoLockGuard::acquire(&astvcs).unwrap();

    for cmd in [
        &["gc"] as &[&str],
        &["gc", "--prune"],
        &["gc", "--prune-history"],
        &["fsck"],
        &["fsck", "--repair"],
        &["fsck", "--prune-refs"],
    ] {
        let out = run_astvcs(Some(dir.path()), cmd);
        assert!(!out.status.success(), "expected lock failure for {cmd:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("repository is locked by another process"),
            "{err}"
        );
        assert!(err.contains("repo.lock"), "{err}");
    }
}

#[test]
fn path_rename_status_and_diff_integration() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("old.rs"), "fn foo() {}\n").unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    repo.commit("add").unwrap();
    fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();

    let status = repo.status().unwrap();
    assert_eq!(
        status.entries.get("new.rs"),
        Some(&FileStatus::unstaged_renamed("old.rs".into()))
    );
    assert!(!status.entries.contains_key("old.rs"));

    let diff = run_astvcs(Some(dir.path()), &["diff", "new.rs"]);
    assert_astvcs_ok(&diff, "diff renamed path");
    let out = String::from_utf8_lossy(&diff.stdout);
    assert!(out.contains("rename path `old.rs` -> `new.rs`"), "{out}");
}

#[test]
fn commit_without_identity_fails_with_actionable_error() {
    let dir = TempDir::new().unwrap();
    Repo::init(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let out = run_astvcs(Some(dir.path()), &["commit", "-m", "blocked"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("author identity not configured"), "{err}");
    assert!(err.contains("identity set"), "{err}");
}

#[test]
fn identity_set_and_read_roundtrip_via_repo_open() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init(dir.path()).unwrap();
    set_identity(&repo, "Ada Lovelace", "ada@example.com", false).unwrap();
    let repo2 = Repo::open(dir.path()).unwrap();
    let id = configured_identity(&repo2, false).unwrap().unwrap();
    assert_eq!(id.name, "Ada Lovelace");
    assert_eq!(id.email, "ada@example.com");
}

#[test]
fn identity_clear_repo_drops_override() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init(dir.path()).unwrap();
    set_identity(&repo, "Repo Only", "repo@example.com", false).unwrap();

    let clear = run_astvcs(Some(dir.path()), &["identity", "clear"]);
    assert_astvcs_ok(&clear, "identity clear");
    let msg = String::from_utf8_lossy(&clear.stdout);
    assert!(msg.contains("Cleared repository author identity"), "{msg}");

    let repo2 = Repo::open(dir.path()).unwrap();
    assert!(configured_identity(&repo2, false).unwrap().is_none());

    let config_path = dir.path().join(".astvcs").join("config.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(config.get("author").is_none());
    assert!(config.get("default_branch").is_some());

    let get = run_astvcs(Some(dir.path()), &["identity", "get"]);
    assert!(!get.status.success());
    let err = String::from_utf8_lossy(&get.stderr);
    assert!(err.contains("author identity not configured"), "{err}");
}

#[test]
fn identity_recorded_on_commit_merge_and_revert() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    set_identity(&repo, "Record Author", "record@example.com", false).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let commit_id = repo.commit("commit").unwrap().state_id;
    let commit_entry = repo.load_timeline_entry(&commit_id).unwrap();
    assert_eq!(commit_entry.author_name, "Record Author");
    assert_eq!(commit_entry.author_email, "record@example.com");

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("note.txt"), "feature\n").unwrap();
    repo.commit("feature").unwrap();
    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() { /* edit */ }\n").unwrap();
    let main_edit_id = repo.commit("main edit").unwrap().state_id;
    let merge_id = repo.merge_branch("feature", "merge").unwrap();
    let merge_entry = repo.load_timeline_entry(&merge_id).unwrap();
    assert_eq!(merge_entry.author_name, "Record Author");
    assert_eq!(merge_entry.author_email, "record@example.com");

    let revert_id = repo
        .revert_state(&main_edit_id, "revert main edit")
        .unwrap()
        .state_id;
    let revert_entry = repo.load_timeline_entry(&revert_id).unwrap();
    assert_eq!(revert_entry.author_name, "Record Author");
    assert_eq!(revert_entry.author_email, "record@example.com");
}

#[test]
fn identity_does_not_change_content_addressed_state_id() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let id = repo.commit("v1").unwrap().state_id;
    let manifest = repo.load_manifest(&id).unwrap();
    let manifest_id = astvcs::hash_manifest(&manifest);
    assert_ne!(
        id, manifest_id,
        "commit id should differ from manifest content hash"
    );
    assert!(
        repo.astvcs_dir()
            .join("states")
            .join(format!("{manifest_id}.json"))
            .is_file()
    );
    let entry = repo.load_timeline_entry(&id).unwrap();
    assert_eq!(entry.author_name, "Test User");
    assert_eq!(entry.author_email, "test@example.com");
}

#[test]
fn structured_errors_match_plain_messages_and_kinds() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("dirty.txt"), "dirty\n").unwrap();
    repo.commit("base").unwrap();

    let plain = repo.resolve_state_ref("no-such-branch").unwrap_err();
    assert_eq!(plain.kind, RepoErrorKind::UnknownRef);
    assert!(plain.contains("unknown branch or state"));

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    repo.commit("feature").unwrap();
    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("dirty.txt"), "changed\n").unwrap();

    let dirty = repo.merge_branch("feature", "merge").unwrap_err();
    assert_eq!(dirty.kind, RepoErrorKind::DirtyWorkingTree);
    assert!(dirty.contains("uncommitted changes"));

    use astvcs::store::RepoLockGuard;

    let _guard = RepoLockGuard::acquire(&repo.astvcs_dir()).unwrap();
    let lock_out = run_astvcs(Some(dir.path()), &["--json", "status"]);
    assert!(!lock_out.status.success());
    let lock_json = String::from_utf8_lossy(&lock_out.stderr);
    assert!(
        lock_json.contains("\"kind\":\"lock_contention\""),
        "{lock_json}"
    );
    assert!(
        lock_json.contains("repository is locked by another process"),
        "{lock_json}"
    );
    drop(_guard);

    let json_out = run_astvcs(Some(dir.path()), &["--json", "branch", "remove", "main"]);
    assert!(!json_out.status.success());
    let json_err = String::from_utf8_lossy(&json_out.stderr);
    assert!(json_err.contains("\"kind\":\"branch_guard\""), "{json_err}");
    assert!(json_err.contains("checked-out branch"), "{json_err}");
    assert!(!json_err.starts_with("error:"), "{json_err}");
}

const PNG_HEADER: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

#[test]
fn binary_commit_status_and_diff() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("logo.png"), PNG_HEADER).unwrap();
    repo.commit("add png").unwrap();

    let status = repo.status().unwrap();
    assert!(status.entries.is_empty());

    fs::write(
        dir.path().join("logo.png"),
        [PNG_HEADER.as_slice(), &[0x00]].concat(),
    )
    .unwrap();
    let status = repo.status().unwrap();
    assert_eq!(
        status.entries.get("logo.png"),
        Some(&FileStatus::unstaged_modified())
    );

    let diff = repo.diff_working("logo.png").unwrap();
    assert!(diff.contains("binary file"), "{diff}");
    assert!(diff.contains("content diff omitted"), "{diff}");
}

#[test]
fn binary_roundtrip_checkout_on_branch() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    let payload: Vec<u8> = [PNG_HEADER.as_slice(), b"fixture-bytes"].concat();
    fs::write(dir.path().join("asset.bin"), &payload).unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("asset.bin"), b"feature-only").unwrap();
    repo.commit("feature binary").unwrap();

    repo.checkout_branch("main").unwrap();
    assert_eq!(fs::read(dir.path().join("asset.bin")).unwrap(), payload);

    repo.checkout_branch("feature").unwrap();
    assert_eq!(
        fs::read(dir.path().join("asset.bin")).unwrap(),
        b"feature-only".as_slice()
    );
}

#[test]
fn binary_merge_add_add_conflict() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("data.bin"), [1u8, 2, 3]).unwrap();
    repo.commit("add on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("data.bin"), [4u8, 5, 6]).unwrap();
    repo.commit("add on main").unwrap();

    let err = repo.merge_branch("feature", "merge binaries").unwrap_err();
    assert!(
        err.contains("both branches added different content"),
        "{err}"
    );
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn binary_fsck_clean_after_commit() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("nul.dat"), [b'a', 0, b'b']).unwrap();
    repo.commit("binary with nul").unwrap();

    let out = run_astvcs(Some(dir.path()), &["fsck"]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("repository ok"), "{stdout}");
}

#[test]
fn binary_push_clone_roundtrip() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    let bytes: Vec<u8> = [PNG_HEADER.as_slice(), b"sync"].concat();
    fs::write(upstream.path().join("icon.png"), &bytes).unwrap();
    upstream_repo.commit("binary upstream").unwrap();

    let clone_dir = TempDir::new().unwrap();
    let out = run_astvcs(
        None,
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(clone_dir.path().join("icon.png")).unwrap(), bytes);
}

#[test]
fn binary_reset_hard_roundtrip() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    let payload: Vec<u8> = [PNG_HEADER.as_slice(), b"original"].concat();
    fs::write(dir.path().join("data.bin"), &payload).unwrap();
    repo.commit("add binary").unwrap();

    fs::write(dir.path().join("data.bin"), b"dirty bytes").unwrap();
    let head = repo.head_state().unwrap();
    repo.reset(&head, false, false, false).unwrap();

    assert_eq!(fs::read(dir.path().join("data.bin")).unwrap(), payload);
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn binary_diff_state() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("pic.bin"), PNG_HEADER).unwrap();
    let id1 = repo.commit("v1").unwrap().state_id;
    fs::write(
        dir.path().join("pic.bin"),
        [PNG_HEADER.as_slice(), &[0xFF]].concat(),
    )
    .unwrap();
    let id2 = repo.commit("v2").unwrap().state_id;

    let diff = repo.diff_state_path(&id1, &id2, "pic.bin").unwrap();
    assert!(diff.contains("binary file"), "{diff}");
    assert!(diff.contains("content diff omitted"), "{diff}");
}

#[test]
fn symlink_commit_and_checkout() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("target.txt"), "hello\n").unwrap();
    create_test_symlink(Path::new("target.txt"), &dir.path().join("link.txt"));
    repo.commit("add symlink").unwrap();

    fs::remove_file(dir.path().join("link.txt")).unwrap();
    repo.checkout_branch("main").unwrap();

    assert!(dir.path().join("link.txt").is_symlink());
    assert_eq!(
        fs::read_link(dir.path().join("link.txt"))
            .unwrap()
            .to_string_lossy(),
        "target.txt"
    );
}

#[test]
fn executable_mode_commit_and_checkout() {
    use astvcs::store::FileMode;

    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    let script = dir.path().join("run.sh");
    fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }
    repo.commit("executable script").unwrap();

    let state_id = repo.head_state().unwrap();
    let files = repo.load_state_files(&state_id).unwrap();
    assert_eq!(files.get("run.sh").unwrap().mode, FileMode::Executable);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&script, perms).unwrap();
    }
    #[cfg(windows)]
    {
        fs::write(&script, "echo hi\n").unwrap();
    }
    repo.checkout_branch("main").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let restored = fs::metadata(&script).unwrap().permissions().mode() & 0o111;
        assert_ne!(restored, 0, "executable bit should be restored on checkout");
    }
    #[cfg(windows)]
    {
        let content = fs::read_to_string(&script).unwrap();
        assert!(
            content.starts_with("#!/"),
            "checkout should restore shebang script content"
        );
        let files = repo.load_state_files(&repo.head_state().unwrap()).unwrap();
        assert_eq!(files.get("run.sh").unwrap().mode, FileMode::Executable);
        assert!(repo.status().unwrap().entries.is_empty());
    }
}

#[test]
fn symlink_vs_file_merge_conflict() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();

    repo.checkout_branch("feature").unwrap();
    create_test_symlink(Path::new("main.rs"), &dir.path().join("link.txt"));
    repo.commit("symlink on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("link.txt"), "regular file content\n").unwrap();
    repo.commit("regular file on main").unwrap();

    let err = repo
        .merge_branch("feature", "merge symlink vs file")
        .unwrap_err();
    assert!(err.contains("symlink and regular file conflict"), "{err}");
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn parse_fallback_status_annotation() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("valid baseline").unwrap();

    fs::write(dir.path().join("main.rs"), "fn {{{\n").unwrap();
    let status = repo.status().unwrap();
    assert_eq!(
        status.entries.get("main.rs"),
        Some(&FileStatus::unstaged_modified())
    );
    assert!(status.text_fallback_paths.contains("main.rs"));

    let out = run_astvcs(Some(dir.path()), &["status"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(" M main.rs (text fallback)"),
        "expected text fallback suffix in status: {stdout}"
    );
}

#[test]
fn parse_fallback_diff_annotation() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("valid baseline").unwrap();

    fs::write(dir.path().join("main.rs"), "fn {{{\n").unwrap();

    let out = run_astvcs(Some(dir.path()), &["diff", "main.rs"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("text fallback - structural diff unavailable"),
        "expected fallback banner in diff: {stdout}"
    );
    assert!(
        stdout.contains("parse mode:") && stdout.contains("text fallback"),
        "expected parse mode intent in diff: {stdout}"
    );
}

#[test]
fn parse_fallback_md_commit_stays_silent() {
    trace::clear_log();
    trace::clear_warned();
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("notes.md"), "# doc\n").unwrap();
    repo.commit("markdown").unwrap();
    let log = trace::take_log();
    assert!(
        !log.iter().any(|l| l.contains("warning:")),
        "markdown commit should not warn: {log:?}"
    );
}

#[test]
fn parse_fallback_broken_rs_stderr_warning() {
    trace::clear_log();
    trace::clear_warned();
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("valid baseline").unwrap();

    trace::clear_log();
    trace::clear_warned();
    fs::write(dir.path().join("main.rs"), "fn {{{\n").unwrap();
    let out = run_astvcs(Some(dir.path()), &["commit", "-m", "broken"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning:") && stderr.contains("main.rs"),
        "broken Rust should warn on stderr: {stderr}"
    );
    assert!(
        stderr.contains("syntax errors") || stderr.contains("AST parse failed"),
        "warning should mention parse failure: {stderr}"
    );
}

#[test]
fn parse_fallback_verbose_notice_detail() {
    trace::clear_log();
    trace::clear_warned();
    trace::set_verbose(true);
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn {{{\n").unwrap();
    repo.commit("broken").unwrap();
    let log = trace::take_log();
    assert!(
        log.iter()
            .any(|l| l.contains("notice:") && l.contains("text fallback")),
        "verbose commit should include text fallback notice: {log:?}"
    );
    trace::set_verbose(false);
}

#[test]
fn repack_roundtrip_and_fsck() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("init").unwrap();
    fs::write(dir.path().join("lib.rs"), "pub fn hi() {}\n").unwrap();
    repo.commit("second").unwrap();

    let out = run_astvcs(Some(dir.path()), &["repack"]);
    assert_astvcs_ok(&out, "repack");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("repack: packed"), "{stdout}");

    let repo = Repo::open(dir.path()).unwrap();
    assert!(repo.working_tree_is_clean().unwrap());

    let fsck = run_astvcs(Some(dir.path()), &["fsck"]);
    assert_astvcs_ok(&fsck, "fsck after repack");
    assert!(
        String::from_utf8_lossy(&fsck.stdout).contains("repository ok"),
        "{}",
        String::from_utf8_lossy(&fsck.stdout)
    );
}

#[test]
fn gc_preserves_packed_blobs() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("kept", None).unwrap();
    repo.checkout_branch("kept").unwrap();
    fs::write(dir.path().join("note.txt"), "remote kept\n").unwrap();
    let kept_tip = repo.commit("kept commit").unwrap().state_id;
    repo.create_branch("orphan", None).unwrap();
    repo.checkout_branch("orphan").unwrap();
    fs::write(dir.path().join("orphan.txt"), "drop me\n").unwrap();
    repo.commit("orphan commit").unwrap();
    repo.checkout_branch("main").unwrap();
    fs::create_dir_all(repo.astvcs_dir().join("refs/remotes/origin")).unwrap();
    repo.write_remote_ref("origin", "kept", &kept_tip).unwrap();
    repo.remove_branch("kept").unwrap();
    repo.remove_branch("orphan").unwrap();

    let kept_blob = repo
        .load_manifest(&kept_tip)
        .unwrap()
        .get("note.txt")
        .map(|e| e.blob.clone())
        .unwrap();

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["repack"]),
        "repack before gc",
    );
    assert!(repo.has_blob(&kept_blob));

    let prune = run_astvcs(Some(dir.path()), &["gc", "--prune"]);
    assert_astvcs_ok(&prune, "gc --prune with packed blobs");
    assert!(
        String::from_utf8_lossy(&prune.stdout).contains("removed"),
        "{}",
        String::from_utf8_lossy(&prune.stdout)
    );
    assert!(repo.has_blob(&kept_blob));
}

#[test]
fn repack_fetch_push_roundtrip() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "v1\n").unwrap();
    upstream_repo.commit("v1").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(upstream.path()), &["repack"]),
        "repack upstream",
    );

    let clone_dir = TempDir::new().unwrap();
    assert_astvcs_ok(
        &run_astvcs(
            None,
            &[
                "clone",
                upstream.path().to_str().unwrap(),
                clone_dir.path().to_str().unwrap(),
            ],
        ),
        "clone from repacked upstream",
    );
    assert_eq!(
        fs::read_to_string(clone_dir.path().join("note.txt")).unwrap(),
        "v1\n"
    );

    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &[
                "identity",
                "set",
                "--name",
                "Test User",
                "--email",
                "test@example.com",
            ],
        ),
        "identity set",
    );
    fs::write(clone_dir.path().join("note.txt"), "v2\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(clone_dir.path()), &["commit", "-m", "v2"]),
        "commit v2",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &["push", "origin", "--branch", "main"],
        ),
        "push after repack",
    );
    assert_eq!(
        upstream_repo.head_state().unwrap(),
        Repo::open(clone_dir.path()).unwrap().head_state().unwrap()
    );
    assert_astvcs_ok(
        &run_astvcs(Some(clone_dir.path()), &["fsck"]),
        "fsck clone after push from repacked upstream",
    );
}

#[test]
fn format_version_migrates_on_open_and_lock() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    let config_path = dir.path().join(".astvcs").join("config.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    value["format_version"] = serde_json::json!(0);
    fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["status"]),
        "status triggers migration",
    );
    let migrated: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(migrated["format_version"], 1);
}

#[test]
fn cli_fsck_warns_on_unknown_format_version() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    let config_path = dir.path().join(".astvcs").join("config.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    value["format_version"] = serde_json::json!(99);
    fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let out = run_astvcs(Some(dir.path()), &["fsck"]);
    assert!(
        !out.status.success(),
        "fsck should report unknown format version"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("unknown format version"), "{stdout}");
    assert!(stdout.contains("format_version 99"), "{stdout}");
}

#[test]
fn partial_commit_only_stages_paths() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("a.txt"), "a v1\n").unwrap();
    fs::write(dir.path().join("b.txt"), "b v1\n").unwrap();
    repo.commit("baseline").unwrap();

    fs::write(dir.path().join("a.txt"), "a v2\n").unwrap();
    fs::write(dir.path().join("b.txt"), "b v2\n").unwrap();
    repo.add(&["a.txt".into()], false, false).unwrap();
    let outcome = repo.commit("only a").unwrap();
    assert!(outcome.created);

    let files = repo.load_state_files(&outcome.state_id).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "a v2\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        "b v2\n"
    );
    let head_before = repo.history(2).unwrap();
    let prior = head_before
        .into_iter()
        .find(|e| e.message == "baseline")
        .unwrap();
    let prior_files = repo.load_state_files(&prior.id).unwrap();
    assert_eq!(
        files.get("b.txt").unwrap().content_hash().unwrap(),
        prior_files.get("b.txt").unwrap().content_hash().unwrap(),
        "b.txt should remain at committed v1 in new state"
    );
    assert_ne!(
        files.get("a.txt").unwrap().content_hash().unwrap(),
        prior_files.get("a.txt").unwrap().content_hash().unwrap(),
        "a.txt should reflect staged v2 in new state"
    );
}

#[test]
fn cli_add_dot_stages_tracked_changes() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("a.txt"), "a v1\n").unwrap();
    fs::write(dir.path().join("b.txt"), "b v1\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "baseline"]),
        "baseline",
    );

    fs::write(dir.path().join("a.txt"), "a v2\n").unwrap();
    fs::write(dir.path().join("c.txt"), "new\n").unwrap();
    assert_astvcs_ok(&run_astvcs(Some(dir.path()), &["add", "."]), "add dot");
    let out = run_astvcs(Some(dir.path()), &["status"]);
    assert_astvcs_ok(&out, "status after add dot");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("M  a.txt") || stdout.contains("M a.txt"),
        "expected staged a.txt: {stdout}"
    );
    assert!(
        stdout.contains("A  c.txt") || stdout.contains("A c.txt"),
        "add . should stage untracked files: {stdout}"
    );
}

#[test]
fn parallel_branches_identical_content_keep_distinct_log_messages() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("f.txt"), "content\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("b1", None).unwrap();
    repo.create_branch("b2", None).unwrap();

    repo.checkout_branch("b1").unwrap();
    fs::write(dir.path().join("f.txt"), "content\nsame edit\n").unwrap();
    repo.add(&[".".into()], false, true).unwrap();
    repo.commit("b1: edit A").unwrap();

    repo.checkout_branch("b2").unwrap();
    fs::write(dir.path().join("f.txt"), "content\nsame edit\n").unwrap();
    repo.add(&[".".into()], false, true).unwrap();
    repo.commit("b2: edit A (different message, identical content)")
        .unwrap();

    repo.checkout_branch("b1").unwrap();
    let log = repo.history(1).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].message, "b1: edit A");
    let b1_tip = repo.head_state().unwrap();

    repo.checkout_branch("b2").unwrap();
    let b2_tip = repo.head_state().unwrap();
    assert_ne!(b1_tip, b2_tip, "branch tips should be distinct commit ids");
    let manifest_b1 = repo.load_manifest(&b1_tip).unwrap();
    let manifest_b2 = repo.load_manifest(&b2_tip).unwrap();
    assert_eq!(
        astvcs::hash_manifest(&manifest_b1),
        astvcs::hash_manifest(&manifest_b2),
        "identical trees share manifest hash"
    );

    let report = repo.gc(true, true).unwrap();
    assert_eq!(
        report.states_removed,
        0,
        "shared manifest and both branch tips must stay reachable: {}",
        report.format_output()
    );
    let manifest_id = astvcs::hash_manifest(&manifest_b1);
    assert!(
        dir.path()
            .join(".astvcs/states")
            .join(format!("{manifest_id}.json"))
            .is_file(),
        "shared manifest file must survive gc --prune-history"
    );
    assert!(repo.load_manifest(&b1_tip).is_ok());
    assert!(repo.load_manifest(&b2_tip).is_ok());
}

#[test]
fn status_shows_staged_and_unstaged_columns() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("both.txt"), "v1\n").unwrap();
    fs::write(dir.path().join("staged.txt"), "v1\n").unwrap();
    repo.commit("baseline").unwrap();

    fs::write(dir.path().join("both.txt"), "v2\n").unwrap();
    repo.add(&["both.txt".into()], false, false).unwrap();
    fs::write(dir.path().join("both.txt"), "v3\n").unwrap();
    fs::write(dir.path().join("staged.txt"), "v2\n").unwrap();
    repo.add(&["staged.txt".into()], false, false).unwrap();

    let out = run_astvcs(Some(dir.path()), &["status"]);
    assert_astvcs_ok(&out, "status");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MM both.txt"),
        "expected staged+unstaged modification column: {stdout}"
    );
    assert!(
        stdout.contains("M  staged.txt"),
        "expected staged-only line: {stdout}"
    );
}

#[test]
fn merge_refuses_with_staged_changes() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    repo.commit("feature file").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("dirty.txt"), "local edit\n").unwrap();
    repo.add(&["dirty.txt".into()], false, false).unwrap();

    let err = repo.merge_branch("feature", "merge feature").unwrap_err();
    assert!(
        err.contains("staged changes"),
        "expected staged merge refusal: {err}"
    );
}

#[test]
fn checkout_force_with_staged_changes() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    repo.commit("base").unwrap();
    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feature.txt"), "on feature\n").unwrap();
    repo.commit("feature").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("note.txt"), "staged edit\n").unwrap();
    repo.add(&["note.txt".into()], false, false).unwrap();

    let checkout = run_astvcs(
        Some(dir.path()),
        &["checkout", "--branch", "feature", "--force"],
    );
    assert_astvcs_ok(&checkout, "checkout --force with staged changes");
    let stderr = String::from_utf8_lossy(&checkout.stderr);
    assert!(
        stderr.contains("warning: checkout --force: discarded uncommitted changes in note.txt"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("feature.txt")).unwrap(),
        "on feature\n"
    );
}

#[test]
fn reset_mixed_unstages_and_keeps_disk() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("keep.txt"), "v1\n").unwrap();
    let v1 = repo.commit("v1").unwrap().state_id;
    fs::write(dir.path().join("keep.txt"), "v2 on disk\n").unwrap();
    repo.add(&["keep.txt".into()], false, false).unwrap();

    let out = run_astvcs(Some(dir.path()), &["reset", "--mixed", &v1]);
    assert_astvcs_ok(&out, "reset --mixed");
    assert_eq!(
        fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "v2 on disk\n"
    );
    let staging: astvcs::store::StagingIndex =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".astvcs/staging.json")).unwrap())
            .unwrap();
    assert!(staging.entries.is_empty(), "staging should be cleared");
    assert_eq!(repo.head_state().unwrap(), v1);
}

#[test]
fn reset_modes_soft_mixed_hard_comparison() {
    fn setup_v2_with_dirty_disk_and_staging() -> (TempDir, StateId) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        fs::write(dir.path().join("f.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("f.txt"), "v2\n").unwrap();
        repo.commit("v2").unwrap();
        fs::write(dir.path().join("f.txt"), "dirty\n").unwrap();
        repo.add(&["f.txt".into()], false, false).unwrap();
        (dir, v1)
    }

    let (soft_dir, v1) = setup_v2_with_dirty_disk_and_staging();
    let out = run_astvcs(Some(soft_dir.path()), &["reset", "--soft", &v1]);
    assert_astvcs_ok(&out, "reset --soft");
    assert_eq!(
        fs::read_to_string(soft_dir.path().join("f.txt")).unwrap(),
        "dirty\n"
    );
    let staging: astvcs::store::StagingIndex = serde_json::from_str(
        &fs::read_to_string(soft_dir.path().join(".astvcs/staging.json")).unwrap(),
    )
    .unwrap();
    assert!(
        !staging.entries.is_empty(),
        "soft reset leaves staging unchanged"
    );

    let (mixed_dir, v1) = setup_v2_with_dirty_disk_and_staging();
    let out = run_astvcs(Some(mixed_dir.path()), &["reset", "--mixed", &v1]);
    assert_astvcs_ok(&out, "reset --mixed");
    assert_eq!(
        fs::read_to_string(mixed_dir.path().join("f.txt")).unwrap(),
        "dirty\n"
    );
    let staging: astvcs::store::StagingIndex = serde_json::from_str(
        &fs::read_to_string(mixed_dir.path().join(".astvcs/staging.json")).unwrap(),
    )
    .unwrap();
    assert!(staging.entries.is_empty(), "mixed reset clears staging");
    let mixed_repo = Repo::open(mixed_dir.path()).unwrap();
    let status = mixed_repo.status().unwrap();
    assert_eq!(
        status.entries.get("f.txt"),
        Some(&astvcs::store::FileStatus::unstaged_modified())
    );

    let (hard_dir, v1) = setup_v2_with_dirty_disk_and_staging();
    let out = run_astvcs(Some(hard_dir.path()), &["reset", &v1]);
    assert!(!out.status.success(), "hard reset refuses dirty tree");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("uncommitted changes"),
        "expected dirty-tree error"
    );
    let out = run_astvcs(Some(hard_dir.path()), &["reset", &v1, "--force"]);
    assert_astvcs_ok(&out, "reset --force");
    assert_eq!(
        fs::read_to_string(hard_dir.path().join("f.txt")).unwrap(),
        "v1\n"
    );
}

#[test]
fn cli_commit_empty_staging_errors() {
    let dir = TempDir::new().unwrap();
    Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "baseline"]),
        "baseline",
    );
    fs::write(dir.path().join("a.txt"), "v2\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["add", "a.txt"]),
        "add to activate staging",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "staged only"]),
        "commit staged",
    );
    fs::write(dir.path().join("a.txt"), "v3\n").unwrap();
    let out = run_astvcs(Some(dir.path()), &["commit", "-m", "nothing staged"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nothing staged"),
        "expected empty-staging commit error: {stderr}"
    );
}

#[test]
fn stash_before_checkout() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "base\n").unwrap();
    repo.commit("base").unwrap();

    fs::write(dir.path().join("note.txt"), "wip\n").unwrap();
    let push = run_astvcs(Some(dir.path()), &["stash", "push"]);
    assert_astvcs_ok(&push, "stash push");
    assert!(repo.working_tree_is_clean().unwrap());

    repo.create_branch("feature", None).unwrap();
    let checkout = run_astvcs(Some(dir.path()), &["checkout", "--branch", "feature"]);
    assert_astvcs_ok(&checkout, "checkout feature after stash");
}

#[test]
fn stash_pop_restores_files() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "base\n").unwrap();
    repo.commit("base").unwrap();

    fs::write(dir.path().join("note.txt"), "stashed\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["stash", "push"]),
        "stash push",
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "base\n"
    );

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["stash", "pop"]),
        "stash pop",
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "stashed\n"
    );
    assert!(!repo.working_tree_is_clean().unwrap());
}

#[test]
fn stash_pop_preserves_unstashed_tracked_files() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("util.go"), "package main\n").unwrap();
    repo.commit("initial").unwrap();

    let mut main_rs = fs::read_to_string(dir.path().join("main.rs")).unwrap();
    main_rs.push_str("// dirty\n");
    fs::write(dir.path().join("main.rs"), main_rs).unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["stash", "push"]),
        "stash push",
    );
    assert!(fs::metadata(dir.path().join("util.go")).is_ok());

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["stash", "pop"]),
        "stash pop",
    );
    assert!(
        fs::metadata(dir.path().join("util.go")).is_ok(),
        "util.go should remain on disk after stash pop"
    );
    let status = run_astvcs(Some(dir.path()), &["status"]);
    assert_astvcs_ok(&status, "status after stash pop");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        !stdout.contains("util.go"),
        "util.go should not appear as deleted: {stdout}"
    );
}

#[test]
fn stash_pop_conflict_keeps_entry() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    repo.commit("v1").unwrap();

    fs::write(dir.path().join("note.txt"), "stashed\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["stash", "push"]),
        "stash push",
    );

    fs::write(dir.path().join("note.txt"), "conflict\n").unwrap();
    repo.commit("conflict edit").unwrap();
    assert!(repo.working_tree_is_clean().unwrap());

    let pop = run_astvcs(Some(dir.path()), &["stash", "pop"]);
    assert!(!pop.status.success());
    let stderr = String::from_utf8_lossy(&pop.stderr);
    assert!(stderr.contains("merge would conflict"), "{stderr}");
    assert!(!stderr.contains("--resolve"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "conflict\n"
    );

    let list = run_astvcs(Some(dir.path()), &["stash", "list"]);
    assert_astvcs_ok(&list, "stash list");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("stash@{0}:"), "{stdout}");
}

#[test]
fn tag_create_and_list() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    repo.commit("v1").unwrap();

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["tag", "create", "v1.0", "main"]),
        "tag create",
    );
    let list = run_astvcs(Some(dir.path()), &["tag", "list"]);
    assert_astvcs_ok(&list, "tag list");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("v1.0 ("), "{stdout}");

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["tag", "remove", "v1.0"]),
        "tag remove",
    );
    let list = run_astvcs(Some(dir.path()), &["tag", "list"]);
    assert_astvcs_ok(&list, "tag list");
    assert!(String::from_utf8_lossy(&list.stdout).trim().is_empty());
}

#[test]
fn checkout_tag_detached() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
    repo.commit("v1").unwrap();
    fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
    let tagged = repo.commit("v2").unwrap().state_id;

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["tag", "create", "release", "main"]),
        "tag create",
    );
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["checkout", "--state", "release"]),
        "checkout tag",
    );

    let repo = Repo::open(dir.path()).unwrap();
    assert!(repo.is_detached().unwrap());
    assert_eq!(repo.head_state().unwrap(), tagged);
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "v2\n"
    );
}

#[test]
fn tag_fetch_push_between_repos() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "v1\n").unwrap();
    upstream_repo.commit("v1").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(upstream.path()), &["tag", "create", "v1.0", "main"]),
        "upstream tag",
    );

    let clone_dir = TempDir::new().unwrap();
    assert_astvcs_ok(
        &run_astvcs(
            None,
            &[
                "clone",
                upstream.path().to_str().unwrap(),
                clone_dir.path().to_str().unwrap(),
            ],
        ),
        "clone",
    );
    let clone_repo = Repo::open(clone_dir.path()).unwrap();
    let tags = clone_repo.list_tags().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "v1.0");
    assert_eq!(tags[0].state_id, upstream_repo.head_state().unwrap());

    set_identity(&clone_repo, "Test User", "test@example.com", false).unwrap();
    fs::write(clone_dir.path().join("note.txt"), "v2\n").unwrap();
    clone_repo.commit("v2").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(clone_dir.path()), &["tag", "create", "v2.0", "main"]),
        "clone tag",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &["push", "origin", "--branch", "main"],
        ),
        "push",
    );

    let upstream_tags = upstream_repo.list_tags().unwrap();
    assert!(
        upstream_tags.iter().any(|t| t.name == "v2.0"),
        "expected v2.0 tag on upstream: {upstream_tags:?}"
    );
}

fn hooks_dir(repo: &Path) -> PathBuf {
    repo.join(".astvcs").join("hooks")
}

fn write_hook(repo: &Path, name: &str, body: &str) {
    let dir = hooks_dir(repo);
    fs::create_dir_all(&dir).expect("create hooks dir");
    #[cfg(unix)]
    {
        let path = dir.join(name);
        fs::write(&path, body).expect("write hook");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod hook");
    }
    #[cfg(windows)]
    {
        let path = dir.join(format!("{name}.cmd"));
        fs::write(&path, body).expect("write hook");
    }
}

#[test]
fn hook_pre_commit_aborts_commit() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("file.txt"), "one\n").unwrap();
    repo.commit("base").unwrap();
    let head_before = repo.head_state().unwrap();

    fs::write(dir.path().join("file.txt"), "two\n").unwrap();
    #[cfg(unix)]
    write_hook(dir.path(), "pre-commit", "#!/bin/sh\nexit 1\n");
    #[cfg(windows)]
    write_hook(dir.path(), "pre-commit", "@echo off\r\nexit /b 1\r\n");

    let err = repo.commit("should fail").unwrap_err();
    assert_eq!(err.kind, RepoErrorKind::HookFailed);
    assert_eq!(repo.head_state().unwrap(), head_before);
}

#[test]
fn hook_commit_msg_edits_message() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("file.txt"), "one\n").unwrap();
    repo.commit("base").unwrap();

    fs::write(dir.path().join("file.txt"), "two\n").unwrap();
    #[cfg(unix)]
    write_hook(
        dir.path(),
        "commit-msg",
        "#!/bin/sh\nprintf 'edited: ' > \"$ASTVCS_COMMIT_MSG_FILE.tmp\"\ncat \"$ASTVCS_COMMIT_MSG_FILE\" >> \"$ASTVCS_COMMIT_MSG_FILE.tmp\"\nmv \"$ASTVCS_COMMIT_MSG_FILE.tmp\" \"$ASTVCS_COMMIT_MSG_FILE\"\n",
    );
    #[cfg(windows)]
    write_hook(
        dir.path(),
        "commit-msg",
        "@echo off\r\nset /p msg=<\"%ASTVCS_COMMIT_MSG_FILE%\"\r\necho edited: %msg%> \"%ASTVCS_COMMIT_MSG_FILE%\"\r\n",
    );

    let id = repo.commit("original").unwrap().state_id;
    let entry = repo.load_timeline_entry(&id).unwrap();
    assert_eq!(entry.message, "edited: original");
}

#[test]
fn hook_nested_astvcs_status_in_pre_commit() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("file.txt"), "one\n").unwrap();
    repo.commit("base").unwrap();

    fs::write(dir.path().join("file.txt"), "two\n").unwrap();
    let bin = astvcs_bin();
    #[cfg(unix)]
    write_hook(
        dir.path(),
        "pre-commit",
        &format!(
            "#!/bin/sh\n\"{}\" --repo \"{}\" status\n",
            bin.display(),
            dir.path().display()
        ),
    );
    #[cfg(windows)]
    write_hook(
        dir.path(),
        "pre-commit",
        &format!(
            "@echo off\r\n\"{}\" --repo \"{}\" status\r\n",
            bin.display(),
            dir.path().display()
        ),
    );

    assert!(repo.commit("with nested status").unwrap().created);
}

#[test]
fn hook_no_verify_skips_pre_commit() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("file.txt"), "one\n").unwrap();
    repo.commit("base").unwrap();

    fs::write(dir.path().join("file.txt"), "two\n").unwrap();
    #[cfg(unix)]
    write_hook(dir.path(), "pre-commit", "#!/bin/sh\nexit 1\n");
    #[cfg(windows)]
    write_hook(dir.path(), "pre-commit", "@echo off\r\nexit /b 1\r\n");

    let out = run_astvcs(
        Some(dir.path()),
        &["commit", "-m", "skip hooks", "--no-verify"],
    );
    assert_astvcs_ok(&out, "commit with --no-verify");
}

#[test]
fn hook_pre_merge_aborts() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    repo.commit("base").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
    repo.commit("feature work").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("main.txt"), "main\n").unwrap();
    repo.commit("main work").unwrap();
    let main_before = repo.head_state().unwrap();

    #[cfg(unix)]
    write_hook(dir.path(), "pre-merge", "#!/bin/sh\nexit 1\n");
    #[cfg(windows)]
    write_hook(dir.path(), "pre-merge", "@echo off\r\nexit /b 1\r\n");

    let err = repo.merge_branch("feature", "merge").unwrap_err();
    assert_eq!(err.kind, RepoErrorKind::HookFailed);
    assert_eq!(repo.head_state().unwrap(), main_before);
}

#[test]
fn rebase_linear_success() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("app.txt"), "v1\n").unwrap();
    repo.commit("base").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feat1.txt"), "one\n").unwrap();
    repo.commit("feature 1").unwrap();
    fs::write(dir.path().join("feat2.txt"), "two\n").unwrap();
    repo.commit("feature 2").unwrap();
    let feature_tip_before = repo.head_state().unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("app.txt"), "v2-main\n").unwrap();
    repo.commit("main advance").unwrap();
    let main_tip = repo.head_state().unwrap();

    repo.checkout_branch("feature").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["rebase", "main"]),
        "rebase onto main",
    );

    assert_ne!(repo.head_state().unwrap(), feature_tip_before);
    let disk = fs::read_to_string(dir.path().join("app.txt")).unwrap();
    assert!(disk.contains("v2-main"), "{disk}");
    assert!(
        fs::read_to_string(dir.path().join("feat1.txt"))
            .unwrap()
            .contains("one")
    );
    assert!(
        fs::read_to_string(dir.path().join("feat2.txt"))
            .unwrap()
            .contains("two")
    );
    assert!(!dir.path().join(".astvcs/rebase-state.json").exists());
    assert_eq!(repo.merge_base_refs("feature", "main").unwrap(), main_tip);
}

#[test]
fn rebase_conflict_abort_restores() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let z = 1;\n}\n",
    )
    .unwrap();
    repo.commit("z on feature").unwrap();
    let feature_tip = repo.head_state().unwrap();
    let feature_disk = fs::read_to_string(dir.path().join("main.rs")).unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    repo.commit("y on main").unwrap();

    repo.checkout_branch("feature").unwrap();
    let rebase = run_astvcs(Some(dir.path()), &["rebase", "main"]);
    assert!(!rebase.status.success(), "rebase should stop on conflict");
    let stderr = String::from_utf8_lossy(&rebase.stderr);
    assert!(stderr.contains("rebase would conflict"), "{stderr}");
    assert!(
        stderr.contains("rebase --continue --resolve main.rs:ours"),
        "{stderr}"
    );
    assert!(!stderr.contains("merge would conflict in"), "{stderr}");
    assert!(dir.path().join(".astvcs/rebase-state.json").exists());
    assert_eq!(repo.head_state().unwrap(), feature_tip);

    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["rebase", "--abort"]),
        "rebase abort",
    );
    assert!(!dir.path().join(".astvcs/rebase-state.json").exists());
    assert_eq!(repo.head_state().unwrap(), feature_tip);
    assert_eq!(
        fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        feature_disk
    );
}

#[test]
fn rebase_conflict_continue() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let z = 1;\n}\n",
    )
    .unwrap();
    repo.commit("z on feature").unwrap();

    repo.checkout_branch("main").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    repo.commit("y on main").unwrap();

    repo.checkout_branch("feature").unwrap();
    let rebase = run_astvcs(Some(dir.path()), &["rebase", "main"]);
    assert!(!rebase.status.success());

    assert_astvcs_ok(
        &run_astvcs(
            Some(dir.path()),
            &["rebase", "--continue", "--resolve", "main.rs:theirs"],
        ),
        "rebase continue",
    );
    assert!(!dir.path().join(".astvcs/rebase-state.json").exists());
    let disk = fs::read_to_string(dir.path().join("main.rs")).unwrap();
    assert!(
        disk.contains("z"),
        "expected feature side after resolve: {disk}"
    );
    assert!(!disk.contains("y"), "should not keep main side: {disk}");
}

#[test]
fn cherry_pick_clean_commit() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(dir.path().join("app.txt"), "v1\n").unwrap();
    repo.commit("base").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(dir.path().join("feat.txt"), "feature change\n").unwrap();
    let feature_commit = repo.commit("feature change").unwrap().state_id;

    repo.checkout_branch("main").unwrap();
    fs::write(dir.path().join("app.txt"), "v2-main\n").unwrap();
    repo.commit("main advance").unwrap();
    let main_before = repo.head_state().unwrap();

    assert_astvcs_ok(
        &run_astvcs(
            Some(dir.path()),
            &["cherry-pick", &feature_commit, "-m", "pick feature"],
        ),
        "cherry-pick",
    );

    assert_ne!(repo.head_state().unwrap(), main_before);
    assert_eq!(
        fs::read_to_string(dir.path().join("app.txt")).unwrap(),
        "v2-main\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("feat.txt")).unwrap(),
        "feature change\n"
    );
    assert!(repo.working_tree_is_clean().unwrap());
}

#[test]
fn cherry_pick_conflict_leaves_head_unchanged() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    repo.commit("base").unwrap();

    repo.create_branch("feature", None).unwrap();
    repo.checkout_branch("feature").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let z = 1;\n}\n",
    )
    .unwrap();
    let feature_commit = repo.commit("z on feature").unwrap().state_id;

    repo.checkout_branch("main").unwrap();
    fs::write(
        dir.path().join("main.rs"),
        "fn foo() {\n    let y = 1;\n}\n",
    )
    .unwrap();
    repo.commit("y on main").unwrap();
    let tip = repo.head_state().unwrap();
    let disk_before = fs::read_to_string(dir.path().join("main.rs")).unwrap();

    let out = run_astvcs(
        Some(dir.path()),
        &["cherry-pick", &feature_commit, "-m", "pick z"],
    );
    assert!(!out.status.success(), "cherry-pick should fail on conflict");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cherry-pick would conflict"), "{stderr}");
    assert!(stderr.contains("current HEAD: rename"), "{stderr}");
    assert!(stderr.contains("picked state: rename"), "{stderr}");
    assert!(!stderr.contains("--resolve"), "{stderr}");
    assert_eq!(repo.head_state().unwrap(), tip);
    assert_eq!(
        fs::read_to_string(dir.path().join("main.rs")).unwrap(),
        disk_before
    );
}

#[test]
fn cherry_pick_from_remote_tracking_ref() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("shared.txt"), "base\n").unwrap();
    upstream_repo.commit("base").unwrap();

    upstream_repo.create_branch("feature", None).unwrap();
    upstream_repo.checkout_branch("feature").unwrap();
    fs::write(upstream.path().join("feature.txt"), "from feature\n").unwrap();
    upstream_repo.commit("feature commit").unwrap();

    upstream_repo.checkout_branch("main").unwrap();

    let clone_dir = TempDir::new().unwrap();
    assert_astvcs_ok(
        &run_astvcs(
            None,
            &[
                "clone",
                upstream.path().to_str().unwrap(),
                clone_dir.path().to_str().unwrap(),
            ],
        ),
        "clone",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &[
                "identity",
                "set",
                "--name",
                "Test User",
                "--email",
                "test@example.com",
            ],
        ),
        "identity set",
    );

    assert_astvcs_ok(
        &run_astvcs(Some(clone_dir.path()), &["fetch", "origin"]),
        "fetch origin",
    );

    fs::write(clone_dir.path().join("shared.txt"), "local main\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(clone_dir.path()), &["commit", "-m", "local advance"]),
        "local commit",
    );

    assert_astvcs_ok(
        &run_astvcs(
            Some(clone_dir.path()),
            &["cherry-pick", "origin/feature", "-m", "pick feature"],
        ),
        "cherry-pick origin/feature",
    );

    assert_eq!(
        fs::read_to_string(clone_dir.path().join("feature.txt")).unwrap(),
        "from feature\n"
    );
    assert_eq!(
        fs::read_to_string(clone_dir.path().join("shared.txt")).unwrap(),
        "local main\n"
    );
    let clone_repo = Repo::open(clone_dir.path()).unwrap();
    assert!(clone_repo.working_tree_is_clean().unwrap());
}

#[test]
fn blame_linear_two_commits() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    set_identity(&repo, "Alice", "alice@example.com", false).unwrap();

    fs::write(dir.path().join("notes.txt"), "line one\nline two\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "add notes"]),
        "first commit",
    );

    fs::write(
        dir.path().join("notes.txt"),
        "line one\nline two changed\nline three\n",
    )
    .unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "edit and append"]),
        "second commit",
    );

    let out = run_astvcs(Some(dir.path()), &["blame", "notes.txt"]);
    assert_astvcs_ok(&out, "blame notes.txt");
    let blame = String::from_utf8_lossy(&out.stdout);

    assert!(
        blame.contains("add notes"),
        "first line should blame first commit: {blame}"
    );
    assert!(
        blame.contains("edit and append"),
        "changed and appended lines should blame second commit: {blame}"
    );

    let lines: Vec<&str> = blame.lines().collect();
    assert_eq!(
        lines.len(),
        6,
        "expected 3 blame pairs (header+content): {blame}"
    );

    assert!(lines[0].contains("add notes"));
    assert_eq!(lines[1], "line one");

    assert!(lines[2].contains("edit and append"));
    assert_eq!(lines[3], "line two changed");

    assert!(lines[4].contains("edit and append"));
    assert_eq!(lines[5], "line three");

    let blame_lines = repo.blame("notes.txt").unwrap();
    assert_eq!(blame_lines.len(), 3);
    assert_eq!(blame_lines[0].message, "add notes");
    assert_eq!(blame_lines[1].message, "edit and append");
    assert_eq!(blame_lines[2].message, "edit and append");
}

#[test]
fn blame_reorder_preserves_attribution_for_moved_lines() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    let original = "fn a() {\n    1\n}\nfn b() {\n    2\n}\n";
    fs::write(dir.path().join("sample.rs"), original).unwrap();
    repo.commit("add functions").unwrap();

    let reordered = "fn b() {\n    2\n}\nfn a() {\n    1\n}\n";
    fs::write(dir.path().join("sample.rs"), reordered).unwrap();
    repo.commit("reorder functions").unwrap();

    let blame_lines = repo.blame("sample.rs").unwrap();
    assert!(
        blame_lines
            .iter()
            .all(|line| line.message == "add functions"),
        "reordered lines should stay attributed to the introducing commit: {:?}",
        blame_lines
            .iter()
            .map(|l| (&l.content, &l.message))
            .collect::<Vec<_>>()
    );
}

fn write_bisect_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}

#[test]
fn bisect_linear_four_commits() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    set_identity(&repo, "Alice", "alice@example.com", false).unwrap();

    fs::write(dir.path().join("file.txt"), "v1\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "commit 1"]),
        "commit 1",
    );

    fs::write(dir.path().join("file.txt"), "v2\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "commit 2"]),
        "commit 2",
    );

    fs::write(dir.path().join("file.txt"), "v3\n").unwrap();
    fs::write(dir.path().join("bisect-marker.txt"), "bad\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "commit 3"]),
        "commit 3",
    );

    fs::write(dir.path().join("file.txt"), "v4\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(dir.path()), &["commit", "-m", "commit 4"]),
        "commit 4",
    );

    let good_id = repo
        .history(10)
        .unwrap()
        .last()
        .expect("oldest commit")
        .id
        .clone();

    #[cfg(unix)]
    {
        let script = dir.path().parent().unwrap().join("bisect-test.sh");
        write_bisect_script(
            &script,
            "#!/bin/sh\nif [ -f bisect-marker.txt ]; then exit 1; fi\nexit 0\n",
        );
        assert_astvcs_ok(
            &run_astvcs(
                Some(dir.path()),
                &["bisect", "start", "HEAD", good_id.as_str()],
            ),
            "bisect start",
        );
        let out = run_astvcs(
            Some(dir.path()),
            &["bisect", "run", script.to_str().unwrap()],
        );
        assert_astvcs_ok(&out, "bisect run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("first bad state:") && stdout.contains("commit 3"),
            "expected first bad commit 3: {stdout}"
        );
    }

    #[cfg(windows)]
    {
        let script = dir.path().parent().unwrap().join("bisect-test.ps1");
        write_bisect_script(
            &script,
            &format!(
                "Set-Location -LiteralPath \"{}\"\r\nif (Test-Path bisect-marker.txt) {{ exit 1 }} else {{ exit 0 }}\r\n",
                dir.path().display()
            ),
        );
        assert_astvcs_ok(
            &run_astvcs(
                Some(dir.path()),
                &["bisect", "start", "HEAD", good_id.as_str()],
            ),
            "bisect start",
        );
        let out = run_astvcs(
            Some(dir.path()),
            &[
                "bisect",
                "run",
                "powershell",
                "-File",
                script.to_str().unwrap(),
            ],
        );
        assert_astvcs_ok(&out, "bisect run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("first bad state:") && stdout.contains("commit 3"),
            "expected first bad commit 3: {stdout}"
        );
    }
}

#[test]
fn bisect_run_releases_lock_for_nested_astvcs() {
    let dir = TempDir::new().unwrap();
    let repo = Repo::init_with_identity(dir.path()).unwrap();
    set_identity(&repo, "Alice", "alice@example.com", false).unwrap();

    fs::write(dir.path().join("file.txt"), "v1\n").unwrap();
    repo.commit("commit 1").unwrap();

    fs::write(dir.path().join("file.txt"), "v2\n").unwrap();
    repo.commit("commit 2").unwrap();

    fs::write(dir.path().join("file.txt"), "v3\n").unwrap();
    fs::write(dir.path().join("bisect-marker.txt"), "bad\n").unwrap();
    repo.commit("commit 3").unwrap();

    fs::write(dir.path().join("file.txt"), "v4\n").unwrap();
    repo.commit("commit 4").unwrap();

    let good_id = repo
        .history(10)
        .unwrap()
        .last()
        .expect("oldest commit")
        .id
        .clone();
    let bin = astvcs_bin();

    #[cfg(unix)]
    {
        let script = dir.path().parent().unwrap().join("bisect-nested.sh");
        write_bisect_script(
            &script,
            &format!(
                "#!/bin/sh\n\"{}\" --repo \"{}\" status\nif [ -f bisect-marker.txt ]; then exit 1; fi\nexit 0\n",
                bin.display(),
                dir.path().display()
            ),
        );
        assert_astvcs_ok(
            &run_astvcs(
                Some(dir.path()),
                &["bisect", "start", "HEAD", good_id.as_str()],
            ),
            "bisect start",
        );
        let script = dir.path().parent().unwrap().join("bisect-nested.sh");
        assert_astvcs_ok(
            &run_astvcs(
                Some(dir.path()),
                &["bisect", "run", script.to_str().unwrap()],
            ),
            "bisect run with nested astvcs status",
        );
    }

    #[cfg(windows)]
    {
        let script = dir.path().parent().unwrap().join("bisect-nested.ps1");
        write_bisect_script(
            &script,
            &format!(
                "Set-Location -LiteralPath \"{}\"\r\n& \"{}\" --repo \"{}\" status\r\nif (Test-Path bisect-marker.txt) {{ exit 1 }} else {{ exit 0 }}\r\n",
                dir.path().display(),
                bin.display(),
                dir.path().display()
            ),
        );
        assert_astvcs_ok(
            &run_astvcs(
                Some(dir.path()),
                &["bisect", "start", "HEAD", good_id.as_str()],
            ),
            "bisect start",
        );
        let script = dir.path().parent().unwrap().join("bisect-nested.ps1");
        assert_astvcs_ok(
            &run_astvcs(
                Some(dir.path()),
                &[
                    "bisect",
                    "run",
                    "powershell",
                    "-File",
                    script.to_str().unwrap(),
                ],
            ),
            "bisect run with nested astvcs status",
        );
    }
}

#[test]
fn shallow_clone_has_fewer_timeline_entries_than_full_clone() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    for (i, content) in ["v1\n", "v2\n", "v3\n", "v4\n", "v5\n"].iter().enumerate() {
        fs::write(upstream.path().join("note.txt"), content).unwrap();
        upstream_repo.commit(&format!("v{}", i + 1)).unwrap();
    }

    let shallow_dir = TempDir::new().unwrap();
    let out = run_astvcs(
        None,
        &[
            "clone",
            "--depth",
            "2",
            upstream.path().to_str().unwrap(),
            shallow_dir.path().to_str().unwrap(),
        ],
    );
    assert_astvcs_ok(&out, "shallow clone");

    let full_dir = TempDir::new().unwrap();
    let out = run_astvcs(
        None,
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            full_dir.path().to_str().unwrap(),
        ],
    );
    assert_astvcs_ok(&out, "full clone");

    let shallow_count = fs::read_dir(shallow_dir.path().join(".astvcs/timeline"))
        .unwrap()
        .count();
    let full_count = fs::read_dir(full_dir.path().join(".astvcs/timeline"))
        .unwrap()
        .count();
    assert!(shallow_count < full_count);
    assert_eq!(shallow_count, 3);
    assert!(shallow_dir.path().join(".astvcs/shallow.json").is_file());
}

#[test]
fn full_fetch_deepens_shallow_clone() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    for (i, content) in ["v1\n", "v2\n", "v3\n", "v4\n", "v5\n"].iter().enumerate() {
        fs::write(upstream.path().join("note.txt"), content).unwrap();
        upstream_repo.commit(&format!("v{}", i + 1)).unwrap();
    }
    let full_count = fs::read_dir(upstream.path().join(".astvcs/timeline"))
        .unwrap()
        .count();

    let shallow_dir = TempDir::new().unwrap();
    let out = run_astvcs(
        None,
        &[
            "clone",
            "--depth",
            "2",
            upstream.path().to_str().unwrap(),
            shallow_dir.path().to_str().unwrap(),
        ],
    );
    assert_astvcs_ok(&out, "shallow clone");
    let shallow_count = fs::read_dir(shallow_dir.path().join(".astvcs/timeline"))
        .unwrap()
        .count();
    assert_eq!(shallow_count, 3);

    let out = run_astvcs(Some(shallow_dir.path()), &["fetch", "origin"]);
    assert_astvcs_ok(&out, "full fetch");

    let deepened_count = fs::read_dir(shallow_dir.path().join(".astvcs/timeline"))
        .unwrap()
        .count();
    assert_eq!(deepened_count, full_count);

    let shallow_json = fs::read_to_string(shallow_dir.path().join(".astvcs/shallow.json")).unwrap();
    assert_eq!(shallow_json.trim(), "[]");

    let out = run_astvcs(Some(shallow_dir.path()), &["log"]);
    assert_astvcs_ok(&out, "log after full fetch");
}

#[test]
fn merge_base_fails_on_shallow_clone_with_incomplete_history() {
    let upstream = TempDir::new().unwrap();
    let upstream_repo = Repo::init_with_identity(upstream.path()).unwrap();
    fs::write(upstream.path().join("note.txt"), "root\n").unwrap();
    upstream_repo.commit("root").unwrap();
    fs::write(upstream.path().join("note.txt"), "main2\n").unwrap();
    upstream_repo.commit("main2").unwrap();
    upstream_repo.create_branch("feature", None).unwrap();
    fs::write(upstream.path().join("note.txt"), "feature2\n").unwrap();
    upstream_repo.commit("feature2").unwrap();

    let shallow_dir = TempDir::new().unwrap();
    assert_astvcs_ok(
        &run_astvcs(
            None,
            &[
                "clone",
                "--depth",
                "1",
                upstream.path().to_str().unwrap(),
                shallow_dir.path().to_str().unwrap(),
            ],
        ),
        "shallow clone main",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(shallow_dir.path()),
            &["fetch", "origin", "--branch", "feature", "--depth", "1"],
        ),
        "shallow fetch feature",
    );
    let shallow_repo = Repo::open(shallow_dir.path()).unwrap();
    let feature_tip = shallow_repo
        .read_remote_ref("origin", "feature")
        .unwrap()
        .unwrap();
    shallow_repo
        .write_branch_ref("feature", &feature_tip)
        .unwrap();

    let out = run_astvcs(Some(shallow_dir.path()), &["merge-base", "main", "feature"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("shallow history"));

    let out = run_astvcs(
        Some(shallow_dir.path()),
        &["merge", "feature", "-m", "try merge"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("shallow history"));
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Git Test")
        .env("GIT_AUTHOR_EMAIL", "git@test.example")
        .env("GIT_COMMITTER_NAME", "Git Test")
        .env("GIT_COMMITTER_EMAIL", "git@test.example")
        .args(args)
        .output()
        .expect("spawn git")
}

#[test]
fn import_git_snapshot_from_subprocess() {
    if !git_available() {
        eprintln!("skip import_git_snapshot_from_subprocess: git not on PATH");
        return;
    }

    let parent = TempDir::new().unwrap();
    let git_dir = parent.path().join("git-repo");
    fs::create_dir_all(&git_dir).unwrap();

    assert_astvcs_ok(&run_git(&git_dir, &["init"]), "git init");
    fs::write(git_dir.join("hello.txt"), "hello from git\n").unwrap();
    assert_astvcs_ok(&run_git(&git_dir, &["add", "hello.txt"]), "git add");
    assert_astvcs_ok(
        &run_git(&git_dir, &["commit", "-m", "git baseline"]),
        "git commit",
    );

    let astvcs_dir = parent.path().join("astvcs-repo");
    fs::create_dir_all(&astvcs_dir).unwrap();
    assert_astvcs_ok(
        &run_astvcs(None, &["init", astvcs_dir.to_str().unwrap()]),
        "astvcs init",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(&astvcs_dir),
            &[
                "identity",
                "set",
                "--name",
                "Import Test",
                "--email",
                "import@test.example",
            ],
        ),
        "identity set",
    );

    let import = run_astvcs(
        Some(&astvcs_dir),
        &[
            "import-git",
            git_dir.to_str().unwrap(),
            "-m",
            "Imported git snapshot",
        ],
    );
    assert_astvcs_ok(&import, "import-git");
    let stdout = String::from_utf8_lossy(&import.stdout);
    assert!(
        stdout.contains("Imported git snapshot"),
        "expected import message in stdout: {stdout}"
    );

    let content = fs::read_to_string(astvcs_dir.join("hello.txt")).unwrap();
    assert_eq!(content, "hello from git\n");

    let repo = Repo::open(&astvcs_dir).unwrap();
    let history = repo.history(5).unwrap();
    assert_eq!(history.len(), 2, "empty root + import commit");
    assert_eq!(history[0].message, "Imported git snapshot");
    let files = repo.load_state_files(&history[0].id).unwrap();
    assert_eq!(
        fs::read_to_string(astvcs_dir.join("hello.txt")).unwrap(),
        match &files["hello.txt"].content {
            astvcs::FileContent::Text(blob) => blob.content.clone(),
            other => panic!("expected text blob, got {other:?}"),
        }
    );
}

#[test]
fn import_git_ignores_stray_untracked_files() {
    if !git_available() {
        eprintln!("skip import_git_ignores_stray_untracked_files: git not on PATH");
        return;
    }

    let parent = TempDir::new().unwrap();
    let repo_root = parent.path().join("in-place");
    fs::create_dir_all(&repo_root).unwrap();

    assert_astvcs_ok(&run_git(&repo_root, &["init"]), "git init");
    fs::write(repo_root.join("f.txt"), "from git\n").unwrap();
    assert_astvcs_ok(&run_git(&repo_root, &["add", "f.txt"]), "git add");
    assert_astvcs_ok(
        &run_git(&repo_root, &["commit", "-m", "git baseline"]),
        "git commit",
    );
    fs::write(
        repo_root.join("stray.txt"),
        "totally unrelated stray file, never touched git\n",
    )
    .unwrap();

    assert_astvcs_ok(
        &run_astvcs(None, &["init", repo_root.to_str().unwrap()]),
        "astvcs init",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(&repo_root),
            &[
                "identity",
                "set",
                "--name",
                "Import Test",
                "--email",
                "import@test.example",
            ],
        ),
        "identity set",
    );

    let import = run_astvcs(
        Some(&repo_root),
        &["import-git", ".", "-m", "import in-place with stray file"],
    );
    assert_astvcs_ok(&import, "import-git in-place");

    let astvcs_repo = Repo::open(&repo_root).unwrap();
    let history = astvcs_repo.history(5).unwrap();
    assert_eq!(history[0].message, "import in-place with stray file");
    let files = astvcs_repo.load_state_files(&history[0].id).unwrap();
    assert_eq!(files.len(), 1, "manifest should contain only git paths");
    assert!(files.contains_key("f.txt"));
    assert!(!files.contains_key("stray.txt"));
}

#[test]
fn import_git_does_not_commit_skipped_binary_stray() {
    if !git_available() {
        eprintln!("skip import_git_does_not_commit_skipped_binary_stray: git not on PATH");
        return;
    }

    let parent = TempDir::new().unwrap();
    let git_dir = parent.path().join("git-repo");
    fs::create_dir_all(&git_dir).unwrap();

    assert_astvcs_ok(&run_git(&git_dir, &["init"]), "git init");
    fs::write(git_dir.join("hello.txt"), "hello from git\n").unwrap();
    assert_astvcs_ok(&run_git(&git_dir, &["add", "hello.txt"]), "git add");
    assert_astvcs_ok(
        &run_git(&git_dir, &["commit", "-m", "git baseline"]),
        "git commit",
    );

    let astvcs_dir = parent.path().join("astvcs-repo");
    fs::create_dir_all(&astvcs_dir).unwrap();
    assert_astvcs_ok(
        &run_astvcs(None, &["init", astvcs_dir.to_str().unwrap()]),
        "astvcs init",
    );
    assert_astvcs_ok(
        &run_astvcs(
            Some(&astvcs_dir),
            &[
                "identity",
                "set",
                "--name",
                "Import Test",
                "--email",
                "import@test.example",
            ],
        ),
        "identity set",
    );

    let mut binary = vec![0u8, 1, 2, 3];
    binary.extend_from_slice(b"stray binary never in git");
    fs::write(astvcs_dir.join("stray.bin"), &binary).unwrap();

    let import = run_astvcs(
        Some(&astvcs_dir),
        &[
            "import-git",
            git_dir.to_str().unwrap(),
            "-m",
            "Imported git snapshot",
        ],
    );
    assert_astvcs_ok(&import, "import-git");

    let repo = Repo::open(&astvcs_dir).unwrap();
    let history = repo.history(5).unwrap();
    let files = repo.load_state_files(&history[0].id).unwrap();
    assert_eq!(files.len(), 1);
    assert!(files.contains_key("hello.txt"));
    assert!(!files.contains_key("stray.bin"));
}

#[test]
fn cli_diff_view_writes_html_with_alignment() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    fs::write(root.join("main.rs"), "fn foo() {\n    let x = 1;\n}\n").unwrap();
    assert_astvcs_ok(
        &run_astvcs(Some(root), &["commit", "-m", "baseline"]),
        "baseline",
    );
    fs::write(root.join("main.rs"), "fn foo() {\n    let y = 1;\n}\n").unwrap();

    let out = Command::new(astvcs_bin())
        .arg("--repo")
        .arg(root)
        .args(["diff", "--view", "main.rs"])
        .env("ASTVCS_NO_BROWSER", "1")
        .output()
        .expect("spawn astvcs");
    assert_astvcs_ok(&out, "diff --view");
    let html_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        html_path.ends_with(".html"),
        "expected HTML path on stdout, got {html_path:?}"
    );
    let html = fs::read_to_string(&html_path).expect("read viewer HTML");
    assert!(
        html.contains("astvcs diff"),
        "missing viewer chrome: {}",
        &html[..html.len().min(200)]
    );
    assert!(html.contains("main.rs"), "missing path in embedded JSON");
    assert!(
        html.contains("rename") || html.contains("EditLiteral") || html.contains("edit literal"),
        "expected rename/edit intent in embedded JSON"
    );
    assert!(
        html.contains("\"alignment\"") || html.contains("\\\"alignment\\\""),
        "expected alignment array in embedded JSON"
    );
    assert!(
        html.contains("Match") || html.contains("match") || html.contains("\"kind\""),
        "expected alignment kind markers in embedded JSON"
    );
    for control in [
        "Next change",
        "Previous change",
        "Show full tree",
        "Collapse unchanged",
        "Expand changed",
        "Keyboard help",
    ] {
        assert!(html.contains(control), "missing viewer control {control}");
    }
    assert!(html.contains("prefers-reduced-motion"));
    assert!(html.contains("aria-live"));
    assert!(html.contains("unchanged node"));
}

#[test]
fn cli_diff_defaults_to_compact_intents_and_details_restores_mutations() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    fs::write(root.join("main.rs"), "fn sample() { let value = 1; }\n").unwrap();
    Repo::open(root).unwrap().commit("baseline").unwrap();
    fs::write(root.join("main.rs"), "fn sample() { let renamed = 1; }\n").unwrap();

    let compact = run_astvcs(Some(root), &["diff", "main.rs"]);
    assert_astvcs_ok(&compact, "compact diff");
    let compact_stdout = String::from_utf8_lossy(&compact.stdout);
    assert!(
        compact_stdout.contains("rename `value` to `renamed`"),
        "{compact_stdout}"
    );
    assert!(!compact_stdout.contains("mutations:"), "{compact_stdout}");

    let detailed = run_astvcs(Some(root), &["--details", "diff", "main.rs"]);
    assert_astvcs_ok(&detailed, "detailed diff");
    let detailed_stdout = String::from_utf8_lossy(&detailed.stdout);
    assert!(detailed_stdout.contains("mutations:"), "{detailed_stdout}");
    assert!(
        detailed_stdout.contains("RenameIdentifier"),
        "{detailed_stdout}"
    );
}

#[test]
fn cli_diff_view_large_file_keeps_change_first_controls() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    Repo::init_with_identity(root).unwrap();
    let before = (0..300)
        .map(|i| format!("fn item_{i}() -> i32 {{ {i} }}\n"))
        .collect::<String>();
    fs::write(root.join("large.rs"), &before).unwrap();
    Repo::open(root).unwrap().commit("baseline").unwrap();
    let after = before.replace(
        "fn item_150() -> i32 { 150 }",
        "fn item_150() -> i32 { 999 }",
    );
    fs::write(root.join("large.rs"), after).unwrap();

    let out = Command::new(astvcs_bin())
        .arg("--repo")
        .arg(root)
        .args(["diff", "--view", "large.rs"])
        .env("ASTVCS_NO_BROWSER", "1")
        .output()
        .expect("spawn astvcs");
    assert_astvcs_ok(&out, "large diff --view");
    let html_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let html = fs::read_to_string(html_path).unwrap();
    assert!(html.contains("large.rs"));
    assert!(html.contains("Next change"));
    assert!(html.contains("unchanged node"));
}
