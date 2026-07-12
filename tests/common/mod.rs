//! Shared helpers for property, history, and differential integration tests.
#![allow(dead_code)]

use astvcs::diff::diff_graphs;
use astvcs::frontend::{FileContent, parse_source};
use astvcs::graph::AstGraph;
use astvcs::merge::{MergeOutcome, merge_files};
use astvcs::store::{BlobStore, FsckOptions, FsckReport, Repo};
use astvcs::unparse;
use proptest::prelude::*;
use std::path::Path;

pub const RUST_CALC_PATH: &str = "calc.rs";

pub const RUST_CALC_BASE: &str = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a;\n    z\n}\n";

/// Default proptest case count; override with `PROPTEST_CASES`.
pub fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64)
}

pub fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: proptest_cases(),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

pub fn format_signed_delta(delta: i32) -> String {
    if delta >= 0 {
        format!(" + {delta}")
    } else {
        format!(" - {}", delta.unsigned_abs())
    }
}

pub fn rust_calc_with_y_delta(delta: i32) -> String {
    let tail = format_signed_delta(delta);
    format!(
        "pub fn process(a: i32, b: i32, c: i32) -> i32 {{\n    let x = a + b;\n    let y = x * c{tail};\n    let z = y - a;\n    z\n}}\n"
    )
}

pub fn rust_calc_with_z_delta(delta: i32) -> String {
    let tail = format_signed_delta(delta);
    format!(
        "pub fn process(a: i32, b: i32, c: i32) -> i32 {{\n    let x = a + b;\n    let y = x * c;\n    let z = y - a{tail};\n    z\n}}\n"
    )
}

pub fn rust_calc_renamed() -> String {
    RUST_CALC_BASE.replace("process", "compute")
}

pub fn parse_rust_calc(source: &str) -> AstGraph {
    parse_source(RUST_CALC_PATH, source).expect("parse calc.rs")
}

pub fn assert_graph_valid(graph: &AstGraph) {
    graph.validate().expect("graph validate");
}

pub fn assert_merged_ast_parseable(path: &str, graph: &AstGraph) {
    assert_graph_valid(graph);
    let text = unparse(graph);
    let reparsed = parse_source(path, &text).expect("merged source must parse");
    assert_graph_valid(&reparsed);
    let drift = diff_graphs(graph, &reparsed);
    assert!(
        drift.mutations.is_empty(),
        "structural drift after unparse/re-parse: {:?}",
        drift.mutations
    );
}

pub fn merge_outcomes_equivalent(left: &MergeOutcome, right: &MergeOutcome) -> bool {
    match (left, right) {
        (MergeOutcome::Merged(l), MergeOutcome::Merged(r)) => l.semantic_eq(r),
        (MergeOutcome::Conflict(l), MergeOutcome::Conflict(r)) => {
            l.message == r.message && l.overlapping.len() == r.overlapping.len()
        }
        _ => false,
    }
}

pub fn merge_three_way_deterministic(
    base: &FileContent,
    left: &FileContent,
    right: &FileContent,
) -> MergeOutcome {
    let first = merge_files(base, left, right);
    let second = merge_files(base, left, right);
    assert!(
        merge_outcomes_equivalent(&first, &second),
        "merge must be deterministic: {first:?} vs {second:?}"
    );
    first
}

pub fn assert_disjoint_calc_merge(y_delta: i32, z_delta: i32) {
    let base = parse_rust_calc(RUST_CALC_BASE);
    let left_src = rust_calc_with_y_delta(y_delta);
    let right_src = rust_calc_with_z_delta(-z_delta);
    let left = parse_rust_calc(&left_src);
    let right = parse_rust_calc(&right_src);

    let base_content = FileContent::Ast(base);
    let left_content = FileContent::Ast(left);
    let right_content = FileContent::Ast(right);

    let outcome = merge_three_way_deterministic(&base_content, &left_content, &right_content);
    let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
        panic!("expected clean merge for y_delta={y_delta} z_delta={z_delta}, got {outcome:?}");
    };
    assert_merged_ast_parseable(RUST_CALC_PATH, &merged);
    let text = unparse(&merged);
    assert!(
        !text.contains(",,"),
        "merged source must not contain phantom commas: {text:?}"
    );
}

pub fn assert_fsck_clean(repo: &Repo) {
    let report = repo.fsck(FsckOptions::default()).expect("fsck");
    assert_fsck_report_clean(&report);
}

pub fn assert_fsck_report_clean(report: &FsckReport) {
    assert!(report.is_clean(), "fsck findings: {:?}", report.findings);
}

pub fn assert_blob_ast_roundtrip(store: &BlobStore, graph: &AstGraph) {
    let original = FileContent::Ast(graph.clone());
    let id = store.write(&original).expect("blob write");
    let loaded = store.write(&original).expect("blob dedup write");
    assert_eq!(id, loaded);
    let read = store.read(&id).expect("blob read");
    assert!(
        original.semantic_eq(&read),
        "blob roundtrip must preserve semantics"
    );
}

pub fn assert_snapshot_roundtrip(graph: &AstGraph) {
    let snap = graph.to_snapshot();
    let back = AstGraph::from_snapshot(snap);
    assert_graph_valid(&back);
    assert!(
        FileContent::Ast(graph.clone()).semantic_eq(&FileContent::Ast(back)),
        "snapshot roundtrip must preserve semantics"
    );
}

pub fn working_tree_bytes(repo_root: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(repo_root.join(rel)).expect("read working tree file")
}
