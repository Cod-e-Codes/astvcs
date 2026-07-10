use crate::frontend::FileContent;
use crate::merge::{PathMergeConflict, PathMergeTrackedOutcome, merge_tracked_path};
use crate::store::atomic::{self, write_atomic_json};
use crate::store::blobs::BlobStore;
use crate::store::error::{RepoError, RepoResult};
use crate::store::manifest::{FileMode, ManifestEntry, ManifestMap};
use crate::store::repo::{MaterializeOptions, WORKING_TREE_DIRTY_ERR};
use crate::store::scan_cache;
use crate::store::tracked::{TrackedFile, tracked_eq};
use crate::store::working::load_working_tracked;
use crate::store::{Repo, ScanOptions, StateId};
use crate::trace;
use crate::unparser::unparse;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const STASH_DIR: &str = "stash";
const STACK_FILE: &str = "stack.json";

pub type StashId = u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashStack {
    pub ids: Vec<StashId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashEntry {
    pub id: StashId,
    pub message: String,
    pub base_state_id: StateId,
    pub created_at: String,
    pub manifest: ManifestMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashInfo {
    pub index: usize,
    pub id: StashId,
    pub message: String,
    pub base_state_id: StateId,
}

pub fn stash_dir(astvcs: &Path) -> PathBuf {
    astvcs.join(STASH_DIR)
}

fn stack_path(astvcs: &Path) -> PathBuf {
    stash_dir(astvcs).join(STACK_FILE)
}

fn entry_path(astvcs: &Path, id: StashId) -> PathBuf {
    stash_dir(astvcs).join(format!("{id}.json"))
}

pub fn load_stack(astvcs: &Path) -> Result<StashStack, String> {
    let path = stack_path(astvcs);
    if !path.is_file() {
        return Ok(StashStack { ids: Vec::new() });
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(StashStack { ids: Vec::new() });
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub fn save_stack(astvcs: &Path, stack: &StashStack) -> Result<(), String> {
    fs::create_dir_all(stash_dir(astvcs)).map_err(|e| e.to_string())?;
    write_atomic_json(&stack_path(astvcs), stack)
}

pub fn load_entry(astvcs: &Path, id: StashId) -> Result<StashEntry, String> {
    let path = entry_path(astvcs, id);
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub fn save_entry(astvcs: &Path, entry: &StashEntry) -> Result<(), String> {
    fs::create_dir_all(stash_dir(astvcs)).map_err(|e| e.to_string())?;
    write_atomic_json(&entry_path(astvcs, entry.id), entry)
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn next_stash_id(stack: &StashStack) -> StashId {
    stack.ids.iter().copied().max().map(|m| m + 1).unwrap_or(0)
}

fn load_stash_files(
    store: &BlobStore,
    manifest: &ManifestMap,
) -> Result<HashMap<String, TrackedFile>, String> {
    let mut files = HashMap::new();
    for (path, entry) in manifest {
        let content = store.read(&entry.blob)?;
        files.insert(path.clone(), TrackedFile::new(content, entry.mode));
    }
    Ok(files)
}

struct StashApplyPlan {
    merged_files: HashMap<String, TrackedFile>,
    removed_paths: Vec<String>,
    conflicts: Vec<PathMergeConflict>,
}

fn plan_stash_apply(
    base_files: &HashMap<String, TrackedFile>,
    head_files: &HashMap<String, TrackedFile>,
    stash_files: &HashMap<String, TrackedFile>,
) -> StashApplyPlan {
    let mut merged_files = HashMap::new();
    let mut removed_paths = Vec::new();
    let mut conflicts = Vec::new();

    // Only paths recorded in the stash manifest are patched onto the working tree.
    // Untouched tracked files must stay on disk (merge_path treats absent stash
    // sides as deletion when base/head are unchanged).
    for path in stash_files.keys() {
        let base = base_files.get(path);
        let left = head_files.get(path);
        let right = stash_files.get(path);
        match merge_tracked_path(path, base, left, right) {
            PathMergeTrackedOutcome::Keep(tracked) => {
                merged_files.insert(path.clone(), tracked);
            }
            PathMergeTrackedOutcome::Remove => {
                removed_paths.push(path.clone());
            }
            PathMergeTrackedOutcome::Conflict(c) => {
                conflicts.push(c);
            }
        }
    }

    StashApplyPlan {
        merged_files,
        removed_paths,
        conflicts,
    }
}

fn format_stash_conflicts(
    base_id: &StateId,
    head_id: &StateId,
    conflicts: &[PathMergeConflict],
) -> String {
    let mut out = String::from("merge would conflict\n");
    out.push_str(&format!("base: {base_id}\n"));
    out.push_str(&format!("head: {head_id}\n"));
    for conflict in conflicts {
        out.push_str(&conflict.format_report());
    }
    out
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
                "stash apply: could not create symlink at {} -> {target}: {e}; skipped",
                path.display()
            ));
        }
        Ok(())
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

impl Repo {
    pub fn stash_push(
        &self,
        message: Option<String>,
        include_untracked: bool,
    ) -> RepoResult<StashId> {
        let _lock = self.repo_lock()?;
        self.stash_push_unlocked(message, include_untracked)
    }

    fn stash_push_unlocked(
        &self,
        message: Option<String>,
        include_untracked: bool,
    ) -> RepoResult<StashId> {
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let manifest = self.collect_stash_manifest(&head, &head_files, include_untracked)?;
        let msg = message.unwrap_or_else(|| self.default_stash_message(&head));
        let astvcs = self.astvcs_dir();
        let mut stack = load_stack(&astvcs).map_err(RepoError::from_message)?;
        let id = next_stash_id(&stack);
        let entry = StashEntry {
            id,
            message: msg,
            base_state_id: head.clone(),
            created_at: now_iso(),
            manifest,
        };
        save_entry(&astvcs, &entry).map_err(RepoError::from_message)?;
        stack.ids.insert(0, id);
        save_stack(&astvcs, &stack).map_err(RepoError::from_message)?;
        trace::notice(format!("stash push: saved stash@{{0}} id={id}"));

        let materialize_opts = MaterializeOptions::new("stash push");
        self.materialize_state_inner(&head, Vec::new(), &materialize_opts)?;
        Ok(id)
    }

    fn default_stash_message(&self, head: &StateId) -> String {
        let branch_label = match self.head_branch_unlocked() {
            Ok(Some(name)) => name,
            _ => "detached HEAD".to_string(),
        };
        let head_short = if head.len() >= 8 { &head[..8] } else { head };
        format!("WIP on {branch_label}: {head_short}")
    }

    fn collect_stash_manifest(
        &self,
        head: &StateId,
        head_files: &HashMap<String, TrackedFile>,
        include_untracked: bool,
    ) -> RepoResult<ManifestMap> {
        let store = self.blobs();
        let (working_files, _, _) = self.scan_working(head, ScanOptions::default())?;
        let mut manifest = ManifestMap::new();
        let mut has_changes = false;

        for path in head_files.keys() {
            if working_files.contains(path) {
                let current = load_working_tracked(self.root_path(), path)
                    .map_err(RepoError::from_message)?;
                let head_entry = head_files.get(path).unwrap();
                if !tracked_eq(head_entry, &current) {
                    has_changes = true;
                    let blob_id = store
                        .write(&current.content)
                        .map_err(RepoError::from_message)?;
                    manifest.insert(
                        path.clone(),
                        ManifestEntry::with_mode(blob_id, current.mode),
                    );
                }
            } else {
                has_changes = true;
            }
        }

        if include_untracked {
            for path in &working_files {
                if !head_files.contains_key(path) {
                    has_changes = true;
                    let current = load_working_tracked(self.root_path(), path.as_str())
                        .map_err(RepoError::from_message)?;
                    let blob_id = store
                        .write(&current.content)
                        .map_err(RepoError::from_message)?;
                    manifest.insert(
                        path.clone(),
                        ManifestEntry::with_mode(blob_id, current.mode),
                    );
                }
            }
        }

        if !has_changes {
            return Err(RepoError::invalid_input("no local changes to stash"));
        }
        Ok(manifest)
    }

    pub fn stash_list(&self) -> RepoResult<Vec<StashInfo>> {
        let _lock = self.repo_lock()?;
        let stack = load_stack(&self.astvcs_dir()).map_err(RepoError::from_message)?;
        let mut out = Vec::new();
        for (index, id) in stack.ids.iter().enumerate() {
            let entry = load_entry(&self.astvcs_dir(), *id).map_err(RepoError::from_message)?;
            out.push(StashInfo {
                index,
                id: entry.id,
                message: entry.message,
                base_state_id: entry.base_state_id,
            });
        }
        Ok(out)
    }

    pub fn stash_apply(&self, index: usize) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.stash_apply_unlocked(index, false)
    }

    pub fn stash_pop(&self, index: usize) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.stash_apply_unlocked(index, true)
    }

    fn stash_apply_unlocked(&self, index: usize, pop: bool) -> RepoResult<()> {
        if !self.working_tree_is_clean_unlocked()? {
            return Err(RepoError::dirty_working_tree(WORKING_TREE_DIRTY_ERR));
        }

        let astvcs = self.astvcs_dir();
        let mut stack = load_stack(&astvcs).map_err(RepoError::from_message)?;
        let stash_id = *stack
            .ids
            .get(index)
            .ok_or_else(|| RepoError::invalid_input(format!("stash@{{{index}}} not found")))?;
        let entry = load_entry(&astvcs, stash_id).map_err(RepoError::from_message)?;

        let head = self.head_state_unlocked()?;
        let base_files = self.load_state_files_unlocked(&entry.base_state_id)?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let stash_files =
            load_stash_files(&self.blobs(), &entry.manifest).map_err(RepoError::from_message)?;

        let plan = plan_stash_apply(&base_files, &head_files, &stash_files);
        if !plan.conflicts.is_empty() {
            trace::warn("stash: aborted due to conflicts");
            return Err(RepoError::merge_conflict(format_stash_conflicts(
                &entry.base_state_id,
                &head,
                &plan.conflicts,
            )));
        }

        for path in &plan.removed_paths {
            let full = self.root_path().join(path);
            if full.exists() || full.is_symlink() {
                remove_working_path(&full).map_err(RepoError::from_message)?;
                trace::notice(format!("stash apply: removed {path}"));
            }
        }
        for (path, tracked) in &plan.merged_files {
            write_tracked_to_disk(self.root_path(), path, tracked)
                .map_err(RepoError::from_message)?;
            trace::notice(format!("stash apply: wrote {path}"));
        }
        scan_cache::invalidate_scan_cache(&astvcs).map_err(RepoError::from_message)?;

        if pop {
            stack.ids.remove(index);
            save_stack(&astvcs, &stack).map_err(RepoError::from_message)?;
            let path = entry_path(&astvcs, stash_id);
            if path.is_file() {
                fs::remove_file(&path).map_err(|e| RepoError::from_io("remove stash entry", e))?;
            }
            trace::notice(format!("stash pop: removed stash@{{{index}}}"));
        } else {
            trace::notice(format!("stash apply: applied stash@{{{index}}}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn stash_stack_save_load() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        fs::create_dir_all(&astvcs).unwrap();
        let stack = StashStack { ids: vec![2, 1, 0] };
        save_stack(&astvcs, &stack).unwrap();
        let loaded = load_stack(&astvcs).unwrap();
        assert_eq!(loaded, stack);
    }

    #[test]
    fn stash_entry_roundtrip() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        fs::create_dir_all(&astvcs).unwrap();
        let entry = StashEntry {
            id: 0,
            message: "WIP".into(),
            base_state_id: "0".repeat(64),
            created_at: "1".into(),
            manifest: ManifestMap::from([("a.txt".into(), ManifestEntry::regular("abc".into()))]),
        };
        save_entry(&astvcs, &entry).unwrap();
        let loaded = load_entry(&astvcs, 0).unwrap();
        assert_eq!(loaded, entry);
    }
}
