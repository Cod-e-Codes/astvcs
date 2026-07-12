//! Seeded random repository histories for reachability, fsck, and lifecycle stress.

mod common;

use astvcs::diff::diff_graphs;
use astvcs::frontend::parse_source;
use astvcs::store::Repo;
use common::{
    RUST_CALC_BASE, RUST_CALC_PATH, assert_fsck_clean, proptest_cases, rust_calc_with_y_delta,
    rust_calc_with_z_delta,
};
use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const SMOKE_SEED: u64 = 2_777_501_440;
const SMOKE_OPS: usize = 50;
const LONG_OPS: usize = 200;

#[derive(Clone, Copy, Debug)]
enum Op {
    CommitEdit,
    CreateBranch,
    CheckoutBranch,
    TryMerge,
    Fsck,
    GcDryRun,
    DiffHead,
    Repack,
}

struct HistoryModel {
    repo: Repo,
    root: PathBuf,
    rng: StdRng,
    branches: Vec<String>,
    current: String,
    calc_source: String,
    next_branch: u32,
}

impl HistoryModel {
    fn new(root: &Path, seed: u64) -> Self {
        let repo = Repo::init_with_identity(root).expect("init");
        std::fs::write(root.join(RUST_CALC_PATH), RUST_CALC_BASE).expect("write base");
        repo.commit("init").expect("commit init");
        Self {
            repo,
            root: root.to_path_buf(),
            rng: StdRng::seed_from_u64(seed),
            branches: vec!["main".to_string()],
            current: "main".to_string(),
            calc_source: RUST_CALC_BASE.to_string(),
            next_branch: 0,
        }
    }

    fn run(&mut self, ops: usize) {
        for step in 0..ops {
            let op = self.pick_op();
            self.apply_op(op)
                .unwrap_or_else(|err| panic!("step {step} op {op:?} failed: {err}"));
            if step % 10 == 9 {
                assert_fsck_clean(&self.repo);
            }
        }
        self.repair_working_tree();
        assert_fsck_clean(&self.repo);
    }

    fn repair_working_tree(&mut self) {
        self.repo
            .checkout_branch_with_force(&self.current, true)
            .expect("repair checkout");
        self.calc_source =
            std::fs::read_to_string(self.root.join(RUST_CALC_PATH)).expect("read calc.rs");
    }

    fn pick_op(&mut self) -> Op {
        const OPS: [Op; 8] = [
            Op::CommitEdit,
            Op::CreateBranch,
            Op::CheckoutBranch,
            Op::TryMerge,
            Op::Fsck,
            Op::GcDryRun,
            Op::DiffHead,
            Op::Repack,
        ];
        OPS[self.rng.random_range(0..OPS.len())]
    }

    fn apply_op(&mut self, op: Op) -> Result<(), String> {
        match op {
            Op::CommitEdit => self.commit_edit(),
            Op::CreateBranch => self.create_branch(),
            Op::CheckoutBranch => self.checkout_branch(),
            Op::TryMerge => self.try_merge(),
            Op::Fsck => {
                assert_fsck_clean(&self.repo);
                Ok(())
            }
            Op::GcDryRun => self
                .repo
                .gc(false, false)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Op::DiffHead => self.diff_head(),
            Op::Repack => self.repo.repack().map(|_| ()).map_err(|e| e.to_string()),
        }
    }

    fn commit_edit(&mut self) -> Result<(), String> {
        let delta: i32 = self.rng.random_range(1..=5);
        self.calc_source = if self.rng.random_bool(0.5) {
            rust_calc_with_y_delta(delta)
        } else {
            rust_calc_with_z_delta(-delta)
        };
        parse_source(RUST_CALC_PATH, &self.calc_source)
            .map_err(|e| format!("generated calc source must parse: {e}"))?;
        std::fs::write(self.root.join(RUST_CALC_PATH), &self.calc_source)
            .map_err(|e| e.to_string())?;
        self.repo
            .commit(&format!("edit on {} delta {delta}", self.current))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn create_branch(&mut self) -> Result<(), String> {
        let name = format!("b{}", self.next_branch);
        self.next_branch += 1;
        self.repo
            .create_branch(&name, None)
            .map_err(|e| e.to_string())?;
        self.branches.push(name);
        Ok(())
    }

    fn checkout_branch(&mut self) -> Result<(), String> {
        if self.branches.len() <= 1 {
            return Ok(());
        }
        let idx = self.rng.random_range(0..self.branches.len());
        let name = self.branches[idx].clone();
        self.repo
            .checkout_branch_with_force(&name, true)
            .map_err(|e| e.to_string())?;
        self.current = name;
        self.calc_source =
            std::fs::read_to_string(self.root.join(RUST_CALC_PATH)).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn try_merge(&mut self) -> Result<(), String> {
        if self.branches.len() <= 1 {
            return Ok(());
        }
        let candidates: Vec<String> = self
            .branches
            .iter()
            .filter(|name| *name != &self.current)
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Ok(());
        }
        let target = &candidates[self.rng.random_range(0..candidates.len())];
        match self.repo.merge_branch_with_resolutions_force(
            target,
            &format!("merge {target} into {}", self.current),
            &[],
            true,
            false,
        ) {
            Ok(_) => Ok(()),
            Err(err) if err.kind == astvcs::store::RepoErrorKind::MergeConflict => Ok(()),
            Err(err) if err.to_string().contains("already up to date") => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }

    fn diff_head(&mut self) -> Result<(), String> {
        let head = self.repo.head_state().map_err(|e| e.to_string())?;
        let parent = self
            .repo
            .load_timeline_entry(&head)
            .map_err(|e| e.to_string())?
            .parent
            .ok_or_else(|| "head has no parent".to_string())?;
        let head_files = self
            .repo
            .load_state_files(&head)
            .map_err(|e| e.to_string())?;
        let parent_files = self
            .repo
            .load_state_files(&parent)
            .map_err(|e| e.to_string())?;
        for path in head_files
            .keys()
            .chain(parent_files.keys())
            .collect::<HashSet<_>>()
        {
            if let (Some(old), Some(new)) = (parent_files.get(path), head_files.get(path))
                && let (astvcs::FileContent::Ast(old_g), astvcs::FileContent::Ast(new_g)) =
                    (&old.content, &new.content)
            {
                let _diff = diff_graphs(old_g, new_g);
            }
        }
        std::fs::read_to_string(self.root.join(RUST_CALC_PATH))
            .map_err(|e| e.to_string())
            .and_then(|source| {
                parse_source(RUST_CALC_PATH, &source)
                    .map_err(|e| format!("working calc must parse: {e}"))?;
                Ok(())
            })?;
        Ok(())
    }
}

#[test]
fn history_smoke_seeded_repo() {
    let dir = TempDir::new().expect("tempdir");
    let mut model = HistoryModel::new(dir.path(), SMOKE_SEED);
    model.run(SMOKE_OPS);
}

#[test]
#[ignore = "long random history; run with cargo test history_long -- --ignored"]
fn history_long_random_repo() {
    let dir = TempDir::new().expect("tempdir");
    let seed = std::env::var("HISTORY_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(SMOKE_SEED);
    let ops = std::env::var("HISTORY_OPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(LONG_OPS);
    eprintln!(
        "history_long_random_repo: seed={seed} ops={ops} proptest_cases={}",
        proptest_cases()
    );
    let mut model = HistoryModel::new(dir.path(), seed);
    model.run(ops);
}
