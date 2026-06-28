use crate::diff::{DiffResult, TextEdit, diff_graphs, diff_text};
use crate::frontend::FileContent;
use crate::graph::{AstGraph, Mutation, NodeId};
use crate::intent::{self};

/// Result of attempting to merge two file states.
#[derive(Clone, Debug)]
pub enum MergeOutcome {
    Merged(FileContent),
    Conflict(MergeConflict),
}

#[derive(Clone, Debug)]
pub struct MergeConflict {
    pub message: String,
    pub left_mutations: Vec<Mutation>,
    pub right_mutations: Vec<Mutation>,
    pub left_intent_lines: Vec<String>,
    pub right_intent_lines: Vec<String>,
    pub overlapping: Vec<MutationOverlap>,
    pub text_line: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct MutationOverlap {
    pub left_index: usize,
    pub right_index: usize,
    pub reason: OverlapReason,
}

#[derive(Clone, Debug)]
pub enum OverlapReason {
    SamePrimaryNode(NodeId),
    SameTouchedParent(NodeId),
    DeletionCoversEdit { deleted: NodeId, edited: NodeId },
    SameIntent(String),
}

impl std::fmt::Display for OverlapReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SamePrimaryNode(id) => write!(f, "same node {id}"),
            Self::SameTouchedParent(id) => write!(f, "same parent {id}"),
            Self::DeletionCoversEdit { deleted, edited } => {
                write!(f, "delete of {deleted} covers edit at {edited}")
            }
            Self::SameIntent(summary) => write!(f, "same intent: {summary}"),
        }
    }
}

impl MergeConflict {
    pub fn format_report(&self, path: &str) -> String {
        let mut out = format!("merge conflict in {path}: {}\n", self.message);
        if !self.left_intent_lines.is_empty() {
            out.push_str("\nleft (HEAD) intents from base:\n");
            for line in &self.left_intent_lines {
                out.push_str(&format!("{line}\n"));
            }
        } else if !self.left_mutations.is_empty() {
            out.push_str("\nleft (HEAD) intents from base:\n");
            for line in intent::format_intent_lines(None, &self.left_mutations) {
                out.push_str(&format!("{line}\n"));
            }
        }
        if !self.left_mutations.is_empty() {
            out.push_str("\nleft (HEAD) mutations from base:\n");
            for (i, m) in self.left_mutations.iter().enumerate() {
                out.push_str(&format!("  [{i}] {m:?}\n"));
            }
        }
        if !self.right_intent_lines.is_empty() {
            out.push_str("\nright (other branch) intents from base:\n");
            for line in &self.right_intent_lines {
                out.push_str(&format!("{line}\n"));
            }
        } else if !self.right_mutations.is_empty() {
            out.push_str("\nright (other branch) intents from base:\n");
            for line in intent::format_intent_lines(None, &self.right_mutations) {
                out.push_str(&format!("{line}\n"));
            }
        }
        if !self.right_mutations.is_empty() {
            out.push_str("\nright (other branch) mutations from base:\n");
            for (i, m) in self.right_mutations.iter().enumerate() {
                out.push_str(&format!("  [{i}] {m:?}\n"));
            }
        }
        if !self.overlapping.is_empty() {
            out.push_str("\noverlapping edit pairs:\n");
            for ov in &self.overlapping {
                out.push_str(&format!(
                    "  left [{}] + right [{}]: {}\n",
                    ov.left_index, ov.right_index, ov.reason
                ));
            }
        }
        if let Some(line) = self.text_line {
            out.push_str(&format!("\nconflicting line: {line}\n"));
        }
        out
    }
}

/// Per-path merge failure with structured conflict detail.
#[derive(Clone, Debug)]
pub struct PathMergeConflict {
    pub path: String,
    pub detail: MergeConflict,
}

impl PathMergeConflict {
    pub fn format_report(&self) -> String {
        self.detail.format_report(&self.path)
    }
}

/// Result of merging one path across base, head, and other branch.
#[derive(Clone, Debug)]
pub enum PathMergeOutcome {
    Keep(FileContent),
    Remove,
    Conflict(PathMergeConflict),
}

/// Three-way merge for a single tracked path (presence/absence included).
pub fn merge_path(
    path: &str,
    base: Option<&FileContent>,
    left: Option<&FileContent>,
    right: Option<&FileContent>,
) -> PathMergeOutcome {
    match (base, left, right) {
        (Some(b), Some(l), Some(r)) => match merge_files(b, l, r) {
            MergeOutcome::Merged(content) => PathMergeOutcome::Keep(content),
            MergeOutcome::Conflict(c) => PathMergeOutcome::Conflict(PathMergeConflict {
                path: path.to_string(),
                detail: c,
            }),
        },
        (None, Some(l), Some(r)) => {
            if l.semantic_eq(r) {
                PathMergeOutcome::Keep(l.clone())
            } else {
                PathMergeOutcome::Conflict(PathMergeConflict {
                    path: path.to_string(),
                    detail: MergeConflict {
                        message: "both branches added different content".into(),
                        left_mutations: vec![],
                        right_mutations: vec![],
                        left_intent_lines: vec![],
                        right_intent_lines: vec![],
                        overlapping: vec![],
                        text_line: None,
                    },
                })
            }
        }
        (None, Some(l), None) => PathMergeOutcome::Keep(l.clone()),
        (None, None, Some(r)) => PathMergeOutcome::Keep(r.clone()),
        (Some(b), Some(l), None) => {
            if l.semantic_eq(b) {
                PathMergeOutcome::Remove
            } else {
                PathMergeOutcome::Keep(l.clone())
            }
        }
        (Some(b), None, Some(r)) => {
            if r.semantic_eq(b) {
                PathMergeOutcome::Remove
            } else {
                PathMergeOutcome::Keep(r.clone())
            }
        }
        (_, None, None) => PathMergeOutcome::Remove,
    }
}

fn ast_merge_conflict(
    base: &AstGraph,
    message: String,
    left_mutations: Vec<Mutation>,
    right_mutations: Vec<Mutation>,
    overlapping: Vec<MutationOverlap>,
) -> MergeConflict {
    MergeConflict {
        message,
        left_intent_lines: intent::format_intent_lines(Some(base), &left_mutations),
        right_intent_lines: intent::format_intent_lines(Some(base), &right_mutations),
        left_mutations,
        right_mutations,
        overlapping,
        text_line: None,
    }
}

/// Merge two divergent states against a common base using structural disjointness.
pub fn merge_files(base: &FileContent, left: &FileContent, right: &FileContent) -> MergeOutcome {
    match (base, left, right) {
        (FileContent::Ast(b), FileContent::Ast(l), FileContent::Ast(r)) => merge_ast(b, l, r),
        (FileContent::Text(b), FileContent::Text(l), FileContent::Text(r)) => merge_text(b, l, r),
        _ => MergeOutcome::Conflict(MergeConflict {
            message: "cannot merge AST state with text blob state".into(),
            left_mutations: vec![],
            right_mutations: vec![],
            left_intent_lines: vec![],
            right_intent_lines: vec![],
            overlapping: vec![],
            text_line: None,
        }),
    }
}

/// Find mutation pairs from base->left and base->right diffs that cannot merge.
pub fn find_overlapping_mutations(
    base: &AstGraph,
    left: &DiffResult,
    right: &DiffResult,
) -> Vec<MutationOverlap> {
    let mut out = Vec::new();
    for (li, lm) in left.mutations.iter().enumerate() {
        for (ri, rm) in right.mutations.iter().enumerate() {
            if let Some(reason) = overlap_reason(base, lm, rm) {
                out.push(MutationOverlap {
                    left_index: li,
                    right_index: ri,
                    reason,
                });
            }
        }
    }
    out
}

fn merge_ast(base: &AstGraph, left: &AstGraph, right: &AstGraph) -> MergeOutcome {
    let left_content = FileContent::Ast(left.clone());
    let right_content = FileContent::Ast(right.clone());
    let base_content = FileContent::Ast(base.clone());

    if left_content.semantic_eq(&right_content) {
        return MergeOutcome::Merged(left_content);
    }
    if base_content.semantic_eq(&left_content) {
        return MergeOutcome::Merged(right_content);
    }
    if base_content.semantic_eq(&right_content) {
        return MergeOutcome::Merged(left_content);
    }

    let left_diff = diff_graphs(base, left);
    let right_diff = diff_graphs(base, right);
    let overlapping = find_overlapping_mutations(base, &left_diff, &right_diff);

    if !overlapping.is_empty() {
        return MergeOutcome::Conflict(ast_merge_conflict(
            base,
            "overlapping structural edits".into(),
            left_diff.mutations,
            right_diff.mutations,
            overlapping,
        ));
    }

    let mut merged = base.clone();
    let mut combined = left_diff.mutations.clone();
    combined.extend(right_diff.mutations.clone());
    if let Err(e) = merged.apply_batch(&combined) {
        return MergeOutcome::Conflict(ast_merge_conflict(
            base,
            e,
            left_diff.mutations,
            right_diff.mutations,
            vec![],
        ));
    }

    MergeOutcome::Merged(FileContent::Ast(merged))
}

fn merge_text(
    base: &crate::frontend::TextBlob,
    left: &crate::frontend::TextBlob,
    right: &crate::frontend::TextBlob,
) -> MergeOutcome {
    if left.content == right.content {
        return MergeOutcome::Merged(FileContent::Text(left.clone()));
    }
    if left.content == base.content {
        return MergeOutcome::Merged(FileContent::Text(right.clone()));
    }
    if right.content == base.content {
        return MergeOutcome::Merged(FileContent::Text(left.clone()));
    }

    let left_edits = diff_text(&base.content, &left.content);
    let right_edits = diff_text(&base.content, &right.content);

    let left_lines: std::collections::HashSet<usize> =
        left_edits.iter().map(|e| e.line()).collect();
    let right_lines: std::collections::HashSet<usize> =
        right_edits.iter().map(|e| e.line()).collect();

    for line in &left_lines {
        if right_lines.contains(line) {
            return MergeOutcome::Conflict(MergeConflict {
                message: format!("both branches edited line {line}"),
                left_mutations: vec![],
                right_mutations: vec![],
                left_intent_lines: vec![],
                right_intent_lines: vec![],
                overlapping: vec![],
                text_line: Some(*line),
            });
        }
    }

    let merged = apply_disjoint_edits(&base.content, &left_edits, &right_edits);
    MergeOutcome::Merged(FileContent::Text(crate::frontend::TextBlob::new(merged)))
}

fn apply_disjoint_edits(base: &str, left_edits: &[TextEdit], right_edits: &[TextEdit]) -> String {
    let mut lines: Vec<String> = base.lines().map(String::from).collect();
    let mut all: Vec<(usize, &TextEdit)> = left_edits
        .iter()
        .chain(right_edits.iter())
        .map(|e| (e.line(), e))
        .collect();
    all.sort_by_key(|(line, _)| std::cmp::Reverse(*line));
    for (_, edit) in all {
        match edit {
            TextEdit::ReplaceLine { line, new, .. } => {
                if *line < lines.len() {
                    lines[*line] = new.clone();
                }
            }
            TextEdit::DeleteLine { line, .. } => {
                if *line < lines.len() {
                    lines.remove(*line);
                }
            }
            TextEdit::InsertLine { line, content } => {
                let idx = (*line).min(lines.len());
                lines.insert(idx, content.clone());
            }
        }
    }
    let mut out = lines.join("\n");
    if base.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn overlap_reason(base: &AstGraph, a: &Mutation, b: &Mutation) -> Option<OverlapReason> {
    if are_disjoint_edits(a, b) {
        return None;
    }
    let ia = intent::classify_mutation(Some(base), a);
    let ib = intent::classify_mutation(Some(base), b);
    if intent::intents_disjoint(&ia, &ib) {
        return None;
    }
    if ia == ib {
        return Some(OverlapReason::SameIntent(intent::format_intent(
            Some(base),
            &ia,
        )));
    }
    if let Some((deleted, edited)) = deletion_covers_primary(base, a, b) {
        return Some(OverlapReason::DeletionCoversEdit { deleted, edited });
    }
    if let Some((deleted, edited)) = deletion_covers_primary(base, b, a) {
        return Some(OverlapReason::DeletionCoversEdit { deleted, edited });
    }
    match (a.primary_node(), b.primary_node()) {
        (Some(an), Some(bn)) if an == bn => Some(OverlapReason::SamePrimaryNode(an)),
        _ => match (a.touched_parent(), b.touched_parent()) {
            (Some(ap), Some(bp)) if ap == bp => {
                if touches_same_insert_site(a, b) {
                    Some(OverlapReason::SameTouchedParent(ap))
                } else {
                    None
                }
            }
            _ => None,
        },
    }
}

fn deletion_covers_primary(
    base: &AstGraph,
    delete: &Mutation,
    edit: &Mutation,
) -> Option<(NodeId, NodeId)> {
    let Mutation::DeleteSubtree {
        node_id: deleted, ..
    } = delete
    else {
        return None;
    };
    let edited = edit.primary_node()?;
    if *deleted != edited && base.is_ancestor_of(deleted, &edited) {
        Some((*deleted, edited))
    } else {
        None
    }
}

fn touches_same_insert_site(a: &Mutation, b: &Mutation) -> bool {
    match (a, b) {
        (
            Mutation::InsertSubtree {
                parent: p1,
                before: b1,
                ..
            },
            Mutation::InsertSubtree {
                parent: p2,
                before: b2,
                ..
            },
        ) => p1 == p2 && b1 == b2,
        (
            Mutation::DeleteSubtree { node_id: n1, .. },
            Mutation::DeleteSubtree { node_id: n2, .. },
        ) => n1 == n2,
        (Mutation::DeleteSubtree { node_id, .. }, Mutation::InsertSubtree { before, .. })
        | (Mutation::InsertSubtree { before, .. }, Mutation::DeleteSubtree { node_id, .. }) => {
            *before == Some(*node_id)
        }
        _ => false,
    }
}

fn are_disjoint_edits(a: &Mutation, b: &Mutation) -> bool {
    match (a, b) {
        (Mutation::RenameIdentifier { .. }, Mutation::InsertSubtree { .. })
        | (Mutation::InsertSubtree { .. }, Mutation::RenameIdentifier { .. })
        | (Mutation::EditPayload { .. }, Mutation::InsertSubtree { .. })
        | (Mutation::InsertSubtree { .. }, Mutation::EditPayload { .. })
        | (Mutation::RenameIdentifier { .. }, Mutation::EditPayload { .. })
        | (Mutation::EditPayload { .. }, Mutation::RenameIdentifier { .. })
        | (Mutation::SetTrivia { .. }, Mutation::EditPayload { .. })
        | (Mutation::EditPayload { .. }, Mutation::SetTrivia { .. })
        | (Mutation::SetTrivia { .. }, Mutation::RenameIdentifier { .. })
        | (Mutation::RenameIdentifier { .. }, Mutation::SetTrivia { .. })
        | (Mutation::SetTrivia { .. }, Mutation::InsertSubtree { .. })
        | (Mutation::InsertSubtree { .. }, Mutation::SetTrivia { .. })
        | (Mutation::SetRootTrailingTrivia { .. }, Mutation::EditPayload { .. })
        | (Mutation::EditPayload { .. }, Mutation::SetRootTrailingTrivia { .. })
        | (Mutation::SetRootTrailingTrivia { .. }, Mutation::RenameIdentifier { .. })
        | (Mutation::RenameIdentifier { .. }, Mutation::SetRootTrailingTrivia { .. })
        | (Mutation::SetRootTrailingTrivia { .. }, Mutation::InsertSubtree { .. })
        | (Mutation::InsertSubtree { .. }, Mutation::SetRootTrailingTrivia { .. }) => true,
        (
            Mutation::SetTrivia {
                parent: p1,
                child: c1,
                occurrence: o1,
                ..
            },
            Mutation::SetTrivia {
                parent: p2,
                child: c2,
                occurrence: o2,
                ..
            },
        ) if p1 == p2 && c1 == c2 && o1 == o2 => false,
        (Mutation::SetRootTrailingTrivia { .. }, Mutation::SetRootTrailingTrivia { .. }) => false,
        (Mutation::EditPayload { node_id: a, .. }, Mutation::EditPayload { node_id: b, .. })
        | (
            Mutation::RenameIdentifier { node_id: a, .. },
            Mutation::RenameIdentifier { node_id: b, .. },
        ) if a != b => true,
        (
            Mutation::InsertSubtree {
                parent: p1,
                before: b1,
                ..
            },
            Mutation::InsertSubtree {
                parent: p2,
                before: b2,
                ..
            },
        ) => p1 != p2 || b1 != b2,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::diff_graphs;
    use crate::frontend::parse_rust;

    #[test]
    fn disjoint_ast_edits_merge() {
        let base = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let x = 1;\n    let z = 2;\n}\n").unwrap();
        assert!(matches!(
            merge_files(
                &FileContent::Ast(base),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            ),
            MergeOutcome::Merged(_)
        ));
    }

    #[test]
    fn rename_vs_parent_delete_overlaps() {
        let base = parse_rust("fn foo() {\n    let x = 1;\n    let z = 2;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n    let z = 2;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let z = 2;\n}\n").unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Conflict(c) = outcome else {
            panic!("expected conflict when rename overlaps parent deletion");
        };
        assert!(!c.overlapping.is_empty(), "expected ancestry overlap");
        assert!(
            c.overlapping
                .iter()
                .any(|ov| matches!(ov.reason, OverlapReason::DeletionCoversEdit { .. })),
            "expected DeletionCoversEdit, got {:?}",
            c.overlapping
        );
        let report = c.format_report("main.rs");
        assert!(report.contains("delete"));
        assert!(report.contains("rename"));
    }

    #[test]
    fn overlapping_mutations_report_node_id() {
        let base = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let z = 1;\n}\n").unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Conflict(c) = outcome else {
            panic!("expected conflict");
        };
        assert!(!c.overlapping.is_empty(), "expected overlap pairs");
        assert!(!c.left_mutations.is_empty());
        assert!(!c.right_mutations.is_empty());
        let report = c.format_report("main.rs");
        assert!(report.contains("overlapping edit pairs"));
        assert!(report.contains("left ["));
        assert!(report.contains("intents from base"));
        assert!(report.contains("rename"));
    }

    #[test]
    fn sibling_rename_and_delete_merge() {
        let base = parse_rust("fn foo() {\n    let x = 1;\n    let z = 2;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n    let z = 2;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        assert!(matches!(
            merge_files(
                &FileContent::Ast(base),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            ),
            MergeOutcome::Merged(_)
        ));
    }

    #[test]
    fn trailing_comment_survives_sibling_literal_edit_merge() {
        use crate::unparser::unparse;

        let base = parse_rust("fn main() {\n    println!(\"Hello, World!\");\n}\n").unwrap();
        let left = parse_rust("fn main() {\n    println!(\"sup?\");\n}\n").unwrap();
        let right = parse_rust("fn main() {\n    println!(\"Hello, World!\"); // waddup fool\n}\n")
            .unwrap();
        let left_diff = diff_graphs(&base, &left);
        let right_diff = diff_graphs(&base, &right);
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("expected merge, left={left_diff:?} right={right_diff:?} outcome={outcome:?}");
        };
        let text = unparse(&merged);
        assert!(
            text.contains("// waddup fool"),
            "merged text missing comment payload: {text:?}\nleft={left_diff:?}\nright={right_diff:?}"
        );
        assert!(text.contains("\"sup?\""), "merged text: {text:?}");
    }

    #[test]
    fn conflicting_trailing_comments_on_same_slot_conflict() {
        let base = parse_rust("fn main() {\n    println!(\"a\");\n}\n").unwrap();
        let left = parse_rust("fn main() {\n    println!(\"a\"); // left\n}\n").unwrap();
        let right = parse_rust("fn main() {\n    println!(\"a\"); // right\n}\n").unwrap();
        assert!(
            matches!(
                merge_files(
                    &FileContent::Ast(base),
                    &FileContent::Ast(left),
                    &FileContent::Ast(right),
                ),
                MergeOutcome::Conflict(_)
            ),
            "expected conflict when both branches set different trailing comment trivia"
        );
    }

    #[test]
    fn block_comment_survives_sibling_literal_edit_merge() {
        use crate::unparser::unparse;

        let base = parse_rust("fn main() {\n    println!(\"Hello, World!\");\n}\n").unwrap();
        let left = parse_rust("fn main() {\n    println!(\"sup?\");\n}\n").unwrap();
        let right =
            parse_rust("fn main() {\n    println!(\"Hello, World!\"); /* waddup fool */\n}\n")
                .unwrap();
        let MergeOutcome::Merged(FileContent::Ast(merged)) = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        ) else {
            panic!("expected block comment merge");
        };
        let text = unparse(&merged);
        assert!(text.contains("waddup fool"), "merged: {text:?}");
        assert!(text.contains("\"sup?\""), "merged: {text:?}");
    }

    #[test]
    fn trailing_comment_before_next_statement_survives_literal_edit() {
        use crate::unparser::unparse;

        let base = parse_rust("fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let left = parse_rust("fn main() {\n    let x = 9;\n    let y = 2;\n}\n").unwrap();
        let right = parse_rust("fn main() {\n    let x = 1; // note\n    let y = 2;\n}\n").unwrap();
        let MergeOutcome::Merged(FileContent::Ast(merged)) = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        ) else {
            panic!("expected merge with comment before next statement");
        };
        let text = unparse(&merged);
        assert!(text.contains("// note"), "merged: {text:?}");
        assert!(text.contains("let x = 9;"), "merged: {text:?}");
    }

    #[test]
    fn sibling_literal_edits_merge() {
        let base = parse_rust(
            "fn labels() -> (&'static str, &'static str) {\n    (\"left\", \"right\")\n}\n",
        )
        .unwrap();
        let left = parse_rust(
            "fn labels() -> (&'static str, &'static str) {\n    (\"LEFT\", \"right\")\n}\n",
        )
        .unwrap();
        let right = parse_rust(
            "fn labels() -> (&'static str, &'static str) {\n    (\"left\", \"RIGHT\")\n}\n",
        )
        .unwrap();
        assert!(matches!(
            merge_files(
                &FileContent::Ast(base),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            ),
            MergeOutcome::Merged(_)
        ));
    }

    #[test]
    fn unchanged_side_short_circuits() {
        let base = parse_rust("pub mod core;\npub mod util;\n").unwrap();
        let left = parse_rust("//! doc\npub mod core;\npub mod util;\n").unwrap();
        let right = base.clone();
        assert!(matches!(
            merge_files(
                &FileContent::Ast(base),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            ),
            MergeOutcome::Merged(_)
        ));
    }

    #[test]
    fn text_three_way_disjoint_lines() {
        let base = crate::frontend::TextBlob::new("a\nb\nc\n".into());
        let left = crate::frontend::TextBlob::new("a\nB\nc\n".into());
        let right = crate::frontend::TextBlob::new("a\nb\nC\n".into());
        let outcome = merge_files(
            &FileContent::Text(base),
            &FileContent::Text(left),
            &FileContent::Text(right),
        );
        assert!(matches!(outcome, MergeOutcome::Merged(_)));
        if let MergeOutcome::Merged(FileContent::Text(t)) = outcome {
            assert!(t.content.contains('B'));
            assert!(t.content.contains('C'));
        }
    }

    #[test]
    fn text_conflict_on_same_line() {
        let base = crate::frontend::TextBlob::new("a\nb\nc\n".into());
        let left = crate::frontend::TextBlob::new("a\nX\nc\n".into());
        let right = crate::frontend::TextBlob::new("a\nY\nc\n".into());
        let outcome = merge_files(
            &FileContent::Text(base),
            &FileContent::Text(left),
            &FileContent::Text(right),
        );
        assert!(matches!(outcome, MergeOutcome::Conflict(_)));
        if let MergeOutcome::Conflict(c) = outcome {
            assert_eq!(c.text_line, Some(1));
        }
    }

    #[test]
    fn path_add_add_same_content() {
        let content = FileContent::Text(crate::frontend::TextBlob::new("new\n".into()));
        assert!(matches!(
            merge_path("f.txt", None, Some(&content), Some(&content)),
            PathMergeOutcome::Keep(_)
        ));
    }

    #[test]
    fn path_add_add_different_content_conflicts() {
        let left = FileContent::Text(crate::frontend::TextBlob::new("a\n".into()));
        let right = FileContent::Text(crate::frontend::TextBlob::new("b\n".into()));
        assert!(matches!(
            merge_path("f.txt", None, Some(&left), Some(&right)),
            PathMergeOutcome::Conflict(_)
        ));
    }

    #[test]
    fn path_delete_on_one_side_removed_when_other_unchanged() {
        let base = FileContent::Text(crate::frontend::TextBlob::new("x\n".into()));
        assert!(matches!(
            merge_path("f.txt", Some(&base), None, Some(&base)),
            PathMergeOutcome::Remove
        ));
        assert!(matches!(
            merge_path("f.txt", Some(&base), Some(&base), None),
            PathMergeOutcome::Remove
        ));
    }

    #[test]
    fn path_modify_delete_keeps_modification() {
        let base = FileContent::Text(crate::frontend::TextBlob::new("a\nb\nc\n".into()));
        let modified = FileContent::Text(crate::frontend::TextBlob::new("a\nB\nc\n".into()));
        assert!(matches!(
            merge_path("f.txt", Some(&base), Some(&modified), None),
            PathMergeOutcome::Keep(_)
        ));
    }
}
