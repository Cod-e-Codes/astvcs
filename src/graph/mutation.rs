use super::edge::TriviaRecord;
use super::node::{Node, NodeId};

/// Structural edit operations recorded on the mutation timeline.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mutation {
    /// Insert a subtree under `parent` immediately before `before`, or at the end when `before` is None.
    /// `descendants` must contain every node reachable from `node`.
    /// `trivia` carries leading gaps from the source graph for the inserted subtree.
    InsertSubtree {
        parent: NodeId,
        before: Option<NodeId>,
        node: Node,
        descendants: Vec<Node>,
        trivia: Vec<TriviaRecord>,
    },
    DeleteSubtree {
        parent: NodeId,
        node_id: NodeId,
    },
    /// Reposition `node_id` under `new_parent`, before `before` (or at end when None).
    MoveNode {
        node_id: NodeId,
        new_parent: NodeId,
        before: Option<NodeId>,
    },
    /// Relocate an unchanged or edit-only subtree detected after LCS alignment fails.
    /// Preserves `node_id` so concurrent payload edits on the other branch still apply.
    MoveSubtree {
        node_id: NodeId,
        new_parent: NodeId,
        before: Option<NodeId>,
    },
    RenameIdentifier {
        node_id: NodeId,
        new_name: String,
    },
    EditPayload {
        node_id: NodeId,
        new_payload: String,
    },
    /// Replace the full child order under `parent`.
    ReorderChildren {
        parent: NodeId,
        new_order: Vec<NodeId>,
    },
    /// Set leading trivia before `child` under `parent` at `occurrence`.
    SetTrivia {
        parent: NodeId,
        child: NodeId,
        occurrence: u32,
        leading: String,
    },
    /// Replace trailing trivia after the root node.
    SetRootTrailingTrivia {
        trailing: String,
    },
}

impl Mutation {
    pub fn primary_node(&self) -> Option<NodeId> {
        match self {
            Self::InsertSubtree { node, .. } => Some(node.id),
            Self::DeleteSubtree { node_id, .. } => Some(*node_id),
            Self::MoveNode { node_id, .. } | Self::MoveSubtree { node_id, .. } => Some(*node_id),
            Self::RenameIdentifier { node_id, .. } => Some(*node_id),
            Self::EditPayload { node_id, .. } => Some(*node_id),
            Self::SetTrivia { child, .. } => Some(*child),
            Self::ReorderChildren { .. } | Self::SetRootTrailingTrivia { .. } => None,
        }
    }

    pub fn touched_parent(&self) -> Option<NodeId> {
        match self {
            Self::InsertSubtree { parent, .. }
            | Self::DeleteSubtree { parent, .. }
            | Self::ReorderChildren { parent, .. }
            | Self::SetTrivia { parent, .. } => Some(*parent),
            Self::MoveNode { new_parent, .. } | Self::MoveSubtree { new_parent, .. } => {
                Some(*new_parent)
            }
            Self::RenameIdentifier { .. } | Self::EditPayload { .. } => None,
            Self::SetRootTrailingTrivia { .. } => None,
        }
    }
}
