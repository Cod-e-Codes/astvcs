use super::{Repo, RepoError, RepoResult, StateId};
use crate::store::atomic::write_atomic_json;
use crate::store::repo::read_json_unlocked;
use std::collections::HashSet;
use std::path::Path;

pub const SHALLOW_FILE: &str = "shallow.json";

pub const SHALLOW_HISTORY_MSG: &str = "shallow history: cannot compute merge-base across incomplete history; \
     use fetch without --depth or increase --depth";

pub fn load_shallow_boundaries(astvcs_dir: &Path) -> RepoResult<HashSet<StateId>> {
    let path = astvcs_dir.join(SHALLOW_FILE);
    if !path.is_file() {
        return Ok(HashSet::new());
    }
    let ids: Vec<StateId> = read_json_unlocked(&path)?;
    Ok(ids.into_iter().collect())
}

pub fn save_shallow_boundaries(astvcs_dir: &Path, boundaries: &HashSet<StateId>) -> RepoResult<()> {
    let mut ids: Vec<&StateId> = boundaries.iter().collect();
    ids.sort();
    let owned: Vec<StateId> = ids.into_iter().cloned().collect();
    write_atomic_json(&astvcs_dir.join(SHALLOW_FILE), &owned).map_err(RepoError::from_message)
}

impl Repo {
    pub fn load_shallow_boundaries(&self) -> RepoResult<HashSet<StateId>> {
        let _lock = self.repo_lock()?;
        load_shallow_boundaries(&self.astvcs_dir())
    }

    pub fn is_shallow_boundary(&self, state_id: &StateId) -> bool {
        load_shallow_boundaries(&self.astvcs_dir())
            .map(|b| b.contains(state_id))
            .unwrap_or(false)
    }

    pub fn update_shallow_boundaries(
        &self,
        new_boundary: Option<&StateId>,
        fetched_states: &[StateId],
        depth: Option<usize>,
    ) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let mut boundaries = load_shallow_boundaries(&self.astvcs_dir())?;
        if depth.is_none() {
            boundaries.clear();
        } else {
            boundaries.retain(|boundary_id| {
                if let Ok(entry) = self.load_timeline_entry_unlocked(boundary_id) {
                    for parent in entry.parents.iter().chain(entry.parent.iter()) {
                        if fetched_states.contains(parent) {
                            return false;
                        }
                    }
                }
                true
            });
        }
        if let Some(id) = new_boundary {
            boundaries.insert(id.clone());
        }
        save_shallow_boundaries(&self.astvcs_dir(), &boundaries)
    }
}

pub fn shallow_history_error(context: &str) -> String {
    format!("{SHALLOW_HISTORY_MSG} ({context})")
}
