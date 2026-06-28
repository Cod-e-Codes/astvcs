use crate::graph::{AstGraph, Mutation, NodeId, NodeKind};

/// Semantic classification of a structural mutation for display and merge reasoning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditIntent {
    RenameIdentifier {
        node_id: NodeId,
        new_name: String,
    },
    EditLiteral {
        node_id: NodeId,
        new_value: String,
    },
    EditPayload {
        node_id: NodeId,
        kind: NodeKind,
        new_payload: String,
    },
    PrependComment,
    InsertStatement,
    DeleteStatement,
    InsertSubtree {
        parent: NodeId,
        before: Option<NodeId>,
        kind: NodeKind,
    },
    DeleteSubtree {
        parent: NodeId,
        node_id: NodeId,
        kind: NodeKind,
    },
    MoveNode {
        node_id: NodeId,
        new_parent: NodeId,
    },
    /// Entire subtree relocated with structure preserved (post-LCS detection).
    MoveSubtree {
        node_id: NodeId,
        new_parent: NodeId,
    },
    ReorderMembers {
        parent: NodeId,
    },
    /// Manifest path renamed (paired delete + add).
    RenamePath {
        from: String,
        to: String,
        with_edits: bool,
    },
    SetTrivia {
        parent: NodeId,
        child: NodeId,
        occurrence: u32,
    },
    SetRootTrailingTrivia,
}

pub fn classify_mutation(base: Option<&AstGraph>, mutation: &Mutation) -> EditIntent {
    match mutation {
        Mutation::RenameIdentifier { node_id, new_name } => EditIntent::RenameIdentifier {
            node_id: *node_id,
            new_name: new_name.clone(),
        },
        Mutation::EditPayload {
            node_id,
            new_payload,
        } => {
            let kind = base
                .and_then(|g| g.get(node_id))
                .map(|n| n.kind.clone())
                .unwrap_or(NodeKind::Unknown(String::new()));
            if kind == NodeKind::Literal {
                EditIntent::EditLiteral {
                    node_id: *node_id,
                    new_value: new_payload.clone(),
                }
            } else {
                EditIntent::EditPayload {
                    node_id: *node_id,
                    kind,
                    new_payload: new_payload.clone(),
                }
            }
        }
        Mutation::InsertSubtree {
            parent,
            before,
            node,
            ..
        } => classify_insert(base, *parent, *before, node),
        Mutation::DeleteSubtree { parent, node_id } => {
            let kind = base
                .and_then(|g| g.get(node_id))
                .map(|n| n.kind.clone())
                .unwrap_or(NodeKind::Unknown(String::new()));
            if matches!(kind, NodeKind::Statement | NodeKind::Declaration) {
                EditIntent::DeleteStatement
            } else {
                EditIntent::DeleteSubtree {
                    parent: *parent,
                    node_id: *node_id,
                    kind,
                }
            }
        }
        Mutation::MoveNode {
            node_id,
            new_parent,
            ..
        } => EditIntent::MoveNode {
            node_id: *node_id,
            new_parent: *new_parent,
        },
        Mutation::MoveSubtree {
            node_id,
            new_parent,
            ..
        } => EditIntent::MoveSubtree {
            node_id: *node_id,
            new_parent: *new_parent,
        },
        Mutation::ReorderChildren { parent, .. } => EditIntent::ReorderMembers { parent: *parent },
        Mutation::SetTrivia {
            parent,
            child,
            occurrence,
            ..
        } => EditIntent::SetTrivia {
            parent: *parent,
            child: *child,
            occurrence: *occurrence,
        },
        Mutation::SetRootTrailingTrivia { .. } => EditIntent::SetRootTrailingTrivia,
    }
}

fn classify_insert(
    base: Option<&AstGraph>,
    parent: NodeId,
    before: Option<NodeId>,
    node: &crate::graph::Node,
) -> EditIntent {
    if node.kind == NodeKind::Comment {
        let at_start = base
            .and_then(|g| g.get(&parent))
            .is_none_or(|p| match before {
                None => p.children.is_empty(),
                Some(anchor) => p.children.first() == Some(&anchor),
            });
        if at_start {
            return EditIntent::PrependComment;
        }
    }
    if matches!(node.kind, NodeKind::Statement | NodeKind::Declaration) {
        return EditIntent::InsertStatement;
    }
    EditIntent::InsertSubtree {
        parent,
        before,
        kind: node.kind.clone(),
    }
}

pub fn classify_path_rename(rename: &crate::diff::PathRename) -> EditIntent {
    EditIntent::RenamePath {
        from: rename.from.clone(),
        to: rename.to.clone(),
        with_edits: rename.kind == crate::diff::PathRenameKind::WithEdits,
    }
}

pub fn classify_mutations(
    base: Option<&AstGraph>,
    mutations: &[Mutation],
) -> Vec<(usize, EditIntent)> {
    mutations
        .iter()
        .enumerate()
        .map(|(i, m)| (i, classify_mutation(base, m)))
        .collect()
}

pub fn format_intent(base: Option<&AstGraph>, intent: &EditIntent) -> String {
    match intent {
        EditIntent::RenameIdentifier { node_id, new_name } => {
            let old = base
                .and_then(|g| g.get(node_id))
                .map(|n| n.payload.as_str())
                .unwrap_or("?");
            format!("rename `{old}` to `{new_name}` at {node_id}")
        }
        EditIntent::EditLiteral { node_id, new_value } => {
            format!("edit literal to `{new_value}` at {node_id}")
        }
        EditIntent::EditPayload {
            node_id,
            kind,
            new_payload,
        } => {
            format!(
                "edit {} payload to `{new_payload}` at {node_id}",
                kind.as_str()
            )
        }
        EditIntent::PrependComment => "prepend comment".into(),
        EditIntent::InsertStatement => "insert statement".into(),
        EditIntent::DeleteStatement => "delete statement".into(),
        EditIntent::InsertSubtree {
            parent,
            before,
            kind,
        } => format!(
            "insert {} under {parent}{}",
            kind.as_str(),
            before
                .map(|b| format!(" before {b}"))
                .unwrap_or_else(|| " at end".into())
        ),
        EditIntent::DeleteSubtree { node_id, kind, .. } => {
            format!("delete {} subtree at {node_id}", kind.as_str())
        }
        EditIntent::MoveNode {
            node_id,
            new_parent,
        } => format!("move {node_id} under {new_parent}"),
        EditIntent::MoveSubtree {
            node_id,
            new_parent,
        } => format!("move subtree {node_id} under {new_parent}"),
        EditIntent::RenamePath {
            from,
            to,
            with_edits,
        } => {
            if *with_edits {
                format!("rename path `{from}` -> `{to}` (with edits)")
            } else {
                format!("rename path `{from}` -> `{to}`")
            }
        }
        EditIntent::ReorderMembers { parent } => format!("reorder members under {parent}"),
        EditIntent::SetTrivia {
            parent,
            child,
            occurrence,
        } => format!("set trivia before {child} under {parent} (occ {occurrence})"),
        EditIntent::SetRootTrailingTrivia => "set root trailing trivia".into(),
    }
}

pub fn format_intent_lines(base: Option<&AstGraph>, mutations: &[Mutation]) -> Vec<String> {
    classify_mutations(base, mutations)
        .into_iter()
        .map(|(i, intent)| format!("  [{i}] {}", format_intent(base, &intent)))
        .collect()
}

/// Whether two intents touch disjoint logical edit sites and can merge together.
pub fn intents_disjoint(a: &EditIntent, b: &EditIntent) -> bool {
    match (a, b) {
        (
            EditIntent::RenameIdentifier { node_id: left, .. }
            | EditIntent::EditLiteral { node_id: left, .. }
            | EditIntent::EditPayload { node_id: left, .. },
            EditIntent::RenameIdentifier { node_id: right, .. }
            | EditIntent::EditLiteral { node_id: right, .. }
            | EditIntent::EditPayload { node_id: right, .. },
        ) => left != right,
        (
            EditIntent::InsertSubtree {
                parent: p1,
                before: b1,
                ..
            },
            EditIntent::InsertSubtree {
                parent: p2,
                before: b2,
                ..
            },
        ) => p1 != p2 || b1 != b2,
        (EditIntent::PrependComment, EditIntent::InsertStatement)
        | (EditIntent::InsertStatement, EditIntent::PrependComment)
        | (EditIntent::PrependComment, EditIntent::RenameIdentifier { .. })
        | (EditIntent::RenameIdentifier { .. }, EditIntent::PrependComment)
        | (EditIntent::PrependComment, EditIntent::EditLiteral { .. })
        | (EditIntent::EditLiteral { .. }, EditIntent::PrependComment)
        | (EditIntent::PrependComment, EditIntent::EditPayload { .. })
        | (EditIntent::EditPayload { .. }, EditIntent::PrependComment)
        | (EditIntent::InsertStatement, EditIntent::RenameIdentifier { .. })
        | (EditIntent::RenameIdentifier { .. }, EditIntent::InsertStatement)
        | (EditIntent::InsertStatement, EditIntent::EditLiteral { .. })
        | (EditIntent::EditLiteral { .. }, EditIntent::InsertStatement)
        | (EditIntent::InsertStatement, EditIntent::EditPayload { .. })
        | (EditIntent::EditPayload { .. }, EditIntent::InsertStatement) => true,
        (EditIntent::ReorderMembers { .. }, EditIntent::InsertSubtree { .. })
        | (EditIntent::InsertSubtree { .. }, EditIntent::ReorderMembers { .. })
        | (EditIntent::ReorderMembers { .. }, EditIntent::RenameIdentifier { .. })
        | (EditIntent::RenameIdentifier { .. }, EditIntent::ReorderMembers { .. })
        | (EditIntent::ReorderMembers { .. }, EditIntent::EditLiteral { .. })
        | (EditIntent::EditLiteral { .. }, EditIntent::ReorderMembers { .. })
        | (EditIntent::ReorderMembers { .. }, EditIntent::EditPayload { .. })
        | (EditIntent::EditPayload { .. }, EditIntent::ReorderMembers { .. }) => true,
        (
            EditIntent::SetTrivia {
                parent: p1,
                child: c1,
                occurrence: o1,
            },
            EditIntent::SetTrivia {
                parent: p2,
                child: c2,
                occurrence: o2,
            },
        ) => p1 != p2 || c1 != c2 || o1 != o2,
        (
            EditIntent::RenameIdentifier { node_id: left, .. }
            | EditIntent::EditLiteral { node_id: left, .. }
            | EditIntent::EditPayload { node_id: left, .. },
            EditIntent::SetTrivia { child: right, .. },
        )
        | (
            EditIntent::SetTrivia { child: left, .. },
            EditIntent::RenameIdentifier { node_id: right, .. }
            | EditIntent::EditLiteral { node_id: right, .. }
            | EditIntent::EditPayload { node_id: right, .. },
        ) => left != right,
        (EditIntent::SetTrivia { .. }, EditIntent::InsertStatement)
        | (EditIntent::InsertStatement, EditIntent::SetTrivia { .. })
        | (EditIntent::SetTrivia { .. }, EditIntent::PrependComment)
        | (EditIntent::PrependComment, EditIntent::SetTrivia { .. })
        | (EditIntent::SetRootTrailingTrivia, EditIntent::RenameIdentifier { .. })
        | (EditIntent::RenameIdentifier { .. }, EditIntent::SetRootTrailingTrivia)
        | (EditIntent::SetRootTrailingTrivia, EditIntent::EditLiteral { .. })
        | (EditIntent::EditLiteral { .. }, EditIntent::SetRootTrailingTrivia)
        | (EditIntent::SetRootTrailingTrivia, EditIntent::EditPayload { .. })
        | (EditIntent::EditPayload { .. }, EditIntent::SetRootTrailingTrivia)
        | (EditIntent::SetRootTrailingTrivia, EditIntent::InsertStatement)
        | (EditIntent::InsertStatement, EditIntent::SetRootTrailingTrivia)
        | (EditIntent::SetRootTrailingTrivia, EditIntent::PrependComment)
        | (EditIntent::PrependComment, EditIntent::SetRootTrailingTrivia) => true,
        (
            EditIntent::MoveNode { node_id: left, .. }
            | EditIntent::MoveSubtree { node_id: left, .. },
            EditIntent::RenameIdentifier { node_id: right, .. }
            | EditIntent::EditLiteral { node_id: right, .. }
            | EditIntent::EditPayload { node_id: right, .. },
        )
        | (
            EditIntent::RenameIdentifier { node_id: left, .. }
            | EditIntent::EditLiteral { node_id: left, .. }
            | EditIntent::EditPayload { node_id: left, .. },
            EditIntent::MoveNode { node_id: right, .. }
            | EditIntent::MoveSubtree { node_id: right, .. },
        ) if left == right => true,
        (
            EditIntent::MoveSubtree { node_id: p1, .. },
            EditIntent::MoveSubtree { node_id: p2, .. },
        ) if p1 != p2 => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::parse_rust;

    #[test]
    fn classifies_literal_edit() {
        let base = parse_rust("pub fn answer() -> i32 {\n    42\n}\n").unwrap();
        let next = parse_rust("pub fn answer() -> i32 {\n    43\n}\n").unwrap();
        let diff = crate::diff::diff_graphs(&base, &next);
        let intents = classify_mutations(Some(&base), &diff.mutations);
        assert!(
            intents
                .iter()
                .any(|(_, i)| matches!(i, EditIntent::EditLiteral { .. })),
            "expected literal edit intent, got {intents:?}"
        );
    }

    #[test]
    fn disjoint_literal_intents() {
        let a = EditIntent::EditLiteral {
            node_id: NodeId::nil(),
            new_value: "1".into(),
        };
        let b = EditIntent::EditLiteral {
            node_id: NodeId::from_parts("Literal", "x", &[]),
            new_value: "2".into(),
        };
        assert!(intents_disjoint(&a, &b));
    }
}
