use crate::diff::{DiffResult, diff_graphs, diff_text};
use crate::frontend::{FileContent, parse_text_or_blob};
use crate::intent;
use crate::merge::{MergeConflict, PathMergeConflict, PathMergeOutcome, merge_path};
use crate::store::blobs::{BlobStore, hash_manifest};
use crate::store::history::{merge_base, walk_history};
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

/// What HEAD points at: a branch name or a detached state id.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HeadTarget {
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
    Untracked,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unchanged => write!(f, "unchanged"),
            Self::Modified => write!(f, "modified"),
            Self::Added => write!(f, "added"),
            Self::Removed => write!(f, "removed"),
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
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let root = path.as_ref().to_path_buf();
        if !root.join(ASTVCS_DIR).is_dir() {
            return Err(format!("not an astvcs repository: {}", root.display()));
        }
        Ok(Self { root })
    }

    pub fn init(path: impl AsRef<Path>) -> Result<Self, String> {
        let root = path.as_ref().to_path_buf();
        let astvcs = root.join(ASTVCS_DIR);
        if astvcs.exists() {
            return Err("repository already exists".into());
        }
        fs::create_dir_all(astvcs.join("refs/heads")).map_err(|e| e.to_string())?;
        fs::create_dir_all(astvcs.join("states")).map_err(|e| e.to_string())?;
        fs::create_dir_all(astvcs.join("timeline")).map_err(|e| e.to_string())?;
        BlobStore::new(&astvcs).ensure_dirs()?;

        write_json(
            &astvcs.join(CONFIG_FILE),
            &RepoConfig {
                version: 2,
                default_branch: "main".into(),
            },
        )?;

        let empty_state = StateId::from("0".repeat(64));
        fs::write(astvcs.join(HEAD_FILE), "main\n").map_err(|e| e.to_string())?;
        fs::write(astvcs.join("refs/heads/main"), format!("{empty_state}\n"))
            .map_err(|e| e.to_string())?;
        write_json(
            &astvcs.join(INDEX_FILE),
            &HashMap::<String, IndexEntry>::new(),
        )?;

        let entry = TimelineEntry {
            id: empty_state.clone(),
            parent: None,
            parents: vec![],
            message: "initial empty state".into(),
            timestamp: now_iso(),
            manifest: HashMap::new(),
            files: None,
        };
        write_json(
            &astvcs.join("timeline").join(format!("{empty_state}.json")),
            &entry,
        )?;
        write_json(
            &astvcs.join("states").join(format!("{empty_state}.json")),
            &HashMap::<String, String>::new(),
        )?;

        trace::notice(format!(
            "init: repository created at {} (branch main -> {empty_state})",
            root.display()
        ));
        Ok(Self { root })
    }

    fn scan_working(&self) -> Result<HashSet<String>, String> {
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

    pub fn head_branch(&self) -> Result<Option<String>, String> {
        match self.read_head_target()? {
            HeadTarget::Branch(name) => Ok(Some(name)),
            HeadTarget::Detached(_) => Ok(None),
        }
    }

    pub fn is_detached(&self) -> Result<bool, String> {
        Ok(matches!(self.read_head_target()?, HeadTarget::Detached(_)))
    }

    pub fn head_state(&self) -> Result<StateId, String> {
        match self.read_head_target()? {
            HeadTarget::Branch(name) => self.branch_state(&name),
            HeadTarget::Detached(id) => Ok(id),
        }
    }

    pub fn branch_state(&self, branch: &str) -> Result<StateId, String> {
        let text = fs::read_to_string(self.astvcs_dir().join("refs/heads").join(branch))
            .map_err(|e| e.to_string())?;
        Ok(text.trim().to_string())
    }

    pub fn list_branches(&self) -> Result<Vec<BranchInfo>, String> {
        let dir = self.astvcs_dir().join("refs/heads");
        let mut branches = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            branches.push(BranchInfo {
                name: name.clone(),
                state_id: self.branch_state(&name)?,
            });
        }
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(branches)
    }

    pub fn create_branch(&self, name: &str, from: Option<&str>) -> Result<(), String> {
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if ref_path.exists() {
            return Err(format!("branch already exists: {name}"));
        }
        let state = match from {
            Some(b) => self.branch_state(b)?,
            None => self.head_state()?,
        };
        fs::write(ref_path, format!("{state}\n")).map_err(|e| e.to_string())?;
        trace::notice(format!("branch: created {name} at state {state}"));
        Ok(())
    }

    /// Remove a branch ref. States remain in the store; only the named ref is deleted.
    pub fn remove_branch(&self, name: &str) -> Result<(), String> {
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if !ref_path.exists() {
            return Err(format!("branch not found: {name}"));
        }
        if self.head_branch()? == Some(name.to_string()) {
            return Err(format!("cannot remove the checked-out branch: {name}"));
        }
        if self.list_branches()?.len() <= 1 {
            return Err("cannot remove the last branch".into());
        }
        fs::remove_file(ref_path).map_err(|e| e.to_string())?;
        trace::notice(format!("branch: removed {name}"));
        Ok(())
    }

    pub fn checkout_branch(&self, name: &str) -> Result<(), String> {
        let ref_path = self.astvcs_dir().join("refs/heads").join(name);
        if !ref_path.exists() {
            return Err(format!("branch not found: {name}"));
        }
        self.write_head_target(&HeadTarget::Branch(name.to_string()))?;
        let state = self.branch_state(name)?;
        trace::notice(format!("checkout: branch {name} -> state {state}"));
        self.materialize_state(&state)
    }

    pub fn load_manifest(&self, state_id: &StateId) -> Result<HashMap<String, String>, String> {
        let path = self
            .astvcs_dir()
            .join("states")
            .join(format!("{state_id}.json"));
        if path.exists() {
            return read_json(&path);
        }
        let entry = self.load_timeline_entry(state_id)?;
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

    pub fn load_state_files(
        &self,
        state_id: &StateId,
    ) -> Result<HashMap<String, FileContent>, String> {
        let manifest = self.load_manifest(state_id)?;
        let store = self.blobs();
        let mut files = HashMap::new();
        for (path, blob_id) in manifest {
            files.insert(path, store.read(&blob_id)?);
        }
        Ok(files)
    }

    pub fn load_timeline_entry(&self, state_id: &StateId) -> Result<TimelineEntry, String> {
        let path = self
            .astvcs_dir()
            .join("timeline")
            .join(format!("{state_id}.json"));
        read_json(&path)
    }

    pub fn history(&self, limit: usize) -> Result<Vec<TimelineEntry>, String> {
        let head = self.head_state()?;
        walk_history(&head, limit, |id| self.load_timeline_entry(id))
    }

    pub fn status(&self) -> Result<WorkingStatus, String> {
        let head = self.head_state()?;
        let head_files = self.load_state_files(&head)?;
        let index: HashMap<String, IndexEntry> = read_json(&self.astvcs_dir().join(INDEX_FILE))?;
        self.check_index_consistency(&head, &head_files, &index);

        let mut entries = HashMap::new();
        let working_files = self.scan_working()?;

        for path in &working_files {
            let disk = read_working_file(&self.root, path)?;
            let current = parse_text_or_blob(path, &disk);
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

        Ok(WorkingStatus { entries })
    }

    pub fn diff_working(&self, path: &str) -> Result<String, String> {
        let head = self.head_state()?;
        let head_files = self.load_state_files(&head)?;
        let disk = read_working_file(&self.root, path)?;
        let working = parse_text_or_blob(path, &disk);
        match head_files.get(path) {
            None => Ok(format!("--- /dev/null\n+++ {path}\n(new file)\n")),
            Some(base) => format_diff(path, base, &working),
        }
    }

    /// Resolve a branch name, remote-tracking ref, or 64-character state id.
    pub fn resolve_state_ref(&self, reference: &str) -> Result<StateId, String> {
        if is_state_id(reference) {
            self.load_timeline_entry(&reference.to_string())?;
            trace::notice(format!("resolved state {reference}"));
            return Ok(reference.to_string());
        }
        let ref_path = self.astvcs_dir().join("refs/heads").join(reference);
        if ref_path.is_file() {
            let id = self.branch_state(reference)?;
            trace::notice(format!("resolved branch {reference} -> state {id}"));
            return Ok(id);
        }
        if let Some((remote, branch)) = reference.split_once('/')
            && let Some(id) = self.read_remote_ref(remote, branch)?
        {
            trace::notice(format!("resolved remote ref {reference} -> state {id}"));
            return Ok(id);
        }
        Err(format!("unknown branch or state: {reference}"))
    }

    /// Lowest common ancestor of two branch names or state ids.
    pub fn merge_base_refs(&self, left: &str, right: &str) -> Result<StateId, String> {
        let left_id = self.resolve_state_ref(left)?;
        let right_id = self.resolve_state_ref(right)?;
        let base = merge_base(&left_id, &right_id, |id| self.load_timeline_entry(id))?;
        trace::notice(format!("merge-base: {left_id} + {right_id} -> {base}"));
        Ok(base)
    }

    pub fn diff_three_way(
        &self,
        base: &StateId,
        left: &StateId,
        right: &StateId,
        path: Option<&str>,
    ) -> Result<String, String> {
        trace::notice(format!(
            "diff three-way: base={base} left={left} right={right}{}",
            path.map(|p| format!(" path={p}")).unwrap_or_default()
        ));
        let base_files = self.load_state_files(base)?;
        let left_files = self.load_state_files(left)?;
        let right_files = self.load_state_files(right)?;

        let paths: Vec<String> = match path {
            Some(p) => {
                if !base_files.contains_key(p)
                    && !left_files.contains_key(p)
                    && !right_files.contains_key(p)
                {
                    return Err(format!("path not tracked in base, left, or right: {p}"));
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

    pub fn plan_merge(&self, branch: &str) -> Result<MergePlan, String> {
        let head = self.head_state()?;
        let other = self.branch_state(branch)?;
        if head == other {
            return Err("already up to date".into());
        }

        let base_id = merge_base(&head, &other, |id| self.load_timeline_entry(id))?;
        trace::notice(format!(
            "merge plan: base={base_id} head={head} other={other}"
        ));
        let base_files = self.load_state_files(&base_id)?;
        let head_files = self.load_state_files(&head)?;
        let other_files = self.load_state_files(&other)?;

        let mut merged_files = base_files.clone();
        let mut conflicts = Vec::new();
        let mut all_paths: HashSet<String> = head_files.keys().cloned().collect();
        all_paths.extend(other_files.keys().cloned());
        all_paths.extend(base_files.keys().cloned());

        for path in all_paths {
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
                    merged_files.remove(&path);
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

    pub fn plan_revert(&self, target_id: &StateId) -> Result<RevertPlan, String> {
        let entry = self.load_timeline_entry(target_id)?;
        if entry.parents.len() > 1 {
            return Err(format!("cannot revert merge state {target_id}"));
        }
        let parent_id = match entry
            .parent
            .clone()
            .or_else(|| entry.parents.first().cloned())
        {
            Some(id) => id,
            None => return Err(format!("cannot revert root state {target_id}")),
        };

        let head = self.head_state()?;
        if !self.is_ancestor_of(target_id, &head)? {
            return Err(format!(
                "state {target_id} is not an ancestor of HEAD {head}"
            ));
        }

        trace::notice(format!(
            "revert plan: target={target_id} parent={parent_id} head={head}"
        ));
        let base_files = self.load_state_files(target_id)?;
        let left_files = self.load_state_files(&parent_id)?;
        let head_files = self.load_state_files(&head)?;

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

    pub fn revert_state(&self, reference: &str, message: &str) -> Result<RevertOutcome, String> {
        let target_id = self.resolve_state_ref(reference)?;
        let plan = self.plan_revert(&target_id)?;
        if !plan.is_clean() {
            trace::warn("revert: aborted due to conflicts");
            return Err(plan.format_conflicts());
        }
        self.finish_revert(&plan, message)
    }

    pub fn revert_state_dry_run(&self, reference: &str) -> Result<RevertPlan, String> {
        let target_id = self.resolve_state_ref(reference)?;
        self.plan_revert(&target_id)
    }

    fn finish_revert(&self, plan: &RevertPlan, message: &str) -> Result<RevertOutcome, String> {
        let head = plan.head_id.clone();
        let head_files = self.load_state_files(&head)?;

        if manifest_unchanged(&head_files, &plan.reverted_files) {
            trace::notice(format!("revert: no changes; state {head} unchanged"));
            return Ok(RevertOutcome {
                state_id: head,
                created: false,
            });
        }

        let parent_files = self.load_state_files(&plan.parent_id)?;
        if manifest_unchanged(&parent_files, &plan.reverted_files) {
            match self.read_head_target()? {
                HeadTarget::Branch(branch) => {
                    fs::write(
                        self.astvcs_dir().join("refs/heads").join(&branch),
                        format!("{}\n", plan.parent_id),
                    )
                    .map_err(|e| e.to_string())?;
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
            self.materialize_state(&plan.parent_id)?;
            trace::notice(format!(
                "revert: restored parent state {} undoing {}",
                plan.parent_id, plan.target_id
            ));
            return Ok(RevertOutcome {
                state_id: plan.parent_id.clone(),
                created: true,
            });
        }

        let state_id = self.persist_state(
            &plan.reverted_files,
            message,
            Some(head.clone()),
            vec![head.clone()],
        )?;
        match self.read_head_target()? {
            HeadTarget::Branch(branch) => {
                fs::write(
                    self.astvcs_dir().join("refs/heads").join(&branch),
                    format!("{state_id}\n"),
                )
                .map_err(|e| e.to_string())?;
                trace::notice(format!("revert: updated branch {branch} -> {state_id}"));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
                trace::notice(format!("revert: detached HEAD -> {state_id}"));
            }
        }
        self.materialize_state(&state_id)?;
        trace::notice(format!(
            "revert: created state {state_id} undoing {}",
            plan.target_id
        ));
        Ok(RevertOutcome {
            state_id,
            created: true,
        })
    }

    pub fn reset(&self, reference: &str, soft: bool, force: bool) -> Result<StateId, String> {
        let target = self.resolve_state_ref(reference)?;
        let prior_head = self.head_state()?;

        if !soft && !force && target != prior_head && !self.working_tree_is_clean()? {
            return Err(
                "working tree has uncommitted changes; commit, soft reset, or pass --force".into(),
            );
        }

        let clobbered_paths: Vec<String> = if !soft && force && !self.working_tree_is_clean()? {
            self.status()?
                .entries
                .iter()
                .filter(|(_, status)| !matches!(status, FileStatus::Unchanged))
                .map(|(path, _)| path.clone())
                .collect()
        } else {
            Vec::new()
        };

        match self.read_head_target()? {
            HeadTarget::Branch(ref branch) => {
                fs::write(
                    self.astvcs_dir().join("refs/heads").join(branch),
                    format!("{target}\n"),
                )
                .map_err(|e| e.to_string())?;
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

        if !soft {
            self.materialize_state(&target)?;
            for path in clobbered_paths {
                trace::warn(format!(
                    "reset --force: discarded uncommitted changes in {path}"
                ));
            }
        }

        Ok(target)
    }

    pub fn diff_state_path(
        &self,
        from: &StateId,
        to: &StateId,
        path: &str,
    ) -> Result<String, String> {
        let from_files = self.load_state_files(from)?;
        let to_files = self.load_state_files(to)?;
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

    pub fn diff_states(&self, from: &StateId, to: &StateId) -> Result<String, String> {
        let from_files = self.load_state_files(from)?;
        let to_files = self.load_state_files(to)?;
        let mut out = String::new();
        let mut paths: HashSet<String> = from_files.keys().cloned().collect();
        paths.extend(to_files.keys().cloned());
        let mut sorted: Vec<_> = paths.into_iter().collect();
        sorted.sort();
        for path in sorted {
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

    pub fn commit(&self, message: &str) -> Result<CommitOutcome, String> {
        let head = self.head_state()?;
        let head_files = self.load_state_files(&head)?;
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

        let state_id = self.persist_state(&new_files, message, Some(head.clone()), vec![head])?;
        match self.read_head_target()? {
            HeadTarget::Branch(branch) => {
                fs::write(
                    self.astvcs_dir().join("refs/heads").join(&branch),
                    format!("{state_id}\n"),
                )
                .map_err(|e| e.to_string())?;
                trace::notice(format!("commit: updated branch {branch} -> {state_id}"));
            }
            HeadTarget::Detached(_) => {
                self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
                trace::notice(format!("commit: detached HEAD -> {state_id}"));
            }
        }
        self.sync_index_to_state(&new_files, &state_id)?;
        trace::notice(format!("commit: created state {state_id}"));
        Ok(CommitOutcome {
            state_id,
            created: true,
        })
    }

    pub fn prepare_merge(
        &self,
        branch: &str,
        resolutions: &[MergeResolution],
    ) -> Result<MergePlan, String> {
        let mut plan = self.plan_merge(branch)?;
        if !resolutions.is_empty() {
            let head_files = self.load_state_files(&plan.head_id)?;
            let other_files = self.load_state_files(&plan.other_id)?;
            apply_merge_resolutions(&mut plan, &head_files, &other_files, resolutions)?;
        }
        Ok(plan)
    }

    pub fn merge_branch(&self, branch: &str, message: &str) -> Result<StateId, String> {
        self.merge_branch_with_resolutions(branch, message, &[])
    }

    pub fn merge_branch_with_resolutions(
        &self,
        branch: &str,
        message: &str,
        resolutions: &[MergeResolution],
    ) -> Result<StateId, String> {
        let plan = self.prepare_merge(branch, resolutions)?;
        if !plan.is_clean() {
            trace::warn("merge: aborted due to conflicts");
            return Err(plan.format_conflicts());
        }
        self.finish_merge(&plan, message)
    }

    fn finish_merge(&self, plan: &MergePlan, message: &str) -> Result<StateId, String> {
        let head = plan.head_id.clone();
        let other = plan.other_id.clone();
        let merged_files = plan.merged_files.clone();

        let state_id = self.persist_state(
            &merged_files,
            message,
            None,
            vec![head.clone(), other.clone()],
        )?;
        let current_branch = self.head_branch()?;
        if let Some(branch) = current_branch {
            fs::write(
                self.astvcs_dir().join("refs/heads").join(&branch),
                format!("{state_id}\n"),
            )
            .map_err(|e| e.to_string())?;
        } else {
            self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
        }
        self.materialize_state(&state_id)?;
        trace::notice(format!(
            "merge: created state {state_id} from {head} + {other}"
        ));
        Ok(state_id)
    }

    /// Write a state's files to the working tree, remove dropped tracked paths, sync index.
    pub fn materialize_state(&self, state_id: &StateId) -> Result<(), String> {
        let files = self.load_state_files(state_id)?;
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
            fs::write(full, content_to_string(content)).map_err(|e| e.to_string())?;
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
        Ok(())
    }

    pub fn checkout_state(&self, state_id: &StateId) -> Result<(), String> {
        self.load_timeline_entry(state_id)?;
        self.write_head_target(&HeadTarget::Detached(state_id.clone()))?;
        trace::notice(format!("checkout: detached state {state_id}"));
        self.materialize_state(state_id)
    }

    pub fn working_tree_is_clean(&self) -> Result<bool, String> {
        Ok(self
            .status()?
            .entries
            .values()
            .all(|s| matches!(s, FileStatus::Unchanged)))
    }

    fn persist_state(
        &self,
        files: &HashMap<String, FileContent>,
        message: &str,
        parent: Option<StateId>,
        parents: Vec<StateId>,
    ) -> Result<StateId, String> {
        let store = self.blobs();
        let mut manifest = HashMap::new();
        for (path, content) in files {
            let blob_id = store.write(content)?;
            manifest.insert(path.clone(), blob_id);
        }
        let state_id = hash_manifest(&manifest);

        write_json(
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
            manifest: manifest.clone(),
            files: None,
        };
        write_json(
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
    ) -> Result<HashMap<String, String>, String> {
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
    ) -> Result<(), String> {
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
        write_json(&self.astvcs_dir().join(INDEX_FILE), &index)
    }

    fn read_head_target(&self) -> Result<HeadTarget, String> {
        let text =
            fs::read_to_string(self.astvcs_dir().join(HEAD_FILE)).map_err(|e| e.to_string())?;
        let line = text.trim();
        if is_state_id(line) {
            Ok(HeadTarget::Detached(line.to_string()))
        } else {
            Ok(HeadTarget::Branch(line.to_string()))
        }
    }

    fn write_head_target(&self, target: &HeadTarget) -> Result<(), String> {
        let line = match target {
            HeadTarget::Branch(name) => name.as_str(),
            HeadTarget::Detached(id) => id.as_str(),
        };
        fs::write(self.astvcs_dir().join(HEAD_FILE), format!("{line}\n")).map_err(|e| e.to_string())
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    pub fn load_config(&self) -> Result<RepoConfig, String> {
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

    pub fn read_blob_bytes(&self, id: &str) -> Result<Vec<u8>, String> {
        self.blobs().read_bytes(&id.to_string())
    }

    pub fn import_blob_bytes(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        self.blobs().write_bytes(&id.to_string(), bytes)
    }

    pub fn import_state_manifest(
        &self,
        state_id: &StateId,
        manifest: &HashMap<String, String>,
    ) -> Result<(), String> {
        if hash_manifest(manifest) != *state_id {
            return Err(format!("state id mismatch for {state_id}"));
        }
        write_json(
            &self
                .astvcs_dir()
                .join("states")
                .join(format!("{state_id}.json")),
            manifest,
        )
    }

    pub fn import_timeline_entry(&self, entry: &TimelineEntry) -> Result<(), String> {
        write_json(
            &self
                .astvcs_dir()
                .join("timeline")
                .join(format!("{}.json", entry.id)),
            entry,
        )
    }

    pub fn write_branch_ref(&self, branch: &str, state_id: &StateId) -> Result<(), String> {
        fs::write(
            self.astvcs_dir().join("refs/heads").join(branch),
            format!("{state_id}\n"),
        )
        .map_err(|e| e.to_string())
    }

    pub fn read_remote_ref(&self, remote: &str, branch: &str) -> Result<Option<StateId>, String> {
        let path = self
            .astvcs_dir()
            .join("refs/remotes")
            .join(remote)
            .join(branch);
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(Some(text.trim().to_string()))
    }

    pub fn write_remote_ref(
        &self,
        remote: &str,
        branch: &str,
        state_id: &StateId,
    ) -> Result<(), String> {
        let path = self
            .astvcs_dir()
            .join("refs/remotes")
            .join(remote)
            .join(branch);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(path, format!("{state_id}\n")).map_err(|e| e.to_string())
    }

    pub fn is_ancestor_of(&self, ancestor: &StateId, descendant: &StateId) -> Result<bool, String> {
        if ancestor == descendant {
            return Ok(true);
        }
        let anc = crate::store::history::ancestors(descendant, |id| self.load_timeline_entry(id))?;
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

fn format_diff(path: &str, old: &FileContent, new: &FileContent) -> Result<String, String> {
    let mut out = format!("--- {path}\n+++ {path}\n");
    out.push_str(&format_mutation_diff(old, new));
    Ok(out)
}

fn read_working_file(root: &Path, rel: &str) -> Result<String, String> {
    fs::read_to_string(root.join(rel)).map_err(|e| e.to_string())
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
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
        let repo = Repo::init(dir.path()).unwrap();
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
        let lca = merge_base(&main_id, &feature_id, |id| repo.load_timeline_entry(id)).unwrap();
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
        let solo_repo = Repo::init(solo.path()).unwrap();
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
        repo.merge_branch("feature", "merge comment and literal").unwrap();

        repo.revert_state(&feature_tip, "drop merged comment").unwrap();
        let text = fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(
            text.contains("sup?") && !text.contains("waddup fool") && !text.contains("//"),
            "revert after merge should keep literal and drop comment: {text}"
        );
        assert!(repo.working_tree_is_clean().unwrap());
    }
}
