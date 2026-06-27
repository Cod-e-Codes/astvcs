use super::node::NodeId;

/// Positional metadata: the gap before a specific child occurrence under a parent.
///
/// Trivia is keyed by `(parent, child, occurrence)` because content-addressed child ids
/// may appear more than once under the same parent (e.g. two `"` quote tokens).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TriviaSlot {
    pub parent: NodeId,
    pub child: NodeId,
    pub occurrence: u32,
}

impl TriviaSlot {
    pub fn before_child(parent: NodeId, child: NodeId, occurrence: u32) -> Self {
        Self {
            parent,
            child,
            occurrence,
        }
    }
}

/// Serializable leading trivia attached before a child occurrence.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TriviaRecord {
    pub parent: NodeId,
    pub child: NodeId,
    pub occurrence: u32,
    pub leading: String,
}
