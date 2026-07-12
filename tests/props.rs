//! Property-based tests for merge determinism, AST roundtrips, and serialization.

mod common;

use astvcs::frontend::{FileContent, parse_source};
use astvcs::merge::{MergeOutcome, language_merge_cases};
use astvcs::store::BlobStore;
use common::{
    RUST_CALC_BASE, RUST_CALC_PATH, assert_blob_ast_roundtrip, assert_disjoint_calc_merge,
    assert_fsck_clean, assert_merged_ast_parseable, assert_snapshot_roundtrip,
    merge_outcomes_equivalent, merge_three_way_deterministic, rust_calc_with_y_delta,
    rust_calc_with_z_delta,
};
use proptest::prelude::*;
use tempfile::TempDir;

proptest! {
    #![proptest_config(common::proptest_config())]

    #[test]
    fn merge_disjoint_literal_deltas_are_deterministic_and_parseable(
        y_delta in 1i32..=20,
        z_delta in 1i32..=20,
    ) {
        assert_disjoint_calc_merge(y_delta, z_delta);
    }

    #[test]
    fn ast_snapshot_roundtrip_preserves_semantics(y_delta in -20i32..=20) {
        let source = rust_calc_with_y_delta(y_delta);
        let graph = parse_source(RUST_CALC_PATH, &source).expect("parse");
        assert_snapshot_roundtrip(&graph);
    }

    #[test]
    fn blob_store_ast_roundtrip_preserves_semantics(z_delta in -20i32..=20) {
        let source = rust_calc_with_z_delta(z_delta);
        let graph = parse_source(RUST_CALC_PATH, &source).expect("parse");
        let dir = TempDir::new().expect("tempdir");
        let store = BlobStore::new(dir.path());
        assert_blob_ast_roundtrip(&store, &graph);
    }
}

#[test]
fn language_fixture_disjoint_merges_are_deterministic() {
    for case in language_merge_cases() {
        let base = parse_source(case.path, case.base).expect("parse base");
        let left = parse_source(case.path, case.left).expect("parse left");
        let right = parse_source(case.path, case.right).expect("parse right");
        let base_c = FileContent::Ast(base);
        let left_c = FileContent::Ast(left);
        let right_c = FileContent::Ast(right);
        let outcome = merge_three_way_deterministic(&base_c, &left_c, &right_c);
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("{}: expected merge, got {outcome:?}", case.label);
        };
        assert_merged_ast_parseable(case.path, &merged);
    }
}

#[test]
fn merge_commutes_for_disjoint_calc_edits() {
    let base = parse_source(RUST_CALC_PATH, RUST_CALC_BASE).expect("parse base");
    let left = parse_source(RUST_CALC_PATH, &rust_calc_with_y_delta(3)).expect("parse left");
    let right = parse_source(RUST_CALC_PATH, &rust_calc_with_z_delta(-2)).expect("parse right");
    let base_c = FileContent::Ast(base);
    let left_c = FileContent::Ast(left);
    let right_c = FileContent::Ast(right);

    let lr = merge_three_way_deterministic(&base_c, &left_c, &right_c);
    let rl = merge_three_way_deterministic(&base_c, &right_c, &left_c);
    assert!(
        merge_outcomes_equivalent(&lr, &rl),
        "disjoint edits should commute: {lr:?} vs {rl:?}"
    );
}

#[test]
fn checkout_roundtrip_leaves_working_tree_unchanged() {
    use astvcs::store::Repo;
    use common::working_tree_bytes;

    let dir = TempDir::new().expect("tempdir");
    let repo = Repo::init_with_identity(dir.path()).expect("init");
    std::fs::write(dir.path().join(RUST_CALC_PATH), RUST_CALC_BASE).expect("write");
    repo.commit("base").expect("commit");
    repo.create_branch("feature", None).expect("branch");
    repo.checkout_branch("feature").expect("checkout feature");
    std::fs::write(dir.path().join(RUST_CALC_PATH), rust_calc_with_y_delta(1))
        .expect("write feature");
    repo.commit("feature edit").expect("commit feature");
    let before = working_tree_bytes(dir.path(), RUST_CALC_PATH);
    repo.checkout_branch("main").expect("checkout main");
    repo.checkout_branch("feature")
        .expect("checkout feature again");
    let after = working_tree_bytes(dir.path(), RUST_CALC_PATH);
    assert_eq!(
        before, after,
        "checkout roundtrip must not change file bytes"
    );
    assert_fsck_clean(&repo);
}
