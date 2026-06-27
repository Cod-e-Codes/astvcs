use crate::diff::lcs::lcs_pairs;
use crate::graph::{AstGraph, Mutation, NodeId, NodeKind, TriviaRecord};

/// Result of diffing two AST graphs.
#[derive(Clone, Debug)]
pub struct DiffResult {
    pub mutations: Vec<Mutation>,
}

/// Compute structural mutations transforming `old` into `new`.
pub fn diff_graphs(old: &AstGraph, new: &AstGraph) -> DiffResult {
    let mut mutations = Vec::new();
    diff_subtree(old, new, old.root, new.root, &mut mutations);
    DiffResult { mutations }
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

fn same_id_multiset(a: &[NodeId], b: &[NodeId]) -> bool {
    let mut left = a.to_vec();
    let mut right = b.to_vec();
    left.sort();
    right.sort();
    left == right
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
        }
    }

    for (oi, old_child) in old_children.iter().enumerate() {
        if matched_old[oi] {
            continue;
        }
        for (ni, new_child) in new_children.iter().enumerate() {
            if matched_new[ni] {
                continue;
            }
            let oc = old.get(old_child).unwrap();
            let nc = new.get(new_child).unwrap();
            if oc.kind == nc.kind && !oc.is_leaf() {
                matched_old[oi] = true;
                matched_new[ni] = true;
                if oi != ni {
                    out.push(Mutation::MoveNode {
                        node_id: *old_child,
                        new_parent: old_node_id,
                        before: insert_anchor(&new_children, ni),
                    });
                }
                diff_subtree(old, new, *old_child, *new_child, out);
                break;
            }
        }
    }

    for (oi, old_child) in old_children.iter().enumerate() {
        if matched_old[oi] {
            continue;
        }
        for (ni, new_child) in new_children.iter().enumerate() {
            if matched_new[ni] {
                continue;
            }
            let oc = old.get(old_child).unwrap();
            let nc = new.get(new_child).unwrap();
            if oc.kind == nc.kind
                && oc.is_leaf()
                && nc.is_leaf()
                && is_payload_editable_leaf(&oc.kind)
                && oc.payload != nc.payload
            {
                matched_old[oi] = true;
                matched_new[ni] = true;
                if oi != ni {
                    out.push(Mutation::MoveNode {
                        node_id: *old_child,
                        new_parent: old_node_id,
                        before: insert_anchor(&new_children, ni),
                    });
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
                break;
            }
        }
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
    use crate::graph::Mutation;
    use crate::unparser::unparse;

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
}
