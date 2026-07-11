use crate::diff::{
    DiffResult, DiffViewDocument, DiffViewFile, DiffViewGroup, build_rename_map,
    detect_path_renames, diff_graphs, diff_text, file_from_contents, file_from_rename,
    rename_targets_conflict, side_path_for_base,
};
use crate::frontend::{FileContent, path_has_text_fallback};
use crate::intent;
use crate::merge::{
    ConflictResolutionStyle, MergeConflict, PathMergeConflict, PathMergeTrackedOutcome,
    merge_tracked_path,
};
use crate::store::atomic::{self, write_atomic_json, write_atomic_text};
use crate::store::blobs::BlobStore;
use crate::store::error::{RepoError, RepoResult};
use crate::store::history::{merge_base_checked, walk_history};
use crate::store::identity::{AuthorIdentity, resolve_author_identity};
use crate::store::lock::{self, RepoLockGuard};
use crate::store::manifest::{FileMode, ManifestEntry, ManifestMap, hash_commit, hash_manifest};
use crate::store::merge_resolve::{MergeResolution, apply_merge_resolutions};
use crate::store::scan_cache::{self, ScanCache};
use crate::store::staging::{
    STAGING_FILE, StagedEntry, StagingIndex, clear_staging_entries, load_staging, save_staging,
    staged_to_tracked,
};
use crate::store::tracked::{TrackedFile, tracked_eq};
use crate::store::working::load_working_tracked;
use crate::trace;
use crate::unparser::unparse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub type StateId = String;

use crate::store::walk::{self, ASTVCS_DIR};

const HEAD_FILE: &str = "HEAD";
const INDEX_FILE: &str = "index.json";
pub(crate) const CONFIG_FILE: &str = "config.json";
const STATE_ID_LEN: usize = 64;

/// Options for working-tree scan used by `status` and `commit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanOptions {
    /// Force a complete directory walk instead of an incremental scan.
    pub full_scan: bool,
}

/// Options for `commit_with_options`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitOptions {
    pub scan: ScanOptions,
    /// Skip client hooks (`pre-commit`, `commit-msg`).
    pub no_verify: bool,
    /// Restrict legacy whole-tree commit to these paths (HEAD paths absent from the set are removed).
    pub only_paths: Option<HashSet<String>>,
}

impl ScanOptions {
    fn effective_full_scan(self) -> bool {
        self.full_scan || trace::is_verbose()
    }
}

pub(crate) const WORKING_TREE_DIRTY_ERR: &str =
    "working tree has uncommitted changes; commit or pass --force";

const RESET_WORKING_TREE_DIRTY_ERR: &str =
    "working tree has uncommitted changes; commit, soft reset, or pass --force";

/// Dirty-tree policy applied before writing a state manifest to disk.
pub(crate) struct MaterializeOptions<'a> {
    force: bool,
    command: &'a str,
    /// Overwrite disk when HEAD already matches `state_id` (repair drift without `--force`).
    allow_dirty: bool,
    refuse_message: &'a str,
}

impl<'a> MaterializeOptions<'a> {
    pub(crate) fn new(command: &'a str) -> Self {
        Self {
            force: false,
            command,
            allow_dirty: false,
            refuse_message: WORKING_TREE_DIRTY_ERR,
        }
    }

    pub(crate) fn force(mut self, force: bool) -> Self {
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
    /// Legacy `config.json` schema revision (currently `2` on init).
    pub version: u32,
    pub default_branch: String,
    /// On-disk repository layout version; absent or `0` means pre-format-versioning.
    #[serde(default)]
    pub format_version: u32,
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
    #[serde(
        default,
        deserialize_with = "crate::store::manifest::deserialize_manifest_map"
    )]
    pub manifest: ManifestMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<HashMap<String, FileContent>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinearParentError {
    MergeCommit(StateId),
    NoParent(StateId),
}

pub(crate) fn linear_timeline_parent(entry: &TimelineEntry) -> Result<StateId, LinearParentError> {
    if entry.parents.len() > 1 {
        return Err(LinearParentError::MergeCommit(entry.id.clone()));
    }
    entry
        .parent
        .clone()
        .or_else(|| entry.parents.first().cloned())
        .ok_or(LinearParentError::NoParent(entry.id.clone()))
}

#[derive(Clone, Debug)]
pub struct BranchInfo {
    pub name: String,
    pub state_id: StateId,
}

/// One column of git-style status (`M`, `A`, `D`, or clean).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChangeColumn {
    #[default]
    Clean,
    Modified,
    Added,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    pub staged: ChangeColumn,
    pub unstaged: ChangeColumn,
    pub staged_rename_from: Option<String>,
    pub unstaged_rename_from: Option<String>,
    pub untracked: bool,
}

impl FileStatus {
    pub fn is_clean(&self) -> bool {
        matches!(self.staged, ChangeColumn::Clean)
            && matches!(self.unstaged, ChangeColumn::Clean)
            && !self.untracked
    }

    pub fn unstaged_modified() -> Self {
        Self {
            staged: ChangeColumn::Clean,
            unstaged: ChangeColumn::Modified,
            staged_rename_from: None,
            unstaged_rename_from: None,
            untracked: false,
        }
    }

    pub fn unstaged_added() -> Self {
        Self {
            staged: ChangeColumn::Clean,
            unstaged: ChangeColumn::Added,
            staged_rename_from: None,
            unstaged_rename_from: None,
            untracked: false,
        }
    }

    pub fn unstaged_removed() -> Self {
        Self {
            staged: ChangeColumn::Clean,
            unstaged: ChangeColumn::Deleted,
            staged_rename_from: None,
            unstaged_rename_from: None,
            untracked: false,
        }
    }

    pub fn unstaged_renamed(from: String) -> Self {
        Self {
            staged: ChangeColumn::Clean,
            unstaged: ChangeColumn::Added,
            staged_rename_from: None,
            unstaged_rename_from: Some(from),
            untracked: false,
        }
    }

    pub fn untracked() -> Self {
        Self {
            staged: ChangeColumn::Clean,
            unstaged: ChangeColumn::Clean,
            staged_rename_from: None,
            unstaged_rename_from: None,
            untracked: true,
        }
    }
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.untracked {
            return write!(f, "untracked");
        }
        if let Some(from) = &self.staged_rename_from {
            return write!(f, "staged rename from {from}");
        }
        if let Some(from) = &self.unstaged_rename_from {
            return write!(f, "renamed from {from}");
        }
        match (self.staged, self.unstaged) {
            (ChangeColumn::Clean, ChangeColumn::Clean) => write!(f, "unchanged"),
            (s, u) => write!(f, "staged={s:?} unstaged={u:?}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkingStatus {
    pub entries: HashMap<String, FileStatus>,
    /// Paths where AST-capable sources are stored as text blobs on either side.
    pub text_fallback_paths: HashSet<String>,
}

/// Result of simulating a merge without writing refs or the working tree.
#[derive(Clone, Debug)]
pub struct MergePlan {
    pub base_id: StateId,
    pub head_id: StateId,
    pub other_id: StateId,
    pub merged_files: HashMap<String, TrackedFile>,
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

    pub fn format_conflicts_focused(&self) -> String {
        self.format_conflicts_focused_for("merge", "ours", "theirs", ConflictResolutionStyle::Merge)
    }

    pub fn format_rebase_conflicts_focused(&self) -> String {
        self.format_conflicts_focused_for(
            "rebase",
            "ours",
            "theirs",
            ConflictResolutionStyle::RebaseContinue,
        )
    }

    pub fn format_conflicts_focused_for(
        &self,
        operation: &str,
        left_label: &str,
        right_label: &str,
        resolution: ConflictResolutionStyle,
    ) -> String {
        let mut out = format!(
            "{operation} would conflict in {} path(s)\n",
            self.conflicts.len()
        );
        for conflict in &self.conflicts {
            out.push_str(&conflict.detail.format_focused_report_with_labels(
                &conflict.path,
                left_label,
                right_label,
                resolution,
            ));
        }
        out.push_str("use --details for state IDs, mutations, and all overlap diagnostics\n");
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
    pub reverted_files: HashMap<String, TrackedFile>,
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
            out.push_str(&conflict.detail.format_report_with_labels(
                &conflict.path,
                "reverted parent",
                "current HEAD",
            ));
        }
        out
    }

    pub fn format_conflicts_focused(&self) -> String {
        let mut out = format!(
            "revert would conflict in {} path(s)\n",
            self.conflicts.len()
        );
        for conflict in &self.conflicts {
            out.push_str(&conflict.detail.format_focused_report_with_labels(
                &conflict.path,
                "reverted parent",
                "current HEAD",
                ConflictResolutionStyle::None,
            ));
        }
        out.push_str("use --details for state IDs, mutations, and all overlap diagnostics\n");
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
            super::format::ensure_format_current(self)?;
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
        fs::create_dir_all(astvcs.join("refs/tags")).map_err(|e| RepoError::from_io("init", e))?;
        fs::create_dir_all(astvcs.join("hooks")).map_err(|e| RepoError::from_io("init", e))?;
        fs::create_dir_all(astvcs.join("states")).map_err(|e| RepoError::from_io("init", e))?;
        fs::create_dir_all(astvcs.join("timeline")).map_err(|e| RepoError::from_io("init", e))?;
        BlobStore::new(&astvcs).ensure_dirs()?;

        write_atomic_json(
            &astvcs.join(CONFIG_FILE),
            &RepoConfig {
                version: 2,
                default_branch: "main".into(),
                format_version: super::format::CURRENT_FORMAT_VERSION,
            },
        )?;

        let empty_state = StateId::from("0".repeat(64));
        write_atomic_text(&astvcs.join(HEAD_FILE), "main\n")?;
        write_atomic_text(&astvcs.join("refs/heads/main"), &format!("{empty_state}\n"))?;
        write_atomic_json(
            &astvcs.join(INDEX_FILE),
            &HashMap::<String, IndexEntry>::new(),
        )?;
        write_atomic_json(&astvcs.join(STAGING_FILE), &StagingIndex::default())?;

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

    pub(crate) fn scan_working(
        &self,
        head: &StateId,
        opts: ScanOptions,
    ) -> RepoResult<(HashSet<String>, ScanCache, walk::ScanMetrics)> {
        let full_scan = opts.effective_full_scan();
        let prior = scan_cache::load_scan_cache(&self.astvcs_dir())
            .map_err(RepoError::from_message)?
            .filter(|c| c.is_valid_for(head));

        let index: HashMap<String, IndexEntry> = read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        let index_paths: HashSet<String> = index.keys().cloned().collect();

        let (mut report, mut cache) =
            walk::scan_working_with_cache(&self.root, prior.as_ref(), full_scan, head)
                .map_err(RepoError::from_message)?;

        if let Some(prior_cache) = prior {
            cache.verified = prior_cache.verified;
        }

        let mut metrics = walk::last_scan_metrics();
        walk::merge_index_paths_into_scan(
            &self.root,
            &index_paths,
            &mut report,
            &mut cache,
            &mut metrics,
        )
        .map_err(RepoError::from_message)?;

        for skip in &report.skipped {
            trace::warn(format!("scan: skipped {} ({})", skip.path, skip.reason));
        }
        let mode = match metrics.mode {
            Some(walk::ScanMode::Full) => "full",
            Some(walk::ScanMode::Incremental) => "incremental",
            None => "unknown",
        };
        trace::notice(format!(
            "scan: {} tracked, {} skipped ({mode}; stat {} reused {})",
            report.files.len(),
            report.skipped.len(),
            metrics.paths_statted,
            metrics.paths_reused,
        ));

        scan_cache::save_scan_cache(&self.astvcs_dir(), &cache).map_err(RepoError::from_message)?;

        Ok((report.files, cache, metrics))
    }

    fn check_index_consistency(
        &self,
        head: &StateId,
        head_files: &HashMap<String, TrackedFile>,
        index: &HashMap<String, IndexEntry>,
    ) {
        for (path, entry) in index {
            if !head_files.contains_key(path) {
                trace::warn(format!(
                    "index: {path} tracked in index but absent from HEAD state {head}"
                ));
            } else if entry.state_id != *head {
                trace::notice(format!(
                    "index: {path} state_id {} differs from HEAD {head}",
                    entry.state_id
                ));
            } else if let Some(tracked) = head_files.get(path) {
                let kind = index_content_kind(tracked);
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

    pub(crate) fn blobs(&self) -> BlobStore {
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

    fn branch_ref_exists_unlocked(&self, name: &str) -> bool {
        self.astvcs_dir().join("refs/heads").join(name).is_file()
    }

    pub(crate) fn update_config_unlocked<F>(&self, f: F) -> RepoResult<()>
    where
        F: FnOnce(&mut RepoConfig) -> RepoResult<()>,
    {
        let path = self.astvcs_dir().join(CONFIG_FILE);
        let mut config = read_json_unlocked(&path)?;
        f(&mut config)?;
        write_atomic_json(&path, &config)?;
        Ok(())
    }

    fn sync_default_branch_config_unlocked(&self) -> RepoResult<()> {
        let config: RepoConfig = read_json_unlocked(&self.astvcs_dir().join(CONFIG_FILE))?;
        if self.branch_ref_exists_unlocked(&config.default_branch) {
            return Ok(());
        }
        let branches = self.list_branches_unlocked()?;
        if let Some(name) = pick_default_branch(&branches) {
            self.update_config_unlocked(|c| {
                c.default_branch = name;
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn create_branch(&self, name: &str, from: Option<&str>) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if ref_path.exists() {
            return Err(RepoError::already_exists(format!(
                "branch already exists: {name}"
            )));
        }
        let default_stale = {
            let config: RepoConfig = read_json_unlocked(&self.astvcs_dir().join(CONFIG_FILE))?;
            !self.branch_ref_exists_unlocked(&config.default_branch)
        };
        let state = match from {
            Some(b) => self.read_branch_ref(b)?,
            None => self.head_state_unlocked()?,
        };
        write_atomic_text(&ref_path, &format!("{state}\n"))?;
        if default_stale {
            let new_default = name.to_string();
            self.update_config_unlocked(|c| {
                c.default_branch = new_default;
                Ok(())
            })?;
        }
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
        self.sync_default_branch_config_unlocked()?;
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

    pub fn load_manifest(&self, state_id: &StateId) -> RepoResult<ManifestMap> {
        let _lock = self.repo_lock()?;
        self.load_manifest_unlocked(state_id)
    }

    pub(crate) fn load_manifest_unlocked(&self, state_id: &StateId) -> RepoResult<ManifestMap> {
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

    pub fn load_state_files(&self, state_id: &StateId) -> RepoResult<HashMap<String, TrackedFile>> {
        let _lock = self.repo_lock()?;
        self.load_state_files_unlocked(state_id)
    }

    pub(crate) fn load_state_files_unlocked(
        &self,
        state_id: &StateId,
    ) -> RepoResult<HashMap<String, TrackedFile>> {
        let manifest = self.load_manifest_unlocked(state_id)?;
        let store = self.blobs();
        let mut files = HashMap::new();
        for (path, entry) in manifest {
            let content = store.read(&entry.blob)?;
            files.insert(path, TrackedFile::new(content, entry.mode));
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
        self.status_with_options(ScanOptions::default())
    }

    pub fn status_with_options(&self, opts: ScanOptions) -> RepoResult<WorkingStatus> {
        let _lock = self.repo_lock()?;
        self.status_unlocked(opts)
    }

    pub(crate) fn load_staging_unlocked(&self) -> RepoResult<StagingIndex> {
        load_staging(&self.astvcs_dir()).map_err(RepoError::from_message)
    }

    pub(crate) fn save_staging_unlocked(&self, index: &StagingIndex) -> RepoResult<()> {
        save_staging(&self.astvcs_dir(), index).map_err(RepoError::from_message)
    }

    fn build_effective_files(
        &self,
        head_files: &HashMap<String, TrackedFile>,
        staging: &StagingIndex,
    ) -> RepoResult<HashMap<String, TrackedFile>> {
        let store = self.blobs();
        let mut effective = head_files.clone();
        for (path, entry) in &staging.entries {
            if entry.deleted {
                effective.remove(path);
            } else if let Some(tracked) =
                staged_to_tracked(&store, entry).map_err(RepoError::from_message)?
            {
                effective.insert(path.clone(), tracked);
            }
        }
        Ok(effective)
    }

    fn staged_tracked_map(
        &self,
        staging: &StagingIndex,
    ) -> RepoResult<HashMap<String, TrackedFile>> {
        let store = self.blobs();
        let mut map = HashMap::new();
        for (path, entry) in &staging.entries {
            if entry.deleted {
                continue;
            }
            if let Some(tracked) =
                staged_to_tracked(&store, entry).map_err(RepoError::from_message)?
            {
                map.insert(path.clone(), tracked);
            }
        }
        Ok(map)
    }

    /// Stage paths into `staging.json`. Use `update_only` for tracked changes only; `all` for every change.
    pub fn add(&self, paths: &[String], update_only: bool, all: bool) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let (working_files, _, _) = self.scan_working(&head, ScanOptions::default())?;
        let targets =
            self.collect_add_targets(paths, &head_files, &working_files, update_only, all)?;
        if targets.is_empty() {
            return Ok(());
        }

        let mut staging = self.load_staging_unlocked()?;
        staging.active = true;
        let store = self.blobs();

        for path in &targets {
            if working_files.contains(path) {
                let tracked = load_working_tracked(&self.root, path)?;
                let blob_id = store.write(&tracked.content)?;
                staging.entries.insert(
                    path.clone(),
                    StagedEntry::from_tracked(blob_id, index_content_kind(&tracked), tracked.mode),
                );
                trace::notice(format!("add: staged {path}"));
            } else if head_files.contains_key(path) {
                staging
                    .entries
                    .insert(path.clone(), StagedEntry::deletion());
                trace::notice(format!("add: staged deletion {path}"));
            }
        }

        self.save_staging_unlocked(&staging)?;
        Ok(())
    }

    fn collect_add_targets(
        &self,
        paths: &[String],
        head_files: &HashMap<String, TrackedFile>,
        working_files: &HashSet<String>,
        update_only: bool,
        all: bool,
    ) -> RepoResult<HashSet<String>> {
        if paths.is_empty() && !update_only && !all {
            return Err(RepoError::invalid_input(
                "specify paths to stage, or use -u/--update or -A/--all",
            ));
        }

        let mut candidates: HashSet<String> = HashSet::new();
        if paths.is_empty() {
            if all {
                candidates.extend(working_files.iter().cloned());
                candidates.extend(head_files.keys().cloned());
            } else if update_only {
                candidates.extend(head_files.keys().cloned());
            }
        } else {
            for raw in paths {
                let rel = normalize_repo_path(raw)?;
                if rel.is_empty() || rel == "." {
                    candidates.extend(working_files.iter().cloned());
                    candidates.extend(head_files.keys().cloned());
                } else if self.root.join(&rel).is_dir() {
                    let prefix = format!("{rel}/");
                    for p in working_files {
                        if p == &rel || p.starts_with(&prefix) {
                            candidates.insert(p.clone());
                        }
                    }
                    for p in head_files.keys() {
                        if p == &rel || p.starts_with(&prefix) {
                            candidates.insert(p.clone());
                        }
                    }
                } else {
                    candidates.insert(rel);
                }
            }
        }

        let mut targets = HashSet::new();
        for path in candidates {
            let on_disk = working_files.contains(&path);
            let tracked = head_files.contains_key(&path);
            if all {
                if on_disk || tracked {
                    targets.insert(path);
                }
                continue;
            }
            if update_only {
                if !tracked {
                    continue;
                }
                if !on_disk {
                    targets.insert(path);
                    continue;
                }
                let current = load_working_tracked(&self.root, &path)?;
                let head_entry = head_files.get(&path).unwrap();
                if !tracked_eq(head_entry, &current) {
                    targets.insert(path);
                }
                continue;
            }
            // explicit paths: stage if exists on disk or is tracked deletion
            if on_disk || tracked {
                targets.insert(path);
            }
        }
        Ok(targets)
    }

    pub(crate) fn status_unlocked(&self, opts: ScanOptions) -> RepoResult<WorkingStatus> {
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let index: HashMap<String, IndexEntry> = read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        self.check_index_consistency(&head, &head_files, &index);
        let staging = self.load_staging_unlocked()?;
        let effective_files = self.build_effective_files(&head_files, &staging)?;

        let mut entries = HashMap::new();
        let mut text_fallback_paths = HashSet::new();
        let (working_files, mut scan_cache, scan_metrics) = self.scan_working(&head, opts)?;
        let mut working_map = HashMap::new();
        let skip_content_reads =
            !opts.full_scan && scan_metrics.mode == Some(walk::ScanMode::Incremental);

        // Staged vs HEAD
        if staging.active {
            let staged_map = self.staged_tracked_map(&staging)?;
            let staged_renames = detect_path_renames(
                &tracked_content_map(&head_files),
                &tracked_content_map(&staged_map),
            );
            let staged_renamed_from: HashSet<String> =
                staged_renames.iter().map(|r| r.from.clone()).collect();
            let staged_renamed_to: HashSet<String> =
                staged_renames.iter().map(|r| r.to.clone()).collect();

            for rename in &staged_renames {
                if let Some(head_stored) = head_files.get(&rename.from)
                    && path_has_text_fallback(
                        &rename.to,
                        Some(&head_stored.content),
                        &staged_map[&rename.to].content,
                    )
                {
                    text_fallback_paths.insert(rename.to.clone());
                }
                entries.insert(
                    rename.to.clone(),
                    FileStatus {
                        staged: ChangeColumn::Added,
                        unstaged: ChangeColumn::Clean,
                        staged_rename_from: Some(rename.from.clone()),
                        unstaged_rename_from: None,
                        untracked: false,
                    },
                );
            }

            for (path, entry) in &staging.entries {
                if staged_renamed_from.contains(path) || staged_renamed_to.contains(path) {
                    continue;
                }
                let staged_col = if entry.deleted {
                    if head_files.contains_key(path) {
                        ChangeColumn::Deleted
                    } else {
                        continue;
                    }
                } else if head_files.contains_key(path) {
                    let staged_tracked = staged_map.get(path).unwrap();
                    let head_tracked = head_files.get(path).unwrap();
                    if tracked_eq(head_tracked, staged_tracked) {
                        continue;
                    }
                    ChangeColumn::Modified
                } else {
                    ChangeColumn::Added
                };
                if let Some(tracked) = staged_map.get(path) {
                    let head_content = head_files.get(path).map(|s| &s.content);
                    if path_has_text_fallback(path, head_content, &tracked.content) {
                        text_fallback_paths.insert(path.clone());
                    }
                } else if let Some(head_stored) = head_files.get(path)
                    && head_stored.content.is_text_fallback_at_path(path)
                {
                    text_fallback_paths.insert(path.clone());
                }
                entries.insert(
                    path.clone(),
                    FileStatus {
                        staged: staged_col,
                        unstaged: ChangeColumn::Clean,
                        staged_rename_from: None,
                        unstaged_rename_from: None,
                        untracked: false,
                    },
                );
            }
        }

        // Unstaged vs effective index (or HEAD when staging inactive)
        let compare_base = if staging.active {
            &effective_files
        } else {
            &head_files
        };

        for path in &working_files {
            if skip_content_reads
                && compare_base.contains_key(path)
                && scan_cache.verified.get(path).is_some_and(|entry| {
                    scan_cache::path_verified_unchanged(&self.root, path, entry)
                })
            {
                if !entries.contains_key(path) {
                    entries.insert(
                        path.clone(),
                        FileStatus {
                            staged: ChangeColumn::Clean,
                            unstaged: ChangeColumn::Clean,
                            staged_rename_from: None,
                            unstaged_rename_from: None,
                            untracked: false,
                        },
                    );
                }
                continue;
            }
            let current = load_working_tracked(&self.root, path)?;
            working_map.insert(path.clone(), current.clone());
            let base_content = compare_base.get(path).map(|stored| &stored.content);
            if path_has_text_fallback(path, base_content, &current.content) {
                text_fallback_paths.insert(path.clone());
            }
            let unstaged_col = match compare_base.get(path) {
                None => {
                    if head_files.contains_key(path) {
                        ChangeColumn::Added
                    } else {
                        entries.insert(path.clone(), FileStatus::untracked());
                        continue;
                    }
                }
                Some(stored) if !tracked_eq(stored, &current) => ChangeColumn::Modified,
                Some(_) => ChangeColumn::Clean,
            };
            if matches!(unstaged_col, ChangeColumn::Clean) {
                if let Ok(entry) = scan_cache::verify_entry_for_path(&self.root, path) {
                    scan_cache.verified.insert(path.clone(), entry);
                }
            } else {
                scan_cache.verified.remove(path);
                trace::notice(format!("status: {path} unstaged {unstaged_col:?}"));
            }
            if unstaged_col != ChangeColumn::Clean {
                match entries.get_mut(path) {
                    Some(existing) => existing.unstaged = unstaged_col,
                    None => {
                        entries.insert(
                            path.clone(),
                            FileStatus {
                                staged: ChangeColumn::Clean,
                                unstaged: unstaged_col,
                                staged_rename_from: None,
                                unstaged_rename_from: None,
                                untracked: false,
                            },
                        );
                    }
                }
            } else if !entries.contains_key(path) {
                entries.insert(
                    path.clone(),
                    FileStatus {
                        staged: ChangeColumn::Clean,
                        unstaged: ChangeColumn::Clean,
                        staged_rename_from: None,
                        unstaged_rename_from: None,
                        untracked: false,
                    },
                );
            }
        }

        scan_cache::save_scan_cache(&self.astvcs_dir(), &scan_cache)
            .map_err(RepoError::from_message)?;

        for path in compare_base.keys() {
            if !working_files.contains(path) {
                trace::notice(format!("status: {path} unstaged Deleted"));
                match entries.get_mut(path) {
                    Some(existing) => existing.unstaged = ChangeColumn::Deleted,
                    None => {
                        entries.insert(
                            path.clone(),
                            FileStatus {
                                staged: ChangeColumn::Clean,
                                unstaged: ChangeColumn::Deleted,
                                staged_rename_from: None,
                                unstaged_rename_from: None,
                                untracked: false,
                            },
                        );
                    }
                }
                if let Some(stored) = compare_base.get(path)
                    && stored.content.is_text_fallback_at_path(path)
                {
                    text_fallback_paths.insert(path.clone());
                }
            }
        }

        let renames = detect_path_renames(
            &tracked_content_map(compare_base),
            &tracked_content_map(&working_map),
        );
        for rename in &renames {
            entries.remove(&rename.from);
            let mut status = entries.remove(&rename.to).unwrap_or(FileStatus {
                staged: ChangeColumn::Clean,
                unstaged: ChangeColumn::Clean,
                staged_rename_from: None,
                unstaged_rename_from: None,
                untracked: false,
            });
            status.unstaged = ChangeColumn::Added;
            status.unstaged_rename_from = Some(rename.from.clone());
            status.untracked = false;
            if path_has_text_fallback(
                &rename.to,
                compare_base.get(&rename.from).map(|s| &s.content),
                &working_map[&rename.to].content,
            ) {
                text_fallback_paths.insert(rename.to.clone());
            }
            entries.insert(rename.to.clone(), status);
        }

        entries.retain(|_, status| !status.is_clean());

        Ok(WorkingStatus {
            entries,
            text_fallback_paths,
        })
    }

    pub fn diff_working(&self, path: &str) -> RepoResult<String> {
        self.diff_working_inner(path, false)
    }

    pub fn diff_staged(&self, path: &str) -> RepoResult<String> {
        self.diff_working_inner(path, true)
    }

    fn diff_working_inner(&self, path: &str, staged: bool) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let staging = self.load_staging_unlocked()?;
        let effective_files = if staging.active && !staged {
            self.build_effective_files(&head_files, &staging)?
        } else {
            head_files.clone()
        };
        let base_files = if staged {
            &head_files
        } else {
            &effective_files
        };
        let compare_files = if staged {
            &self.staged_tracked_map(&staging)?
        } else {
            &HashMap::new()
        };

        if staged {
            if let Some(rename) = detect_path_renames(
                &tracked_content_map(base_files),
                &tracked_content_map(compare_files),
            )
            .into_iter()
            .find(|r| r.from == path || r.to == path)
            {
                let old = base_files.get(&rename.from).unwrap();
                let new = compare_files.get(&rename.to).unwrap();
                return Ok(format_path_rename(&rename, &old.content, &new.content));
            }
            match (base_files.get(path), compare_files.get(path)) {
                (None, Some(new)) => {
                    let mut out = format!("--- /dev/null\n+++ {path}\n(new file)\n");
                    out.push_str(&content_preview(&new.content));
                    Ok(out)
                }
                (Some(_old), None) => Ok(format!("--- {path}\n+++ /dev/null\n(deleted)\n")),
                (Some(old), Some(new)) if !tracked_eq(old, new) => {
                    format_diff(path, &old.content, &new.content)
                }
                _ => Ok(format!("--- {path}\n+++ {path}\n(no changes)\n")),
            }
        } else {
            let working = load_working_tracked(&self.root, path)?;
            let mut working_map = HashMap::new();
            working_map.insert(path.to_string(), working.clone());
            for p in self.scan_working(&head, ScanOptions::default())?.0 {
                if p != path {
                    working_map.insert(p.clone(), load_working_tracked(&self.root, &p)?);
                }
            }
            let renames = detect_path_renames(
                &tracked_content_map(base_files),
                &tracked_content_map(&working_map),
            );
            if let Some(rename) = renames.iter().find(|r| r.to == path) {
                let old = base_files.get(&rename.from).unwrap();
                return Ok(format_path_rename(rename, &old.content, &working.content));
            }
            match base_files.get(path) {
                None => Ok(format!("--- /dev/null\n+++ {path}\n(new file)\n")),
                Some(base) => format_diff(path, &base.content, &working.content),
            }
        }
    }

    pub fn diff_working_tree(&self) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let status = self.status_unlocked(ScanOptions::default())?;
        let mut out = String::new();
        for (path, st) in &status.entries {
            if st.untracked
                || matches!(st.unstaged, ChangeColumn::Modified | ChangeColumn::Added)
                || st.unstaged_rename_from.is_some()
            {
                out.push_str(&self.diff_working_inner(path, false)?);
            } else if matches!(st.unstaged, ChangeColumn::Deleted) {
                out.push_str(&format!("--- {path}\n+++ /dev/null\n(deleted)\n"));
            }
        }
        Ok(out)
    }

    pub fn diff_staged_tree(&self) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let staging = self.load_staging_unlocked()?;
        if !staging.active || staging.entries.is_empty() {
            return Ok(String::new());
        }
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let staged_map = self.staged_tracked_map(&staging)?;
        let renames = detect_path_renames(
            &tracked_content_map(&head_files),
            &tracked_content_map(&staged_map),
        );
        let renamed_from: HashSet<String> = renames.iter().map(|r| r.from.clone()).collect();
        let renamed_to: HashSet<String> = renames.iter().map(|r| r.to.clone()).collect();
        let mut out = String::new();
        for rename in &renames {
            let old = head_files.get(&rename.from).unwrap();
            let new = staged_map.get(&rename.to).unwrap();
            out.push_str(&format_path_rename(rename, &old.content, &new.content));
        }
        for path in staging.entries.keys() {
            if renamed_from.contains(path) || renamed_to.contains(path) {
                continue;
            }
            out.push_str(&self.diff_staged(path)?);
        }
        Ok(out)
    }

    /// Resolve a branch name, tag, remote-tracking ref, or 64-character state id.
    pub fn resolve_state_ref(&self, reference: &str) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        self.resolve_state_ref_unlocked(reference)
    }

    pub(crate) fn resolve_state_ref_unlocked(&self, reference: &str) -> RepoResult<StateId> {
        if reference.eq_ignore_ascii_case("HEAD") {
            let id = self.head_state_unlocked()?;
            trace::notice(format!("resolved HEAD -> state {id}"));
            return Ok(id);
        }
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
        let tag_path = self.astvcs_dir().join("refs/tags").join(reference);
        if tag_path.is_file() {
            let id = self.read_tag_unlocked(reference)?;
            trace::notice(format!("resolved tag {reference} -> state {id}"));
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
        let shallow = crate::store::shallow::load_shallow_boundaries(&self.astvcs_dir())?;
        let base = merge_base_checked(
            &left_id,
            &right_id,
            |id| {
                self.load_timeline_entry_unlocked(id)
                    .map_err(|e| e.to_string())
            },
            |id| shallow.contains(id),
        )?;
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
            if trace::is_detailed() {
                out.push_str(&format!("base:  {base}\n"));
                out.push_str(&format!("left:  {left}\n"));
                out.push_str(&format!("right: {right}\n"));
            }
            match (base_files.get(&p), left_files.get(&p), right_files.get(&p)) {
                (None, None, None) => out.push('\n'),
                (base_c, left_c, right_c) => {
                    if let (Some(b), Some(l)) = (base_c, left_c) {
                        if !tracked_eq(b, l) {
                            out.push_str("\nbase -> left:\n");
                            out.push_str(&format_pairwise_content_diff(&p, &b.content, &l.content));
                        } else {
                            out.push_str("\nbase -> left: (unchanged)\n");
                        }
                    } else if left_c.is_some() {
                        out.push_str("\nbase -> left: (added on left)\n");
                    } else if base_c.is_some() {
                        out.push_str("\nbase -> left: (removed on left)\n");
                    }
                    if let (Some(b), Some(r)) = (base_c, right_c) {
                        if !tracked_eq(b, r) {
                            out.push_str("\nbase -> right:\n");
                            out.push_str(&format_pairwise_content_diff(&p, &b.content, &r.content));
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
        let other = self.resolve_state_ref_unlocked(branch)?;
        if head == other {
            return Err("already up to date".into());
        }

        let shallow = crate::store::shallow::load_shallow_boundaries(&self.astvcs_dir())?;
        let base_id = merge_base_checked(
            &head,
            &other,
            |id| {
                self.load_timeline_entry_unlocked(id)
                    .map_err(|e| e.to_string())
            },
            |id| shallow.contains(id),
        )?;
        trace::notice(format!(
            "merge plan: base={base_id} head={head} other={other}"
        ));
        self.plan_three_way_unlocked(&base_id, &head, &other)
    }

    pub(crate) fn plan_three_way_unlocked(
        &self,
        base_id: &StateId,
        left_id: &StateId,
        right_id: &StateId,
    ) -> RepoResult<MergePlan> {
        let base_files = self.load_state_files_unlocked(base_id)?;
        let head_files = self.load_state_files_unlocked(left_id)?;
        let other_files = self.load_state_files_unlocked(right_id)?;

        let head_renames = detect_path_renames(
            &tracked_content_map(&base_files),
            &tracked_content_map(&head_files),
        );
        let other_renames = detect_path_renames(
            &tracked_content_map(&base_files),
            &tracked_content_map(&other_files),
        );
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
            let head_path = side_path_for_base(
                base_path,
                &tracked_content_map(&head_files),
                &head_rename_map,
            )
            .or_else(|| {
                if head_files.contains_key(&result_path) && !base_files.contains_key(&result_path) {
                    Some(result_path.clone())
                } else {
                    None
                }
            });
            let other_path = side_path_for_base(
                base_path,
                &tracked_content_map(&other_files),
                &other_rename_map,
            )
            .or_else(|| {
                if other_files.contains_key(&result_path) && !base_files.contains_key(&result_path)
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
                && !tracked_eq(head_dest, other_dest)
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

            match merge_tracked_path(
                &result_path,
                base_files.get(base_path),
                head_path.as_ref().and_then(|p| head_files.get(p)),
                other_path.as_ref().and_then(|p| other_files.get(p)),
            ) {
                PathMergeTrackedOutcome::Keep(tracked) => {
                    trace::notice(format!("merge plan: {result_path} keep"));
                    merged_files.insert(result_path, tracked);
                }
                PathMergeTrackedOutcome::Remove => {
                    trace::notice(format!("merge plan: {result_path} remove"));
                }
                PathMergeTrackedOutcome::Conflict(c) => {
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
            match merge_tracked_path(&path, base, left, right) {
                PathMergeTrackedOutcome::Keep(tracked) => {
                    trace::notice(format!("merge plan: {path} keep"));
                    merged_files.insert(path, tracked);
                }
                PathMergeTrackedOutcome::Remove => {
                    trace::notice(format!("merge plan: {path} remove"));
                }
                PathMergeTrackedOutcome::Conflict(c) => {
                    trace::warn(format!("merge plan: {} conflict", c.path));
                    conflicts.push(c);
                }
            }
        }

        Ok(MergePlan {
            base_id: base_id.clone(),
            head_id: left_id.clone(),
            other_id: right_id.clone(),
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
            match merge_tracked_path(&path, base, left, right) {
                PathMergeTrackedOutcome::Keep(tracked) => {
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
                    reverted_files.insert(path, tracked);
                }
                PathMergeTrackedOutcome::Remove => {
                    trace::notice(format!("revert plan: {path} remove"));
                    reverted_files.remove(&path);
                }
                PathMergeTrackedOutcome::Conflict(c) => {
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
            return Err(RepoError::revert_conflict(plan.format_conflicts())
                .with_concise(plan.format_conflicts_focused()));
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

    pub fn reset(
        &self,
        reference: &str,
        soft: bool,
        mixed: bool,
        force: bool,
    ) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        let target = self.resolve_state_ref_unlocked(reference)?;
        let prior_head = self.head_state_unlocked()?;
        let materialize_opts = if soft || mixed {
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

        if !soft && !mixed {
            let opts = materialize_opts.expect("hard reset materialize options");
            self.materialize_state_inner(&target, clobbered, &opts)?;
        } else if mixed {
            let files = self.load_state_files_unlocked(&target)?;
            self.sync_index_to_state(&files, &target)?;
            let mut staging = self.load_staging_unlocked()?;
            clear_staging_entries(&mut staging);
            self.save_staging_unlocked(&staging)?;
            scan_cache::invalidate_scan_cache(&self.astvcs_dir())
                .map_err(RepoError::from_message)?;
        }

        match self.read_head_target()? {
            HeadTarget::Branch(ref branch) => {
                self.write_branch_ref_unlocked(branch, &target)?;
                let mode = if soft {
                    "soft"
                } else if mixed {
                    "mixed"
                } else {
                    "hard"
                };
                trace::notice(format!(
                    "reset {mode}: branch {branch} {prior_head} -> {target}"
                ));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(target.clone()))?;
                let mode = if soft {
                    "soft"
                } else if mixed {
                    "mixed"
                } else {
                    "hard"
                };
                trace::notice(format!("reset {mode}: detached {prior_head} -> {target}"));
            }
        }

        Ok(target)
    }

    pub fn diff_state_path(&self, from: &StateId, to: &StateId, path: &str) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let from_files = self.load_state_files_unlocked(from)?;
        let to_files = self.load_state_files_unlocked(to)?;
        let renames = detect_path_renames(
            &tracked_content_map(&from_files),
            &tracked_content_map(&to_files),
        );
        if let Some(rename) = renames.iter().find(|r| r.from == path || r.to == path) {
            let old = from_files.get(&rename.from).unwrap();
            let new = to_files.get(&rename.to).unwrap();
            return Ok(format_path_rename(rename, &old.content, &new.content));
        }
        match (from_files.get(path), to_files.get(path)) {
            (None, Some(new)) => {
                let mut out = format!("--- /dev/null\n+++ {path}\n(new file)\n");
                out.push_str(&content_preview(&new.content));
                Ok(out)
            }
            (Some(_), None) => Ok(format!("--- {path}\n+++ /dev/null\n(deleted)\n")),
            (Some(old), Some(new)) if !tracked_eq(old, new) => {
                format_diff(path, &old.content, &new.content)
            }
            _ => Ok(format!("--- {path}\n+++ {path}\n(no changes)\n")),
        }
    }

    pub fn diff_states(&self, from: &StateId, to: &StateId) -> RepoResult<String> {
        let _lock = self.repo_lock()?;
        let from_files = self.load_state_files_unlocked(from)?;
        let to_files = self.load_state_files_unlocked(to)?;
        let renames = detect_path_renames(
            &tracked_content_map(&from_files),
            &tracked_content_map(&to_files),
        );
        let renamed_from: HashSet<String> = renames.iter().map(|r| r.from.clone()).collect();
        let renamed_to: HashSet<String> = renames.iter().map(|r| r.to.clone()).collect();
        let mut out = String::new();
        for rename in &renames {
            let old = from_files.get(&rename.from).unwrap();
            let new = to_files.get(&rename.to).unwrap();
            out.push_str(&format_path_rename(rename, &old.content, &new.content));
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
                    out.push_str(&content_preview(&new.content));
                }
                (Some(_), None) => {
                    out.push_str(&format!("--- {path}\n+++ /dev/null\n(deleted)\n"));
                }
                (Some(old), Some(new)) if !tracked_eq(old, new) => {
                    out.push_str(&format_diff(&path, &old.content, &new.content)?);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Alignment-first diff view of the working tree vs the index or HEAD.
    pub fn diff_view_working(&self, path: Option<&str>) -> RepoResult<DiffViewDocument> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let staging = self.load_staging_unlocked()?;
        let effective_files = if staging.active {
            self.build_effective_files(&head_files, &staging)?
        } else {
            head_files.clone()
        };
        let base_files = if staging.active {
            &effective_files
        } else {
            &head_files
        };

        let working_files = self.scan_working(&head, ScanOptions::default())?.0;
        let mut working_map = HashMap::new();
        for p in working_files {
            let tracked = load_working_tracked(&self.root, &p)?;
            working_map.insert(p, tracked);
        }

        let files = diff_view_files_between(base_files, &working_map, path);
        let left_label = if staging.active { "index" } else { "HEAD" };
        Ok(DiffViewDocument {
            left_label: left_label.to_string(),
            right_label: "working tree".to_string(),
            groups: vec![DiffViewGroup {
                title: String::new(),
                files,
            }],
        })
    }

    /// Alignment-first diff view of the staged changes vs HEAD.
    pub fn diff_view_staged(&self, path: Option<&str>) -> RepoResult<DiffViewDocument> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let staging = self.load_staging_unlocked()?;
        let staged_map = self.staged_tracked_map(&staging)?;

        let files = if path.is_none() && (!staging.active || staging.entries.is_empty()) {
            Vec::new()
        } else {
            diff_view_files_between(&head_files, &staged_map, path)
        };
        Ok(DiffViewDocument {
            left_label: "HEAD".to_string(),
            right_label: "staged".to_string(),
            groups: vec![DiffViewGroup {
                title: String::new(),
                files,
            }],
        })
    }

    /// Alignment-first diff view between two states.
    pub fn diff_view_states(
        &self,
        from: &StateId,
        to: &StateId,
        path: Option<&str>,
    ) -> RepoResult<DiffViewDocument> {
        let _lock = self.repo_lock()?;
        let from_files = self.load_state_files_unlocked(from)?;
        let to_files = self.load_state_files_unlocked(to)?;
        let files = diff_view_files_between(&from_files, &to_files, path);
        Ok(DiffViewDocument {
            left_label: short_state(from),
            right_label: short_state(to),
            groups: vec![DiffViewGroup {
                title: String::new(),
                files,
            }],
        })
    }

    /// Alignment-first three-way diff view: base against left and right.
    pub fn diff_view_three_way(
        &self,
        base: &StateId,
        left: &StateId,
        right: &StateId,
        path: Option<&str>,
    ) -> RepoResult<DiffViewDocument> {
        let _lock = self.repo_lock()?;
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

        let mut left_group = Vec::new();
        let mut right_group = Vec::new();
        for p in &paths {
            let b = base_files.get(p);
            let l = left_files.get(p);
            let r = right_files.get(p);
            if side_differs(b, l) {
                left_group.push(file_from_contents(
                    p,
                    b.map(|t| &t.content),
                    l.map(|t| &t.content),
                ));
            }
            if side_differs(b, r) {
                right_group.push(file_from_contents(
                    p,
                    b.map(|t| &t.content),
                    r.map(|t| &t.content),
                ));
            }
        }

        Ok(DiffViewDocument {
            left_label: "base".to_string(),
            right_label: "left | right".to_string(),
            groups: vec![
                DiffViewGroup {
                    title: "base -> left".to_string(),
                    files: left_group,
                },
                DiffViewGroup {
                    title: "base -> right".to_string(),
                    files: right_group,
                },
            ],
        })
    }

    pub fn commit(&self, message: &str) -> RepoResult<CommitOutcome> {
        self.commit_with_options(message, CommitOptions::default())
    }

    pub fn commit_with_options(
        &self,
        message: &str,
        opts: CommitOptions,
    ) -> RepoResult<CommitOutcome> {
        let _lock = self.repo_lock()?;
        let head = self.head_state_unlocked()?;
        let head_files = self.load_state_files_unlocked(&head)?;
        let mut staging = self.load_staging_unlocked()?;
        let scan_opts = opts.scan;

        let new_files = if staging.active {
            if staging.entries.is_empty() {
                let status = self.status_unlocked(scan_opts)?;
                if !status.entries.is_empty() {
                    return Err(RepoError::invalid_input(
                        "nothing staged; use `astvcs add` to stage changes before commit",
                    ));
                }
                trace::notice(format!("commit: no changes; state {head} unchanged"));
                return Ok(CommitOutcome {
                    state_id: head,
                    created: false,
                });
            }
            let mut new_files = head_files.clone();
            let store = self.blobs();
            for (path, entry) in &staging.entries {
                if entry.deleted {
                    new_files.remove(path);
                    trace::notice(format!("commit: {path} removed (staged)"));
                } else if let Some(tracked) =
                    staged_to_tracked(&store, entry).map_err(RepoError::from_message)?
                {
                    let action = if head_files.contains_key(path) {
                        "modified (staged)"
                    } else {
                        "added (staged)"
                    };
                    trace::notice(format!("commit: {path} {action}"));
                    new_files.insert(path.clone(), tracked);
                }
            }
            new_files
        } else {
            let (mut working_files, mut scan_cache, _) = self.scan_working(&head, scan_opts)?;
            if let Some(only) = &opts.only_paths {
                working_files.retain(|p| only.contains(p));
            }

            let mut new_files = head_files.clone();
            for path in &working_files {
                let tracked = load_working_tracked(&self.root, path)?;
                match head_files.get(path) {
                    Some(old) if tracked_eq(old, &tracked) => {
                        if let Ok(entry) = scan_cache::verify_entry_for_path(&self.root, path) {
                            scan_cache.verified.insert(path.clone(), entry);
                        }
                    }
                    Some(_) => {
                        scan_cache.verified.remove(path);
                        trace::notice(format!("commit: {path} modified"));
                    }
                    None => {
                        scan_cache.verified.remove(path);
                        trace::notice(format!("commit: {path} added"));
                    }
                }
                new_files.insert(path.clone(), tracked);
            }
            for path in head_files.keys().cloned().collect::<Vec<_>>() {
                if !working_files.contains(&path) {
                    scan_cache.verified.remove(&path);
                    trace::notice(format!("commit: {path} removed"));
                    new_files.remove(&path);
                }
            }

            scan_cache::save_scan_cache(&self.astvcs_dir(), &scan_cache)
                .map_err(RepoError::from_message)?;
            new_files
        };

        if manifest_unchanged(&head_files, &new_files) {
            trace::notice(format!("commit: no changes; state {head} unchanged"));
            return Ok(CommitOutcome {
                state_id: head,
                created: false,
            });
        }

        let message = super::hooks::run_commit_hooks(self, &head, message, opts.no_verify)?;

        let author = resolve_author_identity(self)?;
        let state_id = self.persist_state(
            &new_files,
            &message,
            &author,
            Some(head.clone()),
            vec![head],
        )?;
        self.sync_index_to_state(&new_files, &state_id)?;
        clear_staging_entries(&mut staging);
        self.save_staging_unlocked(&staging)?;
        if let Some(mut cache) =
            scan_cache::load_scan_cache(&self.astvcs_dir()).map_err(RepoError::from_message)?
        {
            cache.head_state_id = state_id.clone();
            scan_cache::save_scan_cache(&self.astvcs_dir(), &cache)
                .map_err(RepoError::from_message)?;
        }
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
        self.merge_branch_with_resolutions_force(branch, message, resolutions, false, false)
    }

    pub fn merge_branch_with_resolutions_force(
        &self,
        branch: &str,
        message: &str,
        resolutions: &[MergeResolution],
        force: bool,
        no_verify: bool,
    ) -> RepoResult<StateId> {
        let _lock = self.repo_lock()?;
        if !force {
            let staging = self.load_staging_unlocked()?;
            if staging.staging_in_use() {
                return Err(RepoError::invalid_input(
                    "cannot merge with staged changes; commit or reset --mixed to unstage",
                ));
            }
            let materialize_opts = MaterializeOptions::new("merge").force(false);
            self.materialize_guard(&materialize_opts)?;
        }
        let plan = self.prepare_merge_unlocked(branch, resolutions)?;
        if !plan.is_clean() {
            trace::warn("merge: aborted due to conflicts");
            return Err(RepoError::merge_conflict(plan.format_conflicts())
                .with_concise(plan.format_conflicts_focused()));
        }
        super::hooks::run_pre_merge_hook(self, &plan.head_id, branch, no_verify)?;
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
        let staging = self.load_staging_unlocked()?;
        if staging.staging_in_use() {
            return Err(RepoError::invalid_input(
                "cannot merge with staged changes; commit or reset --mixed to unstage",
            ));
        }
        let head = plan.head_id.clone();
        let other = plan.other_id.clone();
        if plan.is_clean() && plan.base_id == plan.head_id {
            let materialize_opts = MaterializeOptions::new("merge").force(force);
            let clobbered = self.materialize_guard(&materialize_opts)?;
            self.materialize_state_inner(&other, clobbered, &materialize_opts)?;
            let current_branch = self.head_branch_unlocked()?;
            if let Some(branch) = current_branch {
                self.write_branch_ref_unlocked(&branch, &other)?;
            } else {
                self.write_head_target(&HeadTarget::Detached(other.clone()))?;
            }
            trace::notice(format!("merge: fast-forward to {other}"));
            return Ok(other);
        }
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
            .status_unlocked(ScanOptions::default())?
            .entries
            .iter()
            .filter(|(_, status)| !status.is_clean())
            .map(|(path, _)| path.clone())
            .collect())
    }

    pub(crate) fn materialize_guard(
        &self,
        opts: &MaterializeOptions<'_>,
    ) -> RepoResult<Vec<String>> {
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

    pub(crate) fn materialize_state_inner(
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
                if full.exists() {
                    remove_working_path(&full)?;
                    trace::notice(format!("materialize: removed {path}"));
                }
            }
        }

        for (path, tracked) in &files {
            let full = self.root.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            if full.is_symlink() || full.exists() {
                remove_working_path(&full)?;
            }
            if tracked.mode == FileMode::Symlink {
                if let FileContent::Symlink(link) = &tracked.content {
                    materialize_symlink(&full, &link.target)?;
                    trace::notice(format!(
                        "materialize: wrote {path} (symlink -> {})",
                        link.target
                    ));
                }
                continue;
            }
            match &tracked.content {
                FileContent::Binary(blob) => {
                    atomic::write_atomic(&full, &blob.bytes)?;
                }
                other => {
                    atomic::write_atomic_text(&full, &content_to_string(other))?;
                }
            }
            #[cfg(unix)]
            set_unix_mode(&full, tracked.mode)?;
            trace::notice(format!(
                "materialize: wrote {path} ({})",
                index_content_kind(tracked)
            ));
        }

        self.sync_index_to_state(&files, state_id)?;
        let mut staging = self.load_staging_unlocked()?;
        clear_staging_entries(&mut staging);
        self.save_staging_unlocked(&staging)?;
        scan_cache::invalidate_scan_cache(&self.astvcs_dir()).map_err(RepoError::from_message)?;
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

    pub(crate) fn working_tree_is_clean_unlocked(&self) -> RepoResult<bool> {
        let staging = self.load_staging_unlocked()?;
        if staging.staging_in_use() {
            return Ok(false);
        }
        Ok(self
            .status_unlocked(ScanOptions::default())?
            .entries
            .values()
            .all(|s| s.is_clean()))
    }

    pub(crate) fn persist_state(
        &self,
        files: &HashMap<String, TrackedFile>,
        message: &str,
        author: &AuthorIdentity,
        parent: Option<StateId>,
        parents: Vec<StateId>,
    ) -> RepoResult<StateId> {
        let store = self.blobs();
        let mut manifest = ManifestMap::new();
        for (path, tracked) in files {
            let blob_id = store.write(&tracked.content)?;
            manifest.insert(
                path.clone(),
                ManifestEntry::with_mode(blob_id, tracked.mode),
            );
        }
        let manifest_id = hash_manifest(&manifest);
        let timestamp = now_iso();
        let commit_id = hash_commit(
            &manifest_id,
            &parents,
            message,
            &timestamp,
            &author.name,
            &author.email,
        );

        let states_path = self
            .astvcs_dir()
            .join("states")
            .join(format!("{manifest_id}.json"));
        if !states_path.is_file() {
            write_atomic_json(&states_path, &manifest)?;
        }

        let parent_count = parents.len();
        let entry = TimelineEntry {
            id: commit_id.clone(),
            parent: parent.clone(),
            parents,
            message: message.to_string(),
            timestamp,
            author_name: author.name.clone(),
            author_email: author.email.clone(),
            manifest: manifest.clone(),
            files: None,
        };
        write_atomic_json(
            &self
                .astvcs_dir()
                .join("timeline")
                .join(format!("{commit_id}.json")),
            &entry,
        )?;
        trace::notice(format!(
            "persist: commit {commit_id} manifest {manifest_id} ({} paths, parents={parent_count})",
            manifest.len(),
        ));
        Ok(commit_id)
    }

    fn migrate_inline_files(
        &self,
        files: &HashMap<String, FileContent>,
    ) -> RepoResult<ManifestMap> {
        let store = self.blobs();
        let mut manifest = ManifestMap::new();
        for (path, content) in files {
            manifest.insert(path.clone(), ManifestEntry::regular(store.write(content)?));
        }
        Ok(manifest)
    }

    pub(crate) fn repair_index_from_head_unlocked(&self, state_id: &StateId) -> RepoResult<()> {
        if let Ok(files) = self.load_state_files_unlocked(state_id) {
            return self.sync_index_to_state(&files, state_id);
        }
        self.sync_index_from_manifest_unlocked(state_id)
    }

    fn sync_index_from_manifest_unlocked(&self, state_id: &StateId) -> RepoResult<()> {
        let manifest = self.load_manifest_unlocked(state_id)?;
        let existing_index: HashMap<String, IndexEntry> =
            read_json(&self.astvcs_dir().join(INDEX_FILE)).unwrap_or_default();
        let mut index = HashMap::new();
        for path in manifest.keys() {
            let content_kind = existing_index
                .get(path)
                .map(|entry| entry.content_kind.clone())
                .unwrap_or_else(|| "text".to_string());
            index.insert(
                path.clone(),
                IndexEntry {
                    state_id: state_id.to_string(),
                    content_kind,
                },
            );
        }
        write_atomic_json(&self.astvcs_dir().join(INDEX_FILE), &index)?;
        Ok(())
    }

    pub(crate) fn sync_index_to_state(
        &self,
        files: &HashMap<String, TrackedFile>,
        state_id: &StateId,
    ) -> RepoResult<()> {
        let mut index: HashMap<String, IndexEntry> =
            read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        let paths: HashSet<String> = files.keys().cloned().collect();
        index.retain(|path, _| paths.contains(path));
        for (path, tracked) in files {
            index.insert(
                path.clone(),
                IndexEntry {
                    state_id: state_id.to_string(),
                    content_kind: index_content_kind(tracked),
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

    pub(crate) fn write_head_target(&self, target: &HeadTarget) -> RepoResult<()> {
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
        self.load_config_unlocked()
    }

    pub(crate) fn load_config_unlocked(&self) -> RepoResult<RepoConfig> {
        read_json_unlocked(&self.astvcs_dir().join(CONFIG_FILE))
    }

    pub fn has_blob(&self, id: &str) -> bool {
        self.blobs().contains(&id.to_string())
    }

    pub fn has_state(&self, state_id: &StateId) -> bool {
        if self.has_timeline(state_id) {
            return true;
        }
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
        _state_id: &StateId,
        manifest: &ManifestMap,
    ) -> RepoResult<()> {
        let _lock = self.repo_lock()?;
        let manifest_id = hash_manifest(manifest);
        let states_path = self
            .astvcs_dir()
            .join("states")
            .join(format!("{manifest_id}.json"));
        if !states_path.is_file() {
            write_atomic_json(&states_path, manifest)?;
        }
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

    pub(crate) fn write_branch_ref_unlocked(
        &self,
        branch: &str,
        state_id: &StateId,
    ) -> RepoResult<()> {
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

pub(crate) fn normalize_repo_path(raw: &str) -> RepoResult<String> {
    let path = raw.replace('\\', "/");
    if path.is_empty() || path.contains("..") {
        return Err(RepoError::invalid_input(format!("invalid path: {raw}")));
    }
    Ok(path.trim_start_matches("./").to_string())
}

fn manifest_unchanged(
    head: &HashMap<String, TrackedFile>,
    working: &HashMap<String, TrackedFile>,
) -> bool {
    if head.len() != working.len() {
        return false;
    }
    head.iter()
        .all(|(path, tracked)| working.get(path).is_some_and(|w| tracked_eq(tracked, w)))
}

fn tracked_content_map(files: &HashMap<String, TrackedFile>) -> HashMap<String, FileContent> {
    files
        .iter()
        .map(|(k, v)| (k.clone(), v.content.clone()))
        .collect()
}

/// Shorten a full state id to a stable 12-hex-character prefix for display.
fn short_state(id: &StateId) -> String {
    if id.len() > 12 {
        id[..12].to_string()
    } else {
        id.clone()
    }
}

/// Whether one side of a three-way comparison differs from base.
fn side_differs(base: Option<&TrackedFile>, side: Option<&TrackedFile>) -> bool {
    match (base, side) {
        (None, None) => false,
        (Some(a), Some(b)) => !tracked_eq(a, b),
        _ => true,
    }
}

/// Build change-first diff view files between two tracked maps, mirroring the
/// rename-aware, pairwise resolution used by the text diff paths.
fn diff_view_files_between(
    from: &HashMap<String, TrackedFile>,
    to: &HashMap<String, TrackedFile>,
    path: Option<&str>,
) -> Vec<DiffViewFile> {
    let renames = detect_path_renames(&tracked_content_map(from), &tracked_content_map(to));
    let mut files = Vec::new();

    if let Some(p) = path {
        if let Some(rename) = renames.iter().find(|r| r.from == p || r.to == p) {
            let old = from.get(&rename.from).unwrap();
            let new = to.get(&rename.to).unwrap();
            files.push(file_from_rename(rename, &old.content, &new.content));
            return files;
        }
        let old = from.get(p);
        let new = to.get(p);
        if old.is_some() || new.is_some() {
            files.push(file_from_contents(
                p,
                old.map(|t| &t.content),
                new.map(|t| &t.content),
            ));
        }
        return files;
    }

    let renamed_from: HashSet<String> = renames.iter().map(|r| r.from.clone()).collect();
    let renamed_to: HashSet<String> = renames.iter().map(|r| r.to.clone()).collect();
    for rename in &renames {
        let old = from.get(&rename.from).unwrap();
        let new = to.get(&rename.to).unwrap();
        files.push(file_from_rename(rename, &old.content, &new.content));
    }

    let mut paths: HashSet<String> = from.keys().cloned().collect();
    paths.extend(to.keys().cloned());
    let mut sorted: Vec<_> = paths.into_iter().collect();
    sorted.sort();
    for p in sorted {
        if renamed_from.contains(&p) || renamed_to.contains(&p) {
            continue;
        }
        match (from.get(&p), to.get(&p)) {
            (None, Some(new)) => {
                files.push(file_from_contents(&p, None, Some(&new.content)));
            }
            (Some(old), None) => {
                files.push(file_from_contents(&p, Some(&old.content), None));
            }
            (Some(old), Some(new)) if !tracked_eq(old, new) => {
                files.push(file_from_contents(
                    &p,
                    Some(&old.content),
                    Some(&new.content),
                ));
            }
            _ => {}
        }
    }
    files
}

fn index_content_kind(tracked: &TrackedFile) -> String {
    match tracked.mode {
        FileMode::Regular => tracked.content.display_kind().to_string(),
        FileMode::Executable => format!("executable:{}", tracked.content.display_kind()),
        FileMode::Symlink => "symlink".to_string(),
    }
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

fn materialize_symlink(path: &Path, target: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_file;
        if let Err(e) = symlink_file(target, path) {
            crate::trace::warn(format!(
                "materialize: could not create symlink at {} -> {target}: {e}; skipped",
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

fn is_state_id(s: &str) -> bool {
    s.len() == STATE_ID_LEN && s.chars().all(|c| c.is_ascii_hexdigit())
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

fn content_preview(content: &FileContent) -> String {
    match content {
        FileContent::Binary(blob) => format!("(binary file, {} bytes)\n", blob.bytes.len()),
        FileContent::Symlink(blob) => format!("(symlink -> {})\n", blob.target),
        other => {
            let text = content_to_string(other);
            if text.len() > 200 {
                format!("{}...\n", &text[..200])
            } else {
                format!("{text}\n")
            }
        }
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
    if path_has_text_fallback(&rename.to, Some(old), new) {
        out.push_str("(text fallback - structural diff unavailable)\n");
    }
    let rename_intent = intent::classify_path_rename(rename);
    let rename_label = if trace::is_detailed() {
        intent::format_intent_detailed(None, &rename_intent)
    } else {
        intent::format_intent_compact(None, &rename_intent)
    };
    out.push_str(&format!("intents:\n  [0] {rename_label}\n"));
    if rename.kind == crate::diff::PathRenameKind::WithEdits {
        out.push_str(&format_mutation_diff(Some(&rename.to), old, new));
    }
    out
}

fn format_pairwise_content_diff(path: &str, old: &FileContent, new: &FileContent) -> String {
    let mut out = String::new();
    if path_has_text_fallback(path, Some(old), new) {
        out.push_str("(text fallback - structural diff unavailable)\n");
    }
    out.push_str(&format_mutation_diff(Some(path), old, new));
    out
}

fn format_parse_mode_intent(path: &str, old: &FileContent, new: &FileContent) -> Option<String> {
    if !crate::frontend::is_ast_capable_path(path) {
        return None;
    }
    let left = content_parse_mode_label(old);
    let right = content_parse_mode_label(new);
    if left == "ast" && right == "ast" {
        return None;
    }
    Some(format!(
        "intents:\n  [0] parse mode: {left} (left), {right} (right)\n"
    ))
}

fn content_parse_mode_label(content: &FileContent) -> &'static str {
    match content {
        FileContent::Ast(_) => "ast",
        FileContent::Text(_) => "text fallback",
        FileContent::Binary(_) => "binary",
        FileContent::Symlink(_) => "symlink",
    }
}

fn format_mutation_diff(path: Option<&str>, old: &FileContent, new: &FileContent) -> String {
    if old.is_binary() || new.is_binary() {
        if old.semantic_eq(new) {
            return "(binary file - unchanged)\n".into();
        }
        return "(binary file - content diff omitted)\n".into();
    }
    let mut prefix = String::new();
    if let Some(p) = path
        && let Some(note) = format_parse_mode_intent(p, old, new)
    {
        prefix.push_str(&note);
    }
    let body = match (old, new) {
        (FileContent::Ast(o), FileContent::Ast(n)) => {
            let DiffResult { mutations } = diff_graphs(o, n);
            if mutations.is_empty() {
                "(no structural changes)\n".into()
            } else {
                let mut out = String::new();
                out.push_str("intents:\n");
                let lines = if trace::is_detailed() {
                    intent::format_intent_lines_detailed(Some(o), &mutations)
                } else {
                    intent::format_intent_lines_compact(Some(o), &mutations)
                };
                for line in lines {
                    out.push_str(&line);
                    out.push('\n');
                }
                if trace::is_detailed() {
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
        _ => "(content kind changed)\n".into(),
    };
    format!("{prefix}{body}")
}

fn format_diff(path: &str, old: &FileContent, new: &FileContent) -> RepoResult<String> {
    let mut out = format!("--- {path}\n+++ {path}\n");
    out.push_str(&format_pairwise_content_diff(path, old, new));
    Ok(out)
}

fn pick_default_branch(branches: &[BranchInfo]) -> Option<String> {
    if branches.is_empty() {
        return None;
    }
    if branches.iter().any(|b| b.name == "main") {
        return Some("main".into());
    }
    Some(branches[0].name.clone())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> RepoResult<T> {
    read_json_unlocked(path)
}

pub(crate) fn read_json_unlocked<T: serde::de::DeserializeOwned>(path: &Path) -> RepoResult<T> {
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
                .contains(&repo.load_manifest(&id).unwrap()["main.rs"].blob)
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
        let lca = crate::store::merge_base(&main_id, &feature_id, |id| {
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
        let text = match &files["main.rs"].content {
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
        let main_text = match &files["main.rs"].content {
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
        let text = match &files["main.rs"].content {
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
        let text = match &files["main.rs"].content {
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
        let text = match &files["lib.rs"].content {
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
        assert!(!out.contains(&base_id), "{out}");
        assert!(!out.contains("mutations:"), "{out}");

        trace::set_details(true);
        let detailed = repo
            .diff_three_way(&base_id, &main_id, &feature_id, Some("main.rs"))
            .unwrap();
        trace::set_details(false);
        assert!(detailed.contains(&base_id), "{detailed}");
        assert!(detailed.contains("mutations:"), "{detailed}");
    }

    #[test]
    fn diff_three_way_includes_text_fallback_banner() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let base_id = repo.commit("base").unwrap().state_id;
        repo.create_branch("feature", None).unwrap();

        fs::write(dir.path().join("main.rs"), "fn {{{\n").unwrap();
        let left_id = repo.commit("broken on main").unwrap().state_id;

        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() { let x = 1; }\n").unwrap();
        let right_id = repo.commit("on feature").unwrap().state_id;

        let out = repo
            .diff_three_way(&base_id, &left_id, &right_id, Some("main.rs"))
            .unwrap();
        assert!(
            out.contains("text fallback - structural diff unavailable"),
            "{out}"
        );
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

        repo.reset(&v1, false, false, false).unwrap();
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

        repo.reset(&v1, false, false, false).unwrap();
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

        repo.reset(&v1, true, false, false).unwrap();
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
        let status = repo.status().unwrap();
        assert_eq!(
            status.entries.get("note.txt"),
            Some(&FileStatus::unstaged_modified())
        );
    }

    #[test]
    fn reset_mixed_syncs_index_clears_staging_preserves_disk() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        repo.commit("v2").unwrap();
        fs::write(dir.path().join("note.txt"), "dirty\n").unwrap();
        repo.add(&["note.txt".into()], false, false).unwrap();

        repo.reset(&v1, false, true, false).unwrap();
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
        let staging = repo.load_staging_unlocked().unwrap();
        assert!(staging.entries.is_empty());
        let status = repo.status().unwrap();
        assert_eq!(
            status.entries.get("note.txt"),
            Some(&FileStatus::unstaged_modified())
        );
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

        let err = repo.reset(&v1, false, false, false).unwrap_err();
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

        repo.reset(&v1, false, false, true).unwrap();
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

        repo.reset(&v1, true, false, false).unwrap();
        assert!(repo.is_detached().unwrap());
        assert_eq!(repo.head_state().unwrap(), v1);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v2\n"
        );

        repo.reset(&v2, false, false, true).unwrap();
        assert_eq!(repo.head_state().unwrap(), v2);
        assert!(repo.working_tree_is_clean().unwrap());
    }

    #[test]
    fn reset_to_root_empty_state() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        let root = StateId::from("0".repeat(64));

        repo.reset(&root, false, false, false).unwrap();
        assert_eq!(repo.head_state().unwrap(), root);
        assert!(!dir.path().join("note.txt").exists());
    }

    #[test]
    fn reset_unknown_ref_errors_without_writes() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        let v1 = repo.commit("v1").unwrap().state_id;

        let err = repo
            .reset("missing-branch", false, false, false)
            .unwrap_err();
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
    fn merge_remote_tracking_ref() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("v1").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.checkout_branch("feature").unwrap();
        fs::write(dir.path().join("note.txt"), "v2\n").unwrap();
        let v2 = repo.commit("v2 on feature").unwrap().state_id;
        repo.checkout_branch("main").unwrap();
        repo.write_remote_ref("origin", "main", &v2).unwrap();

        let merged = repo.merge_branch("origin/main", "merge upstream").unwrap();
        assert_eq!(merged, v2);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "v2\n"
        );
        assert!(repo.working_tree_is_clean().unwrap());
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
            match &files["note.txt"].content {
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
            match &files["note.txt"].content {
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
            match &files["note.txt"].content {
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
        let text = match &files["lib.rs"].content {
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
        repo.reset(&target, true, false, false).unwrap();

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
    fn remove_default_branch_updates_config() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("baseline").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.create_branch("develop", None).unwrap();
        repo.checkout_branch("feature").unwrap();
        assert_eq!(repo.load_config().unwrap().default_branch, "main");
        repo.remove_branch("main").unwrap();
        assert_eq!(repo.load_config().unwrap().default_branch, "develop");
    }

    #[test]
    fn create_branch_fixes_dangling_default() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("note.txt"), "v1\n").unwrap();
        repo.commit("baseline").unwrap();
        repo.create_branch("feature", None).unwrap();
        repo.checkout_branch("feature").unwrap();
        repo.remove_branch("main").unwrap();
        assert_eq!(repo.load_config().unwrap().default_branch, "feature");

        let config_path = dir.path().join(".astvcs/config.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        value["default_branch"] = "stale".into();
        fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        repo.create_branch("recovery", None).unwrap();
        assert_eq!(repo.load_config().unwrap().default_branch, "recovery");
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
            err.contains("uncommitted changes"),
            "dirty tree should be refused before conflict reporting: {err}"
        );
        assert_eq!(repo.head_state().unwrap(), tip);
        assert_eq!(
            fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[test]
    fn add_dot_stages_working_tree_files() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        repo.commit("baseline").unwrap();

        fs::write(dir.path().join("a.txt"), "a v2\n").unwrap();
        fs::write(dir.path().join("c.txt"), "new\n").unwrap();
        repo.add(&[".".into()], false, false).unwrap();

        let staging = repo.load_staging_unlocked().unwrap();
        assert!(staging.active);
        assert!(staging.entries.contains_key("a.txt"));
        assert!(
            staging.entries.contains_key("c.txt"),
            "add . should stage untracked files under the repository root"
        );
    }

    #[test]
    fn add_all_dot_stages_untracked_files() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        repo.commit("baseline").unwrap();

        fs::write(dir.path().join("a.txt"), "a v2\n").unwrap();
        fs::write(dir.path().join("c.txt"), "new\n").unwrap();
        repo.add(&[".".into()], false, true).unwrap();

        let staging = repo.load_staging_unlocked().unwrap();
        assert!(staging.active);
        assert!(staging.entries.contains_key("a.txt"));
        assert!(staging.entries.contains_key("c.txt"));
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

        repo.merge_branch_with_resolutions_force("feature", "merge", &[], true, false)
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

        repo.merge_branch_with_resolutions_force("feature", "merge", &[], true, false)
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
        let text = match &files.get("renamed.rs").unwrap().content {
            FileContent::Ast(g) => unparse(g),
            FileContent::Text(t) => t.content.clone(),
            FileContent::Binary(_) => panic!("unexpected binary"),
            FileContent::Symlink(_) => panic!("unexpected symlink"),
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
        let text = match &files.get("dst.txt").unwrap().content {
            FileContent::Text(t) => t.content.clone(),
            _ => panic!(),
        };
        assert_eq!(text, "other content\n");
        assert!(!files.contains_key("src.txt"));
    }

    #[test]
    fn incremental_status_reuses_unchanged_file_reads() {
        use crate::store::walk::{ScanMode, last_scan_metrics};
        use crate::store::working::{load_working_count, reset_load_working_count};
        use std::thread;
        use std::time::Duration;

        let (dir, repo) = sample_repo();
        for i in 0..30 {
            fs::write(
                dir.path().join(format!("file{i}.rs")),
                format!("fn f{i}() {{}}\n"),
            )
            .unwrap();
        }
        repo.commit("many files").unwrap();

        reset_load_working_count();
        repo.status_with_options(ScanOptions::default()).unwrap();
        let first_reads = load_working_count();
        assert!(first_reads >= 30, "first_reads={first_reads}");

        reset_load_working_count();
        repo.status_with_options(ScanOptions::default()).unwrap();
        assert_eq!(last_scan_metrics().mode, Some(ScanMode::Incremental));
        assert_eq!(
            load_working_count(),
            0,
            "unchanged tree should skip content reads"
        );

        fs::write(dir.path().join("file0.rs"), "fn f0() { let x = 1; }\n").unwrap();
        thread::sleep(Duration::from_millis(10));
        reset_load_working_count();
        repo.status_with_options(ScanOptions::default()).unwrap();
        assert_eq!(last_scan_metrics().mode, Some(ScanMode::Incremental));
        assert_eq!(
            load_working_count(),
            1,
            "only the touched file should be read"
        );
    }
}
