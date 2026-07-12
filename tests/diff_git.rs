//! Differential tests against Git for curated semantic expectations.

mod common;

use astvcs::store::Repo;
use common::{
    RUST_CALC_BASE, RUST_CALC_PATH, rust_calc_renamed, rust_calc_with_y_delta,
    rust_calc_with_z_delta,
};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

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

fn git_merge_succeeds(root: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["merge", branch, "--no-edit"])
        .current_dir(root)
        .output()
        .expect("spawn git merge")
        .status
        .success()
}

struct TwinRepos {
    _dir: TempDir,
    ast_root: std::path::PathBuf,
    git_root: std::path::PathBuf,
    ast: Repo,
}

impl TwinRepos {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let ast_root = dir.path().join("astvcs");
        let git_root = dir.path().join("git");
        std::fs::create_dir_all(&ast_root).expect("mkdir astvcs");
        std::fs::create_dir_all(&git_root).expect("mkdir git");
        init_git_repo(&git_root);
        let ast = Repo::init_with_identity(&ast_root).expect("astvcs init");
        Self {
            _dir: dir,
            ast_root,
            git_root,
            ast,
        }
    }

    fn write_both(&self, path: &str, contents: &str) {
        std::fs::write(self.ast_root.join(path), contents).expect("write astvcs");
        std::fs::write(self.git_root.join(path), contents).expect("write git");
    }

    fn commit_both(&self, message: &str) {
        git_commit_all(&self.git_root, message);
        self.ast.commit(message).expect("astvcs commit");
    }

    fn create_branch_both(&self, name: &str) {
        self.ast
            .create_branch(name, None)
            .unwrap_or_else(|err| panic!("astvcs create branch {name}: {err}"));
        if name != "main" {
            run_git(&self.git_root, &["checkout", "-b", name]);
        }
    }

    fn checkout_both(&self, branch: &str) {
        self.ast
            .checkout_branch(branch)
            .unwrap_or_else(|err| panic!("astvcs checkout {branch}: {err}"));
        run_git(&self.git_root, &["checkout", branch]);
    }

    fn ast_merge(&self, branch: &str) -> bool {
        self.ast
            .merge_branch(branch, &format!("merge {branch}"))
            .is_ok()
    }
}

#[test]
fn git_and_astvcs_disjoint_calc_edits_diverge() {
    if !git_available() {
        eprintln!("skipping diff_git test: git not on PATH");
        return;
    }

    let repos = TwinRepos::new();
    repos.write_both(RUST_CALC_PATH, RUST_CALC_BASE);
    repos.commit_both("base");

    repos.create_branch_both("feature");
    repos.checkout_both("feature");
    repos.write_both(RUST_CALC_PATH, &rust_calc_with_z_delta(-1));
    repos.commit_both("feature z edit");

    repos.checkout_both("main");
    repos.write_both(RUST_CALC_PATH, &rust_calc_with_y_delta(1));
    repos.commit_both("main y edit");

    assert!(
        repos.ast_merge("feature"),
        "astvcs should merge disjoint statement edits in one function"
    );
    assert!(
        !git_merge_succeeds(&repos.git_root, "feature"),
        "git text merge should conflict on adjacent edits in the same function"
    );
}

#[test]
fn git_and_astvcs_same_line_edits_both_conflict() {
    if !git_available() {
        eprintln!("skipping diff_git test: git not on PATH");
        return;
    }

    let repos = TwinRepos::new();
    repos.write_both(RUST_CALC_PATH, RUST_CALC_BASE);
    repos.commit_both("base");

    repos.create_branch_both("feature");
    repos.checkout_both("feature");
    repos.write_both(RUST_CALC_PATH, &rust_calc_with_y_delta(2));
    repos.commit_both("feature y=+2");

    repos.checkout_both("main");
    repos.write_both(RUST_CALC_PATH, &rust_calc_with_y_delta(3));
    repos.commit_both("main y=+3");

    assert!(
        !repos.ast_merge("feature"),
        "astvcs should conflict when both branches edit the same expression"
    );
    assert!(
        !git_merge_succeeds(&repos.git_root, "feature"),
        "git should conflict when both branches edit the same line"
    );
}

#[test]
fn git_and_astvcs_rename_with_body_edit_both_merge() {
    if !git_available() {
        eprintln!("skipping diff_git test: git not on PATH");
        return;
    }

    let repos = TwinRepos::new();
    repos.write_both(RUST_CALC_PATH, RUST_CALC_BASE);
    repos.commit_both("base");

    repos.create_branch_both("feature");
    repos.checkout_both("feature");
    repos.write_both(RUST_CALC_PATH, &rust_calc_with_y_delta(1));
    repos.commit_both("feature body");

    repos.checkout_both("main");
    repos.write_both(RUST_CALC_PATH, &rust_calc_renamed());
    repos.commit_both("main rename");

    assert!(
        repos.ast_merge("feature"),
        "astvcs should merge rename with disjoint body edit"
    );
    assert!(
        git_merge_succeeds(&repos.git_root, "feature"),
        "git should also merge rename with disjoint body edit in this fixture"
    );
}
