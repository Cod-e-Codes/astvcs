use super::edge::{TriviaRecord, TriviaSlot};
use super::mutation::Mutation;
use super::node::{Node, NodeId};
use std::collections::{HashMap, HashSet};

/// In-memory DAG storage for semantic AST state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AstGraph {
    pub nodes: HashMap<NodeId, Node>,
    pub root: NodeId,
    pub parents: HashMap<NodeId, NodeId>,
    pub trivia: HashMap<TriviaSlot, String>,
    pub root_trailing_trivia: String,
}

/// JSON-serializable representation of an [`AstGraph`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AstGraphSnapshot {
    pub nodes: Vec<Node>,
    pub root: NodeId,
    pub trivia: Vec<TriviaRecord>,
    pub root_trailing_trivia: String,
}

impl AstGraph {
    pub fn to_snapshot(&self) -> AstGraphSnapshot {
        let mut nodes: Vec<_> = self.nodes.values().cloned().collect();
        nodes.sort_by_key(|n| n.id);
        let mut trivia: Vec<_> = self
            .trivia
            .iter()
            .map(|(slot, leading)| TriviaRecord {
                parent: slot.parent,
                child: slot.child,
                occurrence: slot.occurrence,
                leading: leading.clone(),
            })
            .collect();
        trivia.sort_by(|a, b| {
            (a.parent, a.child, a.occurrence).cmp(&(b.parent, b.child, b.occurrence))
        });
        AstGraphSnapshot {
            nodes,
            root: self.root,
            trivia,
            root_trailing_trivia: self.root_trailing_trivia.clone(),
        }
    }

    pub fn from_snapshot(snapshot: AstGraphSnapshot) -> Self {
        let mut nodes = HashMap::new();
        for node in snapshot.nodes {
            nodes.insert(node.id, node);
        }
        let _root = nodes
            .get(&snapshot.root)
            .cloned()
            .unwrap_or_else(|| Node::new(super::node::NodeKind::Module, String::new(), vec![]));
        let mut graph = Self {
            nodes,
            root: snapshot.root,
            parents: HashMap::new(),
            trivia: HashMap::new(),
            root_trailing_trivia: snapshot.root_trailing_trivia,
        };
        graph.rebuild_parents();
        for record in snapshot.trivia {
            graph.set_trivia(
                record.parent,
                record.child,
                record.occurrence,
                record.leading,
            );
        }
        graph
    }
}

impl AstGraph {
    pub fn new(root: Node, mut nodes: HashMap<NodeId, Node>) -> Self {
        nodes.insert(root.id, root.clone());
        let mut g = Self {
            nodes,
            root: root.id,
            parents: HashMap::new(),
            trivia: HashMap::new(),
            root_trailing_trivia: String::new(),
        };
        g.rebuild_parents();
        g
    }

    pub fn empty() -> Self {
        let root = Node::new(super::node::NodeKind::Module, String::new(), vec![]);
        Self::new(root, HashMap::new())
    }

    pub fn get(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn insert(&mut self, node: Node) -> NodeId {
        let id = node.id;
        self.nodes.insert(id, node);
        id
    }

    pub fn parent_of(&self, id: &NodeId) -> Option<NodeId> {
        self.parents.get(id).copied()
    }

    /// Whether `ancestor` appears on the parent chain of `node` (including when equal).
    pub fn is_ancestor_of(&self, ancestor: &NodeId, node: &NodeId) -> bool {
        let mut current = Some(*node);
        while let Some(id) = current {
            if &id == ancestor {
                return true;
            }
            current = self.parent_of(&id);
        }
        false
    }

    pub fn children_of(&self, id: &NodeId) -> Option<Vec<NodeId>> {
        self.nodes.get(id).map(|n| n.children.clone())
    }

    pub fn set_trivia(&mut self, parent: NodeId, child: NodeId, occurrence: u32, trivia: String) {
        let slot = TriviaSlot::before_child(parent, child, occurrence);
        if trivia.is_empty() {
            self.trivia.remove(&slot);
        } else {
            self.trivia.insert(slot, trivia);
        }
    }

    pub fn get_trivia(&self, parent: NodeId, child: NodeId, occurrence: u32) -> &str {
        self.trivia
            .get(&TriviaSlot::before_child(parent, child, occurrence))
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn rebuild_parents(&mut self) {
        self.parents.clear();
        let ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        for id in ids {
            if let Some(children) = self.nodes.get(&id).map(|n| n.children.clone()) {
                for child in children {
                    self.parents.insert(child, id);
                }
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.nodes.contains_key(&self.root) {
            return Err("root node missing".into());
        }
        for node in self.nodes.values() {
            for child in &node.children {
                if !self.nodes.contains_key(child) {
                    return Err(format!("dangling child reference: {child}"));
                }
            }
        }
        Ok(())
    }

    pub fn collect_subtree(&self, root: NodeId) -> Vec<Node> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(node) = self.nodes.get(&id) {
                out.push(node.clone());
                for child in node.children.iter().rev() {
                    stack.push(*child);
                }
            }
        }
        out
    }

    pub fn apply(&mut self, mutation: &Mutation) -> Result<Vec<(NodeId, NodeId)>, String> {
        let mut cascades = Vec::new();
        match mutation {
            Mutation::InsertSubtree {
                parent,
                before,
                before_occurrence,
                node,
                descendants,
                trivia,
            } => {
                if !self.nodes.contains_key(parent) {
                    return Err(format!("insert parent not found: {parent}"));
                }
                for desc in descendants {
                    self.nodes.insert(desc.id, desc.clone());
                }
                self.nodes.insert(node.id, node.clone());
                self.splice_children(
                    *parent,
                    |children| {
                        insert_child_before(children, node.id, *before, *before_occurrence);
                        Ok(())
                    },
                    &mut cascades,
                )?;
                self.apply_trivia_records(trivia, &cascades);
            }
            Mutation::DeleteSubtree { parent, node_id } => {
                if !self.nodes.contains_key(node_id) {
                    return Err(format!("delete target not found: {node_id}"));
                }
                self.splice_children(
                    *parent,
                    |children| {
                        children.retain(|c| c != node_id);
                        Ok(())
                    },
                    &mut cascades,
                )?;
                self.remove_subtree_nodes(*node_id);
            }
            Mutation::MoveNode {
                node_id,
                new_parent,
                before,
            }
            | Mutation::MoveSubtree {
                node_id,
                new_parent,
                before,
            } => {
                if !self.nodes.contains_key(node_id) {
                    return Err(format!("move target not found: {node_id}"));
                }
                let current_parent = self
                    .parents
                    .get(node_id)
                    .copied()
                    .ok_or_else(|| format!("move: node has no parent: {node_id}"))?;
                let mut target_parent = redirect(*new_parent, &cascades);
                let saved_trivia = self.take_child_trivia(current_parent, *node_id);
                if current_parent == target_parent {
                    self.splice_children(
                        current_parent,
                        |children| {
                            reposition_child(children, *node_id, *before)?;
                            Ok(())
                        },
                        &mut cascades,
                    )?;
                    target_parent = redirect(current_parent, &cascades);
                } else {
                    self.splice_children(
                        current_parent,
                        |children| {
                            children.retain(|c| c != node_id);
                            Ok(())
                        },
                        &mut cascades,
                    )?;
                    target_parent = redirect(target_parent, &cascades);
                    self.splice_children(
                        target_parent,
                        |children| {
                            insert_child_before(children, *node_id, *before, None);
                            Ok(())
                        },
                        &mut cascades,
                    )?;
                    target_parent = redirect(target_parent, &cascades);
                }
                let dest_children = self.get(&target_parent).unwrap().children.clone();
                let dest_occ = child_occurrence_at(&dest_children, *node_id);
                for (_, leading) in saved_trivia {
                    self.set_trivia(target_parent, *node_id, dest_occ, leading);
                }
            }
            Mutation::RenameIdentifier {
                node_id,
                new_name,
                parent,
            } => {
                self.edit_payload(*node_id, new_name.clone(), *parent, &mut cascades)?;
            }
            Mutation::EditPayload {
                node_id,
                new_payload,
                parent,
            } => {
                self.edit_payload(*node_id, new_payload.clone(), *parent, &mut cascades)?;
            }
            Mutation::ReorderChildren { parent, new_order } => {
                self.splice_children(
                    *parent,
                    |children| {
                        *children = new_order.clone();
                        Ok(())
                    },
                    &mut cascades,
                )?;
            }
            Mutation::SetTrivia {
                parent,
                child,
                occurrence,
                leading,
            } => {
                self.set_trivia(*parent, *child, *occurrence, leading.clone());
            }
            Mutation::SetRootTrailingTrivia { trailing } => {
                self.root_trailing_trivia = trailing.clone();
            }
        }
        Ok(cascades)
    }

    pub fn apply_batch(&mut self, mutations: &[Mutation]) -> Result<(), String> {
        let mut redirect_table: HashMap<NodeId, NodeId> = HashMap::new();
        for mutation in mutations {
            let remapped = remap_mutation(mutation, &redirect_table);
            let cascades = self.apply(&remapped)?;
            for (old, new) in cascades {
                redirect_table.insert(old, new);
            }
        }
        Ok(())
    }

    fn child_reference_count(&self, node_id: NodeId) -> usize {
        self.nodes
            .values()
            .flat_map(|n| n.children.iter())
            .filter(|c| **c == node_id)
            .count()
    }

    fn migrate_child_trivia(&mut self, parent: NodeId, old_child: NodeId, new_child: NodeId) {
        let slots: Vec<TriviaSlot> = self
            .trivia
            .keys()
            .filter(|slot| slot.parent == parent && slot.child == old_child)
            .copied()
            .collect();
        for slot in slots {
            if let Some(leading) = self.trivia.remove(&slot) {
                self.trivia.insert(
                    TriviaSlot::before_child(parent, new_child, slot.occurrence),
                    leading,
                );
            }
        }
    }

    fn edit_payload(
        &mut self,
        node_id: NodeId,
        new_payload: String,
        scope_parent: Option<NodeId>,
        cascades: &mut Vec<(NodeId, NodeId)>,
    ) -> Result<(), String> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or_else(|| format!("node not found: {node_id}"))?
            .clone();
        let new_node = Node::new(node.kind.clone(), new_payload, node.children.clone());
        let new_id = new_node.id;
        let ref_count = self.child_reference_count(node_id);

        if ref_count > 1 {
            let parent = scope_parent.ok_or_else(|| {
                format!(
                    "cannot edit shared node {node_id} without parent scope ({ref_count} references)"
                )
            })?;
            if !self
                .nodes
                .get(&parent)
                .map(|n| n.children.contains(&node_id))
                .unwrap_or(false)
            {
                return Err(format!(
                    "edit parent {parent} does not reference shared node {node_id}"
                ));
            }
            self.nodes.insert(new_id, new_node);
            self.migrate_child_trivia(parent, node_id, new_id);
            self.splice_children(
                parent,
                |children| {
                    if let Some(pos) = children.iter().position(|c| *c == node_id) {
                        children[pos] = new_id;
                    }
                    Ok(())
                },
                cascades,
            )?;
            return Ok(());
        }

        self.nodes.remove(&node_id);
        self.nodes.insert(new_id, new_node);
        self.rekey_trivia_node(node_id, new_id);
        if let Some(parent) = self.parents.get(&node_id).copied() {
            self.splice_children(
                parent,
                |children| {
                    if let Some(pos) = children.iter().position(|c| *c == node_id) {
                        children[pos] = new_id;
                    }
                    Ok(())
                },
                cascades,
            )?;
        }
        Ok(())
    }

    fn splice_children<F>(
        &mut self,
        parent_id: NodeId,
        mut f: F,
        cascades: &mut Vec<(NodeId, NodeId)>,
    ) -> Result<(), String>
    where
        F: FnMut(&mut Vec<NodeId>) -> Result<(), String>,
    {
        let parent = self
            .nodes
            .get(&parent_id)
            .ok_or_else(|| format!("parent not found: {parent_id}"))?
            .clone();
        let mut children = parent.children.clone();
        f(&mut children)?;
        let new_parent = Node::new(parent.kind.clone(), parent.payload.clone(), children);
        self.nodes.remove(&parent_id);
        self.nodes.insert(new_parent.id, new_parent.clone());
        self.rekey_trivia_node(parent_id, new_parent.id);
        cascades.push((parent_id, new_parent.id));
        self.cascade_id_change(parent_id, new_parent.id, cascades)?;
        Ok(())
    }

    fn cascade_id_change(
        &mut self,
        old_id: NodeId,
        new_id: NodeId,
        cascades: &mut Vec<(NodeId, NodeId)>,
    ) -> Result<(), String> {
        if old_id == new_id {
            return Ok(());
        }
        if self.root == old_id {
            self.root = new_id;
        }

        let mut current_old = old_id;
        let mut current_new = new_id;
        while let Some(ancestor) = self.parents.get(&current_old).copied() {
            let ancestor_node = self
                .nodes
                .get(&ancestor)
                .ok_or_else(|| format!("ancestor not found: {ancestor}"))?
                .clone();
            let mut children = ancestor_node.children.clone();
            if let Some(pos) = children.iter().position(|c| *c == current_old) {
                children[pos] = current_new;
            }
            let resealed = Node::new(
                ancestor_node.kind.clone(),
                ancestor_node.payload.clone(),
                children,
            );
            if resealed.id == ancestor {
                break;
            }
            self.nodes.remove(&ancestor);
            self.nodes.insert(resealed.id, resealed.clone());
            self.rekey_trivia_node(ancestor, resealed.id);
            cascades.push((ancestor, resealed.id));
            if self.root == ancestor {
                self.root = resealed.id;
            }
            current_old = ancestor;
            current_new = resealed.id;
        }
        self.rebuild_parents();
        Ok(())
    }

    fn rekey_trivia_parent(&mut self, old_parent: NodeId, new_parent: NodeId) {
        let entries: Vec<(TriviaSlot, String)> = self
            .trivia
            .iter()
            .filter(|(slot, _)| slot.parent == old_parent)
            .map(|(slot, text)| (*slot, text.clone()))
            .collect();
        for (slot, text) in entries {
            self.trivia.remove(&slot);
            self.trivia.insert(
                TriviaSlot::before_child(new_parent, slot.child, slot.occurrence),
                text,
            );
        }
    }

    fn rekey_trivia_child(&mut self, old_child: NodeId, new_child: NodeId) {
        let entries: Vec<(TriviaSlot, String)> = self
            .trivia
            .iter()
            .filter(|(slot, _)| slot.child == old_child)
            .map(|(slot, text)| (*slot, text.clone()))
            .collect();
        for (slot, text) in entries {
            self.trivia.remove(&slot);
            self.trivia.insert(
                TriviaSlot::before_child(slot.parent, new_child, slot.occurrence),
                text,
            );
        }
    }

    fn rekey_trivia_node(&mut self, old_id: NodeId, new_id: NodeId) {
        if old_id == new_id {
            return;
        }
        self.rekey_trivia_parent(old_id, new_id);
        self.rekey_trivia_child(old_id, new_id);
    }

    /// Leading trivia for every child under nodes in the subtree rooted at `root`.
    pub fn collect_subtree_trivia(&self, root: NodeId) -> Vec<TriviaRecord> {
        let subtree: HashSet<NodeId> = self
            .collect_subtree(root)
            .into_iter()
            .map(|n| n.id)
            .collect();
        let mut records: Vec<TriviaRecord> = self
            .trivia
            .iter()
            .filter(|(slot, _)| subtree.contains(&slot.parent))
            .map(|(slot, leading)| TriviaRecord {
                parent: slot.parent,
                child: slot.child,
                occurrence: slot.occurrence,
                leading: leading.clone(),
            })
            .collect();
        records.sort_by(|a, b| {
            (a.parent, a.child, a.occurrence).cmp(&(b.parent, b.child, b.occurrence))
        });
        records
    }

    pub fn apply_trivia_records(
        &mut self,
        records: &[TriviaRecord],
        redirect_table: &[(NodeId, NodeId)],
    ) {
        for record in records {
            let parent = redirect(record.parent, redirect_table);
            let child = redirect(record.child, redirect_table);
            self.set_trivia(parent, child, record.occurrence, record.leading.clone());
        }
    }

    pub fn take_child_trivia(&mut self, parent: NodeId, child: NodeId) -> Vec<(u32, String)> {
        let slots: Vec<TriviaSlot> = self
            .trivia
            .keys()
            .filter(|slot| slot.parent == parent && slot.child == child)
            .copied()
            .collect();
        let mut out = Vec::new();
        for slot in slots {
            if let Some(text) = self.trivia.remove(&slot) {
                out.push((slot.occurrence, text));
            }
        }
        out.sort_by_key(|(occ, _)| *occ);
        out
    }

    fn remove_subtree_nodes(&mut self, root: NodeId) {
        let candidates: HashSet<NodeId> = self
            .collect_subtree(root)
            .into_iter()
            .map(|n| n.id)
            .collect();
        for node_id in &candidates {
            let referenced_outside = self
                .nodes
                .values()
                .any(|n| !candidates.contains(&n.id) && n.children.contains(node_id));
            if referenced_outside {
                continue;
            }
            self.trivia
                .retain(|slot, _| slot.parent != *node_id && slot.child != *node_id);
            self.parents.remove(node_id);
            self.nodes.remove(node_id);
        }
    }
}

fn child_occurrence_at(children: &[NodeId], child: NodeId) -> u32 {
    let index = children
        .iter()
        .position(|c| *c == child)
        .unwrap_or(children.len().saturating_sub(1));
    children[..index].iter().filter(|c| **c == child).count() as u32
}

fn insert_child_before(
    children: &mut Vec<NodeId>,
    node_id: NodeId,
    before: Option<NodeId>,
    before_occurrence: Option<u32>,
) {
    let idx = match before {
        None => children.len(),
        Some(anchor) => {
            let occ = before_occurrence.unwrap_or(0) as usize;
            children
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == anchor)
                .nth(occ)
                .map(|(i, _)| i)
                .unwrap_or(children.len())
        }
    };
    children.insert(idx, node_id);
}

fn reposition_child(
    children: &mut Vec<NodeId>,
    node_id: NodeId,
    before: Option<NodeId>,
) -> Result<(), String> {
    let current = children
        .iter()
        .position(|c| *c == node_id)
        .ok_or_else(|| format!("move: node not in parent children: {node_id}"))?;
    children.remove(current);
    insert_child_before(children, node_id, before, None);
    Ok(())
}

pub fn redirect(id: NodeId, table: &[(NodeId, NodeId)]) -> NodeId {
    let mut current = id;
    loop {
        let mut changed = false;
        for (old, new) in table {
            if current == *old {
                current = *new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    current
}

pub fn redirect_map(id: NodeId, table: &HashMap<NodeId, NodeId>) -> NodeId {
    let mut current = id;
    loop {
        match table.get(&current) {
            Some(new) if *new != current => current = *new,
            _ => break,
        }
    }
    current
}

pub fn remap_mutation(mutation: &Mutation, table: &HashMap<NodeId, NodeId>) -> Mutation {
    match mutation {
        Mutation::InsertSubtree {
            parent,
            before,
            before_occurrence,
            node,
            descendants,
            trivia,
        } => Mutation::InsertSubtree {
            parent: redirect_map(*parent, table),
            before: before.map(|id| redirect_map(id, table)),
            before_occurrence: *before_occurrence,
            node: node.clone(),
            descendants: descendants.clone(),
            trivia: trivia
                .iter()
                .map(|record| TriviaRecord {
                    parent: redirect_map(record.parent, table),
                    child: redirect_map(record.child, table),
                    occurrence: record.occurrence,
                    leading: record.leading.clone(),
                })
                .collect(),
        },
        Mutation::DeleteSubtree { parent, node_id } => Mutation::DeleteSubtree {
            parent: redirect_map(*parent, table),
            node_id: redirect_map(*node_id, table),
        },
        Mutation::MoveNode {
            node_id,
            new_parent,
            before,
        } => Mutation::MoveNode {
            node_id: redirect_map(*node_id, table),
            new_parent: redirect_map(*new_parent, table),
            before: before.map(|id| redirect_map(id, table)),
        },
        Mutation::MoveSubtree {
            node_id,
            new_parent,
            before,
        } => Mutation::MoveSubtree {
            node_id: redirect_map(*node_id, table),
            new_parent: redirect_map(*new_parent, table),
            before: before.map(|id| redirect_map(id, table)),
        },
        Mutation::RenameIdentifier {
            node_id,
            new_name,
            parent,
        } => Mutation::RenameIdentifier {
            node_id: redirect_map(*node_id, table),
            new_name: new_name.clone(),
            parent: parent.map(|id| redirect_map(id, table)),
        },
        Mutation::EditPayload {
            node_id,
            new_payload,
            parent,
        } => Mutation::EditPayload {
            node_id: redirect_map(*node_id, table),
            new_payload: new_payload.clone(),
            parent: parent.map(|id| redirect_map(id, table)),
        },
        Mutation::ReorderChildren { parent, new_order } => Mutation::ReorderChildren {
            parent: redirect_map(*parent, table),
            new_order: new_order
                .iter()
                .map(|id| redirect_map(*id, table))
                .collect(),
        },
        Mutation::SetTrivia {
            parent,
            child,
            occurrence,
            leading,
        } => Mutation::SetTrivia {
            parent: redirect_map(*parent, table),
            child: redirect_map(*child, table),
            occurrence: *occurrence,
            leading: leading.clone(),
        },
        Mutation::SetRootTrailingTrivia { trailing } => Mutation::SetRootTrailingTrivia {
            trailing: trailing.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::NodeKind;

    fn sample_graph() -> (AstGraph, NodeId, NodeId, NodeId) {
        let x = Node::leaf(NodeKind::Identifier, "x".into());
        let y = Node::leaf(NodeKind::Identifier, "y".into());
        let block_a = Node::new(NodeKind::Block, String::new(), vec![x.id]);
        let block_b = Node::new(NodeKind::Block, String::new(), vec![y.id]);
        let root = Node::new(
            NodeKind::Module,
            String::new(),
            vec![block_a.id, block_b.id],
        );
        let mut nodes = HashMap::new();
        let x_id = x.id;
        nodes.insert(x.id, x);
        nodes.insert(y.id, y);
        nodes.insert(block_a.id, block_a.clone());
        nodes.insert(block_b.id, block_b.clone());
        (AstGraph::new(root, nodes), block_a.id, block_b.id, x_id)
    }

    #[test]
    fn insert_reseals_parent() {
        let (mut g, block_id, _, _) = sample_graph();
        let z = Node::leaf(NodeKind::Identifier, "z".into());
        g.apply(&Mutation::InsertSubtree {
            parent: block_id,
            before: None,
            before_occurrence: None,
            node: z.clone(),
            descendants: vec![z.clone()],
            trivia: vec![],
        })
        .unwrap();
        let parent = g.parent_of(&z.id).unwrap();
        assert!(g.get(&parent).is_some());
        assert_eq!(g.get(&parent).unwrap().children.len(), 2);
    }

    #[test]
    fn edit_payload_copy_on_write_shared_literal() {
        use crate::frontend::parse_rust;
        use crate::unparser::unparse;

        let src = "fn demo() {\n    call(1, 2);\n    let x = 1;\n}\n";
        let mut g = parse_rust(src).unwrap();
        let one_id = g
            .nodes
            .values()
            .find(|n| n.kind == NodeKind::Literal && n.payload == "1")
            .unwrap()
            .id;

        let let_parent = g
            .nodes
            .values()
            .find(|n| {
                n.children.contains(&one_id)
                    && n.children
                        .iter()
                        .any(|c| g.get(c).map(|x| x.payload == "x").unwrap_or(false))
            })
            .unwrap()
            .id;

        g.apply(&Mutation::EditPayload {
            node_id: one_id,
            new_payload: "2".into(),
            parent: Some(let_parent),
        })
        .unwrap();
        g.validate().unwrap();
        let out = unparse(&g);
        assert!(out.contains("call(1, 2)"));
        assert!(out.contains("x = 2"));
    }

    #[test]
    fn rename_cascades_upward() {
        let (mut g, block_a_id, _, x_id) = sample_graph();
        g.apply(&Mutation::RenameIdentifier {
            node_id: x_id,
            new_name: "renamed".into(),
            parent: Some(block_a_id),
        })
        .unwrap();
        assert!(g.nodes.values().any(|n| n.payload == "renamed"));
    }

    #[test]
    fn move_node_between_blocks() {
        let (mut g, _block_a_id, block_b_id, x_id) = sample_graph();
        g.apply(&Mutation::MoveNode {
            node_id: x_id,
            new_parent: block_b_id,
            before: None,
        })
        .unwrap();
        let parent = g.parent_of(&x_id).unwrap();
        let parent_node = g.get(&parent).unwrap();
        assert!(parent_node.children.contains(&x_id));
        assert_eq!(parent_node.children.len(), 2);
    }
}
