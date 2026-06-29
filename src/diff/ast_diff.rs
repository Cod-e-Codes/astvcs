use crate::diff::lcs::lcs_pairs;
use crate::graph::{AstGraph, Mutation, NodeId, NodeKind, TriviaRecord};
use crate::trace;

/// Result of diffing two AST graphs.
#[derive(Clone, Debug)]
pub struct DiffResult {
    pub mutations: Vec<Mutation>,
}

/// Compute structural mutations transforming `old` into `new`.
pub fn diff_graphs(old: &AstGraph, new: &AstGraph) -> DiffResult {
    let mut mutations = Vec::new();
    diff_subtree(old, new, old.root, new.root, &mut mutations);
    if old.root_trailing_trivia != new.root_trailing_trivia {
        mutations.push(Mutation::SetRootTrailingTrivia {
            trailing: new.root_trailing_trivia.clone(),
        });
    }
    DiffResult { mutations }
}

fn diff_child_trivia(
    old: &AstGraph,
    new: &AstGraph,
    old_parent: NodeId,
    new_parent: NodeId,
    old_child: NodeId,
    new_child: NodeId,
    out: &mut Vec<Mutation>,
) {
    let old_children = old.get(&old_parent).unwrap().children.clone();
    let new_children = new.get(&new_parent).unwrap().children.clone();
    let old_occ = child_occurrence_at(&old_children, old_child);
    let new_occ = child_occurrence_at(&new_children, new_child);
    let old_leading = old.get_trivia(old_parent, old_child, old_occ);
    let new_leading = new.get_trivia(new_parent, new_child, new_occ);
    if old_leading != new_leading {
        out.push(Mutation::SetTrivia {
            parent: old_parent,
            child: old_child,
            occurrence: old_occ,
            leading: new_leading.to_string(),
        });
    }
}

fn child_key(graph: &AstGraph, id: &NodeId) -> (NodeKind, String, usize) {
    let n = graph.get(id).unwrap();
    (n.kind.clone(), n.payload.clone(), n.children.len())
}

/// Identity key for sibling matching: kind and child count, ignoring payload.
fn child_role_key(graph: &AstGraph, id: &NodeId) -> (NodeKind, usize) {
    let n = graph.get(id).unwrap();
    (n.kind.clone(), n.children.len())
}

fn is_payload_editable_leaf(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Literal | NodeKind::Identifier | NodeKind::Token | NodeKind::Unknown(_)
    )
}

fn insert_anchor(children: &[NodeId], index: usize) -> Option<NodeId> {
    children.get(index + 1).copied()
}

fn child_occurrence_at(children: &[NodeId], child: NodeId) -> u32 {
    let index = children
        .iter()
        .position(|c| *c == child)
        .unwrap_or(children.len().saturating_sub(1));
    children[..index].iter().filter(|c| **c == child).count() as u32
}

fn insert_trivia(
    new: &AstGraph,
    new_parent: NodeId,
    new_child: NodeId,
    insert_parent: NodeId,
) -> Vec<TriviaRecord> {
    let mut trivia = new.collect_subtree_trivia(new_child);
    let occurrence = child_occurrence_at(&new.get(&new_parent).unwrap().children, new_child);
    let leading = new.get_trivia(new_parent, new_child, occurrence);
    if !leading.is_empty() {
        trivia.push(TriviaRecord {
            parent: insert_parent,
            child: new_child,
            occurrence,
            leading: leading.to_string(),
        });
    }
    trivia.sort_by_key(|a| (a.parent, a.child, a.occurrence));
    trivia
}

fn structure_fingerprint(graph: &AstGraph, id: &NodeId) -> Vec<StructureSig> {
    let n = graph.get(id).unwrap();
    let payload = if n.is_leaf() && is_payload_editable_leaf(&n.kind) {
        n.payload.clone()
    } else {
        String::new()
    };
    let mut out = vec![StructureSig {
        kind: n.kind.clone(),
        child_count: n.children.len(),
        payload,
    }];
    for child in &n.children {
        out.extend(structure_fingerprint(graph, child));
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StructureSig {
    kind: NodeKind,
    child_count: usize,
    payload: String,
}

fn best_structural_match(
    oi: usize,
    old_children: &[NodeId],
    new_children: &[NodeId],
    matched_new: &[bool],
    old: &AstGraph,
    new: &AstGraph,
) -> Option<usize> {
    let oc = old.get(&old_children[oi]).unwrap();
    let mut best: Option<(usize, usize)> = None;
    for (ni, new_child) in new_children.iter().enumerate() {
        if matched_new[ni] {
            continue;
        }
        let nc = new.get(new_child).unwrap();
        if oc.kind != nc.kind || oc.is_leaf() || nc.is_leaf() {
            continue;
        }
        let dist = oi.abs_diff(ni);
        let score = oc.children.len().abs_diff(nc.children.len()) * 1_000 + dist;
        if best.is_none_or(|(best_score, _)| score < best_score) {
            best = Some((score, ni));
        }
    }
    best.map(|(_, ni)| ni)
}

fn best_leaf_payload_match(
    oi: usize,
    old_children: &[NodeId],
    new_children: &[NodeId],
    matched_new: &[bool],
    old: &AstGraph,
    new: &AstGraph,
) -> Option<usize> {
    let oc = old.get(&old_children[oi]).unwrap();
    let mut best: Option<(usize, usize)> = None;
    for (ni, new_child) in new_children.iter().enumerate() {
        if matched_new[ni] {
            continue;
        }
        let nc = new.get(new_child).unwrap();
        if oc.kind != nc.kind
            || !oc.is_leaf()
            || !nc.is_leaf()
            || !is_payload_editable_leaf(&oc.kind)
            || oc.payload == nc.payload
        {
            continue;
        }
        let dist = oi.abs_diff(ni);
        if best.is_none_or(|(best_dist, _)| dist < best_dist) {
            best = Some((dist, ni));
        }
    }
    best.map(|(_, ni)| ni)
}

fn same_id_multiset(a: &[NodeId], b: &[NodeId]) -> bool {
    let mut left = a.to_vec();
    let mut right = b.to_vec();
    left.sort();
    right.sort();
    left == right
}

fn unique_fingerprint_pairs(
    old: &AstGraph,
    new: &AstGraph,
    old_children: &[NodeId],
    new_children: &[NodeId],
    matched_old: &[bool],
    matched_new: &[bool],
) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (oi, old_child) in old_children.iter().enumerate() {
        if matched_old[oi] {
            continue;
        }
        let oc = old.get(old_child).unwrap();
        if oc.is_leaf() {
            continue;
        }
        let fp = structure_fingerprint(old, old_child);
        let new_candidates: Vec<usize> = new_children
            .iter()
            .enumerate()
            .filter(|(ni, new_child)| {
                !matched_new[*ni]
                    && old.get(old_child).unwrap().kind == new.get(new_child).unwrap().kind
                    && !new.get(new_child).unwrap().is_leaf()
                    && structure_fingerprint(new, new_child) == fp
            })
            .map(|(ni, _)| ni)
            .collect();
        if new_candidates.len() != 1 {
            continue;
        }
        let ni = new_candidates[0];
        let old_candidates: Vec<usize> = old_children
            .iter()
            .enumerate()
            .filter(|(oi2, old_child2)| {
                !matched_old[*oi2]
                    && old.get(old_child2).unwrap().kind == new.get(&new_children[ni]).unwrap().kind
                    && !old.get(old_child2).unwrap().is_leaf()
                    && structure_fingerprint(old, old_child2) == fp
            })
            .map(|(oi2, _)| oi2)
            .collect();
        if old_candidates.len() == 1 {
            pairs.push((oi, ni));
        }
    }
    pairs
}

fn diff_subtree(
    old: &AstGraph,
    new: &AstGraph,
    old_id: NodeId,
    new_id: NodeId,
    out: &mut Vec<Mutation>,
) {
    let old_node = old.get(&old_id).unwrap();
    let new_node = new.get(&new_id).unwrap();

    if old_id == new_id {
        if old_node.kind == new_node.kind && !(old_node.is_leaf() && new_node.is_leaf()) {
            diff_children(old, new, old_id, new_id, out);
        }
        return;
    }

    if old_node.kind == new_node.kind
        && old_node.children.is_empty()
        && new_node.children.is_empty()
        && old_node.payload != new_node.payload
    {
        if old_node.kind == NodeKind::Identifier {
            out.push(Mutation::RenameIdentifier {
                node_id: old_id,
                new_name: new_node.payload.clone(),
            });
        } else {
            out.push(Mutation::EditPayload {
                node_id: old_id,
                new_payload: new_node.payload.clone(),
            });
        }
        return;
    }

    if old_node.kind == new_node.kind && !(old_node.is_leaf() && new_node.is_leaf()) {
        diff_children(old, new, old_id, new_id, out);
        return;
    }

    if old.parent_of(&old_id).is_some() {
        out.push(Mutation::DeleteSubtree {
            parent: old.parent_of(&old_id).unwrap(),
            node_id: old_id,
        });
    }
    if let Some(parent) = new.parent_of(&new_id) {
        let old_parent = find_corresponding_parent(old, new, parent);
        let descendants = new.collect_subtree(new_id);
        let top = new.get(&new_id).unwrap().clone();
        let new_parent_children = new.get(&parent).unwrap().children.clone();
        let index = new_parent_children
            .iter()
            .position(|c| *c == new_id)
            .unwrap_or(0);
        out.push(Mutation::InsertSubtree {
            parent: old_parent,
            before: insert_anchor(&new_parent_children, index),
            node: top,
            descendants,
            trivia: insert_trivia(new, parent, new_id, old_parent),
        });
    }
}

fn find_corresponding_parent(old: &AstGraph, new: &AstGraph, new_parent: NodeId) -> NodeId {
    let path = path_from_root(new, new_parent);
    resolve_path_in_old(old, &path).unwrap_or(old.root)
}

fn path_from_root(graph: &AstGraph, target: NodeId) -> Vec<usize> {
    let mut path = Vec::new();
    let mut current = target;
    while current != graph.root {
        if let Some(parent) = graph.parent_of(&current) {
            let idx = graph
                .get(&parent)
                .unwrap()
                .children
                .iter()
                .position(|c| *c == current)
                .unwrap_or(0);
            path.push(idx);
            current = parent;
        } else {
            break;
        }
    }
    path.reverse();
    path
}

fn resolve_path_in_old(old: &AstGraph, path: &[usize]) -> Option<NodeId> {
    let mut current = old.root;
    for &idx in path {
        let children = old.get(&current)?.children.clone();
        current = *children.get(idx)?;
    }
    Some(current)
}

fn diff_children(
    old: &AstGraph,
    new: &AstGraph,
    old_node_id: NodeId,
    new_node_id: NodeId,
    out: &mut Vec<Mutation>,
) {
    let old_children = old.get(&old_node_id).unwrap().children.clone();
    let new_children = new.get(&new_node_id).unwrap().children.clone();

    if same_id_multiset(&old_children, &new_children) && old_children != new_children {
        out.push(Mutation::ReorderChildren {
            parent: old_node_id,
            new_order: new_children.clone(),
        });
        for id in &old_children {
            diff_subtree(old, new, *id, *id, out);
            if new_children.contains(id) {
                diff_child_trivia(old, new, old_node_id, new_node_id, *id, *id, out);
            }
        }
        return;
    }

    let old_keys: Vec<(NodeKind, String, usize)> =
        old_children.iter().map(|id| child_key(old, id)).collect();
    let new_keys: Vec<(NodeKind, String, usize)> =
        new_children.iter().map(|id| child_key(new, id)).collect();
    let old_roles: Vec<(NodeKind, usize)> = old_children
        .iter()
        .map(|id| child_role_key(old, id))
        .collect();
    let new_roles: Vec<(NodeKind, usize)> = new_children
        .iter()
        .map(|id| child_role_key(new, id))
        .collect();

    let id_pairs = lcs_pairs(&old_children, &new_children);
    let role_pairs = lcs_pairs(&old_roles, &new_roles);
    let key_pairs = lcs_pairs(&old_keys, &new_keys);

    let mut matched_old = vec![false; old_children.len()];
    let mut matched_new = vec![false; new_children.len()];

    for (oi, ni) in id_pairs {
        matched_old[oi] = true;
        matched_new[ni] = true;
        diff_subtree(old, new, old_children[oi], new_children[ni], out);
        diff_child_trivia(
            old,
            new,
            old_node_id,
            new_node_id,
            old_children[oi],
            new_children[ni],
            out,
        );
    }

    for (oi, ni) in role_pairs {
        if matched_old[oi] || matched_new[ni] {
            continue;
        }
        let oc = old.get(&old_children[oi]).unwrap();
        let nc = new.get(&new_children[ni]).unwrap();
        if oc.kind != nc.kind || oc.children.len() != nc.children.len() {
            continue;
        }
        matched_old[oi] = true;
        matched_new[ni] = true;
        if old_children[oi] != new_children[ni] && oi != ni {
            out.push(Mutation::MoveNode {
                node_id: old_children[oi],
                new_parent: old_node_id,
                before: insert_anchor(&new_children, ni),
            });
        }
        diff_subtree(old, new, old_children[oi], new_children[ni], out);
        diff_child_trivia(
            old,
            new,
            old_node_id,
            new_node_id,
            old_children[oi],
            new_children[ni],
            out,
        );
    }

    for (oi, ni) in key_pairs {
        if matched_old[oi] || matched_new[ni] {
            continue;
        }
        let oc = old.get(&old_children[oi]).unwrap();
        let nc = new.get(&new_children[ni]).unwrap();
        if oc.kind == nc.kind && !oc.is_leaf() {
            matched_old[oi] = true;
            matched_new[ni] = true;
            if old_children[oi] != new_children[ni] && oi != ni {
                out.push(Mutation::MoveNode {
                    node_id: old_children[oi],
                    new_parent: old_node_id,
                    before: insert_anchor(&new_children, ni),
                });
            }
            diff_subtree(old, new, old_children[oi], new_children[ni], out);
            diff_child_trivia(
                old,
                new,
                old_node_id,
                new_node_id,
                old_children[oi],
                new_children[ni],
                out,
            );
        }
    }

    for (oi, ni) in unique_fingerprint_pairs(
        old,
        new,
        &old_children,
        &new_children,
        &matched_old,
        &matched_new,
    ) {
        matched_old[oi] = true;
        matched_new[ni] = true;
        if oi != ni {
            out.push(Mutation::MoveSubtree {
                node_id: old_children[oi],
                new_parent: old_node_id,
                before: insert_anchor(&new_children, ni),
            });
        }
        diff_subtree(old, new, old_children[oi], new_children[ni], out);
        diff_child_trivia(
            old,
            new,
            old_node_id,
            new_node_id,
            old_children[oi],
            new_children[ni],
            out,
        );
    }

    for (oi, old_child) in old_children.iter().enumerate() {
        if matched_old[oi] {
            continue;
        }
        let Some(ni) =
            best_structural_match(oi, &old_children, &new_children, &matched_new, old, new)
        else {
            continue;
        };
        let new_child = new_children[ni];
        matched_old[oi] = true;
        matched_new[ni] = true;
        if oi != ni {
            out.push(Mutation::MoveNode {
                node_id: *old_child,
                new_parent: old_node_id,
                before: insert_anchor(&new_children, ni),
            });
            trace::notice(format!(
                "diff: structural fallback paired siblings at old[{oi}] new[{ni}] (distance {})",
                oi.abs_diff(ni)
            ));
        }
        diff_subtree(old, new, *old_child, new_child, out);
        diff_child_trivia(
            old,
            new,
            old_node_id,
            new_node_id,
            *old_child,
            new_child,
            out,
        );
    }

    for (oi, old_child) in old_children.iter().enumerate() {
        if matched_old[oi] {
            continue;
        }
        let Some(ni) =
            best_leaf_payload_match(oi, &old_children, &new_children, &matched_new, old, new)
        else {
            continue;
        };
        let new_child = new_children[ni];
        let oc = old.get(old_child).unwrap();
        let nc = new.get(&new_child).unwrap();
        matched_old[oi] = true;
        matched_new[ni] = true;
        if oi != ni {
            out.push(Mutation::MoveNode {
                node_id: *old_child,
                new_parent: old_node_id,
                before: insert_anchor(&new_children, ni),
            });
            trace::notice(format!(
                "diff: leaf fallback paired siblings at old[{oi}] new[{ni}] (distance {})",
                oi.abs_diff(ni)
            ));
        }
        if oc.kind == NodeKind::Identifier {
            out.push(Mutation::RenameIdentifier {
                node_id: *old_child,
                new_name: nc.payload.clone(),
            });
        } else {
            out.push(Mutation::EditPayload {
                node_id: *old_child,
                new_payload: nc.payload.clone(),
            });
        }
        diff_child_trivia(
            old,
            new,
            old_node_id,
            new_node_id,
            *old_child,
            new_child,
            out,
        );
    }

    for (oi, old_child) in old_children.iter().enumerate() {
        if !matched_old[oi] {
            out.push(Mutation::DeleteSubtree {
                parent: old_node_id,
                node_id: *old_child,
            });
        }
    }

    for (ni, new_child) in new_children.iter().enumerate() {
        if !matched_new[ni] {
            let descendants = new.collect_subtree(*new_child);
            let top = new.get(new_child).unwrap().clone();
            out.push(Mutation::InsertSubtree {
                parent: old_node_id,
                before: insert_anchor(&new_children, ni),
                node: top,
                descendants,
                trivia: insert_trivia(new, new_node_id, *new_child, old_node_id),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::parse_rust;
    use crate::graph::{Mutation, Node, NodeKind};
    use crate::unparser::unparse;
    use std::collections::HashMap;

    fn graph_from_two_blocks(first: &[&str], second: &[&str]) -> AstGraph {
        let mut nodes = HashMap::new();
        let build_block = |literals: &[&str], nodes: &mut HashMap<_, _>| -> NodeId {
            let leaves: Vec<_> = literals
                .iter()
                .map(|s| {
                    let n = Node::leaf(NodeKind::Literal, (*s).to_string());
                    nodes.insert(n.id, n.clone());
                    n.id
                })
                .collect();
            let block = Node::new(NodeKind::Block, String::new(), leaves);
            nodes.insert(block.id, block.clone());
            block.id
        };
        let first_id = build_block(first, &mut nodes);
        let second_id = build_block(second, &mut nodes);
        let root = Node::new(NodeKind::Module, String::new(), vec![first_id, second_id]);
        nodes.insert(root.id, root.clone());
        AstGraph::new(root, nodes)
    }

    #[test]
    fn identical_sources_produce_no_mutations() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let a = parse_rust(src).unwrap();
        let b = parse_rust(src).unwrap();
        assert!(diff_graphs(&a, &b).mutations.is_empty());
    }

    #[test]
    fn rename_is_detected() {
        let old = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::RenameIdentifier { .. }))
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn insert_statement_applies() {
        let old = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        working.validate().unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn delete_statement_applies() {
        let old = parse_rust("fn foo() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        working.validate().unwrap();
    }

    #[test]
    fn prepend_comment_is_insert_not_move_cascade() {
        let old = parse_rust("pub mod config;\npub mod event;\n").unwrap();
        let new = parse_rust("//! astvcs demo\npub mod config;\npub mod event;\n").unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::MoveNode { .. })),
            "unexpected moves: {:?}",
            diff.mutations
        );
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::InsertSubtree { .. }))
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn literal_payload_edit_not_delete_insert() {
        let old = parse_rust("pub fn answer() -> i32 {\n    42\n}\n").unwrap();
        let new = parse_rust("pub fn answer() -> i32 {\n    43\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::EditPayload { .. })),
            "expected EditPayload, got {:?}",
            diff.mutations
        );
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::DeleteSubtree { .. })),
            "unexpected delete: {:?}",
            diff.mutations
        );
    }

    #[test]
    fn reorder_applies() {
        let old = parse_rust("fn foo() {\n    let a = 1;\n    let b = 2;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let b = 2;\n    let a = 1;\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        working.validate().unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn trailing_line_comment_diff_includes_trivia_shift() {
        let old = parse_rust("fn main() {\n    println!(\"Hello, World!\");\n}\n").unwrap();
        let new = parse_rust("fn main() {\n    println!(\"Hello, World!\"); // waddup fool\n}\n")
            .unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::SetTrivia { .. })),
            "expected SetTrivia for comment body after sibling, got {:?}",
            diff.mutations
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn comment_removal_clears_trivia() {
        let with_comment = parse_rust("fn main() {\n    println!(\"a\"); // note\n}\n").unwrap();
        let without = parse_rust("fn main() {\n    println!(\"a\");\n}\n").unwrap();
        let diff = diff_graphs(&with_comment, &without);
        assert!(
            diff.mutations.iter().any(|m| match m {
                Mutation::SetTrivia { leading, .. } => leading.is_empty(),
                Mutation::DeleteSubtree { .. } => true,
                _ => false,
            }),
            "expected trivia clear or comment delete, got {:?}",
            diff.mutations
        );
        let mut working = with_comment.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&without));
        assert!(!unparse(&working).contains("// note"));
    }

    #[test]
    fn trivia_only_blank_line_applies() {
        let old = parse_rust("fn main() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let new = parse_rust("fn main() {\n    let x = 1;\n\n    let y = 2;\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::SetTrivia { .. })),
            "expected SetTrivia for blank line, got {:?}",
            diff.mutations
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn reorder_with_trivia_roundtrips() {
        let old = parse_rust("fn foo() {\n    let a = 1;\n    let b = 2;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let b = 2;\n\n    let a = 1;\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        working.validate().unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn disjoint_rename_and_insert_preserves_formatting() {
        let base = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let x = 1;\n    let z = 2;\n}\n").unwrap();
        let left_diff = diff_graphs(&base, &left);
        let right_diff = diff_graphs(&base, &right);
        let mut merged = base.clone();
        let mut combined = left_diff.mutations.clone();
        combined.extend(right_diff.mutations);
        merged.apply_batch(&combined).unwrap();
        let expected = "fn foo() {\n    let y = 1;\n    let z = 2;\n}\n";
        assert_eq!(unparse(&merged), expected);
    }

    #[test]
    fn swapped_blocks_with_different_child_counts_pair_by_proximity() {
        let old = graph_from_two_blocks(&["a", "b"], &["c"]);
        let new = graph_from_two_blocks(&["c"], &["a", "d"]);
        let diff = diff_graphs(&old, &new);
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::DeleteSubtree { .. })),
            "unexpected delete: {:?}",
            diff.mutations
        );
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::InsertSubtree { .. })),
            "unexpected insert: {:?}",
            diff.mutations
        );
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::EditPayload { .. })),
            "expected EditPayload for literal edit: {:?}",
            diff.mutations
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        working.validate().unwrap();
        assert_eq!(working.to_snapshot(), new.to_snapshot());
    }

    #[test]
    fn swapped_leaf_literals_pair_by_proximity() {
        let old = graph_from_two_blocks(&["1", "2"], &["9"]);
        let new = graph_from_two_blocks(&["9"], &["1", "3"]);
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations.iter().any(|m| {
                matches!(m, Mutation::EditPayload { new_payload, .. } if new_payload == "3")
            }),
            "expected literal edit to 3: {:?}",
            diff.mutations
        );
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::DeleteSubtree { .. })),
            "unexpected delete: {:?}",
            diff.mutations
        );
    }

    #[test]
    fn structure_fingerprint_includes_literal_payload_for_moves() {
        let old = parse_rust("fn alpha() { 1 }\nfn beta() { 2 }\n").unwrap();
        let new = parse_rust("fn beta() { 2 }\nfn alpha() { 9 }\n").unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations.iter().any(|m| {
                matches!(m, Mutation::EditPayload { new_payload, .. } if new_payload == "9")
            }),
            "expected literal edit: {:?}",
            diff.mutations
        );
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::DeleteSubtree { .. })),
            "unexpected delete: {:?}",
            diff.mutations
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn moved_function_reports_move_not_delete_insert() {
        let old = parse_rust("fn helper() {}\nstruct S {}\n").unwrap();
        let new = parse_rust("struct S {}\nfn helper() {}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        assert!(
            diff.mutations.iter().any(|m| {
                matches!(
                    m,
                    Mutation::MoveSubtree { .. }
                        | Mutation::MoveNode { .. }
                        | Mutation::ReorderChildren { .. }
                )
            }),
            "expected reposition, got {:?}",
            diff.mutations
        );
        assert!(
            !diff
                .mutations
                .iter()
                .any(|m| matches!(m, Mutation::DeleteSubtree { .. })),
            "unexpected delete: {:?}",
            diff.mutations
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }
}
