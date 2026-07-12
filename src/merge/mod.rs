use crate::diff::{DiffResult, TextEdit, diff_graphs, diff_text};
use crate::frontend::FileContent;
use crate::graph::{AstGraph, Mutation, NodeId, NodeKind};
use crate::intent::{self};
use crate::store::{FileMode, TrackedFile};

mod language_merge_cases;
pub use language_merge_cases::{
    LanguageMergeCase, assert_disjoint_language_merge, language_merge_cases,
};

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

impl OverlapReason {
    fn concise(&self) -> &'static str {
        match self {
            Self::SamePrimaryNode(_) => "both edits target the same node",
            Self::SameTouchedParent(_) => "both edits target the same insertion site",
            Self::DeletionCoversEdit { .. } => "one edit deletes the other edit's context",
            Self::SameIntent(_) => "both sides make the same kind of edit differently",
        }
    }
}

/// How focused conflict output should describe resolution syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictResolutionStyle {
    None,
    Merge,
    RebaseContinue,
}

impl ConflictResolutionStyle {
    fn append_line(self, out: &mut String, path: &str) {
        let line = match self {
            Self::None => return,
            Self::Merge => format!("  resolve: --resolve {path}:ours or --resolve {path}:theirs\n"),
            Self::RebaseContinue => format!(
                "  resolve: rebase --continue --resolve {path}:ours or rebase --continue --resolve {path}:theirs\n"
            ),
        };
        out.push_str(&line);
    }
}

impl MergeConflict {
    pub fn format_report(&self, path: &str) -> String {
        self.format_report_with_labels(path, "left (HEAD)", "right (other branch)")
    }

    pub fn format_report_with_labels(
        &self,
        path: &str,
        left_label: &str,
        right_label: &str,
    ) -> String {
        let mut out = format!("merge conflict in {path}: {}\n", self.message);
        if !self.left_intent_lines.is_empty() {
            out.push_str(&format!("\n{left_label} intents from base:\n"));
            for line in &self.left_intent_lines {
                out.push_str(&format!("{line}\n"));
            }
        } else if !self.left_mutations.is_empty() {
            out.push_str(&format!("\n{left_label} intents from base:\n"));
            for line in intent::format_intent_lines(None, &self.left_mutations) {
                out.push_str(&format!("{line}\n"));
            }
        }
        if !self.left_mutations.is_empty() {
            out.push_str(&format!("\n{left_label} mutations from base:\n"));
            for (i, m) in self.left_mutations.iter().enumerate() {
                out.push_str(&format!("  [{i}] {m:?}\n"));
            }
        }
        if !self.right_intent_lines.is_empty() {
            out.push_str(&format!("\n{right_label} intents from base:\n"));
            for line in &self.right_intent_lines {
                out.push_str(&format!("{line}\n"));
            }
        } else if !self.right_mutations.is_empty() {
            out.push_str(&format!("\n{right_label} intents from base:\n"));
            for line in intent::format_intent_lines(None, &self.right_mutations) {
                out.push_str(&format!("{line}\n"));
            }
        }
        if !self.right_mutations.is_empty() {
            out.push_str(&format!("\n{right_label} mutations from base:\n"));
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

    pub fn format_focused_report(&self, path: &str) -> String {
        self.format_focused_report_with_labels(
            path,
            "ours",
            "theirs",
            ConflictResolutionStyle::Merge,
        )
    }

    pub fn format_focused_report_with_labels(
        &self,
        path: &str,
        left_label: &str,
        right_label: &str,
        resolution: ConflictResolutionStyle,
    ) -> String {
        const MAX_OVERLAPS: usize = 3;
        let mut out = format!("conflict: {path}\n  reason: {}\n", self.message);

        if let Some(line) = self.text_line {
            out.push_str(&format!("  overlapping line: {}\n", line + 1));
        }

        for overlap in self.overlapping.iter().take(MAX_OVERLAPS) {
            let ours = intent_line_at(
                &self.left_intent_lines,
                &self.left_mutations,
                overlap.left_index,
            );
            let theirs = intent_line_at(
                &self.right_intent_lines,
                &self.right_mutations,
                overlap.right_index,
            );
            out.push_str(&format!("  {left_label}: {ours}\n"));
            out.push_str(&format!("  {right_label}: {theirs}\n"));
            out.push_str(&format!("  overlap: {}\n", overlap.reason.concise()));
        }

        if self.overlapping.len() > MAX_OVERLAPS {
            out.push_str(&format!(
                "  {} more overlap example(s) omitted; use --details\n",
                self.overlapping.len() - MAX_OVERLAPS
            ));
        } else if self.overlapping.is_empty() {
            if let Some(line) = self.left_intent_lines.first() {
                out.push_str(&format!(
                    "  {left_label}: {}\n",
                    compact_existing_intent_line(line)
                ));
            }
            if let Some(line) = self.right_intent_lines.first() {
                out.push_str(&format!(
                    "  {right_label}: {}\n",
                    compact_existing_intent_line(line)
                ));
            }
        }

        resolution.append_line(&mut out, path);
        out
    }
}

fn intent_line_at(lines: &[String], mutations: &[Mutation], index: usize) -> String {
    let marker = format!("[{index}] ");
    if let Some(line) = lines.iter().find(|line| line.contains(&marker)) {
        return compact_existing_intent_line(line);
    }
    mutations
        .get(index)
        .map(|mutation| {
            let classified = intent::classify_mutation(None, mutation);
            intent::format_intent_compact(None, &classified)
        })
        .unwrap_or_else(|| "structural edit".into())
}

fn compact_existing_intent_line(line: &str) -> String {
    let label = line
        .trim()
        .split_once("] ")
        .map_or_else(|| line.trim(), |(_, label)| label);
    if label.starts_with("set trivia") || label == "set root trailing trivia" {
        return "update formatting".into();
    }
    if label.starts_with("insert ") {
        return label
            .split_once(" under ")
            .map_or(label, |(summary, _)| summary)
            .to_string();
    }
    if label.starts_with("move subtree") {
        return "move subtree".into();
    }
    if label.starts_with("move ") {
        return "move node".into();
    }
    if label.starts_with("reorder members") {
        return "reorder members".into();
    }
    label
        .split_once(" at ")
        .map_or(label, |(summary, _)| summary)
        .to_string()
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

/// Three-way merge including file mode metadata (executable, symlink).
pub fn merge_tracked_path(
    path: &str,
    base: Option<&TrackedFile>,
    left: Option<&TrackedFile>,
    right: Option<&TrackedFile>,
) -> PathMergeTrackedOutcome {
    if mode_kind_conflict(left, right) {
        return PathMergeTrackedOutcome::Conflict(PathMergeConflict {
            path: path.to_string(),
            detail: MergeConflict {
                message: "symlink and regular file conflict".into(),
                left_mutations: vec![],
                right_mutations: vec![],
                left_intent_lines: vec![],
                right_intent_lines: vec![],
                overlapping: vec![],
                text_line: None,
            },
        });
    }

    let content_out = merge_path(
        path,
        base.map(|t| &t.content),
        left.map(|t| &t.content),
        right.map(|t| &t.content),
    );
    match content_out {
        PathMergeOutcome::Keep(content) => match merge_file_mode(base, left, right) {
            Ok(mode) => PathMergeTrackedOutcome::Keep(TrackedFile::new(content, mode)),
            Err(message) => PathMergeTrackedOutcome::Conflict(PathMergeConflict {
                path: path.to_string(),
                detail: MergeConflict {
                    message,
                    left_mutations: vec![],
                    right_mutations: vec![],
                    left_intent_lines: vec![],
                    right_intent_lines: vec![],
                    overlapping: vec![],
                    text_line: None,
                },
            }),
        },
        PathMergeOutcome::Remove => PathMergeTrackedOutcome::Remove,
        PathMergeOutcome::Conflict(c) => PathMergeTrackedOutcome::Conflict(c),
    }
}

#[derive(Clone, Debug)]
pub enum PathMergeTrackedOutcome {
    Keep(TrackedFile),
    Remove,
    Conflict(PathMergeConflict),
}

fn mode_kind_conflict(left: Option<&TrackedFile>, right: Option<&TrackedFile>) -> bool {
    let left_symlink = left.is_some_and(|f| f.mode == FileMode::Symlink);
    let right_symlink = right.is_some_and(|f| f.mode == FileMode::Symlink);
    left_symlink != right_symlink
}

fn merge_file_mode(
    base: Option<&TrackedFile>,
    left: Option<&TrackedFile>,
    right: Option<&TrackedFile>,
) -> Result<FileMode, String> {
    let left_mode = left.map(|f| f.mode);
    let right_mode = right.map(|f| f.mode);
    match (left_mode, right_mode) {
        (Some(l), Some(r)) if l == r => Ok(l),
        (Some(l), None) => Ok(l),
        (None, Some(r)) => Ok(r),
        (None, None) => Ok(FileMode::Regular),
        (Some(l), Some(r)) => {
            let base_mode = base.map(|f| f.mode);
            let left_changed = base_mode != Some(l);
            let right_changed = base_mode != Some(r);
            match (left_changed, right_changed) {
                (true, false) => Ok(l),
                (false, true) => Ok(r),
                (false, false) => Ok(l),
                (true, true) => Err("both branches changed file mode".into()),
            }
        }
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
        (FileContent::Binary(b), FileContent::Binary(l), FileContent::Binary(r)) => {
            merge_binary(b, l, r)
        }
        (FileContent::Symlink(b), FileContent::Symlink(l), FileContent::Symlink(r)) => {
            merge_symlink(b, l, r)
        }
        _ => MergeOutcome::Conflict(MergeConflict {
            message: "cannot merge different content kinds (ast, text, binary, symlink)".into(),
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
    let combined = combine_mutation_batches(&left_diff.mutations, &right_diff.mutations);
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

/// Opaque whole-file merge for binary blobs: one side wins or they conflict.
fn merge_binary(
    base: &crate::frontend::BinaryBlob,
    left: &crate::frontend::BinaryBlob,
    right: &crate::frontend::BinaryBlob,
) -> MergeOutcome {
    if left.bytes == right.bytes {
        return MergeOutcome::Merged(FileContent::Binary(left.clone()));
    }
    if left.bytes == base.bytes {
        return MergeOutcome::Merged(FileContent::Binary(right.clone()));
    }
    if right.bytes == base.bytes {
        return MergeOutcome::Merged(FileContent::Binary(left.clone()));
    }
    MergeOutcome::Conflict(MergeConflict {
        message: "both branches modified binary file".into(),
        left_mutations: vec![],
        right_mutations: vec![],
        left_intent_lines: vec![],
        right_intent_lines: vec![],
        overlapping: vec![],
        text_line: None,
    })
}

fn merge_symlink(
    base: &crate::frontend::SymlinkBlob,
    left: &crate::frontend::SymlinkBlob,
    right: &crate::frontend::SymlinkBlob,
) -> MergeOutcome {
    if left.target == right.target {
        return MergeOutcome::Merged(FileContent::Symlink(left.clone()));
    }
    if left.target == base.target {
        return MergeOutcome::Merged(FileContent::Symlink(right.clone()));
    }
    if right.target == base.target {
        return MergeOutcome::Merged(FileContent::Symlink(left.clone()));
    }
    MergeOutcome::Conflict(MergeConflict {
        message: "both branches modified symlink target".into(),
        left_mutations: vec![],
        right_mutations: vec![],
        left_intent_lines: vec![],
        right_intent_lines: vec![],
        overlapping: vec![],
        text_line: None,
    })
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
    if mutations_merge_equivalent(a, b) {
        return None;
    }
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
                before_occurrence: o1,
                ..
            },
            Mutation::InsertSubtree {
                parent: p2,
                before: b2,
                before_occurrence: o2,
                ..
            },
        ) => p1 == p2 && b1 == b2 && o1 == o2,
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

/// True when two mutations are the same edit for merge overlap and batch-combine purposes.
fn mutations_merge_equivalent(a: &Mutation, b: &Mutation) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (a, b),
        (
            Mutation::InsertSubtree {
                parent: p1,
                before: b1,
                before_occurrence: bo1,
                node: n1,
                ..
            },
            Mutation::InsertSubtree {
                parent: p2,
                before: b2,
                before_occurrence: bo2,
                node: n2,
                ..
            },
        ) if p1 == p2 && n1.kind == n2.kind && n1.payload == n2.payload && b1 == b2 && bo1 == bo2
    )
}

fn is_punctuation_token_insert(mutation: &Mutation) -> bool {
    let Mutation::InsertSubtree {
        node, descendants, ..
    } = mutation
    else {
        return false;
    };
    node.kind == NodeKind::Token
        && node.children.is_empty()
        && descendants.iter().all(|n| n.children.is_empty())
        && node
            .payload
            .chars()
            .all(|c| c.is_ascii_punctuation() || c.is_ascii_whitespace())
        && !node.payload.is_empty()
}

fn punctuation_insert_at(
    mutations: &[Mutation],
    parent: NodeId,
    before: Option<NodeId>,
    before_occurrence: Option<u32>,
) -> bool {
    mutations.iter().any(|m| {
        matches!(
            m,
            Mutation::InsertSubtree {
                parent: p,
                before: b,
                before_occurrence: o,
                ..
            } if *p == parent && *b == before && *o == before_occurrence
        ) && is_punctuation_token_insert(m)
    })
}

fn omit_shared_punctuation_insert(
    mutation: &Mutation,
    left: &[Mutation],
    right: &[Mutation],
) -> bool {
    let Mutation::InsertSubtree {
        parent,
        before,
        before_occurrence,
        ..
    } = mutation
    else {
        return false;
    };
    if !is_punctuation_token_insert(mutation) {
        return false;
    }
    punctuation_insert_at(left, *parent, *before, *before_occurrence)
        && punctuation_insert_at(right, *parent, *before, *before_occurrence)
}

/// Apply side-unique mutations only; shared equivalent edits are omitted (not applied twice).
fn combine_mutation_batches(left: &[Mutation], right: &[Mutation]) -> Vec<Mutation> {
    let mut combined = Vec::new();
    for lm in left {
        if right.iter().any(|rm| mutations_merge_equivalent(lm, rm)) {
            continue;
        }
        if omit_shared_punctuation_insert(lm, left, right) {
            continue;
        }
        combined.push(lm.clone());
    }
    for rm in right {
        if left.iter().any(|lm| mutations_merge_equivalent(lm, rm)) {
            continue;
        }
        if omit_shared_punctuation_insert(rm, left, right) {
            continue;
        }
        combined.push(rm.clone());
    }
    combined
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
        (Mutation::MoveNode { node_id: a, .. }, Mutation::EditPayload { node_id: b, .. })
        | (Mutation::EditPayload { node_id: b, .. }, Mutation::MoveNode { node_id: a, .. })
        | (Mutation::MoveSubtree { node_id: a, .. }, Mutation::EditPayload { node_id: b, .. })
        | (Mutation::EditPayload { node_id: b, .. }, Mutation::MoveSubtree { node_id: a, .. })
        | (Mutation::MoveNode { node_id: a, .. }, Mutation::RenameIdentifier { node_id: b, .. })
        | (Mutation::RenameIdentifier { node_id: b, .. }, Mutation::MoveNode { node_id: a, .. })
        | (
            Mutation::MoveSubtree { node_id: a, .. },
            Mutation::RenameIdentifier { node_id: b, .. },
        )
        | (
            Mutation::RenameIdentifier { node_id: b, .. },
            Mutation::MoveSubtree { node_id: a, .. },
        ) if a == b => true,
        (Mutation::MoveSubtree { node_id: a, .. }, Mutation::MoveSubtree { node_id: b, .. })
            if a != b =>
        {
            true
        }
        (Mutation::MoveNode { node_id: a, .. }, Mutation::MoveNode { node_id: b, .. })
            if a != b =>
        {
            true
        }
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
    fn multi_element_append_merge_preserves_internal_comma() {
        use crate::unparser::unparse;

        for (i, (base_s, left_s, right_s)) in [
            (
                "fn setup() {\n    call(3, 4, 5, 6);\n    let x = 1;\n}\n",
                "fn setup() {\n    call(3, 4, 5, 6, 7, 8);\n    let x = 1;\n}\n",
                "fn setup() {\n    call(3, 4, 5, 6);\n    let x = 2;\n}\n",
            ),
            (
                "fn main() {\n    setup(3, 4, 5, 6);\n    let x = 1;\n}\n",
                "fn main() {\n    setup(3, 4, 5, 6, 7, 8);\n    let x = 1;\n}\n",
                "fn main() {\n    setup(3, 4, 5, 6);\n    let x = 2;\n}\n",
            ),
            (
                "fn setup() {\n    call(1, 2, 3, 4, 5, 6);\n    let x = 1;\n}\n",
                "fn setup() {\n    call(1, 2, 3, 4, 5, 6, 7, 8);\n    let x = 1;\n}\n",
                "fn setup() {\n    call(1, 2, 3, 4, 5, 6);\n    let x = 2;\n}\n",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let base = parse_rust(base_s).unwrap();
            let left = parse_rust(left_s).unwrap();
            let right = parse_rust(right_s).unwrap();
            let outcome = merge_files(
                &FileContent::Ast(base.clone()),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            );
            let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
                panic!("case {i}: expected clean merge for base={base_s:?}, got {outcome:?}");
            };
            let text = unparse(&merged);
            parse_rust(&text).expect("merged source must parse");
            assert!(
                text.contains("7, 8") || text.contains("7,8"),
                "missing internal comma in {text:?}"
            );
        }
    }

    #[test]
    fn shared_literal_edit_does_not_corrupt_unrelated_call_arg() {
        use crate::unparser::unparse;

        let base_s = "fn f() {\n    call(1, 2);\n    let x = 1;\n}\n";
        let left_s = "fn f() {\n    call(1, 2, 3, 4);\n    let x = 1;\n}\n";
        let right_s = "fn f() {\n    call(1, 2);\n    let x = 2;\n}\n";
        let base = parse_rust(base_s).unwrap();
        let left = parse_rust(left_s).unwrap();
        let right = parse_rust(right_s).unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("expected clean merge, got {outcome:?}");
        };
        let text = unparse(&merged);
        parse_rust(&text).expect("merged source must parse");
        assert!(text.contains("call(1, 2, 3, 4)") || text.contains("call(1,2,3,4)"));
        assert!(text.contains("x = 2"));
        assert!(!text.contains("x = 1"));
    }

    #[test]
    fn shared_literal_merge_commutes_head_and_incoming_order() {
        use crate::unparser::unparse;

        let base_s = "fn f() {\n    call(1, 2);\n    let x = 1;\n}\n";
        let feature_s = "fn f() {\n    call(1, 2, 3, 4);\n    let x = 1;\n}\n";
        let head_s = "fn f() {\n    call(1, 2);\n    let x = 2;\n}\n";
        let base = parse_rust(base_s).unwrap();
        let feature = parse_rust(feature_s).unwrap();
        let head = parse_rust(head_s).unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(head),
            &FileContent::Ast(feature),
        );
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("expected clean merge, got {outcome:?}");
        };
        let text = unparse(&merged);
        parse_rust(&text).expect("merged source must parse");
        assert!(text.contains("call(1, 2, 3, 4)") || text.contains("call(1,2,3,4)"));
        assert!(text.contains("x = 2"));
    }

    #[test]
    fn multi_element_boundary_list_merge_roundtrip() {
        use crate::unparser::unparse;

        let base_s = "fn setup() {\n    call(1, 2, 3, 4, 5, 6);\n    let x = 1;\n}\n";
        let left_s = "fn setup() {\n    call(0, 1, 2, 3, 4, 5, 6, 7, 8);\n    let x = 1;\n}\n";
        let right_s = "fn setup() {\n    call(1, 2, 3, 4, 5, 6);\n    let x = 2;\n}\n";
        let base = parse_rust(base_s).unwrap();
        let left = parse_rust(left_s).unwrap();
        let right = parse_rust(right_s).unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("expected clean merge, got {outcome:?}");
        };
        let text = unparse(&merged);
        parse_rust(&text).expect("merged source must parse");
        assert!(
            text.contains("0, 1") || text.contains("0,1"),
            "missing prepend comma in {text:?}"
        );
        assert!(text.contains("7, 8") || text.contains("7,8"));
        assert!(text.contains("x = 2"));
    }

    #[test]
    fn wide_arglist_prepend_and_append_merge_roundtrip() {
        use crate::unparser::unparse;

        let base_s = "fn setup() {\n    call(1, 2, 3, 4, 5, 6, 7, 8);\n}\n";
        let left_s = "fn setup() {\n    call(0, 1, 2, 3, 4, 5, 6, 7, 8);\n}\n";
        let right_s = "fn setup() {\n    call(1, 2, 3, 4, 5, 6, 7, 8, 9);\n}\n";
        let base = parse_rust(base_s).unwrap();
        let left = parse_rust(left_s).unwrap();
        let right = parse_rust(right_s).unwrap();
        let mut left_only = base.clone();
        left_only
            .apply_batch(&diff_graphs(&base, &left).mutations)
            .unwrap();
        assert_eq!(unparse(&left_only), unparse(&left));
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("expected clean merge, got {outcome:?}");
        };
        let text = unparse(&merged);
        parse_rust(&text).expect("merged source must parse");
        assert!(text.contains("0, 1") || text.contains("0,1"));
        assert!(text.contains("8, 9") || text.contains("8,9"));
    }

    #[test]
    fn identical_literal_siblings_disjoint_edits_conflict() {
        let base_s = "fn demo() {\n    let v = vec![0, 0, 0, 0, 0];\n}\n";
        let left_s = "fn demo() {\n    let v = vec![1, 0, 0, 0, 0];\n}\n";
        let right_s = "fn demo() {\n    let v = vec![0, 0, 0, 0, 2];\n}\n";
        let base = parse_rust(base_s).unwrap();
        let left = parse_rust(left_s).unwrap();
        let right = parse_rust(right_s).unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        assert!(
            matches!(outcome, MergeOutcome::Conflict(_)),
            "content-addressed literals cannot distinguish first vs last zero: {outcome:?}"
        );
    }

    #[test]
    fn move_subtree_and_sibling_payload_edit_merge() {
        use crate::unparser::unparse;

        let base = parse_rust("fn helper() { 1 }\nstruct S {}\n").unwrap();
        let left = parse_rust("struct S {}\nfn helper() { 1 }\n").unwrap();
        let right = parse_rust("fn helper() { 9 }\nstruct S {}\n").unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let MergeOutcome::Merged(FileContent::Ast(merged)) = outcome else {
            panic!("expected merge when one branch moves and the other edits the moved node");
        };
        let text = unparse(&merged);
        assert!(text.contains("struct S"), "merged: {text:?}");
        assert!(
            text.contains('9'),
            "edit should apply at moved location: {text:?}"
        );
    }

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
        let focused = c.format_focused_report("main.rs");
        assert!(focused.contains("conflict: main.rs"), "{focused}");
        assert!(focused.contains("ours: rename"), "{focused}");
        assert!(focused.contains("theirs: rename"), "{focused}");
        assert!(focused.contains("--resolve main.rs:ours"), "{focused}");
        let node_id = c.left_mutations[0].primary_node().unwrap().to_string();
        assert!(!focused.contains(&node_id), "{focused}");
        let contextual = c.format_focused_report_with_labels(
            "main.rs",
            "reverted parent",
            "current HEAD",
            ConflictResolutionStyle::None,
        );
        let rebase = c.format_focused_report_with_labels(
            "main.rs",
            "ours",
            "theirs",
            ConflictResolutionStyle::RebaseContinue,
        );
        assert!(
            rebase.contains("rebase --continue --resolve main.rs:ours"),
            "{rebase}"
        );
        assert!(
            contextual.contains("reverted parent: rename"),
            "{contextual}"
        );
        assert!(contextual.contains("current HEAD: rename"), "{contextual}");
        assert!(!contextual.contains("--resolve"), "{contextual}");
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
    fn identical_mutations_are_not_overlapping() {
        let base = parse_rust("fn wrap(a: i32) {}\n").unwrap();
        let extended = parse_rust("fn wrap(a: i32, b: i32) {}\n").unwrap();
        let diff = diff_graphs(&base, &extended);
        assert!(!diff.mutations.is_empty());
        for mutation in &diff.mutations {
            assert!(
                overlap_reason(&base, mutation, mutation).is_none(),
                "identical mutations must not overlap: {mutation:?}"
            );
        }
    }

    #[test]
    fn cross_branch_identical_mutations_merge_cleanly() {
        let bases = [
            "fn calc(a: i32, b: i32) {\n    let x = 1;\n    let y = 2;\n}\n",
            "fn calc(a: i32, b: i32) -> i32 {\n    let x = 1;\n    let y = 2;\n    x + y\n}\n",
            "fn demo(a: i32, b: i32) {\n    println!(\"{}\", a);\n    println!(\"{}\", b);\n}\n",
            "fn pair(a: i32, b: i32) -> (i32, i32) {\n    (a, b)\n}\n",
        ];
        let edits = [
            (
                "fn calc(a: i32, b: i32) {\n    let x = 9;\n    let y = 2;\n}\n",
                "fn calc(a: i32, b: i32) {\n    let x = 1;\n    let y = 8;\n}\n",
            ),
            (
                "fn calc(a: i32, b: i32) -> i32 {\n    let x = 9;\n    let y = 2;\n    x + y\n}\n",
                "fn calc(a: i32, b: i32) -> i32 {\n    let x = 1;\n    let y = 8;\n    x + y\n}\n",
            ),
            (
                "fn demo(a: i32, b: i32) {\n    println!(\"x{}\", a);\n    println!(\"{}\", b);\n}\n",
                "fn demo(a: i32, b: i32) {\n    println!(\"{}\", a);\n    println!(\"y{}\", b);\n}\n",
            ),
            (
                "fn pair(a: i32, b: i32) -> (i32, i32) {\n    (a + 1, b)\n}\n",
                "fn pair(a: i32, b: i32) -> (i32, i32) {\n    (a, b + 1)\n}\n",
            ),
        ];
        for (base_s, (left_s, right_s)) in bases.iter().zip(edits.iter()) {
            let base = parse_rust(base_s).unwrap();
            let left = parse_rust(left_s).unwrap();
            let right = parse_rust(right_s).unwrap();
            let left_diff = diff_graphs(&base, &left);
            let right_diff = diff_graphs(&base, &right);
            let overlaps = find_overlapping_mutations(&base, &left_diff, &right_diff);
            let outcome = merge_files(
                &FileContent::Ast(base),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            );
            for (li, lm) in left_diff.mutations.iter().enumerate() {
                for (ri, rm) in right_diff.mutations.iter().enumerate() {
                    if mutations_merge_equivalent(lm, rm) {
                        assert!(
                            overlaps.is_empty()
                                || overlaps
                                    .iter()
                                    .all(|ov| ov.left_index != li || ov.right_index != ri),
                            "shared merge-equivalent mutation should not conflict: base={base_s} lm={lm:?}"
                        );
                    }
                }
            }
            if let MergeOutcome::Conflict(c) = &outcome {
                let shared: Vec<_> = left_diff
                    .mutations
                    .iter()
                    .filter(|lm| {
                        right_diff
                            .mutations
                            .iter()
                            .any(|rm| mutations_merge_equivalent(lm, rm))
                    })
                    .collect();
                if !shared.is_empty() {
                    panic!(
                        "merge conflict despite identical cross-branch mutations: base={base_s} shared={shared:?} overlap={:?}",
                        c.overlapping
                    );
                }
            }
        }
    }

    #[test]
    fn same_intent_different_mutations_still_conflict() {
        let base = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let z = 1;\n}\n").unwrap();
        assert!(matches!(
            merge_files(
                &FileContent::Ast(base),
                &FileContent::Ast(left),
                &FileContent::Ast(right),
            ),
            MergeOutcome::Conflict(_)
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

    #[test]
    fn tracked_symlink_vs_regular_file_conflicts() {
        use crate::frontend::SymlinkBlob;
        use crate::store::{FileMode, TrackedFile};

        let regular = TrackedFile::new(
            FileContent::Text(crate::frontend::TextBlob::new("data\n".into())),
            FileMode::Regular,
        );
        let symlink = TrackedFile::new(
            FileContent::Symlink(SymlinkBlob::new("target.txt".into())),
            FileMode::Symlink,
        );
        assert!(matches!(
            merge_tracked_path("link.txt", None, Some(&regular), Some(&symlink)),
            PathMergeTrackedOutcome::Conflict(_)
        ));
    }

    #[test]
    fn new_executable_on_one_branch_does_not_mode_conflict() {
        use crate::store::{FileMode, TrackedFile};

        let script = TrackedFile::new(
            FileContent::Text(crate::frontend::TextBlob::new(
                "#!/bin/sh\necho hi\n".into(),
            )),
            FileMode::Executable,
        );
        assert!(matches!(
            merge_tracked_path("install.sh", None, Some(&script), None),
            PathMergeTrackedOutcome::Keep(t) if t.mode == FileMode::Executable
        ));
        assert!(matches!(
            merge_tracked_path("install.sh", None, None, Some(&script)),
            PathMergeTrackedOutcome::Keep(t) if t.mode == FileMode::Executable
        ));
    }

    fn assert_disjoint_struct_field_merge(
        base: &str,
        feature: &str,
        main: &str,
        extra_checks: &[&str],
    ) {
        use crate::frontend::parse_rust;
        use crate::unparser::unparse;

        let base = parse_rust(base).unwrap();
        let left = parse_rust(feature).unwrap();
        let right = parse_rust(main).unwrap();
        let outcome = merge_files(
            &FileContent::Ast(base),
            &FileContent::Ast(left),
            &FileContent::Ast(right),
        );
        let text = match outcome {
            MergeOutcome::Merged(FileContent::Ast(g)) => {
                g.validate().expect("merged graph valid");
                unparse(&g)
            }
            MergeOutcome::Conflict(c) => panic!("unexpected conflict: {}", c.message),
            other => panic!("unexpected outcome: {other:?}"),
        };
        parse_rust(&text).expect("merged source must parse as rust");
        assert!(text.contains("pub timeout: u64"), "{text}");
        for needle in extra_checks {
            assert!(text.contains(needle), "{text}");
        }
    }

    #[test]
    fn complex_struct_field_and_body_disjoint_merge_roundtrip() {
        assert_disjoint_struct_field_merge(
            r#"pub struct Config {
    pub host: String,
    pub port: u16,
}

pub fn connect(cfg: &Config) -> Result<(), String> {
    Ok(())
}
"#,
            r#"pub struct Config {
    pub host: String,
    pub port: u16,
    pub timeout: u64,
}

pub fn connect(cfg: &Config) -> Result<(), String> {
    Ok(())
}
"#,
            r#"pub struct Config {
    pub host: String,
    pub port: u16,
}

fn validate(cfg: &Config) -> Result<(), String> {
    if cfg.host.is_empty() {
        return Err("host required".into());
    }
    Ok(())
}

pub fn connect(cfg: &Config) -> Result<(), String> {
    validate(cfg)?;
    Ok(())
}
"#,
            &["validate", "validate(cfg)?"],
        );
    }

    #[test]
    fn complex_struct_field_with_println_and_iflet_validate_merge_roundtrip() {
        assert_disjoint_struct_field_merge(
            r#"pub struct Config {
    pub host: String,
    pub port: u16,
}

pub fn connect(cfg: &Config) -> Result<(), String> {
    println!("Connecting to {}:{}", cfg.host, cfg.port);
    Ok(())
}
"#,
            r#"pub struct Config {
    pub host: String,
    pub port: u16,
    pub timeout: u64,
}

pub fn connect(cfg: &Config) -> Result<(), String> {
    println!("Connecting to {}:{}", cfg.host, cfg.port);
    Ok(())
}
"#,
            r#"pub struct Config {
    pub host: String,
    pub port: u16,
}

pub fn connect(cfg: &Config) -> Result<(), String> {
    println!("Connecting to {}:{}", cfg.host, cfg.port);
    if let Err(e) = validate(cfg) {
        return Err(e);
    }
    Ok(())
}

fn validate(cfg: &Config) -> Result<(), String> {
    if cfg.port == 0 {
        Err("Invalid port".to_string())
    } else {
        Ok(())
    }
}
"#,
            &["validate", "if let Err(e) = validate(cfg)"],
        );
    }
}
