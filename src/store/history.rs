use super::{StateId, TimelineEntry};
use std::collections::{HashMap, HashSet, VecDeque};

/// Walk timeline entries from `start` toward ancestors, newest first.
pub fn walk_history<F>(
    start: &StateId,
    limit: usize,
    mut load: F,
) -> Result<Vec<TimelineEntry>, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
{
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    queue.push_back(start.clone());

    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let entry = load(&id)?;
        out.push(entry.clone());
        if out.len() >= limit {
            break;
        }
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            if !seen.contains(parent) {
                queue.push_back(parent.clone());
            }
        }
    }
    Ok(out)
}

/// Collect all ancestors of a state (excluding itself).
pub fn ancestors<F>(start: &StateId, mut load: F) -> Result<HashSet<StateId>, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
{
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    if let Ok(entry) = load(start) {
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            queue.push_back(parent.clone());
        }
    }
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let entry = load(&id)?;
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            if !seen.contains(parent) {
                queue.push_back(parent.clone());
            }
        }
    }
    Ok(seen)
}

/// Find the lowest common ancestor of two states.
pub fn merge_base<F>(a: &StateId, b: &StateId, mut load: F) -> Result<StateId, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
{
    if a == b {
        return Ok(a.clone());
    }

    let anc_a = ancestors_with_depth(a, &mut load)?;
    let anc_b = ancestors_with_depth(b, &mut load)?;

    let mut best: Option<(usize, StateId)> = None;
    for (id, depth_a) in &anc_a {
        if let Some(depth_b) = anc_b.get(id) {
            let total = depth_a + depth_b;
            if best.as_ref().is_none_or(|(d, _)| total < *d) {
                best = Some((total, id.clone()));
            }
        }
    }

    if let Some((_, id)) = best {
        return Ok(id);
    }

    if anc_a.contains_key(a) || a == &"0".repeat(64) {
        return Ok(a.clone());
    }
    if anc_b.contains_key(b) || b == &"0".repeat(64) {
        return Ok(b.clone());
    }

    Err(format!("no common ancestor for {a} and {b}"))
}

fn ancestors_with_depth<F>(start: &StateId, load: &mut F) -> Result<HashMap<StateId, usize>, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
{
    let mut depths = HashMap::new();
    let mut queue = VecDeque::new();
    depths.insert(start.clone(), 0);
    queue.push_back((start.clone(), 0usize));

    while let Some((id, depth)) = queue.pop_front() {
        let entry = load(&id)?;
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            if depths.insert(parent.clone(), depth + 1).is_none() {
                queue.push_back((parent.clone(), depth + 1));
            }
        }
    }
    Ok(depths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture() -> HashMap<StateId, TimelineEntry> {
        let empty = "0".repeat(64);
        let s1 = "1".repeat(64);
        let s2 = "2".repeat(64);
        let s3 = "3".repeat(64);
        let s4 = "4".repeat(64);
        let mut db = HashMap::new();
        db.insert(
            empty.clone(),
            TimelineEntry {
                id: empty.clone(),
                parent: None,
                parents: vec![],
                message: "root".into(),
                timestamp: "0".into(),
                manifest: HashMap::new(),
                files: None,
            },
        );
        db.insert(
            s1.clone(),
            TimelineEntry {
                id: s1.clone(),
                parent: Some(empty.clone()),
                parents: vec![empty.clone()],
                message: "s1".into(),
                timestamp: "1".into(),
                manifest: HashMap::new(),
                files: None,
            },
        );
        db.insert(
            s2.clone(),
            TimelineEntry {
                id: s2.clone(),
                parent: Some(s1.clone()),
                parents: vec![s1.clone()],
                message: "s2".into(),
                timestamp: "2".into(),
                manifest: HashMap::new(),
                files: None,
            },
        );
        db.insert(
            s3.clone(),
            TimelineEntry {
                id: s3.clone(),
                parent: Some(s1.clone()),
                parents: vec![s1.clone()],
                message: "s3".into(),
                timestamp: "3".into(),
                manifest: HashMap::new(),
                files: None,
            },
        );
        db.insert(
            s4.clone(),
            TimelineEntry {
                id: s4.clone(),
                parent: None,
                parents: vec![s2.clone(), s3.clone()],
                message: "merge".into(),
                timestamp: "4".into(),
                manifest: HashMap::new(),
                files: None,
            },
        );
        db
    }

    fn loader(
        db: &HashMap<StateId, TimelineEntry>,
    ) -> impl FnMut(&StateId) -> Result<TimelineEntry, String> + '_ {
        move |id| db.get(id).cloned().ok_or_else(|| format!("missing {id}"))
    }

    #[test]
    fn walk_history_respects_limit() {
        let db = fixture();
        let s4 = "4".repeat(64);
        let entries = walk_history(&s4, 2, loader(&db)).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "merge");
    }

    #[test]
    fn merge_base_finds_fork_point() {
        let db = fixture();
        let s2 = "2".repeat(64);
        let s3 = "3".repeat(64);
        let s1 = "1".repeat(64);
        let base = merge_base(&s2, &s3, loader(&db)).unwrap();
        assert_eq!(base, s1);
    }

    #[test]
    fn merge_base_same_state() {
        let db = fixture();
        let s2 = "2".repeat(64);
        let base = merge_base(&s2, &s2, loader(&db)).unwrap();
        assert_eq!(base, s2);
    }
}
