use crate::diff::{
    DiffResult, build_rename_map, detect_path_renames, diff_graphs, diff_text,
    rename_targets_conflict, side_path_for_base,
};
use crate::frontend::{FileContent, parse_text_or_blob};
use crate::intent;
use crate::merge::{MergeConflict, PathMergeConflict, PathMergeOutcome, merge_path};
use crate::store::atomic::{self, write_atomic_json, write_atomic_text};
use crate::store::blobs::{BlobStore, hash_manifest};
use crate::store::error::{RepoError, RepoResult};
use crate::store::history::{merge_base, walk_history};
use crate::store::identity::{AuthorIdentity, resolve_author_identity};
use crate::store::lock::{self, RepoLockGuard};
use crate::store::merge_resolve::{MergeResolution, apply_merge_resolutions};
use crate::trace;
use crate::unparser::unparse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub type StateId = String;

use crate::store::walk::{self, ASTVCS_DIR};

const HEAD_FILE: &str = "HEAD";
const INDEX_FILE: &str = "index.json";
const CONFIG_FILE: &str = "config.json";
const STATE_ID_LEN: usize = 64;

const WORKING_TREE_DIRTY_ERR: &str = "working tree has uncommitted changes; commit or pass --force";

const RESET_WORKING_TREE_DIRTY_ERR: &str =
    "working tree has uncommitted changes; commit, soft reset, or pass --force";

/// Dirty-tree policy applied before writing a state manifest to disk.
struct MaterializeOptions<'a> {
    force: bool,
    command: &'a str,
    /// Overwrite disk when HEAD already matches `state_id` (repair drift without `--force`).
    allow_dirty: bool,
    refuse_message: &'a str,
}

impl<'a> MaterializeOptions<'a> {
    fn new(command: &'a str) -> Self {
        Self {
            force: false,
            command,
            allow_dirty: false,
            refuse_message: WORKING_TREE_DIRTY_ERR,
        }
    }

    fn force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    fn allow_dirty(mut self, allow: bool) -> Self {
        self.allow_dirty = allow;
        self
    }

    fn reset_refuse(mut self) -> Self {
        self.refuse_message = RESET_WORKING_TREE_DIRTY_ERR;
        self
    }
}

/// What HEAD points at: a branch name or a detached state id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HeadTarget {
    Branch(String),
    Detached(StateId),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RepoConfig {
    pub version: u32,
    pub default_branch: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    pub state_id: StateId,
    pub content_kind: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StateEntry {
    pub path: String,
    pub content: FileContent,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TimelineEntry {
    pub id: StateId,
    pub parent: Option<StateId>,
    pub parents: Vec<StateId>,
    pub message: String,
    pub timestamp: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_email: String,
    #[serde(default)]
    pub manifest: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<HashMap<String, FileContent>>,
}

#[derive(Clone, Debug)]
pub struct BranchInfo {
    pub name: String,
    pub state_id: StateId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Unchanged,
    Modified,
    Added,
    Removed,
    Renamed { from: String },
    Untracked,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unchanged => write!(f, "unchanged"),
            Self::Modified => write!(f, "modified"),
            Self::Added => write!(f, "added"),
            Self::Removed => write!(f, "removed"),
            Self::Renamed { from } => write!(f, "renamed from {from}"),
            Self::Untracked => write!(f, "untracked"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkingStatus {
    pub entries: HashMap<String, FileStatus>,
}

/// Result of simulating a merge without writing refs or the working tree.
#[derive(Clone, Debug)]
pub struct MergePlan {
    pub base_id: StateId,
    pub head_id: StateId,
    pub other_id: StateId,
    pub merged_files: HashMap<String, FileContent>,
    pub conflicts: Vec<PathMergeConflict>,
}

impl MergePlan {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }

    pub fn format_dry_run(&self) -> String {
        if !self.is_clean() {
            return self.format_conflicts();
        }
        let mut out = String::from("merge dry-run: would merge cleanly\n");
        out.push_str(&format!("base: {}\n", self.base_id));
        out.push_str(&format!("head: {}\n", self.head_id));
        out.push_str(&format!("other: {}\n", self.other_id));
        out.push_str(&format!("{} paths in result\n", self.merged_files.len()));
        out
    }

    pub fn format_conflicts(&self) -> String {
        let mut out = String::from("merge would conflict\n");
        out.push_str(&format!("base: {}\n", self.base_id));
        out.push_str(&format!("head: {}\n", self.head_id));
        out.push_str(&format!("other: {}\n", self.other_id));
        for conflict in &self.conflicts {
            out.push_str(&conflict.format_report());
        }
        out
    }
}

/// Result of committing the working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    pub state_id: StateId,
    pub created: bool,
}

/// Result of simulating a revert without writing refs or the working tree.
#[derive(Clone, Debug)]
pub struct RevertPlan {
    pub target_id: StateId,
    pub parent_id: StateId,
    pub head_id: StateId,
    pub reverted_files: HashMap<String, FileContent>,
    pub conflicts: Vec<PathMergeConflict>,
}

impl RevertPlan {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }

    pub fn format_dry_run(&self) -> String {
        if !self.is_clean() {
            return self.format_conflicts();
        }
        let mut out = String::from("revert dry-run: would revert cleanly\n");
        out.push_str(&format!("target: {}\n", self.target_id));
        out.push_str(&format!("parent: {}\n", self.parent_id));
        out.push_str(&format!("head: {}\n", self.head_id));
        out.push_str(&format!("{} paths in result\n", self.reverted_files.len()));
        out
    }

    pub fn format_conflicts(&self) -> String {
        let mut out = String::from("revert would conflict\n");
        out.push_str(&format!("target: {}\n", self.target_id));
        out.push_str(&format!("parent: {}\n", self.parent_id));
        out.push_str(&format!("head: {}\n", self.head_id));
        for conflict in &self.conflicts {
            out.push_str(&conflict.format_report());
        }
        out
    }
}

/// Result of reverting a state on top of HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertOutcome {
    pub state_id: StateId,
    pub created: bool,
}

pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Acquire the repository exclusive lock for one logical command.
    ///
    /// Reentrant on the same thread. Stray atomic-write temp files are removed
    /// on the outermost acquisition.
    pub fn repo_lock(&self) -> RepoResult<RepoLockGuard> {
        let outer = !lock::lock_held();
        let guard = RepoLockGuard::acquire(&self.astvcs_dir())?;
        if outer {
            atomic::cleanup_stray_temp_files(&self.root)?;
        }
        Ok(guard)
    }

    pub fn open(path: impl AsRef<Path>) -> RepoResult<Self> {
        let root = path.as_ref().to_path_buf();
        if !root.join(ASTVCS_DIR).is_dir() {
            return Err(RepoError::not_found(format!(
                "not an astvcs repository: {}",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    /// Initialize a repository and set default author identity (for tests and tooling).
    pub fn init_with_identity(path: impl AsRef<Path>) -> RepoResult<Self> {
        let repo = Self::init(path)?;
        super::identity::set_identity(&repo, "Test User", "test@example.com", false)?;
        Ok(repo)
    }

    pub fn init(path: impl AsRef<Path>) -> RepoResult<Self> {
        let root = path.as_ref().to_path_buf();
        let astvcs = root.join(ASTVCS_DIR);
        if astvcs.exists() {
            return Err(RepoError::already_exists("repository already exists"));
        }
        fs::create_dir_all(astvcs.join("refs/heads")).map_err(|e| RepoError::from_io("init", e))?;
        fs::create_dir_all(astvcs.join("states")).map_err(|e| RepoError::from_io("init", e))?;
        fs::create_dir_all(astvcs.join("timeline")).map_err(|e| RepoError::from_io("init", e))?;
        BlobStore::new(&astvcs).ensure_dirs()?;

        write_atomic_json(
            &astvcs.join(CONFIG_FILE),
            &RepoConfig {
                version: 2,
                default_branch: "main".into(),
            },
        )?;

        let empty_state = StateId::from("0".repeat(64));
        write_atomic_text(&astvcs.join(HEAD_FILE), "main\n")?;
        write_atomic_text(&astvcs.join("refs/heads/main"), &format!("{empty_state}\n"))?;
        write_atomic_json(
            &astvcs.join(INDEX_FILE),
            &HashMap::<String, IndexEntry>::new(),
        )?;

        let entry = TimelineEntry {
            id: empty_state.clone(),
            parent: None,
            parents: vec![],
            message: "initial empty state".into(),
            timestamp: now_iso(),
            author_name: String::new(),
            author_email: String::new(),
            manifest: HashMap::new(),
            files: None,
        };
        write_atomic_json(
            &astvcs.join("timeline").join(format!("{empty_state}.json")),
            &entry,
        )?;
        write_atomic_json(
            &astvcs.join("states").join(format!("{empty_state}.json")),
            &HashMap::<String, String>::new(),
        )?;

        trace::notice(format!(
            "init: repository created at {} (branch main -> {empty_state})",
            root.display()
        ));
        Ok(Self { root })
    }

    fn scan_working(&self) -> RepoResult<HashSet<String>> {
        let report = walk::scan_working_files(&self.root)?;
        for skip in &report.skipped {
            trace::warn(format!("scan: skipped {} ({})", skip.path, skip.reason));
        }
        trace::notice(format!(
            "scan: {} tracked, {} skipped",
            report.files.len(),
            report.skipped.len()
        ));
        Ok(report.files)
    }

    fn check_index_consistency(
        &self,
        head: &StateId,
        head_files: &HashMap<String, FileContent>,
        index: &HashMap<String, IndexEntry>,
    ) {
        for (path, entry) in index {
            if !head_files.contains_key(path) {
                trace::warn(format!(
                    "index: {path} tracked in index but absent from HEAD state {head}"
                ));
            } else if entry.state_id != *head {
                trace::warn(format!(
                    "index: {path} state_id {} differs from HEAD {head}",
                    entry.state_id
                ));
            } else if let Some(content) = head_files.get(path) {
                let kind = content.display_kind();
                if entry.content_kind != kind {
                    trace::warn(format!(
                        "index: {path} kind {} differs from HEAD kind {kind}",
                        entry.content_kind
                    ));
                }
            }
        }
    }

    pub fn astvcs_dir(&self) -> PathBuf {
        self.root.join(ASTVCS_DIR)
    }

    fn blobs(&self) -> BlobStore {
        BlobStore::new(self.astvcs_dir())
    }

    pub(crate) fn blobs_store(&self) -> BlobStore {
        self.blobs()
    }

    pub fn head_branch(&self) -> RepoResult<Option<String>> {
        let _lock = self.repo_lock()?;
        match self.read_head_target()? {
            HeadTarget::Branch(name) => Ok(Some(name)),
            HeadTarget::Detached(_) => Ok(None),
        }
    }

    pub fn is_detached(&self) -> RepoResult<bool> {
        let _lock = self.repo_lock()?;
        Ok(matches!(self.read_head_target()?, HeadTarget::Detached(_)))
    }

    pub fn head_state(&self) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        match self.read_head_target()? {
            HeadTarget::Branch(name) => self.read_branch_ref(&name),
            HeadTarget::Detached(id) => Ok(id),
        }
    }

    pub fn branch_state(&self, branch: &str) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        self.read_branch_ref(branch)
    }

    pub(crate) fn read_branch_ref(&self, branch: &str) -> RepoResult<StateId> {
        let text = fs::read_to_string(self.astvcs_dir().join("refs/heads").join(branch))
            .map_err(|e| e.to_string())?;
        Ok(text.trim().to_string())
    }

    pub fn list_branches(&self) -> RepoResult<Vec<BranchInfo>> {
        let _lock = self.repo_lock()?;
        self.list_branches_unlocked()
    }

    pub(crate) fn list_branches_unlocked(&self) -> RepoResult<Vec<BranchInfo>> {
        let dir = self.astvcs_dir().join("refs/heads");
        let mut branches = Vec::new();
        if !dir.is_dir() {
            return Ok(branches);
        }
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            branches.push(BranchInfo {
                name: name.clone(),
                state_id: self.read_branch_ref(&name)?,
            });
        }
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(branches)
    }

    pub fn create_branch(&self, name: &str, from: Option<&str>) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if ref_path.exists() {
            return Err(RepoError::already_exists(format!(
                "branch already exists: {name}"
            )));
        }
        let state = match from {
            Some(b) => self.read_branch_ref(b)?,
            None => self.head_state_unlocked()?,
        };
        write_atomic_text(&ref_path, &format!("{state}\n"))?;
        trace::notice(format!("branch: created {name} at state {state}"));
        Ok(())
    }

    /// Remove a branch ref. States remain in the store; only the named ref is deleted.
    pub fn remove_branch(&self, name: &str) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if !ref_path.exists() {
            return Err(RepoError::not_found(format!("branch not found: {name}")));
        }
        if self.head_branch_unlocked()? == Some(name.to_string()) {
            return Err(RepoError::branch_guard(format!(
                "cannot remove the checked-out branch: {name}"
            )));
        }
        if self.list_branches_unlocked()?.len() <= 1 {
            return Err(RepoError::branch_guard("cannot remove the last branch"));
        }
        fs::remove_file(ref_path).map_err(|e| e.to_string())?;
        trace::notice(format!("branch: removed {name}"));
        Ok(())
    }

    pub fn checkout_branch(&self, name: &str) -> RepoResult<()> {
        self.checkout_branch_with_force(name, false)
    }

    pub fn checkout_branch_with_force(&self, name: &str, force: bool) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if !ref_path.exists() {
            return Err(RepoError::not_found(format!("branch not found: {name}")));
        }
        let prior_branch = self.head_branch_unlocked()?;
        let prior_head = self.head_state_unlocked()?;
        let state = self.read_branch_ref(name)?;
        let allow_dirty = prior_branch.as_deref() == Some(name) && prior_head == state;
        let materialize_opts = MaterializeOptions::new("checkout")
            .force(force)
            .allow_dirty(allow_dirty);
        let clobbered = self.materialize_guard(&materialize_opts)?;
        self.materialize_state_inner(&state, clobbered, &materialize_opts)?;
        self.write_head_target(&HeadTarget::Branch(name.to_string()))?;
        trace::notice(format!("checkout: branch {name} -> state {state}"));
        Ok(())
    }

    pub fn load_manifest(&self, state_id: &StateId) -> RepoResult<HashMap<String, String>> {
        let _lock = self.repo_lock()?;
        self.load_manifest_unlocked(state_id)
    }

    pub(crate) fn load_manifest_unlocked(
        &self,
        state_id: &StateId,
    ) -> RepoResult<HashMap<String, String>> {
        let path = self
            .astvcs_dir()
            .join("states")
            .join(format!("{state_id}.json"));
        if path.exists() {
            return read_json(&path);
        }
        let entry = self.load_timeline_entry_unlocked(state_id)?;
        if entry.files.is_some() {
            trace::warn(format!(
                "state {state_id}: migrating legacy inline files to blob storage"
            ));
        }
        if let Some(files) = entry.files {
            return self.migrate_inline_files(&files);
        }
        Ok(entry.manifest)
    }

    pub fn load_state_files(&self, state_id: &StateId) -> RepoResult<HashMap<String, FileContent>> {
        let _lock = self.repo_lock()?;
        self.load_state_files_unlocked(state_id)
    }

    fn load_state_files_unlocked(
        &self,
        state_id: &StateId,
    ) -> RepoResult<HashMap<String, FileContent>> {
        let manifest = self.load_manifest_unlocked(state_id)?;
        let store = self.blobs();
        let mut files = HashMap::new();
        for (path, blob_id) in manifest {
            files.insert(path, store.read(&blob_id)?);
        }
        Ok(files)
    }

    pub fn load_timeline_entry(&self, state_id: &StateId) -> RepoResult<TimelineEntry> {
        let _lock = self.repo_lock()?;
        self.load_timeline_entry_unlocked(state_id)
    }

    pub(crate) fn load_timeline_entry_unlocked(
        &self,
        state_id: &StateId,
    ) -> RepoResult<TimelineEntry> {
        let path = self
            .astvcs_dir()
            .join("timeline")
            .join(format!("{state_id}.json"));
        read_json(&path)
    }

    pub fn history(&self, limit: usize) -> RepoResult<Vec<TimelineEntry>> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        walk_history(&head, limit, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })
        .map_err(RepoError::from_message)
    }

    pub fn status(&self) -> RepoResult<WorkingStatus> {
        let _lock = self.repo_lock()?;
        self.status_unlocked()
    }

    fn status_unlocked(&self) -> RepoResult<WorkingStatus> {
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let index: HashMap<String, IndexEntry> = read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        self.check_index_consistency(&head, &head_files, &index);

        let mut entries = HashMap::new();
        let working_files = self.scan_working()?;
        let mut working_map = HashMap::new();
        for path in &working_files {
            let disk = read_working_file(&self.root, path)?;
            let current = parse_text_or_blob(path, &disk);
            working_map.insert(path.clone(), current.clone());
            let status = match head_files.get(path) {
                None => FileStatus::Added,
                Some(stored) if !content_eq(stored, &current) => FileStatus::Modified,
                Some(_) => FileStatus::Unchanged,
            };
            if !matches!(status, FileStatus::Unchanged) {
                trace::notice(format!("status: {path} {status}"));
            }
            entries.insert(path.clone(), status);
        }

        for path in head_files.keys() {
            if !working_files.contains(path) {
                trace::notice(format!("status: {path} Removed"));
                entries.insert(path.clone(), FileStatus::Removed);
            }
        }

        let renames = detect_path_renames(&head_files, &working_map);
        for rename in &renames {
            entries.remove(&rename.from);
            entries.insert(
                rename.to.clone(),
                FileStatus::Renamed {
                    from: rename.from.clone(),
                },
            );
        }

        Ok(WorkingStatus { entries })
    }

    pub fn diff_working(&self, path: &str) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let disk = read_working_file(&self.root, path)?;
        let working = parse_text_or_blob(path, &disk);
        let mut working_map = HashMap::new();
        working_map.insert(path.to_string(), working.clone());
        for p in self.scan_working()? {
            if p != path {
                let disk = read_working_file(&self.root, &p)?;
                working_map.insert(p.clone(), parse_text_or_blob(&p, &disk));
            }
        }
        let renames = detect_path_renames(&head_files, &working_map);
        if let Some(rename) = renames.iter().find(|r| r.to == path) {
            let old = head_files.get(&rename.from).unwrap();
            return Ok(format_path_rename(rename, old, &working));
        }
        match head_files.get(path) {
            None => Ok(format!("--- /dev/null\n+++ {path}\n(new file)\n")),
            Some(base) => format_diff(path, base, &working),
        }
    }

    /// Resolve a branch name, remote-tracking ref, or 64-character state id.
    pub fn resolve_state_ref(&self, reference: &str) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        self.resolve_state_ref_unlocked(reference)
    }

    fn resolve_state_ref_unlocked(&self, reference: &str) -> RepoResult<StateId> {
        if is_state_id(reference) {
            self.load_timeline_entry_unlocked(&reference.to_string())?;
            trace::notice(format!("resolved state {reference}"));
            return Ok(reference.to_string());
        }
        let ref_path = self.astvcs_dir().join("refs/heads").join(reference);
        if ref_path.is_file() {
            let id = self.read_branch_ref(reference)?;
            trace::notice(format!("resolved branch {reference} -> state {id}"));
            return Ok(id);
        }
        if let Some((remote, branch)) = reference.split_once('/')
            && let Some(id) = self.read_remote_ref_unlocked(remote, branch)?
        {
            trace::notice(format!("resolved remote ref {reference} -> state {id}"));
            return Ok(id);
        }
        Err(RepoError::unknown_ref(reference))
    }

    /// Lowest common ancestor of two branch names or state ids.
    pub fn merge_base_refs(&self, left: &str, right: &str) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        let left_id = self.resolve_state_ref_unlocked(left)?;
        let right_id = self.resolve_state_ref_unlocked(right)?;
        let base = merge_base(&left_id, &right_id, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })?;
        trace::notice(format!("merge-base: {left_id} + {right_id} -> {base}"));
        Ok(base)
    }

    pub fn diff_three_way(
        &self,
        base: &StateId,
        left: &StateId,
        right: &StateId,
        path: Option<&str>,
    ) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        trace::notice(format!(
            "diff three-way: base={base} left={left} right={right}{}",
            path.map(|p| format!(" path={p}")).unwrap_or_default()
        ));
        let base_files = self.load_state_files_unlocked(base)?;
        let left_files = self.load_state_files_unlocked(left)?;
        let right_files = self.load_state_files_unlocked(right)?;

        let paths: Vec<String> = match path {
            Some(p) => {
                if !base_files.contains_key(p)
                    && !left_files.contains_key(p)
                    && !right_files.contains_key(p)
                {
                    return Err(format!("path not tracked in base, left, or right: {p}").into());
                }
                vec![p.to_string()]
            }
            None => {
                let mut all: HashSet<String> = base_files.keys().cloned().collect();
                all.extend(left_files.keys().cloned());
                all.extend(right_files.keys().cloned());
                let mut sorted: Vec<_> = all.into_iter().collect();
                sorted.sort();
                sorted
            }
        };

        let mut out = String::new();
        for p in paths {
            out.push_str(&format!("=== {p} ===\n"));
            out.push_str(&format!("base:  {base}\n"));
            out.push_str(&format!("left:  {left}\n"));
            out.push_str(&format!("right: {right}\n"));
            match (base_files.get(&p), left_files.get(&p), right_files.get(&p)) {
                (None, None, None) => out.push('\n'),
                (base_c, left_c, right_c) => {
                    if let (Some(b), Some(l)) = (base_c, left_c) {
                        if !content_eq(b, l) {
                            out.push_str("\nbase -> left:\n");
                            out.push_str(&format_mutation_diff(b, l));
                        } else {
                            out.push_str("\nbase -> left: (unchanged)\n");
                        }
                    } else if left_c.is_some() {
                        out.push_str("\nbase -> left: (added on left)\n");
                    } else if base_c.is_some() {
                        out.push_str("\nbase -> left: (removed on left)\n");
                    }
                    if let (Some(b), Some(r)) = (base_c, right_c) {
                        if !content_eq(b, r) {
                            out.push_str("\nbase -> right:\n");
                            out.push_str(&format_mutation_diff(b, r));
                        } else {
                            out.push_str("\nbase -> right: (unchanged)\n");
                        }
                    } else if right_c.is_some() {
                        out.push_str("\nbase -> right: (added on right)\n");
                    } else if base_c.is_some() {
                        out.push_str("\nbase -> right: (removed on right)\n");
                    }
                }
            }
            out.push('\n');
        }
        Ok(out)
    }

    pub fn plan_merge(&self, branch: &str) -> RepoResult<MergePlan> {
        let _lock = self.repo_lock()?;
        self.plan_merge_unlocked(branch)
    }

    fn plan_merge_unlocked(&self, branch: &str) -> RepoResult<MergePlan> {
        let head = self.head_state_unlocked()?;
        let other = self.read_branch_ref(branch)?;
        if head == other {
            return Err("already up to date".into());
        }

        let base_id = merge_base(&head, &other, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })?;
        trace::notice(format!(
            "merge plan: base={base_id} head={head} other={other}"
        ));
        let base_files = self.load_state_files_unlocked(&base_id)?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let other_files = self.load_state_files_unlocked(&other)?;

        let head_renames = detect_path_renames(&base_files, &head_files);
        let other_renames = detect_path_renames(&base_files, &other_files);
        let head_rename_map = build_rename_map(&head_renames);
        let other_rename_map = build_rename_map(&other_renames);

        let mut merged_files = HashMap::new();
        let mut conflicts = Vec::new();
        let mut processed = HashSet::new();
        let mut all_paths: HashSet<String> = head_files.keys().cloned().collect();
        all_paths.extend(other_files.keys().cloned());
        all_paths.extend(base_files.keys().cloned());

        for base_path in base_files.keys() {
            if rename_targets_conflict(base_path, &head_rename_map, &other_rename_map) {
                conflicts.push(PathMergeConflict {
                    path: base_path.clone(),
                    detail: MergeConflict {
                        message: format!("both branches renamed {base_path} to different paths"),
                        left_mutations: vec![],
                        right_mutations: vec![],
                        left_intent_lines: vec![format!(
                            "  [0] {}",
                            intent::format_intent(
                                None,
                                &intent::classify_path_rename(
                                    head_renames.iter().find(|r| r.from == *base_path).unwrap(),
                                ),
                            )
                        )],
                        right_intent_lines: vec![format!(
                            "  [0] {}",
                            intent::format_intent(
                                None,
                                &intent::classify_path_rename(
                                    other_renames.iter().find(|r| r.from == *base_path).unwrap(),
                                ),
                            )
                        )],
                        overlapping: vec![],
                        text_line: None,
                    },
                });
                processed.insert(base_path.clone());
                if let Some(h) = head_rename_map.get(base_path) {
                    processed.insert(h.clone());
                }
                if let Some(o) = other_rename_map.get(base_path) {
                    processed.insert(o.clone());
                }
                continue;
            }

            let result_path = head_rename_map
                .get(base_path)
                .or_else(|| other_rename_map.get(base_path))
                .cloned()
                .unwrap_or_else(|| base_path.clone());
            let head_path =
                side_path_for_base(base_path, &head_files, &head_rename_map).or_else(|| {
                    if head_files.contains_key(&result_path)
                        && !base_files.contains_key(&result_path)
                    {
                        Some(result_path.clone())
                    } else {
                        None
                    }
                });
            let other_path = side_path_for_base(base_path, &other_files, &other_rename_map)
                .or_else(|| {
                    if other_files.contains_key(&result_path)
                        && !base_files.contains_key(&result_path)
                    {
                        Some(result_path.clone())
                    } else {
                        None
                    }
                });

            if (other_rename_map.contains_key(base_path) || head_rename_map.contains_key(base_path))
                && head_files.contains_key(base_path)
                && let (Some(head_dest), Some(other_dest)) = (
                    head_files.get(&result_path),
                    other_path.as_ref().and_then(|p| other_files.get(p)),
                )
                && !content_eq(head_dest, other_dest)
            {
                conflicts.push(PathMergeConflict {
                    path: result_path.clone(),
                    detail: MergeConflict {
                        message: format!(
                            "path rename to {result_path} conflicts with independent content at that path"
                        ),
                        left_mutations: vec![],
                        right_mutations: vec![],
                        left_intent_lines: vec![format!(
                            "  [0] {}",
                            intent::format_intent(
                                None,
                                &intent::classify_path_rename(&crate::diff::PathRename {
                                    from: base_path.clone(),
                                    to: result_path.clone(),
                                    kind: crate::diff::PathRenameKind::Exact,
                                }),
                            )
                        )],
                        right_intent_lines: vec![],
                        overlapping: vec![],
                        text_line: None,
                    },
                });
                processed.insert(base_path.clone());
                if let Some(h) = &head_path {
                    processed.insert(h.clone());
                }
                if let Some(o) = &other_path {
                    processed.insert(o.clone());
                }
                processed.insert(result_path.clone());
                continue;
            }

            match merge_path(
                &result_path,
                base_files.get(base_path),
                head_path.as_ref().and_then(|p| head_files.get(p)),
                other_path.as_ref().and_then(|p| other_files.get(p)),
            ) {
                PathMergeOutcome::Keep(content) => {
                    trace::notice(format!("merge plan: {result_path} keep"));
                    merged_files.insert(result_path, content);
                }
                PathMergeOutcome::Remove => {
                    trace::notice(format!("merge plan: {result_path} remove"));
                }
                PathMergeOutcome::Conflict(c) => {
                    trace::warn(format!("merge plan: {} conflict", c.path));
                    conflicts.push(c);
                }
            }

            processed.insert(base_path.clone());
            if let Some(h) = head_path {
                processed.insert(h);
            }
            if let Some(o) = other_path {
                processed.insert(o);
            }
        }

        for path in all_paths {
            if processed.contains(&path) {
                continue;
            }
            let base = base_files.get(&path);
            let left = head_files.get(&path);
            let right = other_files.get(&path);
            match merge_path(&path, base, left, right) {
                PathMergeOutcome::Keep(content) => {
                    trace::notice(format!("merge plan: {path} keep"));
                    merged_files.insert(path, content);
                }
                PathMergeOutcome::Remove => {
                    trace::notice(format!("merge plan: {path} remove"));
                }
                PathMergeOutcome::Conflict(c) => {
                    trace::warn(format!("merge plan: {} conflict", c.path));
                    conflicts.push(c);
                }
            }
        }

        Ok(MergePlan {
            base_id,
            head_id: head,
            other_id: other,
            merged_files,
            conflicts,
        })
    }

    pub fn plan_revert(&self, target_id: &StateId) -> RepoResult<RevertPlan> {
        let _lock = self.repo_lock()?;
        self.plan_revert_unlocked(target_id)
    }

    fn plan_revert_unlocked(&self, target_id: &StateId) -> RepoResult<RevertPlan> {
        let entry = self.load_timeline_entry_unlocked(target_id)?;
        if entry.parents.len() > 1 {
            return Err(RepoError::revert_precondition(format!(
                "cannot revert merge state {target_id}"
            )));
        }
        let parent_id = match entry
            .parent
            .clone()
            .or_else(|| entry.parents.first().cloned())
        {
            Some(id) => id,
            None => {
                return Err(RepoError::revert_precondition(format!(
                    "cannot revert root state {target_id}"
                )));
            }
        };

        let head = self.head_state_unlocked()?;
        if !self.is_ancestor_of_unlocked(target_id, &head)? {
            return Err(RepoError::revert_precondition(format!(
                "state {target_id} is not an ancestor of HEAD {head}"
            )));
        }

        trace::notice(format!(
            "revert plan: target={target_id} parent={parent_id} head={head}"
        ));
        let base_files = self.load_state_files_unlocked(target_id)?;
        let left_files = self.load_state_files_unlocked(&parent_id)?;
        let head_files = self.load_state_files_unlocked(&head)?;

        let mut reverted_files = base_files.clone();
        let mut conflicts = Vec::new();
        let mut all_paths: HashSet<String> = base_files.keys().cloned().collect();
        all_paths.extend(left_files.keys().cloned());
        all_paths.extend(head_files.keys().cloned());

        for path in all_paths {
            let base = base_files.get(&path);
            let left = left_files.get(&path);
            let right = head_files.get(&path);
            match merge_path(&path, base, left, right) {
                PathMergeOutcome::Keep(content) => {
                    if let (Some(b), None, Some(r)) = (base, left, right)
                        && !r.semantic_eq(b)
                    {
                        trace::warn(format!(
                            "revert plan: {path} modified after the reverted state"
                        ));
                        conflicts.push(PathMergeConflict {
                            path: path.clone(),
                            detail: MergeConflict {
                                message: "path modified after the reverted state".into(),
                                left_mutations: vec![],
                                right_mutations: vec![],
                                left_intent_lines: vec![],
                                right_intent_lines: vec![],
                                overlapping: vec![],
                                text_line: None,
                            },
                        });
                        continue;
                    }
                    trace::notice(format!("revert plan: {path} keep"));
                    reverted_files.insert(path, content);
                }
                PathMergeOutcome::Remove => {
                    trace::notice(format!("revert plan: {path} remove"));
                    reverted_files.remove(&path);
                }
                PathMergeOutcome::Conflict(c) => {
                    trace::warn(format!("revert plan: {} conflict", c.path));
                    conflicts.push(c);
                }
            }
        }

        Ok(RevertPlan {
            target_id: target_id.clone(),
            parent_id,
            head_id: head,
            reverted_files,
            conflicts,
        })
    }

    pub fn revert_state(&self, reference: &str, message: &str) -> RepoResult<RevertOutcome> {
        self.revert_state_with_force(reference, message, false)
    }

    pub fn revert_state_with_force(
        &self,
        reference: &str,
        message: &str,
        force: bool,
    ) -> RepoResult<RevertOutcome> {
        let _lock = self.repo_lock()?;
        let target_id = self.resolve_state_ref_unlocked(reference)?;
        let plan = self.plan_revert_unlocked(&target_id)?;
        if !plan.is_clean() {
            trace::warn("revert: aborted due to conflicts");
            return Err(RepoError::revert_conflict(plan.format_conflicts()));
        }
        self.finish_revert(&plan, message, force)
    }

    pub fn revert_state_dry_run(&self, reference: &str) -> RepoResult<RevertPlan> {
        let _lock = self.repo_lock()?;
        let target_id = self.resolve_state_ref_unlocked(reference)?;
        self.plan_revert_unlocked(&target_id)
    }

    fn finish_revert(
        &self,
        plan: &RevertPlan,
        message: &str,
        force: bool,
    ) -> RepoResult<RevertOutcome> {
        let materialize_opts = MaterializeOptions::new("revert").force(force);
        let head = plan.head_id.clone();
        let head_files = self.load_state_files_unlocked(&head)?;

        if manifest_unchanged(&head_files, &plan.reverted_files) {
            trace::notice(format!("revert: no changes; state {head} unchanged"));
            return Ok(RevertOutcome {
                state_id: head,
                created: false,
            });
        }

        let parent_files = self.load_state_files_unlocked(&plan.parent_id)?;
        if manifest_unchanged(&parent_files, &plan.reverted_files) {
            let clobbered = self.materialize_guard(&materialize_opts)?;
            self.materialize_state_inner(&plan.parent_id, clobbered, &materialize_opts)?;
            match self.read_head_target()? {
                HeadTarget::Branch(branch) => {
                    self.write_branch_ref_unlocked(&branch, &plan.parent_id)?;
                    trace::notice(format!(
                        "revert: updated branch {branch} -> {}",
                        plan.parent_id
                    ));
                }
                HeadTarget::Detached(_) => {
                    self.write_head_target(&HeadTarget::Detached(plan.parent_id.clone()))?;
                    trace::notice(format!("revert: detached HEAD -> {}", plan.parent_id));
                }
            }
            trace::notice(format!(
                "revert: restored parent state {} undoing {}",
                plan.parent_id, plan.target_id
            ));
            return Ok(RevertOutcome {
                state_id: plan.parent_id.clone(),
                created: true,
            });
        }

        let clobbered = self.materialize_guard(&materialize_opts)?;
        let author = resolve_author_identity(self)?;
        let state_id = self.persist_state(
            &plan.reverted_files,
            message,
            &author,
            Some(head.clone()),
            vec![head.clone()],
        )?;
        self.materialize_state_inner(&state_id, clobbered, &materialize_opts)?;
        match self.read_head_target()? {
            HeadTarget::Branch(branch) => {
                self.write_branch_ref_unlocked(&branch, &state_id)?;
                trace::notice(format!("revert: updated branch {branch} -> {state_id}"));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
                trace::notice(format!("revert: detached HEAD -> {state_id}"));
            }
        }
        trace::notice(format!(
            "revert: created state {state_id} undoing {}",
            plan.target_id
        ));
        Ok(RevertOutcome {
            state_id,
            created: true,
        })
    }

    pub fn reset(&self, reference: &str, soft: bool, force: bool) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        let target = self.resolve_state_ref_unlocked(reference)?;
        let prior_head = self.head_state_unlocked()?;
        let materialize_opts = if soft {
            None
        } else {
            Some(
                MaterializeOptions::new("reset")
                    .reset_refuse()
                    .force(force)
                    .allow_dirty(target == prior_head),
            )
        };
        let clobbered = if let Some(ref opts) = materialize_opts {
            self.materialize_guard(opts)?
        } else {
            Vec::new()
        };

        if !soft {
            let opts = materialize_opts.expect("hard reset materialize options");
            self.materialize_state_inner(&target, clobbered, &opts)?;
        }

        match self.read_head_target()? {
            HeadTarget::Branch(ref branch) => {
                self.write_branch_ref_unlocked(branch, &target)?;
                let mode = if soft { "soft" } else { "hard" };
                trace::notice(format!(
                    "reset {mode}: branch {branch} {prior_head} -> {target}"
                ));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(target.clone()))?;
                let mode = if soft { "soft" } else { "hard" };
                trace::notice(format!("reset {mode}: detached {prior_head} -> {target}"));
            }
        }

        Ok(target)
    }

    pub fn diff_state_path(&self, from: &StateId, to: &StateId, path: &str) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let from_files = self.load_state_files_unlocked(from)?;
        let to_files = self.load_state_files_unlocked(to)?;
        let renames = detect_path_renames(&from_files, &to_files);
        if let Some(rename) = renames.iter().find(|r| r.from == path || r.to == path) {
            let old = from_files.get(&rename.from).unwrap();
            let new = to_files.get(&rename.to).unwrap();
            return Ok(format_path_rename(rename, old, new));
        }
        match (from_files.get(path), to_files.get(path)) {
            (None, Some(new)) => {
                let mut out = format!("--- /dev/null\n+++ {path}\n(new file)\n");
                out.push_str(&content_preview(new));
                Ok(out)
            }
            (Some(_), None) => Ok(format!("--- {path}\n+++ /dev/null\n(deleted)\n")),
            (Some(old), Some(new)) if !content_eq(old, new) => format_diff(path, old, new),
            _ => Ok(format!("--- {path}\n+++ {path}\n(no changes)\n")),
        }
    }

    pub fn diff_states(&self, from: &StateId, to: &StateId) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let from_files = self.load_state_files_unlocked(from)?;
        let to_files = self.load_state_files_unlocked(to)?;
        let renames = detect_path_renames(&from_files, &to_files);
        let renamed_from: HashSet<String> = renames.iter().map(|r| r.from.clone()).collect();
        let renamed_to: HashSet<String> = renames.iter().map(|r| r.to.clone()).collect();
        let mut out = String::new();
        for rename in &renames {
            let old = from_files.get(&rename.from).unwrap();
            let new = to_files.get(&rename.to).unwrap();
            out.push_str(&format_path_rename(rename, old, new));
        }
        let mut paths: HashSet<String> = from_files.keys().cloned().collect();
        paths.extend(to_files.keys().cloned());
        let mut sorted: Vec<_> = paths.into_iter().collect();
        sorted.sort();
        for path in sorted {
            if renamed_from.contains(&path) || renamed_to.contains(&path) {
                continue;
            }
            match (from_files.get(&path), to_files.get(&path)) {
                (None, Some(new)) => {
                    out.push_str(&format!("--- /dev/null\n+++ {path}\n(new file)\n"));
                    out.push_str(&content_preview(new));
                }
                (Some(_), None) => {
                    out.push_str(&format!("--- {path}\n+++ /dev/null\n(deleted)\n"));
                }
                (Some(old), Some(new)) if !content_eq(old, new) => {
                    out.push_str(&format_diff(&path, old, new)?);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    pub fn commit(&self, message: &str) -> RepoResult<CommitOutcome> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let working_files = self.scan_working()?;

        let mut new_files = head_files.clone();
        for path in &working_files {
            let disk = read_working_file(&self.root, path)?;
            let content = parse_text_or_blob(path, &disk);
            match head_files.get(path) {
                Some(old) if content_eq(old, &content) => {}
                Some(_) => trace::notice(format!("commit: {path} modified")),
                None => trace::notice(format!("commit: {path} added")),
            }
            new_files.insert(path.clone(), content);
        }
        for path in head_files.keys().cloned().collect::<Vec<_>>() {
            if !working_files.contains(&path) {
                trace::notice(format!("commit: {path} removed"));
                new_files.remove(&path);
            }
        }

        if manifest_unchanged(&head_files, &new_files) {
            trace::notice(format!("commit: no changes; state {head} unchanged"));
            return Ok(CommitOutcome {
                state_id: head,
                created: false,
            });
        }

        let author = resolve_author_identity(self)?;
        let state_id =
            self.persist_state(&new_files, message, &author, Some(head.clone()), vec![head])?;
        self.sync_index_to_state(&new_files, &state_id)?;
        match self.read_head_target()? {
            HeadTarget::Branch(branch) => {
                self.write_branch_ref_unlocked(&branch, &state_id)?;
                trace::notice(format!("commit: updated branch {branch} -> {state_id}"));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
                trace::notice(format!("commit: detached HEAD -> {state_id}"));
            }
        }
        trace::notice(format!("commit: created state {state_id}"));
        Ok(CommitOutcome {
            state_id,
            created: true,
        })
    }

    pub fn merge_branch(&self, branch: &str, message: &str) -> RepoResult<StateId> {
        self.merge_branch_with_resolutions(branch, message, &[])
    }

    pub fn merge_branch_with_resolutions(
        &self,
        branch: &str,
        message: &str,
        resolutions: &[MergeResolution],
    ) -> RepoResult<StateId> {
        self.merge_branch_with_resolutions_force(branch, message, resolutions, false)
    }

    pub fn merge_branch_with_resolutions_force(
        &self,
        branch: &str,
        message: &str,
        resolutions: &[MergeResolution],
        force: bool,
    ) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        let plan = self.prepare_merge_unlocked(branch, resolutions)?;
        if !plan.is_clean() {
            trace::warn("merge: aborted due to conflicts");
            return Err(RepoError::merge_conflict(plan.format_conflicts()));
        }
        self.finish_merge(&plan, message, force)
    }

    fn prepare_merge_unlocked(
        &self,
        branch: &str,
        resolutions: &[MergeResolution],
    ) -> RepoResult<MergePlan> {
        let mut plan = self.plan_merge_unlocked(branch)?;
        if !resolutions.is_empty() {
            let head_files = self.load_state_files_unlocked(&plan.head_id)?;
            let other_files = self.load_state_files_unlocked(&plan.other_id)?;
            apply_merge_resolutions(&mut plan, &head_files, &other_files, resolutions)?;
        }
        Ok(plan)
    }

    pub fn prepare_merge(
        &self,
        branch: &str,
        resolutions: &[MergeResolution],
    ) -> RepoResult<MergePlan> {
        let _lock = self.repo_lock()?;
        self.prepare_merge_unlocked(branch, resolutions)
    }

    fn finish_merge(&self, plan: &MergePlan, message: &str, force: bool) -> RepoResult<StateId> {
        let head = plan.head_id.clone();
        let other = plan.other_id.clone();
        let merged_files = plan.merged_files.clone();
        let materialize_opts = MaterializeOptions::new("merge").force(force);
        let clobbered = self.materialize_guard(&materialize_opts)?;

        let author = resolve_author_identity(self)?;
        let state_id = self.persist_state(
            &merged_files,
            message,
            &author,
            None,
            vec![head.clone(), other.clone()],
        )?;
        self.materialize_state_inner(&state_id, clobbered, &materialize_opts)?;
        let current_branch = self.head_branch_unlocked()?;
        if let Some(branch) = current_branch {
            self.write_branch_ref_unlocked(&branch, &state_id)?;
        } else {
            self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
        }
        trace::notice(format!(
            "merge: created state {state_id} from {head} + {other}"
        ));
        Ok(state_id)
    }

    fn clobbered_paths(&self) -> RepoResult<Vec<String>> {
        Ok(self
            .status_unlocked()?
            .entries
            .iter()
            .filter(|(_, status)| !matches!(status, FileStatus::Unchanged))
            .map(|(path, _)| path.clone())
            .collect())
    }

    fn materialize_guard(&self, opts: &MaterializeOptions<'_>) -> RepoResult<Vec<String>> {
        if self.working_tree_is_clean_unlocked()? {
            return Ok(Vec::new());
        }
        if opts.force {
            return self.clobbered_paths();
        }
        if opts.allow_dirty {
            return Ok(Vec::new());
        }
        Err(RepoError::dirty_working_tree(opts.refuse_message))
    }

    fn emit_materialize_clobber_warnings(&self, clobbered: &[String], command: &str) {
        for path in clobbered {
            trace::warn(format!(
                "{command} --force: discarded uncommitted changes in {path}"
            ));
        }
    }

    fn materialize_state_inner(
        &self,
        state_id: &StateId,
        clobbered: Vec<String>,
        opts: &MaterializeOptions<'_>,
    ) -> RepoResult<()> {
        let files = self.load_state_files_unlocked(state_id)?;
        let state_paths: HashSet<String> = files.keys().cloned().collect();

        let index: HashMap<String, IndexEntry> = read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        for path in index.keys() {
            if !state_paths.contains(path) {
                let full = self.root.join(path);
                if full.is_file() {
                    fs::remove_file(&full).map_err(|e| e.to_string())?;
                    trace::notice(format!("materialize: removed {path}"));
                }
            }
        }

        for (path, content) in &files {
            let full = self.root.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            atomic::write_atomic_text(&full, &content_to_string(content))?;
            trace::notice(format!(
                "materialize: wrote {path} ({})",
                content.display_kind()
            ));
        }

        self.sync_index_to_state(&files, state_id)?;
        trace::notice(format!(
            "materialize: state {state_id} -> {} paths on disk",
            files.len()
        ));
        self.emit_materialize_clobber_warnings(&clobbered, opts.command);
        Ok(())
    }

    pub fn checkout_state(&self, state_id: &StateId) -> RepoResult<()> {
        self.checkout_state_with_force(state_id, false)
    }

    pub fn checkout_state_with_force(&self, state_id: &StateId, force: bool) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.load_timeline_entry_unlocked(state_id)?;
        let prior_head = self.head_state_unlocked()?;
        let allow_dirty = prior_head == *state_id;
        let materialize_opts = MaterializeOptions::new("checkout")
            .force(force)
            .allow_dirty(allow_dirty);
        let clobbered = self.materialize_guard(&materialize_opts)?;
        self.materialize_state_inner(state_id, clobbered, &materialize_opts)?;
        self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
        trace::notice(format!("checkout: detached state {state_id}"));
        Ok(())
    }

    pub fn working_tree_is_clean(&self) -> RepoResult<bool> {
        let _lock = self.repo_lock()?;
        self.working_tree_is_clean_unlocked()
    }

    fn working_tree_is_clean_unlocked(&self) -> RepoResult<bool> {
        Ok(self
            .status_unlocked()?
            .entries
            .values()
            .all(|s| matches!(s, FileStatus::Unchanged)))
    }

    fn persist_state(
        &self,
        files: &HashMap<String, FileContent>,
        message: &str,
        author: &AuthorIdentity,
        parent: Option<StateId>,
        parents: Vec<StateId>,
    ) -> RepoResult<StateId> {
        let store = self.blobs();
        let mut manifest = HashMap::new();
        for (path, content) in files {
            let blob_id = store.write(content)?;
            manifest.insert(path.clone(), blob_id);
        }
        let state_id = hash_manifest(&manifest);

        write_atomic_json(
            &self
                .astvcs_dir()
                .join("states")
                .join(format!("{state_id}.json")),
            &manifest,
        )?;

        let parent_count = parents.len();
        let entry = TimelineEntry {
            id: state_id.clone(),
            parent: parent.clone(),
            parents,
            message: message.to_string(),
            timestamp: now_iso(),
            author_name: author.name.clone(),
            author_email: author.email.clone(),
            manifest: manifest.clone(),
            files: None,
        };
        write_atomic_json(
            &self
                .astvcs_dir()
                .join("timeline")
                .join(format!("{state_id}.json")),
            &entry,
        )?;
        trace::notice(format!(
            "persist: state {state_id} ({} paths, parents={parent_count})",
            manifest.len(),
        ));
        Ok(state_id)
    }

    fn migrate_inline_files(
        &self,
        files: &HashMap<String, FileContent>,
    ) -> RepoResult<HashMap<String, String>> {
        let store = self.blobs();
        let mut manifest = HashMap::new();
        for (path, content) in files {
            manifest.insert(path.clone(), store.write(content)?);
        }
        Ok(manifest)
    }

    fn sync_index_to_state(
        &self,
        files: &HashMap<String, FileContent>,
        state_id: &StateId,
    ) -> RepoResult<()> {
        let mut index: HashMap<String, IndexEntry> =
            read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        let paths: HashSet<String> = files.keys().cloned().collect();
        index.retain(|path, _| paths.contains(path));
        for (path, content) in files {
            index.insert(
                path.clone(),
                IndexEntry {
                    state_id: state_id.to_string(),
                    content_kind: content.display_kind().to_string(),
                },
            );
        }
        write_atomic_json(&self.astvcs_dir().join(INDEX_FILE), &index)?;
        Ok(())
    }

    pub(crate) fn read_head_target(&self) -> RepoResult<HeadTarget> {
        let text =
            fs::read_to_string(self.astvcs_dir().join(HEAD_FILE)).map_err(|e| e.to_string())?;
        let line = text.trim();
        if is_state_id(line) {
            Ok(HeadTarget::Detached(line.to_string()))
        } else {
            Ok(HeadTarget::Branch(line.to_string()))
        }
    }

    pub(crate) fn head_branch_unlocked(&self) -> RepoResult<Option<String>> {
        match self.read_head_target()? {
            HeadTarget::Branch(name) => Ok(Some(name)),
            HeadTarget::Detached(_) => Ok(None),
        }
    }

    pub(crate) fn is_detached_unlocked(&self) -> RepoResult<bool> {
        Ok(matches!(self.read_head_target()?, HeadTarget::Detached(_)))
    }

    pub(crate) fn head_state_unlocked(&self) -> RepoResult<StateId> {
        match self.read_head_target()? {
            HeadTarget::Branch(name) => self.read_branch_ref(&name),
            HeadTarget::Detached(id) => Ok(id),
        }
    }

    fn write_head_target(&self, target: &HeadTarget) -> RepoResult<()> {
        let line = match target {
            HeadTarget::Branch(name) => name.as_str(),
            HeadTarget::Detached(id) => id.as_str(),
        };
        write_atomic_text(&self.astvcs_dir().join(HEAD_FILE), &format!("{line}\n"))?;
        Ok(())
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    pub fn load_config(&self) -> RepoResult<RepoConfig> {
        let _lock = self.repo_lock()?;
        read_json(&self.astvcs_dir().join(CONFIG_FILE))
    }

    pub fn has_blob(&self, id: &str) -> bool {
        self.blobs().contains(&id.to_string())
    }

    pub fn has_state(&self, state_id: &StateId) -> bool {
        self.astvcs_dir()
            .join("states")
            .join(format!("{state_id}.json"))
            .is_file()
    }

    pub fn has_timeline(&self, state_id: &StateId) -> bool {
        self.astvcs_dir()
            .join("timeline")
            .join(format!("{state_id}.json"))
            .is_file()
    }

    pub fn read_blob_bytes(&self, id: &str) -> RepoResult<Vec<u8>> {
        let _lock = self.repo_lock()?;
        self.blobs()
            .read_bytes(&id.to_string())
            .map_err(RepoError::from_message)
    }

    pub fn import_blob_bytes(&self, id: &str, bytes: &[u8]) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.blobs()
            .write_bytes(&id.to_string(), bytes)
            .map_err(RepoError::from_message)
    }

    pub fn import_state_manifest(
        &self,
        state_id: &StateId,
        manifest: &HashMap<String, String>,
    ) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        if hash_manifest(manifest) != *state_id {
            return Err(RepoError::other(format!(
                "state id mismatch for {state_id}"
            )));
        }
        write_atomic_json(
            &self
                .astvcs_dir()
                .join("states")
                .join(format!("{state_id}.json")),
            manifest,
        )?;
        Ok(())
    }

    pub fn import_timeline_entry(&self, entry: &TimelineEntry) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        write_atomic_json(
            &self
                .astvcs_dir()
                .join("timeline")
                .join(format!("{}.json", entry.id)),
            entry,
        )?;
        Ok(())
    }

    pub fn write_branch_ref(&self, branch: &str, state_id: &StateId) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        self.write_branch_ref_unlocked(branch, state_id)
    }

    fn write_branch_ref_unlocked(&self, branch: &str, state_id: &StateId) -> RepoResult<()> {
        write_atomic_text(
            &self.astvcs_dir().join("refs/heads").join(branch),
            &format!("{state_id}\n"),
        )?;
        Ok(())
    }

    pub fn read_remote_ref(&self, remote: &str, branch: &str) -> RepoResult<Option<StateId>> {
        let _lock = self.repo_lock()?;
        self.read_remote_ref_unlocked(remote, branch)
    }

    pub(crate) fn read_remote_ref_unlocked(
        &self,
        remote: &str,
        branch: &str,
    ) -> RepoResult<Option<StateId>> {
        let path = self
            .astvcs_dir()
            .join("refs/remotes")
            .join(remote)
            .join(branch);
        if !path.is_file() {
            return Ok(None);
        }
        let text =
            fs::read_to_string(path).map_err(|e| RepoError::from_io("read remote ref", e))?;
        Ok(Some(text.trim().to_string()))
    }

    pub fn write_remote_ref(
        &self,
        remote: &str,
        branch: &str,
        state_id: &StateId,
    ) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let path = self
            .astvcs_dir()
            .join("refs/remotes")
            .join(remote)
            .join(branch);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| RepoError::from_io("create remote ref dir", e))?;
        }
        write_atomic_text(&path, &format!("{state_id}\n"))?;
        Ok(())
    }

    pub fn is_ancestor_of(&self, ancestor: &StateId, descendant: &StateId) -> RepoResult<bool> {
        let _lock = self.repo_lock()?;
        self.is_ancestor_of_unlocked(ancestor, descendant)
    }

    fn is_ancestor_of_unlocked(
        &self,
        ancestor: &StateId,
        descendant: &StateId,
    ) -> RepoResult<bool> {
        if ancestor == descendant {
            return Ok(true);
        }
        let anc = crate::store::history::ancestors(descendant, |id| {
            self.load_timeline_entry_unlocked(id)
                .map_err(|e| e.to_string())
        })
        .map_err(RepoError::from_message)?;
        Ok(anc.contains(ancestor))
    }
}

fn content_eq(a: &FileContent, b: &FileContent) -> bool {
    match (BlobStore::hash_content(a), BlobStore::hash_content(b)) {
        (Ok(ha), Ok(hb)) => ha == hb,
        (Err(e), _) | (_, Err(e)) => {
            trace::warn(format!("content hash failed: {e}"));
            false
        }
    }
}

fn manifest_unchanged(
    head: &HashMap<String, FileContent>,
    working: &HashMap<String, FileContent>,
) -> bool {
    if head.len() != working.len() {
        return false;
    }
    head.iter()
        .all(|(path, content)| working.get(path).is_some_and(|w| content_eq(content, w)))
}

fn is_state_id(s: &str) -> bool {
    s.len() == STATE_ID_LEN && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn content_to_string(content: &FileContent) -> String {
    match content {
        FileContent::Ast(graph) => unparse(graph),
        FileContent::Text(blob) => blob.content.clone(),
    }
}

fn content_preview(content: &FileContent) -> String {
    let text = content_to_string(content);
    if text.len() > 200 {
        format!("{}...\n", &text[..200])
    } else {
        format!("{text}\n")
    }
}

fn format_path_rename(
    rename: &crate::diff::PathRename,
    old: &FileContent,
    new: &FileContent,
) -> String {
    let label = match rename.kind {
        crate::diff::PathRenameKind::Exact => "rename",
        crate::diff::PathRenameKind::WithEdits => "rename with edits",
    };
    let mut out = format!("--- {}\n+++ {}\n({label})\n", rename.from, rename.to);
    out.push_str(&format!(
        "intents:\n  [0] {}\n",
        intent::format_intent(None, &intent::classify_path_rename(rename))
    ));
    if rename.kind == crate::diff::PathRenameKind::WithEdits {
        out.push_str(&format_mutation_diff(old, new));
    }
    out
}

fn format_mutation_diff(old: &FileContent, new: &FileContent) -> String {
    match (old, new) {
        (FileContent::Ast(o), FileContent::Ast(n)) => {
            let DiffResult { mutations } = diff_graphs(o, n);
            if mutations.is_empty() {
                "(no structural changes)\n".into()
            } else {
                let mut out = String::new();
                let intents = intent::classify_mutations(Some(o), &mutations);
                out.push_str("intents:\n");
                for (i, intent) in &intents {
                    out.push_str(&format!(
                        "  [{i}] {}\n",
                        intent::format_intent(Some(o), intent)
                    ));
                }
                if trace::is_verbose() {
                    out.push_str("mutations:\n");
                    for (i, m) in mutations.iter().enumerate() {
                        out.push_str(&format!("  [{i}] {m:?}\n"));
                    }
                }
                out
            }
        }
        (FileContent::Text(o), FileContent::Text(n)) => {
            let edits = diff_text(&o.content, &n.content);
            if edits.is_empty() {
                "(no line changes)\n".into()
            } else {
                let mut out = String::new();
                for edit in edits {
                    out.push_str(&format!("  {}\n", edit.summary()));
                }
                out
            }
        }
        _ => "(content kind changed: ast <-> text)\n".into(),
    }
}

fn format_diff(path: &str, old: &FileContent, new: &FileContent) -> RepoResult<String> {
    let mut out = format!("--- {path}\n+++ {path}\n");
    out.push_str(&format_mutation_diff(old, new));
    Ok(out)
}

fn read_working_file(root: &Path, rel: &str) -> RepoResult<String> {
    fs::read_to_string(root.join(rel)).map_err(|e| RepoError::from_io("read working file", e))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> RepoResult<T> {
    let text = fs::read_to_string(path)
        .map_err(|e| RepoError::from_io(&format!("read {}", path.display()), e))?;
    serde_json::from_str(&text)
        .map_err(|e| RepoError::other(format!("parse {}: {e}", path.display())))
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::FileContent;
    use crate::frontend::parse_rust;
    use crate::store::{MergeResolution, MergeResolveSide};
    use crate::unparser::unparse;
    use tempfile::TempDir;

    fn sample_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn commit_and_reload_uses_blobs() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let id = repo.commit("init").unwrap().state_id;
        let files = repo.load_state_files(&id).unwrap();
        assert!(files.contains_key("main.rs"));
        assert!(
            repo.blobs()
                .contains(&repo.load_manifest(&id).unwrap()["main.rs"])
        );
    }

    #[test]
    fn merge_base_across_branches() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let base_id = repo.commit("base").unwrap().state_id;
        repo.create_branch("feature", None).unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
        repo.commit("on main").unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let y = 2; }\n").unwrap();
        let feature_id = repo.commit("on feature").unwrap().state_id;
        let main_id = repo.branch_state("main").unwrap();
        let lca = merge_base(&main_id, &feature_id, |id| {
            repo.load_timeline_entry(id).map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(lca, base_id);
    }

    #[test]
    fn merge_three_way_via_blob_graphs() {
        use crate::merge::{MergeOutcome, merge_files};

        let base = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let right = parse_rust("fn foo() {\n    let x = 1;\n    let z = 2;\n}\n").unwrap();
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path());
        let b = store.write(&FileContent::Ast(base)).unwrap();
        let l = store.write(&FileContent::Ast(left)).unwrap();
        let r = store.write(&FileContent::Ast(right)).unwrap();
        let base_g = match store.read(&b).unwrap() {
            FileContent::Ast(g) => g,
            _ => panic!(),
        };
        let left_g = match store.read(&l).unwrap() {
            FileContent::Ast(g) => g,
            _ => panic!(),
        };
        let right_g = match store.read(&r).unwrap() {
            FileContent::Ast(g) => g,
            _ => panic!(),
        };
        let outcome = merge_files(
            &FileContent::Ast(base_g),
            &FileContent::Ast(left_g),
            &FileContent::Ast(right_g),
        );
        let text = match outcome {
            MergeOutcome::Merged(FileContent::Ast(g)) => unparse(&g),
            other => panic!("{other:?}"),
        };
        assert!(text.contains('y'), "{text}");
        assert!(text.contains('z'), "{text}");
    }

    #[test]
    fn rename_diff_survives_blob_roundtrip() {
        use crate::diff::diff_graphs;
        use crate::frontend::{FileContent, parse_rust};
        use crate::graph::Mutation;
        use crate::store::BlobStore;

        let base = parse_rust("fn foo() {\n    let x = 1;\n}\n").unwrap();
        let left = parse_rust("fn foo() {\n    let y = 1;\n}\n").unwrap();
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path());
        let b_id = store.write(&FileContent::Ast(base)).unwrap();
        let base_loaded = match store.read(&b_id).unwrap() {
            FileContent::Ast(g) => g,
            _ => panic!(),
        };
        let diff = diff_graphs(&base_loaded, &left);
        assert!(
            diff.mutations
                .iter()
                .any(|m| matches!(m, Mutation::RenameIdentifier { .. })),
            "mutations: {:?}",
            diff.mutations
        );
    }

    #[test]
    fn merge_materializes_working_tree() {
        let (dir, repo) = sample_repo();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n}\n",
        )
        .unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn one() -> i32 { 1 }\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("lib.rs"),
            "pub fn one() -> i32 { 1 }\npub fn two() -> i32 { 2 }\n",
        )
        .unwrap();
        repo.commit("feature lib").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let y = 1;\n}\n",
        )
        .unwrap();
        repo.commit("main rename").unwrap();

        repo.merge_branch("feature", "merge").unwrap();
        assert!(
            repo.working_tree_is_clean().unwrap(),
            "working tree should match HEAD after merge"
        );
        let lib = fs::read_to_string(dir.path().join("lib.rs")).unwrap();
        assert!(
            lib.contains("two"),
            "merged lib.rs missing feature edit: {lib}"
        );
        let main_rs = fs::read_to_string(dir.path().join("main.rs")).unwrap();
        assert!(
            main_rs.contains('y'),
            "merged main.rs missing main edit: {main_rs}"
        );
    }

    #[test]
    fn branch_merge_integration() {
        let (dir, repo) = sample_repo();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n}\n",
        )
        .unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let y = 1;\n}\n",
        )
        .unwrap();
        repo.commit("rename on main").unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n    let z = 2;\n}\n",
        )
        .unwrap();
        repo.commit("insert on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        let merged = repo.merge_branch("feature", "merge").unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        let text = match &files["main.rs"] {
            FileContent::Ast(g) => unparse(g),
            _ => panic!("expected ast"),
        };
        assert!(text.contains('y'), "merged text missing rename: {text}");
        assert!(text.contains('z'), "merged text missing insert: {text}");
    }

    #[test]
    fn merge_add_add_includes_new_file() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        fs::write(dir.path().join("lib.rs"), "fn shared() {}\n").unwrap();
        repo.commit("add lib on main").unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn shared() {}\n").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
        repo.commit("add lib and edit main on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        let merged = repo.merge_branch("feature", "merge identical add").unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        assert!(files.contains_key("lib.rs"));
        assert!(dir.path().join("lib.rs").exists());
        let main_text = match &files["main.rs"] {
            FileContent::Ast(g) => unparse(g),
            _ => panic!("expected ast"),
        };
        assert!(
            main_text.contains('x'),
            "feature main edit missing: {main_text}"
        );
    }

    #[test]
    fn merge_add_add_conflict_on_different_content() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        fs::write(dir.path().join("lib.rs"), "fn on_main() {}\n").unwrap();
        repo.commit("add lib on main").unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn on_feature() {}\n").unwrap();
        repo.commit("add lib on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        let err = repo.merge_branch("feature", "merge add/add").unwrap_err();
        assert!(
            err.contains("both branches added different content"),
            "{err}"
        );
        assert!(err.contains("lib.rs"), "{err}");
    }

    fn conflicting_rename_fixture(dir: &TempDir, repo: &Repo) {
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n}\n",
        )
        .unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let y = 1;\n}\n",
        )
        .unwrap();
        repo.commit("rename to y on main").unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let z = 1;\n}\n",
        )
        .unwrap();
        repo.commit("rename to z on feature").unwrap();
        repo.checkout_branch("main").unwrap();
    }

    #[test]
    fn plan_merge_reports_structural_conflict_without_side_effects() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);
        let head_before = repo.head_state().unwrap();

        let plan = repo.plan_merge("feature").unwrap();
        assert!(!plan.is_clean());
        assert_eq!(repo.head_state().unwrap(), head_before);
        let report = plan.format_conflicts();
        assert!(report.contains("overlapping structural edits"), "{report}");
        assert!(report.contains("overlapping edit pairs"), "{report}");
        assert!(report.contains("left (HEAD) intents from base"), "{report}");
        assert!(report.contains("rename `x`"), "{report}");
    }

    #[test]
    fn merge_branch_error_includes_overlap_detail() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);

        let err = repo
            .merge_branch("feature", "merge conflicting renames")
            .unwrap_err();
        assert!(err.contains("overlapping edit pairs"), "{err}");
        assert!(
            repo.working_tree_is_clean().unwrap(),
            "working tree should be unchanged after failed merge"
        );
    }

    #[test]
    fn merge_resolve_theirs_picks_feature_side() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);

        let merged = repo
            .merge_branch_with_resolutions(
                "feature",
                "resolve with theirs",
                &[MergeResolution {
                    path: "main.rs".into(),
                    side: MergeResolveSide::Theirs,
                }],
            )
            .unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        let text = match &files["main.rs"] {
            FileContent::Ast(g) => unparse(g),
            _ => panic!("expected ast"),
        };
        assert!(
            text.contains('z'),
            "theirs should keep feature rename: {text}"
        );
        assert!(
            !text.contains('y'),
            "theirs should not keep main rename: {text}"
        );
        assert!(
            repo.working_tree_is_clean().unwrap(),
            "working tree should match HEAD after resolved merge"
        );
    }

    #[test]
    fn merge_resolve_ours_picks_head_side() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);

        let merged = repo
            .merge_branch_with_resolutions(
                "feature",
                "resolve with ours",
                &[MergeResolution {
                    path: "main.rs".into(),
                    side: MergeResolveSide::Ours,
                }],
            )
            .unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        let text = match &files["main.rs"] {
            FileContent::Ast(g) => unparse(g),
            _ => panic!("expected ast"),
        };
        assert!(text.contains('y'), "ours should keep main rename: {text}");
        assert!(
            !text.contains('z'),
            "ours should not keep feature rename: {text}"
        );
    }

    #[test]
    fn merge_unresolved_and_partial_resolve_fail() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);
        let head_before = repo.head_state().unwrap();

        let err = repo.merge_branch("feature", "no resolution").unwrap_err();
        assert!(err.contains("overlapping"), "{err}");
        assert_eq!(repo.head_state().unwrap(), head_before);

        let err = repo
            .merge_branch_with_resolutions(
                "feature",
                "partial",
                &[MergeResolution {
                    path: "other.rs".into(),
                    side: MergeResolveSide::Theirs,
                }],
            )
            .unwrap_err();
        assert!(err.contains("path not in merge conflicts"), "{err}");
        assert_eq!(repo.head_state().unwrap(), head_before);
    }

    #[test]
    fn merge_resolve_non_conflicted_path_errors() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);

        let err = repo
            .merge_branch_with_resolutions(
                "feature",
                "bad path",
                &[MergeResolution {
                    path: "lib.rs".into(),
                    side: MergeResolveSide::Ours,
                }],
            )
            .unwrap_err();
        assert!(err.contains("path not in merge conflicts"), "{err}");
    }

    #[test]
    fn merge_add_add_resolve_theirs() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        fs::write(dir.path().join("lib.rs"), "fn on_main() {}\n").unwrap();
        repo.commit("add lib on main").unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn on_feature() {}\n").unwrap();
        repo.commit("add lib on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        let merged = repo
            .merge_branch_with_resolutions(
                "feature",
                "resolve add/add",
                &[MergeResolution {
                    path: "lib.rs".into(),
                    side: MergeResolveSide::Theirs,
                }],
            )
            .unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        let text = match &files["lib.rs"] {
            FileContent::Ast(g) => unparse(g),
            _ => panic!("expected ast"),
        };
        assert!(
            text.contains("on_feature"),
            "theirs should keep feature content: {text}"
        );
    }

    #[test]
    fn parse_merge_resolution_rejects_invalid_side_and_duplicates() {
        use crate::store::parse_merge_resolution;
        use crate::store::parse_merge_resolutions;

        let err = parse_merge_resolution("main.rs:both").unwrap_err();
        assert!(err.contains("invalid resolve side"), "{err}");

        let err = parse_merge_resolutions(&["a.rs:ours".into(), "a.rs:theirs".into()]).unwrap_err();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn prepare_merge_dry_run_with_resolution_succeeds_in_memory() {
        let (dir, repo) = sample_repo();
        conflicting_rename_fixture(&dir, &repo);
        let head_before = repo.head_state().unwrap();

        let plan = repo
            .prepare_merge(
                "feature",
                &[MergeResolution {
                    path: "main.rs".into(),
                    side: MergeResolveSide::Theirs,
                }],
            )
            .unwrap();
        assert!(plan.is_clean());
        assert_eq!(repo.head_state().unwrap(), head_before);
    }

    #[test]
    fn merge_base_refs_accepts_branch_names() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let base_id = repo.commit("base").unwrap().state_id;
        repo.create_branch("feature", None).unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
        repo.commit("on main").unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let y = 2; }\n").unwrap();
        repo.commit("on feature").unwrap();
        repo.checkout_branch("main").unwrap();
        let lca = repo.merge_base_refs("main", "feature").unwrap();
        assert_eq!(lca, base_id);
    }

    #[test]
    fn diff_three_way_shows_both_sides() {
        let (dir, repo) = sample_repo();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n}\n",
        )
        .unwrap();
        let base_id = repo.commit("base").unwrap().state_id;
        repo.create_branch("feature", None).unwrap();

        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let y = 1;\n}\n",
        )
        .unwrap();
        let main_id = repo.commit("on main").unwrap().state_id;

        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n    let z = 2;\n}\n",
        )
        .unwrap();
        let feature_id = repo.commit("on feature").unwrap().state_id;

        let out = repo
            .diff_three_way(&base_id, &main_id, &feature_id, Some("main.rs"))
            .unwrap();
        assert!(out.contains("base -> left:"), "{out}");
        assert!(out.contains("base -> right:"), "{out}");
    }

    #[test]
    fn merge_respects_deletion_on_one_branch() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        fs::remove_file(dir.path().join("lib.rs")).unwrap();
        repo.commit("delete lib on main").unwrap();

        repo.checkout_branch("feature").unwrap();
        repo.commit("feature noop").unwrap();

        repo.checkout_branch("main").unwrap();
        let merged = repo.merge_branch("feature", "merge deletion").unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        assert!(!files.contains_key("lib.rs"));
        assert!(!dir.path().join("lib.rs").exists());
    }

    #[test]
    fn checkout_state_detached_status_matches_disk() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
        repo.commit("v2").unwrap();

        repo.checkout_state(&v1).unwrap();
        assert!(repo.is_detached().unwrap());
        assert_eq!(repo.head_state().unwrap(), v1);
        assert!(repo.working_tree_is_clean().unwrap());

        fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        let v3 = repo.commit("v3 from detached").unwrap().state_id;
        let entry = repo.load_timeline_entry(&v3).unwrap();
        assert_eq!(entry.parent.as_deref(), Some(v1.as_str()));
        assert!(repo.is_detached().unwrap());
        assert_eq!(repo.head_state().unwrap(), v3);
    }

    #[test]
    fn empty_commit_is_idempotent() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let id = repo.commit("first").unwrap().state_id;
        let again = repo.commit("duplicate").unwrap();
        assert!(!again.created);
        assert_eq!(again.state_id, id);
        let entry = repo.load_timeline_entry(&id).unwrap();
        assert_eq!(entry.message, "first");
        assert!(entry.parent.is_none() || entry.parent.as_deref() != Some(id.as_str()));
    }

    #[test]
    fn reset_hard_moves_tip_and_materializes() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        repo.commit("v2").unwrap();

        repo.reset(&v1, false, false).unwrap();
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn reset_hard_repairs_drift_at_same_tip() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "drifted\n").unwrap();

        repo.reset(&v1, false, false).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
    }

    #[test]
    fn reset_soft_preserves_dirty_working_tree() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        repo.commit("v2").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();

        repo.reset(&v1, true, false).unwrap();
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
        let status = repo.status().unwrap();
        assert_eq!(status.entries.get("note.txt"), Some(&FileStatus::Modified));
    }

    #[test]
    fn reset_hard_refuses_dirty_tree_without_force() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        repo.commit("v2").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        let tip_before = repo.head_state().unwrap();

        let err = repo.reset(&v1, false, false).unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(repo.head_state().unwrap(), tip_before);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[test]
    fn reset_hard_force_warns_and_clobbers_dirty_paths() {
        trace::clear_log();
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        repo.commit("v2").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();

        repo.reset(&v1, false, true).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
        let log = trace::take_log();
        assert!(
            log.iter()
                .any(|l| l
                    .contains("warning: reset --force: discarded uncommitted changes in note.txt")),
            "{log:?}"
        );
    }

    #[test]
    fn reset_detached_hard_and_soft() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2").unwrap().state_id;
        repo.checkout_state(&v2).unwrap();

        repo.reset(&v1, true, false).unwrap();
        assert!(repo.is_detached().unwrap());
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v2\n"
        );

        repo.reset(&v2, false, true).unwrap();
        assert_eq!(repo.head_state().unwrap(), v2);
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn reset_to_root_empty_state() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        let root = StateId::from("0".repeat(64));

        repo.reset(&root, false, false).unwrap();
        assert_eq!(repo.head_state().unwrap(), root);
        assert!(!dir.path().join("note.txt").exists());
    }

    #[test]
    fn reset_unknown_ref_errors_without_writes() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;

        let err = repo.reset("missing-branch", false, false).unwrap_err();
        assert!(err.contains("unknown"), "{err}");
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
    }

    #[test]
    fn resolve_state_ref_remote_tracking() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        repo.write_remote_ref("origin", "main", &v1).unwrap();

        assert_eq!(repo.resolve_state_ref("origin/main").unwrap(), v1);
    }

    #[test]
    fn revert_add_then_modify_conflicts() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("keep.txt"), "stay\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "seed\n").unwrap();
        repo.commit("seed").unwrap();
        fs::remove_file(dir.path().join("notes.txt")).unwrap();
        repo.commit("remove notes").unwrap();
        let target = {
            fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
            repo.commit("add notes").unwrap().state_id
        };
        fs::write(dir.path().join("notes.txt"), "added later\n").unwrap();
        repo.commit("modify notes").unwrap();

        let err = repo.revert_state(&target, "revert add").unwrap_err();
        assert!(
            err.contains("path modified after the reverted state"),
            "{err}"
        );
        assert!(dir.path().join("notes.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "added later\n"
        );
    }

    #[test]
    fn revert_harmless_add_removes_file() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("keep.txt"), "stay\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "seed\n").unwrap();
        repo.commit("seed").unwrap();
        fs::remove_file(dir.path().join("notes.txt")).unwrap();
        repo.commit("remove notes").unwrap();
        let target = {
            fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
            repo.commit("add notes").unwrap().state_id
        };

        let outcome = repo.revert_state(&target, "revert add").unwrap();
        assert!(outcome.created);
        assert!(!dir.path().join("notes.txt").exists());
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn revert_of_revert_restores_content() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        fs::write(dir.path().join("extra.txt"), "extra\n").unwrap();
        let v2 = repo.commit("v2 add extra").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v3\n").unwrap();
        let v3 = repo.commit("v3").unwrap().state_id;
        let v4 = repo.revert_state(&v2, "revert extra add").unwrap().state_id;
        let outcome = repo.revert_state(&v4, "revert the revert").unwrap();

        assert_eq!(outcome.state_id, v3);
        assert_eq!(repo.head_state().unwrap(), v3);
        let files = repo.load_state_files(&v3).unwrap();
        assert!(files.contains_key("extra.txt"));
        assert_eq!(
            match &files["note.txt"] {
                FileContent::Text(t) => t.content.as_str(),
                _ => panic!(),
            },
            "v3\n"
        );
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn revert_head_tip_undoes_last_commit() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2").unwrap().state_id;

        let outcome = repo.revert_state(&v2, "undo v2").unwrap();
        assert!(outcome.created);
        let files = repo.load_state_files(&outcome.state_id).unwrap();
        assert_eq!(
            match &files["note.txt"] {
                FileContent::Text(t) => t.content.as_str(),
                _ => panic!(),
            },
            "v1\n"
        );
        assert!(repo.working_tree_is_clean().unwrap());
        assert_eq!(outcome.state_id, v1);
    }

    #[test]
    fn revert_older_commit_on_linear_history() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        fs::write(dir.path().join("extra.txt"), "extra\n").unwrap();
        let v2 = repo.commit("v2 add extra").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v3\n").unwrap();
        repo.commit("v3").unwrap();

        let outcome = repo.revert_state(&v2, "revert extra add").unwrap();
        let files = repo.load_state_files(&outcome.state_id).unwrap();
        assert!(!files.contains_key("extra.txt"));
        assert_eq!(
            match &files["note.txt"] {
                FileContent::Text(t) => t.content.as_str(),
                _ => panic!(),
            },
            "v3\n"
        );
        assert!(!dir.path().join("extra.txt").exists());
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn revert_merge_state_errors() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
        repo.commit("main edit").unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn lib() {}\n").unwrap();
        repo.commit("feature add").unwrap();
        repo.checkout_branch("main").unwrap();
        let merge_id = repo.merge_branch("feature", "merge").unwrap();

        let err = repo.revert_state(&merge_id, "revert merge").unwrap_err();
        assert!(err.contains("cannot revert merge state"), "{err}");
    }

    #[test]
    fn revert_non_ancestor_errors() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "base\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("note.txt"), "feature\n").unwrap();
        let feature_id = repo.commit("feature").unwrap().state_id;

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("note.txt"), "main\n").unwrap();
        repo.commit("main").unwrap();

        let err = repo
            .revert_state(&feature_id, "revert foreign")
            .unwrap_err();
        assert!(err.contains("not an ancestor"), "{err}");
    }

    #[test]
    fn revert_noop_when_manifest_unchanged() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("back to v1 content").unwrap();
        let tip = repo.head_state().unwrap();

        let outcome = repo.revert_state(&v2, "noop revert").unwrap();
        assert!(!outcome.created);
        assert_eq!(outcome.state_id, tip);
    }

    #[test]
    fn revert_noop_with_dirty_working_tree_skips_materialize_guard() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("back to v1 content").unwrap();
        let tip = repo.head_state().unwrap();

        fs::write(dir.path().join("scratch.txt"), "dirty\n").unwrap();
        assert!(!repo.working_tree_is_clean().unwrap());

        let outcome = repo.revert_state(&v2, "noop revert").unwrap();
        assert!(!outcome.created);
        assert_eq!(outcome.state_id, tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("scratch.txt")).unwrap(),
            "dirty\n",
            "no-op revert must not materialize or clobber uncommitted files"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
    }

    #[test]
    fn revert_ast_prepended_comment() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("lib.rs"), "pub fn one() {}\n").unwrap();
        repo.commit("baseline").unwrap();
        let with_doc = "//! crate doc\npub fn one() {}\n";
        fs::write(dir.path().join("lib.rs"), with_doc).unwrap();
        let target = repo.commit("prepend doc").unwrap().state_id;

        repo.revert_state(&target, "revert doc").unwrap();
        let head = repo.head_state().unwrap();
        let files = repo.load_state_files(&head).unwrap();
        let text = match &files["lib.rs"] {
            FileContent::Ast(g) => unparse(g),
            _ => panic!("expected ast"),
        };
        assert_eq!(text, "pub fn one() {}\n");
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn revert_after_head_matches_target_add_removes_file() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("keep.txt"), "stay\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "seed\n").unwrap();
        repo.commit("seed").unwrap();
        fs::remove_file(dir.path().join("notes.txt")).unwrap();
        repo.commit("remove notes").unwrap();
        let target = {
            fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
            repo.commit("add notes").unwrap().state_id
        };
        fs::write(dir.path().join("notes.txt"), "added later\n").unwrap();
        repo.commit("modify notes").unwrap();
        fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
        repo.reset(&target, true, false).unwrap();

        repo.revert_state(&target, "revert add").unwrap();
        assert!(!dir.path().join("notes.txt").exists());
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn revert_conflict_aborts_without_side_effects() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("keep.txt"), "stay\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "seed\n").unwrap();
        repo.commit("seed").unwrap();
        fs::remove_file(dir.path().join("notes.txt")).unwrap();
        repo.commit("remove notes").unwrap();
        let target = {
            fs::write(dir.path().join("notes.txt"), "added\n").unwrap();
            repo.commit("add notes").unwrap().state_id
        };
        fs::write(dir.path().join("notes.txt"), "added later\n").unwrap();
        let tip = repo.commit("modify notes").unwrap().state_id;

        let err = repo.revert_state(&target, "revert add").unwrap_err();
        assert!(
            err.contains("path modified after the reverted state"),
            "{err}"
        );
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "added later\n"
        );
    }

    #[test]
    fn remove_branch_guardrails() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("baseline").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.create_branch("archive", None).unwrap();

        assert!(repo.remove_branch("main").is_err());
        repo.remove_branch("feature").unwrap();
        assert!(!dir.path().join(".astvcs/refs/heads/feature").exists());

        repo.checkout_branch("archive").unwrap();
        repo.remove_branch("main").unwrap();
        assert!(repo.remove_branch("archive").is_err());

        let solo = TempDir::new().unwrap();
        let solo_repo = Repo::init_with_identity(solo.path()).unwrap();
        fs::write(solo.path().join("note.txt"), "solo\n").unwrap();
        solo_repo.commit("solo").unwrap();
        assert!(solo_repo.remove_branch("main").is_err());
    }

    #[test]
    fn remove_branch_not_found() {
        let (dir, repo) = sample_repo();
        let err = repo.remove_branch("missing").unwrap_err();
        assert!(err.contains("branch not found"), "{err}");
        assert!(!dir.path().join(".astvcs/refs/heads/missing").exists());
    }

    #[test]
    fn remove_branch_then_recreate_same_name() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("baseline").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.checkout_branch("main").unwrap();
        repo.remove_branch("feature").unwrap();
        repo.create_branch("feature", Some("main")).unwrap();
        assert_eq!(
            repo.branch_state("feature").unwrap(),
            repo.branch_state("main").unwrap()
        );
    }

    #[test]
    fn revert_trailing_comment_commit() {
        let (dir, repo) = sample_repo();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"a\");\n}\n",
        )
        .unwrap();
        repo.commit("baseline").unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"a\"); // note\n}\n",
        )
        .unwrap();
        let with_comment = repo.commit("add comment").unwrap().state_id;
        repo.revert_state(&with_comment, "drop comment").unwrap();
        let text = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(
            !text.contains("// note"),
            "revert should remove trailing comment: {text}"
        );
        assert!(text.contains("println!(\"a\")"), "{text}");
    }

    #[test]
    fn revert_comment_after_disjoint_literal_merge() {
        let (dir, repo) = sample_repo();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"Hello, World!\");\n}\n",
        )
        .unwrap();
        repo.commit("baseline").unwrap();

        repo.create_branch("feature", None).unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"Hello, World!\"); // waddup fool\n}\n",
        )
        .unwrap();
        let feature_tip = repo.commit("add comment").unwrap().state_id;

        repo.checkout_branch("main").unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"sup?\");\n}\n",
        )
        .unwrap();
        repo.commit("change literal").unwrap();
        repo.merge_branch("feature", "merge comment and literal")
            .unwrap();

        repo.revert_state(&feature_tip, "drop merged comment")
            .unwrap();
        let text = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(
            text.contains("sup?") && !text.contains("waddup fool") && !text.contains("//"),
            "revert after merge should keep literal and drop comment: {text}"
        );
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn merge_refuses_dirty_tree_when_merge_is_clean() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn foo() { let x = 1; }\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn feature() -> i32 { 1 }\n").unwrap();
        repo.commit("feature lib").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        let tip = repo.head_state().unwrap();

        let err = repo.merge_branch("feature", "merge").unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
        assert!(!dir.path().join("lib.rs").exists());
    }

    #[test]
    fn merge_dirty_tree_preserved_when_merge_would_conflict() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn foo() { let x = 1; }\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("main.rs"), "fn foo() { let y = 1; }\n").unwrap();
        repo.commit("feature rename").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("main.rs"), "fn foo() { let z = 1; }\n").unwrap();
        repo.commit("main rename").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        let tip = repo.head_state().unwrap();

        let err = repo.merge_branch("feature", "merge").unwrap_err();
        assert!(
            err.contains("conflict") || err.contains("uncommitted changes"),
            "{err}"
        );
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[test]
    fn merge_force_warns_and_clobbers_dirty_paths() {
        trace::clear_log();
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn foo() { let x = 1; }\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn two() -> i32 { 2 }\n").unwrap();
        repo.commit("feature lib").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();

        repo.merge_branch_with_resolutions_force("feature", "merge", &[], true)
            .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            "pub fn two() -> i32 { 2 }\n"
        );
        let log = trace::take_log();
        assert!(
            log.iter()
                .any(|l| l
                    .contains("warning: merge --force: discarded uncommitted changes in note.txt")),
            "{log:?}"
        );
    }

    #[test]
    fn merge_force_on_dirty_overlapping_path_applies_committed_plan() {
        trace::clear_log();
        let (dir, repo) = sample_repo();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n}\n",
        )
        .unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n    let z = 2;\n}\n",
        )
        .unwrap();
        repo.commit("insert on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let y = 1;\n}\n",
        )
        .unwrap();
        repo.commit("rename on main").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let dirty = 99;\n}\n",
        )
        .unwrap();

        repo.merge_branch_with_resolutions_force("feature", "merge", &[], true)
            .unwrap();
        let main_rs = fs::read_to_string(dir.path().join("main.rs")).unwrap();
        assert!(
            main_rs.contains('y'),
            "merged main.rs missing main edit: {main_rs}"
        );
        assert!(
            main_rs.contains('z'),
            "merged main.rs missing feature insert: {main_rs}"
        );
        assert!(
            !main_rs.contains("dirty"),
            "uncommitted edit must not affect merge plan or final content: {main_rs}"
        );
        let log = trace::take_log();
        assert!(
            log.iter()
                .any(|l| l
                    .contains("warning: merge --force: discarded uncommitted changes in main.rs")),
            "{log:?}"
        );
    }

    #[test]
    fn checkout_branch_refuses_dirty_tree_when_switching_branches() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
        repo.commit("feature").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        let tip = repo.head_state().unwrap();

        let err = repo
            .checkout_branch_with_force("feature", false)
            .unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(repo.head_branch().unwrap(), Some("main".into()));
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[test]
    fn checkout_state_refuses_dirty_tree() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        let tip = repo.head_state().unwrap();

        let err = repo.checkout_state_with_force(&v1, false).unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );

        repo.checkout_state_with_force(&v1, true).unwrap();
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
        assert_ne!(v2, v1);
    }

    #[test]
    fn checkout_same_branch_repairs_drift_without_force() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        fs::write(dir.path().join("note.txt"), "drifted\n").unwrap();

        repo.checkout_branch("main").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v1\n"
        );
    }

    #[test]
    fn revert_refuses_dirty_tree() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        let tip = repo.head_state().unwrap();

        let err = repo
            .revert_state_with_force(&v2, "undo v2", false)
            .unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[test]
    fn stray_temp_file_cleaned_on_next_locked_command() {
        use crate::store::atomic::TEMP_SUFFIX;

        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        let canonical = dir.path().join("main.rs");
        let stray = dir.path().join(format!("main.rs{TEMP_SUFFIX}"));
        fs::write(&stray, "partial\n").unwrap();
        fs::write(&canonical, "fn main() { }\n").unwrap();
        repo.commit("after stray").unwrap();
        assert!(!stray.exists());
        assert_eq!(fs::read_to_string(&canonical).unwrap(), "fn main() { }\n");
    }

    #[test]
    fn concurrent_repo_lock_fails_fast_with_actionable_error() {
        use crate::store::lock::RepoLockGuard;
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let astvcs = Arc::new(dir.path().join(".astvcs"));
        let barrier = Arc::new(Barrier::new(2));

        let astvcs_a = Arc::clone(&astvcs);
        let barrier_a = Arc::clone(&barrier);
        let holder = thread::spawn(move || {
            let _guard = RepoLockGuard::acquire(&astvcs_a).unwrap();
            barrier_a.wait();
            thread::sleep(std::time::Duration::from_millis(100));
        });

        barrier.wait();
        let err = repo.commit("blocked").unwrap_err();
        assert!(
            err.contains("repository is locked by another process"),
            "{err}"
        );
        assert!(err.contains("repo.lock"), "{err}");
        holder.join().unwrap();

        repo.commit("after release").unwrap();
    }

    #[test]
    fn merge_conflict_still_leaves_refs_and_disk_unchanged_under_lock() {
        let (dir, repo) = sample_repo();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let x = 1;\n}\n",
        )
        .unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let y = 1;\n}\n",
        )
        .unwrap();
        repo.commit("rename on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "fn foo() {\n    let z = 1;\n}\n",
        )
        .unwrap();
        repo.commit("rename on main").unwrap();
        let tip = repo.head_state().unwrap();
        let disk_before = fs::read_to_string(dir.path().join("main.rs")).unwrap();

        let err = repo.merge_branch("feature", "merge").unwrap_err();
        assert!(err.contains("merge would conflict"), "{err}");
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("main.rs")).unwrap(),
            disk_before
        );
    }

    #[test]
    fn path_rename_exact_reports_rename_intent_in_diff() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("old.rs"), "fn foo() {}\n").unwrap();
        repo.commit("add old").unwrap();
        fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
        let diff = repo.diff_working("new.rs").unwrap();
        assert!(diff.contains("(rename)"), "{diff}");
        assert!(diff.contains("rename path `old.rs` -> `new.rs`"), "{diff}");
        assert!(!diff.contains("(deleted)"), "{diff}");
    }

    #[test]
    fn path_rename_with_edits_reports_rename_with_edits() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("a.rs"), "fn foo() { 1 }\n").unwrap();
        repo.commit("add a").unwrap();
        fs::remove_file(dir.path().join("a.rs")).unwrap();
        fs::write(dir.path().join("b.rs"), "fn foo() { 2 }\n").unwrap();
        let diff = repo.diff_working("b.rs").unwrap();
        assert!(diff.contains("(rename with edits)"), "{diff}");
        assert!(
            diff.contains("EditLiteral") || diff.contains("edit literal"),
            "{diff}"
        );
    }

    #[test]
    fn path_rename_merges_with_independent_content_edit() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::remove_file(dir.path().join("lib.rs")).unwrap();
        fs::write(dir.path().join("renamed.rs"), "fn foo() {}\n").unwrap();
        repo.commit("rename on feature").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("lib.rs"), "fn foo() { 42 }\n").unwrap();
        repo.commit("edit on main").unwrap();

        let merged = repo.merge_branch("feature", "merge rename + edit").unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        let text = match files.get("renamed.rs").unwrap() {
            FileContent::Ast(g) => unparse(g),
            FileContent::Text(t) => t.content.clone(),
        };
        assert!(
            text.contains("42"),
            "merged rename path should keep main edit: {text}"
        );
        assert!(!files.contains_key("lib.rs"));
    }

    #[test]
    fn conflicting_path_renames_report_conflict() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("base.rs"), "fn foo() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::remove_file(dir.path().join("base.rs")).unwrap();
        fs::write(dir.path().join("left.rs"), "fn foo() {}\n").unwrap();
        repo.commit("rename to left").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::remove_file(dir.path().join("base.rs")).unwrap();
        fs::write(dir.path().join("right.rs"), "fn foo() {}\n").unwrap();
        repo.commit("rename to right").unwrap();

        let err = repo.merge_branch("feature", "merge").unwrap_err();
        assert!(err.contains("both branches renamed base.rs"), "{err}");
    }

    #[test]
    fn path_rename_conflicts_with_independent_add_at_destination() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("src.txt"), "base content\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::remove_file(dir.path().join("src.txt")).unwrap();
        fs::write(dir.path().join("dst.txt"), "base content\n").unwrap();
        repo.commit("rename to dst").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::write(dir.path().join("dst.txt"), "other content\n").unwrap();
        repo.commit("add different at dst while keeping src")
            .unwrap();

        let err = repo.merge_branch("feature", "merge").unwrap_err();
        assert!(err.contains("path rename to dst.txt conflicts"), "{err}");
    }

    #[test]
    fn path_rename_with_replace_at_destination_merges_edit() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("src.txt"), "base content\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("feature", None).unwrap();

        repo.checkout_branch("feature").unwrap();
        fs::remove_file(dir.path().join("src.txt")).unwrap();
        fs::write(dir.path().join("dst.txt"), "base content\n").unwrap();
        repo.commit("rename to dst").unwrap();

        repo.checkout_branch("main").unwrap();
        fs::remove_file(dir.path().join("src.txt")).unwrap();
        fs::write(dir.path().join("dst.txt"), "other content\n").unwrap();
        repo.commit("replace with dst").unwrap();

        let merged = repo.merge_branch("feature", "merge").unwrap();
        let files = repo.load_state_files(&merged).unwrap();
        let text = match files.get("dst.txt").unwrap() {
            FileContent::Text(t) => t.content.clone(),
            _ => panic!(),
        };
        assert_eq!(text, "other content\n");
        assert!(!files.contains_key("src.txt"));
    }
}
