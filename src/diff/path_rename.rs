use crate::diff::diff_graphs;
use crate::frontend::FileContent;
use crate::graph::Mutation;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// How closely a removed path and an added path match for pairing as a rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRenameKind {
    /// Byte-identical blob hash (text or AST).
    Exact,
    /// AST files only: `diff_graphs` reports no `DeleteSubtree` or `InsertSubtree`.
    WithEdits,
}

/// A path-level rename from one manifest entry to another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRename {
    pub from: String,
    pub to: String,
    pub kind: PathRenameKind,
}

/// Pair removed and added paths. Text files require an exact content match; AST files
/// also pair when the structural diff is edit-only (no insert/delete subtrees).
pub fn detect_path_renames(
    old_files: &HashMap<String, FileContent>,
    new_files: &HashMap<String, FileContent>,
) -> Vec<PathRename> {
    let mut removed: Vec<String> = old_files
        .keys()
        .filter(|p| !new_files.contains_key(*p))
        .cloned()
        .collect();
    let mut added: Vec<String> = new_files
        .keys()
        .filter(|p| !old_files.contains_key(*p))
        .cloned()
        .collect();
    removed.sort();
    added.sort();

    let mut renames = Vec::new();
    let mut used_removed = HashSet::new();
    let mut used_added = HashSet::new();

    for from in &removed {
        if used_removed.contains(from) {
            continue;
        }
        let old_content = &old_files[from];
        for to in &added {
            if used_added.contains(to) {
                continue;
            }
            let new_content = &new_files[to];
            if let Some(kind) = rename_kind(from, to, old_content, new_content) {
                renames.push(PathRename {
                    from: from.clone(),
                    to: to.clone(),
                    kind,
                });
                used_removed.insert(from.clone());
                used_added.insert(to.clone());
                break;
            }
        }
    }
    renames
}

pub fn build_rename_map(renames: &[PathRename]) -> HashMap<String, String> {
    renames
        .iter()
        .map(|r| (r.from.clone(), r.to.clone()))
        .collect()
}

/// Path in `side_files` that carries the content descended from `base_path`.
pub fn side_path_for_base(
    base_path: &str,
    side_files: &HashMap<String, FileContent>,
    renames: &HashMap<String, String>,
) -> Option<String> {
    if let Some(to) = renames.get(base_path)
        && side_files.contains_key(to)
    {
        return Some(to.clone());
    }
    if side_files.contains_key(base_path) {
        return Some(base_path.to_string());
    }
    None
}

/// Both branches renamed the same base path to different destinations.
pub fn rename_targets_conflict(
    base_path: &str,
    head_renames: &HashMap<String, String>,
    other_renames: &HashMap<String, String>,
) -> bool {
    match (head_renames.get(base_path), other_renames.get(base_path)) {
        (Some(h), Some(o)) => h != o,
        _ => false,
    }
}

fn path_extension(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

fn rename_kind(
    old_path: &str,
    new_path: &str,
    old: &FileContent,
    new: &FileContent,
) -> Option<PathRenameKind> {
    if blobs_equal(old, new) {
        return Some(PathRenameKind::Exact);
    }
    if path_extension(old_path) != path_extension(new_path) {
        return None;
    }
    match (old, new) {
        (FileContent::Ast(o), FileContent::Ast(n)) => {
            let diff = diff_graphs(o, n);
            let structural = diff.mutations.iter().any(|m| {
                matches!(
                    m,
                    Mutation::DeleteSubtree { .. } | Mutation::InsertSubtree { .. }
                )
            });
            if structural {
                None
            } else {
                Some(PathRenameKind::WithEdits)
            }
        }
        _ => None,
    }
}

fn blobs_equal(a: &FileContent, b: &FileContent) -> bool {
    a.semantic_eq(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{parse_rust, parse_text_or_blob};

    #[test]
    fn exact_path_rename_pairs_removed_and_added() {
        let mut old = HashMap::new();
        let mut new = HashMap::new();
        let content = parse_text_or_blob("lib.rs", "fn foo() {}\n");
        old.insert("old.rs".into(), content.clone());
        new.insert("new.rs".into(), content);
        let renames = detect_path_renames(&old, &new);
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].from, "old.rs");
        assert_eq!(renames[0].to, "new.rs");
        assert_eq!(renames[0].kind, PathRenameKind::Exact);
    }

    #[test]
    fn ast_rename_with_edits_pairs_without_delete_insert() {
        let mut old = HashMap::new();
        let mut new = HashMap::new();
        old.insert(
            "a.rs".into(),
            FileContent::Ast(parse_rust("fn foo() { 1 }\n").unwrap()),
        );
        new.insert(
            "b.rs".into(),
            FileContent::Ast(parse_rust("fn foo() { 2 }\n").unwrap()),
        );
        let renames = detect_path_renames(&old, &new);
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].kind, PathRenameKind::WithEdits);
    }

    #[test]
    fn text_near_match_stays_unpaired() {
        let mut old = HashMap::new();
        let mut new = HashMap::new();
        old.insert(
            "a.txt".into(),
            FileContent::Text(crate::frontend::TextBlob::new("hello\n".into())),
        );
        new.insert(
            "b.txt".into(),
            FileContent::Text(crate::frontend::TextBlob::new("hallo\n".into())),
        );
        assert!(detect_path_renames(&old, &new).is_empty());
    }

    #[test]
    fn conflicting_rename_targets_detected() {
        let mut head = HashMap::new();
        head.insert("base.rs".into(), "left.rs".into());
        let mut other = HashMap::new();
        other.insert("base.rs".into(), "right.rs".into());
        assert!(rename_targets_conflict("base.rs", &head, &other));
    }

    #[test]
    fn unrelated_cross_extension_paths_stay_unpaired() {
        let mut old = HashMap::new();
        let mut new = HashMap::new();
        old.insert(
            "util.go".into(),
            parse_text_or_blob(
                "util.go",
                "package main\n\nfunc Add(a, b int) int { return a + b }\n",
            ),
        );
        new.insert(
            "unrelated.yml".into(),
            parse_text_or_blob("unrelated.yml", "name: demo\nversion: 1\n"),
        );
        assert!(detect_path_renames(&old, &new).is_empty());
    }
}
