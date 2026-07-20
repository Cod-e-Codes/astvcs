//! Tests for the Git merge and external-diff driver binaries.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn merge_driver_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_astvcs-merge-driver"))
}

fn diff_driver_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_astvcs-diff-driver"))
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write temp file");
    path
}

fn run_merge(base: &Path, ours: &Path, theirs: &Path, display: &str) -> std::process::Output {
    Command::new(merge_driver_bin())
        .args([
            base.to_str().unwrap(),
            ours.to_str().unwrap(),
            theirs.to_str().unwrap(),
            display,
        ])
        .output()
        .expect("spawn astvcs-merge-driver")
}

fn run_diff(path: &str, old: &Path, new: &Path) -> std::process::Output {
    Command::new(diff_driver_bin())
        .args([
            path,
            old.to_str().unwrap(),
            "oldhex",
            "100644",
            new.to_str().unwrap(),
            "newhex",
            "100644",
        ])
        .output()
        .expect("spawn astvcs-diff-driver")
}

#[test]
fn merge_driver_resolves_disjoint_structural_edits() {
    let dir = TempDir::new().expect("tempdir");
    let base = write(
        dir.path(),
        "base.rs",
        "fn process(item: i32, count: i32) -> i32 {\n    let total = item * count;\n    total\n}\n",
    );
    let ours = write(
        dir.path(),
        "ours.rs",
        "fn process(item: i32, qty: i32) -> i32 {\n    let total = item * qty;\n    total\n}\n",
    );
    let theirs = write(
        dir.path(),
        "theirs.rs",
        "fn process(item: i32, count: i32) -> i32 {\n    let total = item + count;\n    total\n}\n",
    );

    let out = run_merge(&base, &ours, &theirs, "proc.rs");
    assert!(
        out.status.success(),
        "expected clean merge: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let merged = fs::read_to_string(&ours).expect("read merged ours");
    assert!(
        merged.contains("qty") && merged.contains("item + qty"),
        "expected both rename and operator edit in:\n{merged}"
    );
}

#[test]
fn merge_driver_conflicts_on_overlapping_literal_edits() {
    let dir = TempDir::new().expect("tempdir");
    let base = write(
        dir.path(),
        "base.rs",
        "fn calc(x: i32) -> i32 {\n    x + 1\n}\n",
    );
    let ours = write(
        dir.path(),
        "ours.rs",
        "fn calc(x: i32) -> i32 {\n    x + 2\n}\n",
    );
    let theirs = write(
        dir.path(),
        "theirs.rs",
        "fn calc(x: i32) -> i32 {\n    x + 5\n}\n",
    );
    let ours_before = fs::read_to_string(&ours).expect("read ours");

    let out = run_merge(&base, &ours, &theirs, "calc.rs");
    assert!(
        !out.status.success(),
        "expected conflict exit status for overlapping edits"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conflict: calc.rs"),
        "expected focused conflict report: {stderr}"
    );
    assert!(
        stderr.contains("left") && stderr.contains("unchanged") && stderr.contains("unmerged"),
        "expected leave-%A / unmerged note: {stderr}"
    );
    let ours_after = fs::read_to_string(&ours).expect("read ours after conflict");
    assert_eq!(ours_before, ours_after, "conflict must leave %A untouched");
}

#[test]
fn merge_driver_usage_error_on_missing_args() {
    let out = Command::new(merge_driver_bin())
        .output()
        .expect("spawn merge driver");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("astvcs-merge-driver:"),
        "expected usage error: {stderr}"
    );
}

#[test]
fn diff_driver_prints_structural_intents() {
    let dir = TempDir::new().expect("tempdir");
    let old = write(
        dir.path(),
        "old.rs",
        "fn process(item: i32, qty: i32) -> i32 {\n    let total = item + qty;\n    total\n}\n",
    );
    let new = write(
        dir.path(),
        "new.rs",
        "fn process(item: i32, quantity: i32) -> i32 {\n    let total = item + quantity;\n    total\n}\n",
    );

    let out = run_diff("proc.rs", &old, &new);
    assert!(
        out.status.success(),
        "diff driver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("astvcs diff: proc.rs"),
        "expected path header: {stdout}"
    );
    assert!(
        stdout.contains("rename") && stdout.contains("quantity"),
        "expected rename intent: {stdout}"
    );
}

#[test]
fn diff_driver_omits_binary_content() {
    let dir = TempDir::new().expect("tempdir");
    let old = dir.path().join("old.bin");
    let new = dir.path().join("new.bin");
    fs::write(&old, b"a\0b").expect("write old binary");
    fs::write(&new, b"c\0d").expect("write new binary");

    let out = run_diff("blob.bin", &old, &new);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("binary file - content diff omitted"),
        "expected binary omission: {stdout}"
    );
}

#[test]
fn git_invokes_merge_driver_on_disjoint_edits() {
    if !git_available() {
        eprintln!("skipping git merge-driver e2e: git not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    init_git_repo(root);

    fs::write(
        root.join("proc.rs"),
        "fn process(item: i32, count: i32) -> i32 {\n    let total = item * count;\n    total\n}\n",
    )
    .expect("write base");
    fs::write(root.join(".gitattributes"), "*.rs merge=astvcs\n").expect("write attrs");
    git_commit_all(root, "base");

    run_git(root, &["checkout", "-b", "theirs"]);
    fs::write(
        root.join("proc.rs"),
        "fn process(item: i32, count: i32) -> i32 {\n    let total = item + count;\n    total\n}\n",
    )
    .expect("write theirs");
    git_commit_all(root, "theirs op");

    run_git(root, &["checkout", "main"]);
    fs::write(
        root.join("proc.rs"),
        "fn process(item: i32, qty: i32) -> i32 {\n    let total = item * qty;\n    total\n}\n",
    )
    .expect("write ours");
    git_commit_all(root, "ours rename");

    let driver = merge_driver_bin();
    // Git for Windows runs the driver via sh; backslashes are escape chars.
    let driver_path = driver.to_string_lossy().replace('\\', "/");
    run_git(
        root,
        &[
            "config",
            "merge.astvcs.name",
            "astvcs structural merge driver",
        ],
    );
    run_git(
        root,
        &[
            "config",
            "merge.astvcs.driver",
            &format!("\"{driver_path}\" %O %A %B %P"),
        ],
    );

    let merge = Command::new("git")
        .args(["merge", "theirs", "--no-edit"])
        .current_dir(root)
        .output()
        .expect("git merge");
    assert!(
        merge.status.success(),
        "git merge with driver should succeed: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let merged = fs::read_to_string(root.join("proc.rs")).expect("read merged");
    assert!(
        merged.contains("qty") && merged.contains("item + qty"),
        "expected both edits in:\n{merged}"
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn init_git_repo(root: &Path) {
    run_git(root, &["init", "-b", "main"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Test"]);
}

fn run_git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_commit_all(root: &Path, message: &str) {
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "-m", message]);
}
