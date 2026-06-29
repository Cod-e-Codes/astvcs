use crate::store::atomic::write_atomic_json;
use crate::store::blame::short_state_id;
use crate::store::error::{RepoError, RepoResult};
use crate::store::lock::{lock_held, resume_repo_lock, suspend_repo_lock};
use crate::store::repo::{
    HeadTarget, LinearParentError, MaterializeOptions, TimelineEntry, linear_timeline_parent,
};
use crate::store::{Repo, StateId};
use crate::trace;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

pub const BISECT_STATE_FILE: &str = "bisect-state.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BisectState {
    pub original_head: StateId,
    pub original_branch: Option<String>,
    pub good: StateId,
    pub bad: StateId,
    pub skipped: Vec<StateId>,
    pub candidates: Vec<StateId>,
    pub low: usize,
    pub high: usize,
}

pub fn load_bisect_state(astvcs: &Path) -> Result<Option<BisectState>, String> {
    let path = astvcs.join(BISECT_STATE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub fn save_bisect_state(astvcs: &Path, state: &BisectState) -> Result<(), String> {
    write_atomic_json(&astvcs.join(BISECT_STATE_FILE), state)
}

pub fn delete_bisect_state(astvcs: &Path) -> Result<(), String> {
    let path = astvcs.join(BISECT_STATE_FILE);
    if path.is_file() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Commits strictly after `good` through `bad` on the linear first-parent chain, oldest first.
pub fn collect_bisect_candidates<F>(
    bad: &StateId,
    good: &StateId,
    mut load: F,
) -> Result<Vec<StateId>, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
{
    if bad == good {
        return Ok(Vec::new());
    }
    let mut chain = Vec::new();
    let mut current = bad.clone();
    loop {
        chain.push(current.clone());
        if current == *good {
            return Err(format!(
                "good state {good} is not an ancestor of bad state {bad} on the linear first-parent chain"
            ));
        }
        let entry = load(&current)?;
        let parent = linear_timeline_parent(&entry).map_err(|e| match e {
            LinearParentError::MergeCommit(id) => format!(
                "bisect requires linear history; merge commit {id} is in the path between good and bad"
            ),
            LinearParentError::NoParent(id) => format!(
                "good state {good} is not an ancestor of bad state {bad} (reached root {id})"
            ),
        })?;
        if parent == *good {
            break;
        }
        current = parent;
    }
    chain.reverse();
    Ok(chain)
}

fn initial_search_range(candidates: &[StateId]) -> (usize, usize) {
    if candidates.is_empty() {
        (0, 0)
    } else {
        (0, candidates.len() - 1)
    }
}

fn revisions_left(state: &BisectState) -> usize {
    if state.candidates.is_empty() || state.low > state.high {
        0
    } else {
        state.high - state.low + 1
    }
}

fn rough_steps_remaining(low: usize, high: usize) -> usize {
    if low >= high {
        1
    } else {
        (high - low).ilog2() as usize + 1
    }
}

impl Repo {
    pub fn bisect_start(&self, bad_ref: Option<&str>, good_ref: &str) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.bisect_start_unlocked(bad_ref, good_ref)
    }

    pub fn bisect_mark_bad(&self, reference: Option<&str>) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.bisect_mark_unlocked(reference, true)
    }

    pub fn bisect_mark_good(&self, reference: Option<&str>) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.bisect_mark_unlocked(reference, false)
    }

    pub fn bisect_run(&self, script: &str, args: &[&str]) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.bisect_run_unlocked(script, args)
    }

    pub fn bisect_reset(&self) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.bisect_reset_unlocked()
    }

    fn bisect_start_unlocked(&self, bad_ref: Option<&str>, good_ref: &str) -> RepoResult<()> {
        let astvcs = self.astvcs_dir();
        if load_bisect_state(&astvcs)
            .map_err(RepoError::from_message)?
            .is_some()
        {
            return Err(RepoError::invalid_input("bisect already in progress"));
        }

        let original_head = self.head_state_unlocked()?;
        let original_branch = self.head_branch_unlocked()?;

        let bad = match bad_ref {
            Some(r) => self.resolve_state_ref_unlocked(r)?,
            None => original_head.clone(),
        };
        let good = self.resolve_state_ref_unlocked(good_ref)?;

        let candidates = collect_bisect_candidates(&bad, &good, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })
        .map_err(RepoError::from_message)?;

        if candidates.is_empty() {
            return Err(RepoError::invalid_input(
                "nothing to bisect; good and bad are the same or adjacent with no commits between them",
            ));
        }

        let (low, high) = initial_search_range(&candidates);
        let state = BisectState {
            original_head,
            original_branch,
            good,
            bad,
            skipped: Vec::new(),
            candidates,
            low,
            high,
        };
        save_bisect_state(&astvcs, &state).map_err(RepoError::from_message)?;
        println!(
            "Bisecting: {} revisions left to test after this (roughly {} steps)",
            revisions_left(&state),
            (state.candidates.len() as f64).log2().ceil() as usize
        );
        trace::notice(format!(
            "bisect: started good={} bad={} ({} candidates)",
            state.good,
            state.bad,
            state.candidates.len()
        ));
        Ok(())
    }

    fn bisect_mark_unlocked(&self, reference: Option<&str>, mark_bad: bool) -> RepoResult<()> {
        let astvcs = self.astvcs_dir();
        let mut state = load_bisect_state(&astvcs)
            .map_err(RepoError::from_message)?
            .ok_or_else(|| RepoError::invalid_input("no bisect in progress"))?;

        let id = match reference {
            Some(r) => self.resolve_state_ref_unlocked(r)?,
            None => self.head_state_unlocked()?,
        };

        if mark_bad {
            state.bad = id.clone();
        } else {
            state.good = id.clone();
        }

        state.candidates = collect_bisect_candidates(&state.bad, &state.good, |sid| {
            self.load_timeline_entry_unlocked(sid)
                .map_err(|e| e.to_string())
        })
        .map_err(RepoError::from_message)?;

        if state.candidates.is_empty() {
            return Err(RepoError::invalid_input(
                "nothing to bisect after updating boundaries; good and bad may be adjacent",
            ));
        }

        state.skipped.retain(|s| state.candidates.contains(s));
        let (low, high) = initial_search_range(&state.candidates);
        state.low = low;
        state.high = high;

        save_bisect_state(&astvcs, &state).map_err(RepoError::from_message)?;
        println!(
            "Bisecting: {} revisions left to test",
            revisions_left(&state)
        );
        trace::notice(format!(
            "bisect: marked {} {}",
            if mark_bad { "bad" } else { "good" },
            id
        ));
        Ok(())
    }

    fn bisect_run_unlocked(&self, script: &str, args: &[&str]) -> RepoResult<()> {
        let astvcs = self.astvcs_dir();
        if !lock_held() {
            return Err(RepoError::other(
                "internal error: bisect run requires held repository lock",
            ));
        }

        loop {
            let mut state = load_bisect_state(&astvcs)
                .map_err(RepoError::from_message)?
                .ok_or_else(|| RepoError::invalid_input("no bisect in progress"))?;

            if state.candidates.is_empty() || state.low > state.high {
                return Err(RepoError::invalid_input("no untested revisions remain"));
            }

            let mid = (state.low + state.high) / 2;
            let candidate = state.candidates[mid].clone();
            println!(
                "Bisecting: {} revisions left to test after this (roughly {} steps)",
                revisions_left(&state),
                rough_steps_remaining(state.low, state.high)
            );

            self.checkout_bisect_state_unlocked(&candidate)?;

            suspend_repo_lock()?;
            let script_result = run_bisect_script(self.root_path(), &candidate, script, args);
            let guard = resume_repo_lock(&astvcs)?;
            std::mem::forget(guard);
            let exit_code = script_result?;

            state = load_bisect_state(&astvcs)
                .map_err(RepoError::from_message)?
                .ok_or_else(|| RepoError::invalid_input("no bisect in progress"))?;

            match exit_code {
                0 => {
                    state.low = mid + 1;
                    if state.low > state.high {
                        let entry = self.load_timeline_entry_unlocked(&state.bad)?;
                        println!(
                            "first bad state: {} ({})",
                            short_state_id(&state.bad),
                            entry.message
                        );
                        return Ok(());
                    }
                }
                1 => {
                    state.high = mid;
                    if state.low == state.high {
                        let entry =
                            self.load_timeline_entry_unlocked(&state.candidates[state.low])?;
                        println!(
                            "first bad state: {} ({})",
                            short_state_id(&state.candidates[state.low]),
                            entry.message
                        );
                        return Ok(());
                    }
                }
                125 => {
                    state.skipped.push(candidate.clone());
                    state.candidates.remove(mid);
                    if state.candidates.is_empty() {
                        return Err(RepoError::invalid_input(
                            "no untested revisions remain after skips",
                        ));
                    }
                    state.skipped.retain(|s| state.candidates.contains(s));
                    if state.low >= state.candidates.len() {
                        state.low = state.candidates.len().saturating_sub(1);
                    }
                    if state.high >= state.candidates.len() {
                        state.high = state.candidates.len().saturating_sub(1);
                    }
                    if state.low > state.high {
                        state.low = 0;
                        state.high = state.candidates.len().saturating_sub(1);
                    }
                }
                code => {
                    return Err(RepoError::invalid_input(format!(
                        "bisect script exited with {code}; expected 0 (good), 1 (bad), or 125 (skip)"
                    )));
                }
            }

            save_bisect_state(&astvcs, &state).map_err(RepoError::from_message)?;
        }
    }

    fn bisect_reset_unlocked(&self) -> RepoResult<()> {
        let astvcs = self.astvcs_dir();
        let state = load_bisect_state(&astvcs)
            .map_err(RepoError::from_message)?
            .ok_or_else(|| RepoError::invalid_input("no bisect in progress"))?;

        delete_bisect_state(&astvcs).map_err(RepoError::from_message)?;

        let materialize_opts = MaterializeOptions::new("bisect reset").force(true);
        match state.original_branch {
            Some(ref branch) => {
                self.write_head_target(&HeadTarget::Branch(branch.clone()))?;
                let tip = self.read_branch_ref(branch)?;
                self.materialize_state_inner(&tip, Vec::new(), &materialize_opts)?;
                trace::notice(format!("bisect: reset; restored branch {branch}"));
            }
            None => {
                self.write_head_target(&HeadTarget::Detached(state.original_head.clone()))?;
                self.materialize_state_inner(&state.original_head, Vec::new(), &materialize_opts)?;
                trace::notice(format!(
                    "bisect: reset; restored detached HEAD {}",
                    state.original_head
                ));
            }
        }
        Ok(())
    }

    fn checkout_bisect_state_unlocked(&self, state_id: &StateId) -> RepoResult<()> {
        self.load_timeline_entry_unlocked(state_id)?;
        let materialize_opts = MaterializeOptions::new("bisect").force(true);
        let clobbered = self.materialize_guard(&materialize_opts)?;
        self.materialize_state_inner(state_id, clobbered, &materialize_opts)?;
        self.write_head_target(&HeadTarget::Detached(state_id.to_string()))?;
        trace::notice(format!("bisect: checked out {state_id}"));
        Ok(())
    }
}

fn run_bisect_script(
    repo_root: &Path,
    state_id: &StateId,
    script: &str,
    args: &[&str],
) -> RepoResult<i32> {
    let mut cmd = Command::new(script);
    cmd.args(args)
        .current_dir(repo_root)
        .env("ASTVCS_BISECT_STATE", state_id);
    let output = cmd
        .status()
        .map_err(|e| RepoError::other(format!("failed to run bisect script {script}: {e}")))?;
    Ok(output.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(id: &str, parent: Option<&str>) -> TimelineEntry {
        TimelineEntry {
            id: id.to_string(),
            parent: parent.map(|p| p.to_string()),
            parents: parent.map(|p| vec![p.to_string()]).unwrap_or_default(),
            message: id.into(),
            timestamp: "0".into(),
            author_name: String::new(),
            author_email: String::new(),
            manifest: HashMap::new(),
            files: None,
        }
    }

    #[test]
    fn collect_bisect_candidates_orders_oldest_first() {
        let c1 = "a".repeat(64);
        let c2 = "b".repeat(64);
        let c3 = "c".repeat(64);
        let c4 = "d".repeat(64);
        let entries = HashMap::from([
            (c1.clone(), entry(&c1, None)),
            (c2.clone(), entry(&c2, Some(&c1))),
            (c3.clone(), entry(&c3, Some(&c2))),
            (c4.clone(), entry(&c4, Some(&c3))),
        ]);
        let candidates = collect_bisect_candidates(&c4, &c1, |id| {
            entries.get(id).cloned().ok_or_else(|| id.clone())
        })
        .unwrap();
        assert_eq!(candidates, vec![c2, c3, c4]);
    }

    #[test]
    fn collect_bisect_candidates_rejects_non_ancestor() {
        let c1 = "a".repeat(64);
        let c2 = "b".repeat(64);
        let other = "x".repeat(64);
        let entries = HashMap::from([
            (c1.clone(), entry(&c1, None)),
            (c2.clone(), entry(&c2, Some(&c1))),
        ]);
        let err = collect_bisect_candidates(&c2, &other, |id| {
            entries.get(id).cloned().ok_or_else(|| id.clone())
        })
        .unwrap_err();
        assert!(err.contains("not an ancestor"), "{err}");
    }

    #[test]
    fn rough_steps_remaining_handles_single_candidate() {
        assert_eq!(rough_steps_remaining(2, 2), 1);
        assert_eq!(rough_steps_remaining(0, 7), 3);
    }

    #[test]
    fn collect_bisect_candidates_rejects_merge_commit() {
        let good = "a".repeat(64);
        let merge = "m".repeat(64);
        let bad = "b".repeat(64);
        let mut merge_entry = entry(&merge, None);
        merge_entry.parents = vec![good.clone(), "z".repeat(64)];
        let entries = HashMap::from([
            (good.clone(), entry(&good, None)),
            (merge.clone(), merge_entry),
            (bad.clone(), entry(&bad, Some(&merge))),
        ]);
        let err = collect_bisect_candidates(&bad, &good, |id| {
            entries.get(id).cloned().ok_or_else(|| id.clone())
        })
        .unwrap_err();
        assert!(err.contains("merge commit"), "{err}");
    }
}
