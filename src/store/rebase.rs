use crate::frontend::FileContent;
use crate::store::atomic::{self, write_atomic_json};
use crate::store::error::{RepoError, RepoErrorKind, RepoResult};
use crate::store::identity::AuthorIdentity;
use crate::store::manifest::FileMode;
use crate::store::merge_resolve::{MergeResolution, apply_merge_resolutions};
use crate::store::repo::{
    LinearParentError, MaterializeOptions, MergePlan, TimelineEntry, WORKING_TREE_DIRTY_ERR,
    linear_timeline_parent,
};
use crate::store::scan_cache;
use crate::store::staging::{StagingIndex, clear_staging_entries};
use crate::store::tracked::TrackedFile;
use crate::store::working::load_working_tracked;
use crate::store::{Repo, StateId};
use crate::trace;
use crate::unparser::unparse;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub const REBASE_STATE_FILE: &str = "rebase-state.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebaseState {
    pub branch: String,
    pub upstream: String,
    pub onto: StateId,
    pub original_tip: StateId,
    pub current_head: StateId,
    pub remaining: Vec<StateId>,
    pub conflicted: Option<StateId>,
}

pub fn load_rebase_state(astvcs: &Path) -> Result<Option<RebaseState>, String> {
    let path = astvcs.join(REBASE_STATE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub fn save_rebase_state(astvcs: &Path, state: &RebaseState) -> Result<(), String> {
    write_atomic_json(&astvcs.join(REBASE_STATE_FILE), state)
}

pub fn delete_rebase_state(astvcs: &Path) -> Result<(), String> {
    let path = astvcs.join(REBASE_STATE_FILE);
    if path.is_file() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn collect_linear_commits<F>(
    tip: &StateId,
    base: &StateId,
    mut load: F,
) -> Result<Vec<StateId>, String>
where
    F: FnMut(&StateId) -> Result<TimelineEntry, String>,
{
    if tip == base {
        return Ok(Vec::new());
    }
    let mut chain = Vec::new();
    let mut current = tip.clone();
    loop {
        if current == *base {
            break;
        }
        chain.push(current.clone());
        let entry = load(&current)?;
        let parent = linear_timeline_parent(&entry).map_err(|e| match e {
            LinearParentError::MergeCommit(id) => {
                format!("rebase requires linear history; merge commit {id}")
            }
            LinearParentError::NoParent(id) => format!("rebase: no parent for {id}"),
        })?;
        if parent == *base {
            break;
        }
        current = parent;
    }
    chain.reverse();
    Ok(chain)
}

fn remove_working_path(path: &Path) -> Result<(), String> {
    if path.is_symlink() {
        fs::remove_file(path).map_err(|e| e.to_string())
    } else if path.is_dir() {
        Err(format!(
            "refusing to remove directory at {}",
            path.display()
        ))
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

fn content_to_string(content: &FileContent) -> String {
    match content {
        FileContent::Ast(graph) => unparse(graph),
        FileContent::Text(blob) => blob.content.clone(),
        FileContent::Binary(_) => {
            panic!("content_to_string called on binary blob; use write_atomic with raw bytes")
        }
        FileContent::Symlink(blob) => blob.target.clone(),
    }
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: FileMode) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    match mode {
        FileMode::Executable => {
            perms.set_mode(perms.mode() | 0o111);
            fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
        }
        FileMode::Regular => {
            perms.set_mode(perms.mode() & !0o111);
            fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
        }
        FileMode::Symlink => {}
    }
    Ok(())
}

fn materialize_symlink(path: &Path, target: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_file;
        if let Err(e) = symlink_file(target, path) {
            trace::warn(format!(
                "rebase: could not create symlink at {} -> {target}: {e}; skipped",
                path.display()
            ));
        }
        Ok(())
    }
}

fn write_tracked_to_disk(root: &Path, path: &str, tracked: &TrackedFile) -> Result<(), String> {
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if full.is_symlink() || full.exists() {
        remove_working_path(&full)?;
    }
    if tracked.mode == FileMode::Symlink {
        if let FileContent::Symlink(link) = &tracked.content {
            materialize_symlink(&full, &link.target)?;
        }
        return Ok(());
    }
    match &tracked.content {
        FileContent::Binary(blob) => atomic::write_atomic(&full, &blob.bytes)?,
        other => atomic::write_atomic_text(&full, &content_to_string(other))?,
    }
    #[cfg(unix)]
    set_unix_mode(&full, tracked.mode)?;
    Ok(())
}

fn build_replay_working_files(
    plan: &MergePlan,
    left_files: &HashMap<String, TrackedFile>,
) -> HashMap<String, TrackedFile> {
    let mut files = plan.merged_files.clone();
    for conflict in &plan.conflicts {
        if let Some(left) = left_files.get(&conflict.path) {
            files.insert(conflict.path.clone(), left.clone());
        }
    }
    files
}

fn all_replay_paths(
    base_files: &HashMap<String, TrackedFile>,
    left_files: &HashMap<String, TrackedFile>,
    right_files: &HashMap<String, TrackedFile>,
    working_files: &HashMap<String, TrackedFile>,
) -> HashSet<String> {
    let mut paths: HashSet<String> = base_files.keys().cloned().collect();
    paths.extend(left_files.keys().cloned());
    paths.extend(right_files.keys().cloned());
    paths.extend(working_files.keys().cloned());
    paths
}

impl Repo {
    pub fn rebase_start(&self, upstream: &str, force: bool) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.rebase_start_unlocked(upstream, force)
    }

    pub fn rebase_abort(&self) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.rebase_abort_unlocked()
    }

    pub fn rebase_continue(&self, resolutions: &[MergeResolution], force: bool) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.rebase_continue_unlocked(resolutions, force)
    }

    fn rebase_start_unlocked(&self, upstream: &str, force: bool) -> RepoResult<()> {
        let branch = self
            .head_branch_unlocked()?
            .ok_or_else(|| RepoError::invalid_input("rebase requires a checked-out branch"))?;
        if load_rebase_state(&self.astvcs_dir())
            .map_err(RepoError::from_message)?
            .is_some()
        {
            return Err(RepoError::invalid_input("rebase already in progress"));
        }

        let staging = self.load_staging_unlocked()?;
        if staging.staging_in_use() {
            return Err(RepoError::invalid_input(
                "cannot rebase with staged changes; commit or reset --mixed to unstage",
            ));
        }
        if !self.working_tree_is_clean_unlocked()? && !force {
            return Err(RepoError::dirty_working_tree(WORKING_TREE_DIRTY_ERR));
        }

        let onto = self.resolve_state_ref_unlocked(upstream)?;
        let tip = self.read_branch_ref(&branch)?;
        if tip == onto {
            return Err(RepoError::invalid_input("already up to date"));
        }

        let base = crate::store::history::merge_base(&tip, &onto, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })
        .map_err(RepoError::from_message)?;

        let remaining = collect_linear_commits(&tip, &base, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })
        .map_err(RepoError::from_message)?;

        if remaining.is_empty() {
            return Err(RepoError::invalid_input("already up to date"));
        }

        let state = RebaseState {
            branch: branch.clone(),
            upstream: upstream.to_string(),
            onto: onto.clone(),
            original_tip: tip.clone(),
            current_head: onto.clone(),
            remaining,
            conflicted: None,
        };
        save_rebase_state(&self.astvcs_dir(), &state).map_err(RepoError::from_message)?;
        trace::notice(format!(
            "rebase: started onto {onto} ({}) commits to replay",
            state.remaining.len()
        ));
        self.rebase_advance_unlocked(force)
    }

    fn rebase_abort_unlocked(&self) -> RepoResult<()> {
        let astvcs = self.astvcs_dir();
        let state = load_rebase_state(&astvcs)
            .map_err(RepoError::from_message)?
            .ok_or_else(|| RepoError::invalid_input("no rebase in progress"))?;

        self.write_branch_ref_unlocked(&state.branch, &state.original_tip)?;
        let materialize_opts = MaterializeOptions::new("rebase --abort").force(true);
        self.materialize_state_inner(&state.original_tip, Vec::new(), &materialize_opts)?;
        delete_rebase_state(&astvcs).map_err(RepoError::from_message)?;
        trace::notice(format!(
            "rebase: aborted; restored branch {} to {}",
            state.branch, state.original_tip
        ));
        Ok(())
    }

    fn rebase_continue_unlocked(
        &self,
        resolutions: &[MergeResolution],
        force: bool,
    ) -> RepoResult<()> {
        let astvcs = self.astvcs_dir();
        let mut state = load_rebase_state(&astvcs)
            .map_err(RepoError::from_message)?
            .ok_or_else(|| RepoError::invalid_input("no rebase in progress"))?;
        let conflicted = state
            .conflicted
            .clone()
            .ok_or_else(|| RepoError::invalid_input("no rebase conflict to continue"))?;

        let entry = self.load_timeline_entry_unlocked(&conflicted)?;
        let parent = linear_timeline_parent(&entry).map_err(|e| match e {
            LinearParentError::MergeCommit(id) => RepoError::from_message(format!(
                "rebase requires linear history; merge commit {id}"
            )),
            LinearParentError::NoParent(id) => {
                RepoError::from_message(format!("rebase: no parent for {id}"))
            }
        })?;
        let mut plan = self.plan_three_way_unlocked(&parent, &state.current_head, &conflicted)?;
        let left_files = self.load_state_files_unlocked(&state.current_head)?;
        let right_files = self.load_state_files_unlocked(&conflicted)?;

        if !resolutions.is_empty() {
            apply_merge_resolutions(&mut plan, &left_files, &right_files, resolutions)
                .map_err(RepoError::from_message)?;
        }

        if !plan.is_clean() {
            self.apply_working_tree_resolutions(&mut plan, &left_files, &right_files)?;
        }

        if !plan.is_clean() {
            trace::warn("rebase: conflicts remain after continue");
            return Err(RepoError::merge_conflict(plan.format_conflicts())
                .with_concise(plan.format_conflicts_focused()));
        }

        let new_head = self.persist_replay_commit(&entry, &plan, &state.current_head)?;
        state.current_head = new_head.clone();
        state.conflicted = None;
        state.remaining.remove(0);
        self.write_branch_ref_unlocked(&state.branch, &new_head)?;

        if state.remaining.is_empty() {
            delete_rebase_state(&astvcs).map_err(RepoError::from_message)?;
            let materialize_opts = MaterializeOptions::new("rebase").force(true);
            self.materialize_state_inner(&new_head, Vec::new(), &materialize_opts)?;
            trace::notice(format!("rebase: finished at {new_head}"));
        } else {
            save_rebase_state(&astvcs, &state).map_err(RepoError::from_message)?;
            self.rebase_advance_unlocked(force)?;
        }
        Ok(())
    }

    fn apply_working_tree_resolutions(
        &self,
        plan: &mut MergePlan,
        left_files: &HashMap<String, TrackedFile>,
        right_files: &HashMap<String, TrackedFile>,
    ) -> RepoResult<()> {
        let remaining: Vec<String> = plan.conflicts.iter().map(|c| c.path.clone()).collect();
        for path in remaining {
            let tracked = match load_working_tracked(self.root_path(), &path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let left = left_files.get(&path);
            let right = right_files.get(&path);
            let differs_from_sides = left.is_none_or(|l| !tracked_eq_paths(l, &tracked))
                && right.is_none_or(|r| !tracked_eq_paths(r, &tracked));
            if differs_from_sides {
                plan.merged_files.insert(path.clone(), tracked);
                plan.conflicts.retain(|c| c.path != path);
                trace::notice(format!(
                    "rebase continue: resolved {path} from working tree"
                ));
            }
        }
        Ok(())
    }

    fn persist_replay_commit(
        &self,
        source: &TimelineEntry,
        plan: &MergePlan,
        parent: &StateId,
    ) -> RepoResult<StateId> {
        let state_id = self.persist_state(
            &plan.merged_files,
            &source.message,
            &AuthorIdentity {
                name: source.author_name.clone(),
                email: source.author_email.clone(),
            },
            Some(parent.clone()),
            vec![parent.clone()],
        )?;
        trace::notice(format!("rebase: replayed {} as {state_id}", source.id));
        Ok(state_id)
    }

    fn rebase_advance_unlocked(&self, force: bool) -> RepoResult<()> {
        loop {
            let astvcs = self.astvcs_dir();
            let mut state = load_rebase_state(&astvcs)
                .map_err(RepoError::from_message)?
                .ok_or_else(|| RepoError::invalid_input("no rebase in progress"))?;

            if state.remaining.is_empty() {
                delete_rebase_state(&astvcs).map_err(RepoError::from_message)?;
                let materialize_opts = MaterializeOptions::new("rebase").force(true);
                self.materialize_state_inner(&state.current_head, Vec::new(), &materialize_opts)?;
                trace::notice(format!("rebase: finished at {}", state.current_head));
                return Ok(());
            }

            let commit_id = state.remaining[0].clone();
            match self.rebase_replay_one_unlocked(&mut state, &commit_id, force) {
                Ok(()) => continue,
                Err(e) if e.kind == RepoErrorKind::MergeConflict => {
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn rebase_replay_one_unlocked(
        &self,
        state: &mut RebaseState,
        commit_id: &StateId,
        force: bool,
    ) -> RepoResult<()> {
        let entry = self.load_timeline_entry_unlocked(commit_id)?;
        let parent = linear_timeline_parent(&entry).map_err(|e| match e {
            LinearParentError::MergeCommit(id) => RepoError::from_message(format!(
                "rebase requires linear history; merge commit {id}"
            )),
            LinearParentError::NoParent(id) => {
                RepoError::from_message(format!("rebase: no parent for {id}"))
            }
        })?;
        let plan = self.plan_three_way_unlocked(&parent, &state.current_head, commit_id)?;

        if !plan.is_clean() {
            state.conflicted = Some(commit_id.clone());
            save_rebase_state(&self.astvcs_dir(), state).map_err(RepoError::from_message)?;
            self.materialize_replay_conflict_unlocked(&plan, &state.current_head, force)?;
            trace::warn(format!("rebase: conflict replaying {commit_id}"));
            return Err(RepoError::merge_conflict(plan.format_conflicts())
                .with_concise(plan.format_conflicts_focused()));
        }

        let new_head = self.persist_replay_commit(&entry, &plan, &state.current_head)?;
        state.current_head = new_head.clone();
        state.remaining.remove(0);
        self.write_branch_ref_unlocked(&state.branch, &new_head)?;
        save_rebase_state(&self.astvcs_dir(), state).map_err(RepoError::from_message)?;
        trace::notice(format!(
            "rebase: replayed {commit_id} -> {new_head} ({} remaining)",
            state.remaining.len()
        ));
        Ok(())
    }

    fn materialize_replay_conflict_unlocked(
        &self,
        plan: &MergePlan,
        index_state: &StateId,
        force: bool,
    ) -> RepoResult<()> {
        let base_files = self.load_state_files_unlocked(&plan.base_id)?;
        let left_files = self.load_state_files_unlocked(&plan.head_id)?;
        let right_files = self.load_state_files_unlocked(&plan.other_id)?;
        let working_files = build_replay_working_files(plan, &left_files);
        let paths = all_replay_paths(&base_files, &left_files, &right_files, &working_files);

        if !force {
            let staging = self.load_staging_unlocked()?;
            if staging.staging_in_use() {
                return Err(RepoError::invalid_input(
                    "cannot materialize rebase conflict with staged changes",
                ));
            }
        }

        for path in &paths {
            if working_files.contains_key(path) {
                continue;
            }
            let full = self.root_path().join(path);
            if full.exists() || full.is_symlink() {
                remove_working_path(&full).map_err(RepoError::from_message)?;
                trace::notice(format!("rebase: removed {path}"));
            }
        }

        for (path, tracked) in &working_files {
            write_tracked_to_disk(self.root_path(), path, tracked)
                .map_err(RepoError::from_message)?;
            trace::notice(format!("rebase: wrote {path}"));
        }

        self.sync_index_to_state(&working_files, index_state)?;
        let mut staging = StagingIndex::default();
        clear_staging_entries(&mut staging);
        self.save_staging_unlocked(&staging)?;
        scan_cache::invalidate_scan_cache(&self.astvcs_dir()).map_err(RepoError::from_message)?;
        Ok(())
    }
}

fn tracked_eq_paths(a: &TrackedFile, b: &TrackedFile) -> bool {
    crate::store::tracked::tracked_eq(a, b)
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
    fn collect_linear_commits_orders_oldest_first() {
        let base = "b".repeat(64);
        let c1 = "1".repeat(64);
        let c2 = "2".repeat(64);
        let tip = "3".repeat(64);
        let mut db = HashMap::new();
        db.insert(base.clone(), entry(&base, None));
        db.insert(c1.clone(), entry(&c1, Some(&base)));
        db.insert(c2.clone(), entry(&c2, Some(&c1)));
        db.insert(tip.clone(), entry(&tip, Some(&c2)));

        let commits = collect_linear_commits(&tip, &base, |id| {
            db.get(id).cloned().ok_or_else(|| format!("missing {id}"))
        })
        .unwrap();
        assert_eq!(commits, vec![c1, c2, tip]);
    }

    #[test]
    fn collect_linear_commits_empty_when_tip_is_base() {
        let base = "b".repeat(64);
        let db = HashMap::from([(base.clone(), entry(&base, None))]);
        let commits = collect_linear_commits(&base, &base, |id| {
            db.get(id).cloned().ok_or_else(|| format!("missing {id}"))
        })
        .unwrap();
        assert!(commits.is_empty());
    }

    #[test]
    fn collect_linear_commits_rejects_merge_commit() {
        let base = "b".repeat(64);
        let merge = "m".repeat(64);
        let mut db = HashMap::new();
        db.insert(base.clone(), entry(&base, None));
        db.insert(
            merge.clone(),
            TimelineEntry {
                id: merge.clone(),
                parent: None,
                parents: vec!["a".repeat(64), "c".repeat(64)],
                message: "merge".into(),
                timestamp: "0".into(),
                author_name: String::new(),
                author_email: String::new(),
                manifest: HashMap::new(),
                files: None,
            },
        );
        let err = collect_linear_commits(&merge, &base, |id| {
            db.get(id).cloned().ok_or_else(|| format!("missing {id}"))
        })
        .unwrap_err();
        assert!(err.contains("merge commit"), "{err}");
    }
}
