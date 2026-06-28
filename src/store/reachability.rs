use super::{StateId, TimelineEntry};
use std::collections::{HashMap, HashSet, VecDeque};

pub const ROOT_STATE_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// States and blobs reachable from branch tips, remote-tracking tips, and
/// detached HEAD.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reachability {
    pub states: HashSet<StateId>,
    pub blobs: HashSet<StateId>,
}

/// Walk parent links from `tips` and collect every reachable state and blob.
///
/// The root empty state is always included. Read-only; callers must hold the
/// repository lock.
pub fn reachable_from_tips<F, G>(
    tips: impl IntoIterator<Item = StateId>,
    mut load_timeline: F,
    mut load_manifest: G,
) -> Result<Reachability, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
    G: FnMut(&StateId) -> Result<HashMap<String, String>, String>,
{
    let mut out = Reachability::default();
    out.states.insert(ROOT_STATE_ID.to_string());

    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    for tip in tips {
        if seen.insert(tip.clone()) {
            queue.push_back(tip);
        }
    }

    while let Some(id) = queue.pop_front() {
        if !out.states.insert(id.clone()) {
            continue;
        }
        let manifest = load_manifest(&id)?;
        out.blobs.extend(manifest.into_values());

        let entry = load_timeline(&id)?;
        for parent in entry.parents.iter().chain(entry.parent.iter()) {
            if seen.insert(parent.clone()) {
                queue.push_back(parent.clone());
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fixture() -> HashMap<StateId, (TimelineEntry, HashMap<String, String>)> {
        let empty = ROOT_STATE_ID.to_string();
        let s1 = "1".repeat(64);
        let s2 = "2".repeat(64);
        let mut db = HashMap::new();
        db.insert(
            empty.clone(),
            (
                TimelineEntry {
                    id: empty.clone(),
                    parent: None,
                    parents: vec![],
                    message: "root".into(),
                    timestamp: "0".into(),
                    manifest: HashMap::new(),
                    files: None,
                },
                HashMap::new(),
            ),
        );
        let mut m1 = HashMap::new();
        m1.insert("a.txt".into(), "blob-a".into());
        db.insert(
            s1.clone(),
            (
                TimelineEntry {
                    id: s1.clone(),
                    parent: Some(empty.clone()),
                    parents: vec![empty.clone()],
                    message: "s1".into(),
                    timestamp: "1".into(),
                    manifest: m1.clone(),
                    files: None,
                },
                m1,
            ),
        );
        let mut m2 = HashMap::new();
        m2.insert("b.txt".into(), "blob-b".into());
        db.insert(
            s2.clone(),
            (
                TimelineEntry {
                    id: s2.clone(),
                    parent: Some(s1.clone()),
                    parents: vec![s1.clone()],
                    message: "s2".into(),
                    timestamp: "2".into(),
                    manifest: m2.clone(),
                    files: None,
                },
                m2,
            ),
        );
        db
    }

    #[test]
    fn reachable_from_tip_includes_ancestors_and_blobs() {
        let db = fixture();
        let s2 = "2".repeat(64);
        let s1 = "1".repeat(64);
        let reach = reachable_from_tips(
            [s2.clone()],
            |id| {
                db.get(id)
                    .map(|(e, _)| e.clone())
                    .ok_or_else(|| format!("missing {id}"))
            },
            |id| {
                db.get(id)
                    .map(|(_, m)| m.clone())
                    .ok_or_else(|| format!("missing {id}"))
            },
        )
        .unwrap();
        assert!(reach.states.contains(ROOT_STATE_ID));
        assert!(reach.states.contains(&s1));
        assert!(reach.states.contains(&s2));
        assert!(reach.blobs.contains("blob-a"));
        assert!(reach.blobs.contains("blob-b"));
    }

    #[test]
    fn unreachable_branch_tip_not_included_without_tip() {
        let db = fixture();
        let s2 = "2".repeat(64);
        let s1 = "1".repeat(64);
        let reach = reachable_from_tips(
            [s1.clone()],
            |id| {
                db.get(id)
                    .map(|(e, _)| e.clone())
                    .ok_or_else(|| format!("missing {id}"))
            },
            |id| {
                db.get(id)
                    .map(|(_, m)| m.clone())
                    .ok_or_else(|| format!("missing {id}"))
            },
        )
        .unwrap();
        assert!(!reach.states.contains(&s2));
        assert!(!reach.blobs.contains("blob-b"));
    }
}
