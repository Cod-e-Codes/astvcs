use crate::diff::align::{fingerprint_bucket_pairs, pair_in_order_by_key};
use crate::diff::lcs::lcs_pairs;
use crate::graph::{AstGraph, Mutation, NodeId, NodeKind, TriviaRecord};
use crate::trace;
use std::collections::{HashMap, HashSet};

/// How a sibling pair is aligned across old and new graphs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlignKind {
    Match,
    Insert,
    Delete,
}

/// Which algorithm produced a sibling match.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlignMethod {
    Id,
    Key,
    Role,
    Lcs,
    Fingerprint,
    StructuralFallback,
    LeafFallback,
}

/// A single alignment edge between an old sibling and a new sibling.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AlignEdge {
    pub old_id: Option<NodeId>,
    pub new_id: Option<NodeId>,
    pub kind: AlignKind,
    pub method: Option<AlignMethod>,
    pub parent_old: Option<NodeId>,
    pub parent_new: Option<NodeId>,
}

/// Result of diffing two AST graphs, including sibling alignment edges.
#[derive(Clone, Debug)]
pub struct DetailedDiffResult {
    pub mutations: Vec<Mutation>,
    pub alignment: Vec<AlignEdge>,
}

/// Result of diffing two AST graphs.
#[derive(Clone, Debug)]
pub struct DiffResult {
    pub mutations: Vec<Mutation>,
}

/// Shared graphs and output buffers for recursive structural diff.
struct DiffSession<'a> {
    old: &'a AstGraph,
    new: &'a AstGraph,
    out: &'a mut Vec<Mutation>,
    alignment: &'a mut Vec<AlignEdge>,
}

/// Sibling list state while matching children under one parent pair.
struct SiblingMatchFrame<'a> {
    old_children: &'a [NodeId],
    new_children: &'a [NodeId],
    old_node_id: NodeId,
    new_node_id: NodeId,
    matched_old: &'a mut [bool],
    matched_new: &'a mut [bool],
    scope_occurrence: Option<u32>,
}

/// Compute structural mutations transforming `old` into `new`.
pub fn diff_graphs(old: &AstGraph, new: &AstGraph) -> DiffResult {
    DiffResult {
        mutations: diff_graphs_detailed(old, new).mutations,
    }
}

/// Compute structural mutations and sibling alignment transforming `old` into `new`.
pub fn diff_graphs_detailed(old: &AstGraph, new: &AstGraph) -> DetailedDiffResult {
    let mut mutations = Vec::new();
    let mut alignment = Vec::new();
    alignment.push(AlignEdge {
        old_id: Some(old.root),
        new_id: Some(new.root),
        kind: AlignKind::Match,
        method: None,
        parent_old: None,
        parent_new: None,
    });
    let mut sess = DiffSession {
        old,
        new,
        out: &mut mutations,
        alignment: &mut alignment,
    };
    diff_subtree(&mut sess, old.root, new.root, None, None);
    if old.root_trailing_trivia != new.root_trailing_trivia {
        mutations.push(Mutation::SetRootTrailingTrivia {
            trailing: new.root_trailing_trivia.clone(),
        });
    }
    DetailedDiffResult {
        mutations,
        alignment,
    }
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

fn insert_anchor_new(children: &[NodeId], index: usize) -> Option<NodeId> {
    children.get(index + 1).copied()
}

fn child_struct_keys(graph: &AstGraph, children: &[NodeId]) -> Vec<(NodeKind, String)> {
    children
        .iter()
        .map(|id| {
            let node = graph.get(id).unwrap();
            (node.kind.clone(), node.payload.clone())
        })
        .collect()
}

fn sibling_occurrence_before(children: &[NodeId], index: usize) -> u32 {
    children
        .get(index)
        .map(|id| children[..index].iter().filter(|c| **c == *id).count() as u32)
        .unwrap_or(0)
}

fn sibling_occurrence_in_old(old_parent_children: &[NodeId], anchor: NodeId) -> u32 {
    let index = old_parent_children
        .iter()
        .rposition(|c| *c == anchor)
        .unwrap_or(old_parent_children.len());
    sibling_occurrence_before(old_parent_children, index)
}

fn closing_brace_in_children(old: &AstGraph, children: &[NodeId]) -> Option<(usize, NodeId)> {
    children.iter().enumerate().rev().find_map(|(i, id)| {
        old.get(id)
            .filter(|n| n.kind == NodeKind::Token && n.payload == "}")
            .map(|_| (i, *id))
    })
}

/// Content-addressed punctuation can make LCS pair the wrong comma occurrence. When the old
/// list already has a separator after the previous matched sibling, skip the phantom insert.
fn is_redundant_list_separator_insert(
    old_parent_children: &[NodeId],
    new_children: &[NodeId],
    matched_new: &[bool],
    new_index: usize,
    node: &crate::graph::Node,
) -> bool {
    if node.kind != NodeKind::Token {
        return false;
    }
    match node.payload.as_str() {
        "," | ";" => {}
        _ => return false,
    }
    let id = node.id;
    let old_count = old_parent_children.iter().filter(|c| **c == id).count();
    if old_count == 0 {
        return false;
    }
    let prev_matched = new_index > 0 && matched_new.get(new_index - 1).copied().unwrap_or(false);
    if !prev_matched {
        return false;
    }
    let new_ordinal = new_children[..=new_index]
        .iter()
        .filter(|c| **c == id)
        .count();
    new_ordinal <= old_count
}

/// Re-pair content-addressed siblings by ordinal position when LCS pairs the wrong occurrence.
/// When one side has fewer copies (an occurrence was edited to a new id), prefer pairing the
/// remaining identical ids at the same list index so the edited slot stays index-aligned.
fn repair_occurrence_aware_matches(
    old_children: &[NodeId],
    new_children: &[NodeId],
    matched_old: &mut [bool],
    matched_new: &mut [bool],
) {
    let mut old_counts: HashMap<NodeId, usize> = HashMap::new();
    let mut new_counts: HashMap<NodeId, usize> = HashMap::new();
    for id in old_children {
        *old_counts.entry(*id).or_default() += 1;
    }
    for id in new_children {
        *new_counts.entry(*id).or_default() += 1;
    }

    let mut seen = HashSet::new();
    for id in old_children.iter().chain(new_children.iter()) {
        if !seen.insert(*id) {
            continue;
        }
        let old_n = *old_counts.get(id).unwrap_or(&0);
        let new_n = *new_counts.get(id).unwrap_or(&0);
        if old_n <= 1 && new_n <= 1 {
            continue;
        }

        let old_idx: Vec<usize> = old_children
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == *id)
            .map(|(i, _)| i)
            .collect();
        let new_idx: Vec<usize> = new_children
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == *id)
            .map(|(i, _)| i)
            .collect();

        for &oi in &old_idx {
            matched_old[oi] = false;
        }
        for &ni in &new_idx {
            matched_new[ni] = false;
        }

        let mut used_old = vec![false; old_idx.len()];
        let mut used_new = vec![false; new_idx.len()];
        // Prefer same-index pairs first so [A, A] vs [A', A] keeps A@1 with A@1.
        for (oi_pos, &oi) in old_idx.iter().enumerate() {
            if let Some((ni_pos, _)) = new_idx
                .iter()
                .enumerate()
                .find(|(ni_pos, ni)| !used_new[*ni_pos] && **ni == oi)
            {
                matched_old[oi] = true;
                matched_new[new_idx[ni_pos]] = true;
                used_old[oi_pos] = true;
                used_new[ni_pos] = true;
            }
        }
        // Then pair remaining identical ids by minimizing index distance.
        for (oi_pos, &oi) in old_idx.iter().enumerate() {
            if used_old[oi_pos] {
                continue;
            }
            let Some((ni_pos, _)) = new_idx
                .iter()
                .enumerate()
                .filter(|(ni_pos, _)| !used_new[*ni_pos])
                .min_by_key(|(_, ni)| oi.abs_diff(**ni))
            else {
                break;
            };
            matched_old[oi] = true;
            matched_new[new_idx[ni_pos]] = true;
            used_old[oi_pos] = true;
            used_new[ni_pos] = true;
        }
    }
}

fn sibling_occurrence_at_index(children: &[NodeId], index: usize) -> u32 {
    let id = children[index];
    children[..index].iter().filter(|c| **c == id).count() as u32
}

fn duplicate_sibling_occurrence(children: &[NodeId], index: usize) -> Option<u32> {
    let id = children[index];
    if children.iter().filter(|c| **c == id).count() <= 1 {
        return None;
    }
    Some(sibling_occurrence_at_index(children, index))
}

fn ordered_matched_sibling_pairs(
    old_children: &[NodeId],
    new_children: &[NodeId],
    matched_old: &[bool],
    matched_new: &[bool],
) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut seen = HashSet::new();
    for id in old_children {
        if !seen.insert(*id) {
            continue;
        }
        let old_idx: Vec<usize> = old_children
            .iter()
            .enumerate()
            .filter(|(i, c)| **c == *id && matched_old[*i])
            .map(|(i, _)| i)
            .collect();
        let new_idx: Vec<usize> = new_children
            .iter()
            .enumerate()
            .filter(|(i, c)| **c == *id && matched_new[*i])
            .map(|(i, _)| i)
            .collect();
        for k in 0..old_idx.len().min(new_idx.len()) {
            pairs.push((old_idx[k], new_idx[k]));
        }
    }
    pairs.sort_by_key(|(oi, _)| *oi);
    pairs
}

fn is_list_element_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Parameter
            | NodeKind::Field
            | NodeKind::Declaration
            | NodeKind::Statement
            | NodeKind::Expression
            | NodeKind::Literal
    )
}

fn is_list_separator_token(node: &crate::graph::Node) -> bool {
    node.kind == NodeKind::Token && matches!(node.payload.as_str(), "," | ";")
}

/// Map a new-graph sibling index to an insert anchor in the old parent's child list.
/// over pending inserts to the next matched sibling present in the old graph. When the
/// immediate next sibling is new-only and no later matched anchor exists, keep the new
/// sibling id so apply can position the insert relative to later mutations in the batch.
/// Returns `(before, before_occurrence)` so duplicate content-addressed anchors resolve
/// to the intended list position.
fn resolve_insert_before_in_old(
    old: &AstGraph,
    new: &AstGraph,
    old_parent_children: &[NodeId],
    new_children: &[NodeId],
    matched_new: &[bool],
    new_index: usize,
    new_child: &crate::graph::Node,
) -> (Option<NodeId>, Option<u32>) {
    if is_list_separator_token(new_child)
        && let Some(&next) = new_children.get(new_index + 1)
        && !matched_new.get(new_index + 1).copied().unwrap_or(false)
        && new
            .get(&next)
            .is_some_and(|n| is_list_element_kind(&n.kind))
    {
        return (Some(next), None);
    }

    let (before, before_occurrence) = if let Some(&next) = new_children.get(new_index + 1)
        && matched_new.get(new_index + 1).copied().unwrap_or(false)
        && old_parent_children.contains(&next)
    {
        (
            Some(next),
            Some(sibling_occurrence_in_old(old_parent_children, next)),
        )
    } else {
        let mut resolved = (None, None);
        for j in (new_index + 1)..new_children.len() {
            if !matched_new[j] {
                continue;
            }
            let anchor = new_children[j];
            if old_parent_children.contains(&anchor) {
                resolved = (
                    Some(anchor),
                    Some(sibling_occurrence_in_old(old_parent_children, anchor)),
                );
                break;
            }
        }
        if resolved.0.is_none() {
            if let Some(&next) = new_children.get(new_index + 1) {
                (Some(next), None)
            } else {
                (None, None)
            }
        } else {
            resolved
        }
    };

    let anchor_is_comma = before.is_some_and(|anchor| {
        old.get(&anchor)
            .is_some_and(|n| n.kind == NodeKind::Token && n.payload == ",")
    });
    let inserting_field = new_child.kind == NodeKind::Field
        || new_child.kind == NodeKind::Declaration
        || new_child.kind == NodeKind::Statement;
    if inserting_field
        && anchor_is_comma
        && let Some((i, brace)) = closing_brace_in_children(old, old_parent_children)
    {
        return (
            Some(brace),
            Some(sibling_occurrence_before(old_parent_children, i)),
        );
    }
    (before, before_occurrence)
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

fn unmatched_indices(matched: &[bool]) -> Vec<usize> {
    matched
        .iter()
        .enumerate()
        .filter_map(|(i, m)| (!m).then_some(i))
        .collect()
}

fn apply_role_match(
    sess: &mut DiffSession<'_>,
    frame: &mut SiblingMatchFrame<'_>,
    oi: usize,
    ni: usize,
    method: AlignMethod,
) {
    let oc = sess.old.get(&frame.old_children[oi]).unwrap();
    let nc = sess.new.get(&frame.new_children[ni]).unwrap();
    if oc.kind != nc.kind || oc.children.len() != nc.children.len() {
        return;
    }
    frame.matched_old[oi] = true;
    frame.matched_new[ni] = true;
    sess.alignment.push(AlignEdge {
        old_id: Some(frame.old_children[oi]),
        new_id: Some(frame.new_children[ni]),
        kind: AlignKind::Match,
        method: Some(method),
        parent_old: Some(frame.old_node_id),
        parent_new: Some(frame.new_node_id),
    });
    let child_occurrence =
        duplicate_sibling_occurrence(frame.old_children, oi).or(frame.scope_occurrence);
    if frame.old_children[oi] != frame.new_children[ni]
        && oi != ni
        && duplicate_sibling_occurrence(frame.old_children, oi).is_none()
    {
        sess.out.push(Mutation::MoveNode {
            node_id: frame.old_children[oi],
            new_parent: frame.old_node_id,
            before: insert_anchor_new(frame.new_children, ni),
        });
    }
    diff_subtree(
        sess,
        frame.old_children[oi],
        frame.new_children[ni],
        Some(frame.old_node_id),
        child_occurrence,
    );
    diff_child_trivia(
        sess.old,
        sess.new,
        frame.old_node_id,
        frame.new_node_id,
        frame.old_children[oi],
        frame.new_children[ni],
        sess.out,
    );
}

fn apply_key_match(
    sess: &mut DiffSession<'_>,
    frame: &mut SiblingMatchFrame<'_>,
    oi: usize,
    ni: usize,
    method: AlignMethod,
) {
    let oc = sess.old.get(&frame.old_children[oi]).unwrap();
    let nc = sess.new.get(&frame.new_children[ni]).unwrap();
    if oc.kind == nc.kind && !oc.is_leaf() {
        frame.matched_old[oi] = true;
        frame.matched_new[ni] = true;
        sess.alignment.push(AlignEdge {
            old_id: Some(frame.old_children[oi]),
            new_id: Some(frame.new_children[ni]),
            kind: AlignKind::Match,
            method: Some(method),
            parent_old: Some(frame.old_node_id),
            parent_new: Some(frame.new_node_id),
        });
        let child_occurrence =
            duplicate_sibling_occurrence(frame.old_children, oi).or(frame.scope_occurrence);
        if frame.old_children[oi] != frame.new_children[ni]
            && oi != ni
            && duplicate_sibling_occurrence(frame.old_children, oi).is_none()
        {
            sess.out.push(Mutation::MoveNode {
                node_id: frame.old_children[oi],
                new_parent: frame.old_node_id,
                before: insert_anchor_new(frame.new_children, ni),
            });
        }
        diff_subtree(
            sess,
            frame.old_children[oi],
            frame.new_children[ni],
            Some(frame.old_node_id),
            child_occurrence,
        );
        diff_child_trivia(
            sess.old,
            sess.new,
            frame.old_node_id,
            frame.new_node_id,
            frame.old_children[oi],
            frame.new_children[ni],
            sess.out,
        );
    }
}

fn unique_fingerprint_pairs(
    old: &AstGraph,
    new: &AstGraph,
    old_children: &[NodeId],
    new_children: &[NodeId],
    matched_old: &[bool],
    matched_new: &[bool],
) -> Vec<(usize, usize)> {
    let old_fps: Vec<(usize, Vec<StructureSig>)> = old_children
        .iter()
        .enumerate()
        .filter(|(oi, id)| !matched_old[*oi] && !old.get(id).unwrap().is_leaf())
        .map(|(oi, id)| (oi, structure_fingerprint(old, id)))
        .collect();
    let new_fps: Vec<(usize, Vec<StructureSig>)> = new_children
        .iter()
        .enumerate()
        .filter(|(ni, id)| !matched_new[*ni] && !new.get(id).unwrap().is_leaf())
        .map(|(ni, id)| (ni, structure_fingerprint(new, id)))
        .collect();
    fingerprint_bucket_pairs(&old_fps, &new_fps)
        .into_iter()
        .filter(|(oi, ni)| {
            old.get(&old_children[*oi]).unwrap().kind == new.get(&new_children[*ni]).unwrap().kind
        })
        .collect()
}

fn diff_subtree(
    sess: &mut DiffSession<'_>,
    old_id: NodeId,
    new_id: NodeId,
    context_parent_old: Option<NodeId>,
    scope_occurrence: Option<u32>,
) {
    let old_node = sess.old.get(&old_id).unwrap();
    let new_node = sess.new.get(&new_id).unwrap();

    if old_id == new_id {
        if old_node.kind == new_node.kind && !(old_node.is_leaf() && new_node.is_leaf()) {
            diff_children(sess, old_id, new_id, scope_occurrence);
        }
        return;
    }

    if old_node.kind == new_node.kind
        && old_node.children.is_empty()
        && new_node.children.is_empty()
        && old_node.payload != new_node.payload
    {
        if old_node.kind == NodeKind::Identifier {
            sess.out.push(Mutation::RenameIdentifier {
                node_id: old_id,
                new_name: new_node.payload.clone(),
                parent: context_parent_old,
                occurrence: scope_occurrence,
            });
        } else {
            sess.out.push(Mutation::EditPayload {
                node_id: old_id,
                new_payload: new_node.payload.clone(),
                parent: context_parent_old,
                occurrence: scope_occurrence,
            });
        }
        return;
    }

    if old_node.kind == new_node.kind && !(old_node.is_leaf() && new_node.is_leaf()) {
        diff_children(sess, old_id, new_id, scope_occurrence);
        return;
    }

    if sess.old.parent_of(&old_id).is_some() {
        sess.out.push(Mutation::DeleteSubtree {
            parent: sess.old.parent_of(&old_id).unwrap(),
            node_id: old_id,
        });
    }
    if let Some(parent) = sess.new.parent_of(&new_id) {
        let old_parent = find_corresponding_parent(sess.old, sess.new, parent);
        let descendants = sess.new.collect_subtree(new_id);
        let top = sess.new.get(&new_id).unwrap().clone();
        let new_parent_children = sess.new.get(&parent).unwrap().children.clone();
        let index = new_parent_children
            .iter()
            .position(|c| *c == new_id)
            .unwrap_or(0);
        let (before, before_occurrence) = resolve_insert_before_in_old(
            sess.old,
            sess.new,
            &sess.old.get(&old_parent).unwrap().children,
            &new_parent_children,
            &vec![true; new_parent_children.len()],
            index,
            &top,
        );
        sess.out.push(Mutation::InsertSubtree {
            parent: old_parent,
            before,
            before_occurrence,
            node: top,
            descendants,
            trivia: insert_trivia(sess.new, parent, new_id, old_parent),
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
    sess: &mut DiffSession<'_>,
    old_node_id: NodeId,
    new_node_id: NodeId,
    scope_occurrence: Option<u32>,
) {
    let old = sess.old;
    let new = sess.new;
    let old_children = old.get(&old_node_id).unwrap().children.clone();
    let new_children = new.get(&new_node_id).unwrap().children.clone();

    if old_children == new_children {
        for (i, &id) in old_children.iter().enumerate() {
            let child_occurrence =
                duplicate_sibling_occurrence(&old_children, i).or(scope_occurrence);
            sess.alignment.push(AlignEdge {
                old_id: Some(id),
                new_id: Some(id),
                kind: AlignKind::Match,
                method: Some(AlignMethod::Id),
                parent_old: Some(old_node_id),
                parent_new: Some(new_node_id),
            });
            diff_subtree(sess, id, id, Some(old_node_id), child_occurrence);
            diff_child_trivia(
                sess.old,
                sess.new,
                old_node_id,
                new_node_id,
                id,
                id,
                sess.out,
            );
        }
        return;
    }

    if same_id_multiset(&old_children, &new_children) && old_children != new_children {
        sess.out.push(Mutation::ReorderChildren {
            parent: old_node_id,
            new_order: new_children.clone(),
        });
        for (i, id) in old_children.iter().enumerate() {
            let child_occurrence =
                duplicate_sibling_occurrence(&old_children, i).or(scope_occurrence);
            sess.alignment.push(AlignEdge {
                old_id: Some(*id),
                new_id: Some(*id),
                kind: AlignKind::Match,
                method: Some(AlignMethod::Id),
                parent_old: Some(old_node_id),
                parent_new: Some(new_node_id),
            });
            diff_subtree(sess, *id, *id, Some(old_node_id), child_occurrence);
            if new_children.contains(id) {
                diff_child_trivia(
                    sess.old,
                    sess.new,
                    old_node_id,
                    new_node_id,
                    *id,
                    *id,
                    sess.out,
                );
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

    let mut matched_old = vec![false; old_children.len()];
    let mut matched_new = vec![false; new_children.len()];

    let wide_list = old_children.len() * new_children.len() > crate::diff::align::LCS_THRESHOLD;

    if wide_list {
        let old_struct = child_struct_keys(old, &old_children);
        let new_struct = child_struct_keys(new, &new_children);
        for (oi, ni) in lcs_pairs(&old_struct, &new_struct) {
            matched_old[oi] = true;
            matched_new[ni] = true;
            sess.alignment.push(AlignEdge {
                old_id: Some(old_children[oi]),
                new_id: Some(new_children[ni]),
                kind: AlignKind::Match,
                method: Some(AlignMethod::Id),
                parent_old: Some(old_node_id),
                parent_new: Some(new_node_id),
            });
            diff_subtree(
                sess,
                old_children[oi],
                new_children[ni],
                Some(old_node_id),
                duplicate_sibling_occurrence(&old_children, oi).or(scope_occurrence),
            );
            diff_child_trivia(
                sess.old,
                sess.new,
                old_node_id,
                new_node_id,
                old_children[oi],
                new_children[ni],
                sess.out,
            );
        }

        for (oi, ni) in pair_in_order_by_key(&old_keys, &new_keys) {
            if matched_old[oi] || matched_new[ni] {
                continue;
            }
            apply_key_match(
                sess,
                &mut SiblingMatchFrame {
                    old_children: &old_children,
                    new_children: &new_children,
                    old_node_id,
                    new_node_id,
                    matched_old: &mut matched_old,
                    matched_new: &mut matched_new,
                    scope_occurrence,
                },
                oi,
                ni,
                AlignMethod::Key,
            );
        }

        for (oi, ni) in pair_in_order_by_key(&old_roles, &new_roles) {
            if matched_old[oi] || matched_new[ni] {
                continue;
            }
            apply_role_match(
                sess,
                &mut SiblingMatchFrame {
                    old_children: &old_children,
                    new_children: &new_children,
                    old_node_id,
                    new_node_id,
                    matched_old: &mut matched_old,
                    matched_new: &mut matched_new,
                    scope_occurrence,
                },
                oi,
                ni,
                AlignMethod::Role,
            );
        }

        let unmatched_old = unmatched_indices(&matched_old);
        let unmatched_new = unmatched_indices(&matched_new);
        if unmatched_old.len() * unmatched_new.len() <= crate::diff::align::LCS_THRESHOLD {
            for (oi, ni) in lcs_pairs(&old_roles, &new_roles) {
                if matched_old[oi] || matched_new[ni] {
                    continue;
                }
                apply_role_match(
                    sess,
                    &mut SiblingMatchFrame {
                        old_children: &old_children,
                        new_children: &new_children,
                        old_node_id,
                        new_node_id,
                        matched_old: &mut matched_old,
                        matched_new: &mut matched_new,
                        scope_occurrence,
                    },
                    oi,
                    ni,
                    AlignMethod::Lcs,
                );
            }

            for (oi, ni) in lcs_pairs(&old_keys, &new_keys) {
                if matched_old[oi] || matched_new[ni] {
                    continue;
                }
                apply_key_match(
                    sess,
                    &mut SiblingMatchFrame {
                        old_children: &old_children,
                        new_children: &new_children,
                        old_node_id,
                        new_node_id,
                        matched_old: &mut matched_old,
                        matched_new: &mut matched_new,
                        scope_occurrence,
                    },
                    oi,
                    ni,
                    AlignMethod::Lcs,
                );
            }
        }
    } else {
        for (oi, ni) in lcs_pairs(&old_children, &new_children) {
            matched_old[oi] = true;
            matched_new[ni] = true;
        }
        repair_occurrence_aware_matches(
            &old_children,
            &new_children,
            &mut matched_old,
            &mut matched_new,
        );
        for (oi, ni) in
            ordered_matched_sibling_pairs(&old_children, &new_children, &matched_old, &matched_new)
        {
            sess.alignment.push(AlignEdge {
                old_id: Some(old_children[oi]),
                new_id: Some(new_children[ni]),
                kind: AlignKind::Match,
                method: Some(AlignMethod::Id),
                parent_old: Some(old_node_id),
                parent_new: Some(new_node_id),
            });
            diff_subtree(
                sess,
                old_children[oi],
                new_children[ni],
                Some(old_node_id),
                duplicate_sibling_occurrence(&old_children, oi).or(scope_occurrence),
            );
            diff_child_trivia(
                sess.old,
                sess.new,
                old_node_id,
                new_node_id,
                old_children[oi],
                new_children[ni],
                sess.out,
            );
        }

        for (oi, ni) in lcs_pairs(&old_roles, &new_roles) {
            if matched_old[oi] || matched_new[ni] {
                continue;
            }
            apply_role_match(
                sess,
                &mut SiblingMatchFrame {
                    old_children: &old_children,
                    new_children: &new_children,
                    old_node_id,
                    new_node_id,
                    matched_old: &mut matched_old,
                    matched_new: &mut matched_new,
                    scope_occurrence,
                },
                oi,
                ni,
                AlignMethod::Lcs,
            );
        }

        for (oi, ni) in lcs_pairs(&old_keys, &new_keys) {
            if matched_old[oi] || matched_new[ni] {
                continue;
            }
            apply_key_match(
                sess,
                &mut SiblingMatchFrame {
                    old_children: &old_children,
                    new_children: &new_children,
                    old_node_id,
                    new_node_id,
                    matched_old: &mut matched_old,
                    matched_new: &mut matched_new,
                    scope_occurrence,
                },
                oi,
                ni,
                AlignMethod::Lcs,
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
        sess.alignment.push(AlignEdge {
            old_id: Some(old_children[oi]),
            new_id: Some(new_children[ni]),
            kind: AlignKind::Match,
            method: Some(AlignMethod::Fingerprint),
            parent_old: Some(old_node_id),
            parent_new: Some(new_node_id),
        });
        if oi != ni && duplicate_sibling_occurrence(&old_children, oi).is_none() {
            sess.out.push(Mutation::MoveSubtree {
                node_id: old_children[oi],
                new_parent: old_node_id,
                before: insert_anchor_new(&new_children, ni),
            });
        }
        diff_subtree(
            sess,
            old_children[oi],
            new_children[ni],
            Some(old_node_id),
            duplicate_sibling_occurrence(&old_children, oi).or(scope_occurrence),
        );
        diff_child_trivia(
            sess.old,
            sess.new,
            old_node_id,
            new_node_id,
            old_children[oi],
            new_children[ni],
            sess.out,
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
        sess.alignment.push(AlignEdge {
            old_id: Some(*old_child),
            new_id: Some(new_child),
            kind: AlignKind::Match,
            method: Some(AlignMethod::StructuralFallback),
            parent_old: Some(old_node_id),
            parent_new: Some(new_node_id),
        });
        if oi != ni && duplicate_sibling_occurrence(&old_children, oi).is_none() {
            sess.out.push(Mutation::MoveNode {
                node_id: *old_child,
                new_parent: old_node_id,
                before: insert_anchor_new(&new_children, ni),
            });
            trace::notice(format!(
                "diff: structural fallback paired siblings at old[{oi}] new[{ni}] (distance {})",
                oi.abs_diff(ni)
            ));
        }
        diff_subtree(
            sess,
            *old_child,
            new_child,
            Some(old_node_id),
            duplicate_sibling_occurrence(&old_children, oi).or(scope_occurrence),
        );
        diff_child_trivia(
            sess.old,
            sess.new,
            old_node_id,
            new_node_id,
            *old_child,
            new_child,
            sess.out,
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
        sess.alignment.push(AlignEdge {
            old_id: Some(*old_child),
            new_id: Some(new_child),
            kind: AlignKind::Match,
            method: Some(AlignMethod::LeafFallback),
            parent_old: Some(old_node_id),
            parent_new: Some(new_node_id),
        });
        if oi != ni && duplicate_sibling_occurrence(&old_children, oi).is_none() {
            sess.out.push(Mutation::MoveNode {
                node_id: *old_child,
                new_parent: old_node_id,
                before: insert_anchor_new(&new_children, ni),
            });
            trace::notice(format!(
                "diff: leaf fallback paired siblings at old[{oi}] new[{ni}] (distance {})",
                oi.abs_diff(ni)
            ));
        }
        let child_occurrence = duplicate_sibling_occurrence(&old_children, oi).or(scope_occurrence);
        if oc.kind == NodeKind::Identifier {
            sess.out.push(Mutation::RenameIdentifier {
                node_id: *old_child,
                new_name: nc.payload.clone(),
                parent: Some(old_node_id),
                occurrence: child_occurrence,
            });
        } else {
            sess.out.push(Mutation::EditPayload {
                node_id: *old_child,
                new_payload: nc.payload.clone(),
                parent: Some(old_node_id),
                occurrence: child_occurrence,
            });
        }
        diff_child_trivia(
            sess.old,
            sess.new,
            old_node_id,
            new_node_id,
            *old_child,
            new_child,
            sess.out,
        );
    }

    for (oi, old_child) in old_children.iter().enumerate() {
        if !matched_old[oi] {
            sess.alignment.push(AlignEdge {
                old_id: Some(*old_child),
                new_id: None,
                kind: AlignKind::Delete,
                method: None,
                parent_old: Some(old_node_id),
                parent_new: None,
            });
            sess.out.push(Mutation::DeleteSubtree {
                parent: old_node_id,
                node_id: *old_child,
            });
        }
    }

    let mut pending_inserts = Vec::new();
    for (ni, new_child) in new_children.iter().enumerate() {
        if !matched_new[ni] {
            sess.alignment.push(AlignEdge {
                old_id: None,
                new_id: Some(*new_child),
                kind: AlignKind::Insert,
                method: None,
                parent_old: None,
                parent_new: Some(new_node_id),
            });
            let descendants = new.collect_subtree(*new_child);
            let top = new.get(new_child).unwrap().clone();
            if is_redundant_list_separator_insert(
                &old_children,
                &new_children,
                &matched_new,
                ni,
                &top,
            ) {
                continue;
            }
            pending_inserts.push((ni, top, descendants));
        }
    }
    pending_inserts.sort_by_key(|(ni, top, _)| (is_list_separator_token(top), *ni));
    for (ni, top, descendants) in pending_inserts {
        let (before, before_occurrence) = resolve_insert_before_in_old(
            old,
            new,
            &old_children,
            &new_children,
            &matched_new,
            ni,
            &top,
        );
        sess.out.push(Mutation::InsertSubtree {
            parent: old_node_id,
            before,
            before_occurrence,
            node: top,
            descendants,
            trivia: insert_trivia(new, new_node_id, new_children[ni], old_node_id),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::parse_rust;
    use crate::graph::{Mutation, Node, NodeKind};
    use crate::unparser::unparse;
    use std::collections::HashMap;

    fn graph_from_one_block(literals: &[&str]) -> AstGraph {
        let mut nodes = HashMap::new();
        let leaves: Vec<_> = literals
            .iter()
            .map(|s| {
                let n = Node::leaf(NodeKind::Literal, (*s).to_string());
                nodes.insert(n.id, n.clone());
                n.id
            })
            .collect();
        let root = Node::new(NodeKind::Block, String::new(), leaves);
        nodes.insert(root.id, root.clone());
        AstGraph::new(root, nodes)
    }

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

    fn wide_function_module(count: usize) -> String {
        let mut src = String::new();
        for i in 0..count {
            src.push_str(&format!("fn f{i}() {{ let _ = {i}; }}\n"));
        }
        src
    }

    #[test]
    fn wide_sibling_list_diff_is_fast() {
        let count = 600;
        let old_src = wide_function_module(count);
        let mut new_src = old_src.clone();
        new_src = new_src.replacen(
            "fn f300() { let _ = 300; }",
            "fn f300() { let _ = 300; // edited\n}",
            1,
        );
        let old = parse_rust(&old_src).unwrap();
        let new = parse_rust(&new_src).unwrap();
        let start = std::time::Instant::now();
        let diff = diff_graphs(&old, &new);
        assert!(
            start.elapsed().as_millis() < 500,
            "diff took {:?}",
            start.elapsed()
        );
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        working.validate().unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn wide_sibling_list_unchanged_is_no_mutations() {
        let src = wide_function_module(600);
        let old = parse_rust(&src).unwrap();
        let new = parse_rust(&src).unwrap();
        let start = std::time::Instant::now();
        let diff = diff_graphs(&old, &new);
        assert!(
            start.elapsed().as_millis() < 500,
            "diff took {:?}",
            start.elapsed()
        );
        assert!(diff.mutations.is_empty());
    }

    // --- alignment tests ---

    #[test]
    fn alignment_identical_has_root_match_no_insert_delete() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let old = parse_rust(src).unwrap();
        let new = parse_rust(src).unwrap();
        let result = diff_graphs_detailed(&old, &new);
        assert!(
            result.alignment.iter().any(|e| e.kind == AlignKind::Match
                && e.old_id == Some(old.root)
                && e.new_id == Some(new.root)),
            "expected root match edge"
        );
        assert!(
            !result.alignment.iter().any(|e| e.kind == AlignKind::Insert),
            "unexpected insert edges: {:?}",
            result.alignment
        );
        assert!(
            !result.alignment.iter().any(|e| e.kind == AlignKind::Delete),
            "unexpected delete edges: {:?}",
            result.alignment
        );
    }

    #[test]
    fn alignment_insert_statement_has_insert_edge() {
        let old = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();
        let result = diff_graphs_detailed(&old, &new);
        assert!(
            result.alignment.iter().any(|e| e.kind == AlignKind::Insert),
            "expected at least one Insert edge: {:?}",
            result.alignment
        );
    }

    #[test]
    fn alignment_swapped_blocks_has_structural_fallback() {
        // Old: Module[Block(a,b)[2 children], Block(c,d,e)[3 children]]
        // New: Module[Block(c,d,e)[3 children], Block(a)[1 child]]
        // Block(c,d,e) id-matches; Block(a,b) vs Block(a) have different child counts so
        // role-LCS cannot pair them -- structural fallback picks them up.
        let old = graph_from_two_blocks(&["a", "b"], &["c", "d", "e"]);
        let new = graph_from_two_blocks(&["c", "d", "e"], &["a"]);
        let result = diff_graphs_detailed(&old, &new);
        assert!(
            result
                .alignment
                .iter()
                .any(|e| e.kind == AlignKind::Match
                    && e.method == Some(AlignMethod::StructuralFallback)),
            "expected StructuralFallback match edge: {:?}",
            result.alignment
        );
    }

    #[test]
    fn alignment_swapped_leaf_literals_has_leaf_fallback() {
        // Block([Lit("b"), Lit("a")]) vs Block([Lit("a"), Lit("c")])
        // Lit("a") id-matches at (1,0); role-LCS pairs are all skipped because slots are
        // taken; Lit("b") has no role-LCS candidate left, so leaf fallback pairs it with
        // Lit("c") (same kind, different payload, closest by position).
        let old = graph_from_one_block(&["b", "a"]);
        let new = graph_from_one_block(&["a", "c"]);
        let result = diff_graphs_detailed(&old, &new);
        assert!(
            result
                .alignment
                .iter()
                .any(|e| e.kind == AlignKind::Match && e.method == Some(AlignMethod::LeafFallback)),
            "expected LeafFallback match edge: {:?}",
            result.alignment
        );
    }

    #[test]
    fn identical_reparse_with_duplicate_sibling_node_ids_is_empty() {
        let src = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a;\n    z\n}\n";
        let a = parse_rust(src).unwrap();
        let b = parse_rust(src).unwrap();
        assert!(diff_graphs(&a, &b).mutations.is_empty());
    }

    #[test]
    fn disjoint_body_edits_do_not_emit_phantom_comma_inserts() {
        let base_s = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a;\n    z\n}\n";
        let left_s = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c + 1;\n    let z = y - a;\n    z\n}\n";
        let right_s = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a - 1;\n    z\n}\n";
        let base = parse_rust(base_s).unwrap();
        let left = parse_rust(left_s).unwrap();
        let right = parse_rust(right_s).unwrap();
        let token_inserts = |diff: &DiffResult| {
            diff.mutations.iter().any(|m| {
                matches!(
                    m,
                    Mutation::InsertSubtree {
                        node,
                        ..
                    } if node.kind == NodeKind::Token && node.payload == ","
                )
            })
        };
        assert!(!token_inserts(&diff_graphs(&base, &left)));
        assert!(!token_inserts(&diff_graphs(&base, &right)));
    }

    #[test]
    fn identical_reparse_json_array_with_commas_is_empty() {
        use crate::frontend::parse_source;

        let src = "{\"items\": [1, 2, 3]}\n";
        let a = parse_source("data.json", src).unwrap();
        let b = parse_source("data.json", src).unwrap();
        assert!(diff_graphs(&a, &b).mutations.is_empty());
    }

    #[test]
    fn four_parameter_identical_reparse_is_empty() {
        let src = "fn many(a: i32, b: i32, c: i32, d: i32) -> i32 {\n    a + b + c + d\n}\n";
        let a = parse_rust(src).unwrap();
        let b = parse_rust(src).unwrap();
        assert!(diff_graphs(&a, &b).mutations.is_empty());
    }

    #[test]
    fn struct_field_append_diff_applies_roundtrip() {
        let old = parse_rust("pub struct Config {\n    pub host: String,\n    pub port: u16,\n}\n")
            .unwrap();
        let new = parse_rust(
            "pub struct Config {\n    pub host: String,\n    pub port: u16,\n    pub timeout: u64,\n}\n",
        )
        .unwrap();
        let diff = diff_graphs(&old, &new);
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn parameter_count_change_diff_applies_roundtrip() {
        let old = parse_rust("fn pair(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        let new = parse_rust("fn pair(a: i32, b: i32, c: i32) -> i32 {\n    a + b\n}\n").unwrap();
        let diff = diff_graphs(&old, &new);
        let mut working = old.clone();
        working.apply_batch(&diff.mutations).unwrap();
        assert_eq!(unparse(&working), unparse(&new));
    }

    #[test]
    fn calc_left_diff_applies_parseable() {
        let base_s = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c;\n    let z = y - a;\n    z\n}\n";
        let left_s = "pub fn process(a: i32, b: i32, c: i32) -> i32 {\n    let x = a + b;\n    let y = x * c + 1;\n    let z = y - a;\n    z\n}\n";
        let base = parse_rust(base_s).unwrap();
        let left = parse_rust(left_s).unwrap();
        let mut working = base.clone();
        working
            .apply_batch(&diff_graphs(&base, &left).mutations)
            .unwrap();
        let text = unparse(&working);
        parse_rust(&text).expect("applied diff must parse");
        assert!(text.contains("+ 1") || text.contains("+1"));
    }

    #[test]
    fn diff_graphs_and_detailed_produce_identical_mutations_for_rename() {
        let old = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let new = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let simple = diff_graphs(&old, &new);
        let detailed = diff_graphs_detailed(&old, &new);
        assert_eq!(
            simple.mutations, detailed.mutations,
            "mutations must be identical"
        );
    }
}
