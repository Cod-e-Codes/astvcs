use crate::store::atomic;
use crate::store::error::{RepoError, RepoResult};
use crate::store::format::CURRENT_FORMAT_VERSION;
use crate::store::manifest::ManifestMap;
use crate::store::pack::PackStore;
use crate::store::reachability::{ROOT_STATE_ID, Reachability, reachable_from_tips};
use crate::store::repo::IndexEntry;
use crate::store::{Repo, StateId};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GcReport {
    pub blobs_examined: usize,
    pub blobs_removed: usize,
    pub bytes_reclaimed: u64,
    pub pruned: bool,
    pub states_examined: usize,
    pub states_removed: usize,
    pub history_bytes_reclaimable: u64,
    pub history_pruned: bool,
}

impl GcReport {
    pub fn format_output(&self) -> String {
        let mut out = String::new();

        if !self.pruned {
            if self.blobs_removed == 0 {
                out.push_str("gc dry-run: no unreachable blobs\n");
            } else {
                out.push_str(&format!(
                    "gc dry-run: {} unreachable blob(s) (examined {}); would reclaim {}\n",
                    self.blobs_removed,
                    self.blobs_examined,
                    format_bytes(self.bytes_reclaimed)
                ));
            }
        } else if self.blobs_removed == 0 {
            out.push_str(&format!(
                "gc: examined {} blob(s); nothing to prune\n",
                self.blobs_examined
            ));
        } else {
            out.push_str(&format!(
                "gc: examined {} blob(s); removed {} unreachable; reclaimed {}\n",
                self.blobs_examined,
                self.blobs_removed,
                format_bytes(self.bytes_reclaimed)
            ));
        }

        if !self.history_pruned {
            if self.states_removed == 0 {
                out.push_str(&format!(
                    "gc: {} state(s) examined; no unreachable history\n",
                    self.states_examined
                ));
            } else {
                out.push_str(&format!(
                    "gc dry-run: {} unreachable state(s) (examined {}); would reclaim {} history (use --prune-history to delete)\n",
                    self.states_removed,
                    self.states_examined,
                    format_bytes(self.history_bytes_reclaimable)
                ));
            }
        } else if self.states_removed == 0 {
            out.push_str(&format!(
                "gc: examined {} state(s); no unreachable history to prune\n",
                self.states_examined
            ));
        } else {
            out.push_str(&format!(
                "gc: examined {} state(s); removed {} unreachable; reclaimed {} history\n",
                self.states_examined,
                self.states_removed,
                format_bytes(self.history_bytes_reclaimable)
            ));
        }

        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsckKind {
    MissingBlob,
    DanglingRef,
    HeadBranchMissing,
    IndexInconsistent,
    OrphanTempFile,
    MissingStateManifest,
    OrphanedStateManifest,
    PackCorrupt,
    UnknownFormatVersion,
}

impl FsckKind {
    fn label(&self) -> &'static str {
        match self {
            Self::MissingBlob => "missing blob",
            Self::DanglingRef => "dangling ref",
            Self::HeadBranchMissing => "HEAD branch missing",
            Self::IndexInconsistent => "index inconsistent",
            Self::OrphanTempFile => "orphan temp file",
            Self::MissingStateManifest => "missing state manifest",
            Self::OrphanedStateManifest => "orphaned state manifest",
            Self::PackCorrupt => "pack corrupt",
            Self::UnknownFormatVersion => "unknown format version",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsckFinding {
    pub kind: FsckKind,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FsckOptions {
    pub repair: bool,
    pub prune_refs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsckRepairKind {
    IndexRewritten,
    DanglingRefPruned,
    OrphanTempRemoved,
}

impl FsckRepairKind {
    fn label(&self) -> &'static str {
        match self {
            Self::IndexRewritten => "index rewritten",
            Self::DanglingRefPruned => "dangling ref pruned",
            Self::OrphanTempRemoved => "orphan temp removed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsckRepair {
    pub kind: FsckRepairKind,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsckReport {
    pub findings: Vec<FsckFinding>,
    pub repairs: Vec<FsckRepair>,
}

impl FsckReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn format_output(&self) -> String {
        let mut out = String::new();
        if !self.repairs.is_empty() {
            out.push_str(&format!("fsck: {} repair(s) applied\n", self.repairs.len()));
            for repair in &self.repairs {
                out.push_str(&format!("  {}: {}\n", repair.kind.label(), repair.detail));
            }
        }
        if self.is_clean() {
            out.push_str("fsck: repository ok\n");
            return out;
        }
        out.push_str(&format!("fsck: {} issue(s) found\n", self.findings.len()));
        for finding in &self.findings {
            out.push_str(&format!("  {}: {}\n", finding.kind.label(), finding.detail));
        }
        out
    }
}

impl Repo {
    fn reachability_unlocked(&self) -> RepoResult<Reachability> {
        let tips = self.ref_tips_unlocked()?;
        reachable_from_tips(
            tips,
            |id| {
                self.load_timeline_entry_unlocked(id)
                    .map_err(|e| e.to_string())
            },
            |id| self.load_manifest_unlocked(id).map_err(|e| e.to_string()),
        )
        .map_err(RepoError::from_message)
    }

    fn ref_tips_unlocked(&self) -> RepoResult<Vec<StateId>> {
        let mut tips = Vec::new();
        for branch in self.list_branches_unlocked()? {
            tips.push(branch.state_id);
        }
        for tag in self.list_tags_unlocked()? {
            tips.push(tag.state_id);
        }
        tips.extend(self.list_remote_ref_tips_unlocked()?);
        if self.is_detached_unlocked()? {
            tips.push(self.head_state_unlocked()?);
        }
        Ok(tips)
    }

    fn list_remote_ref_tips_unlocked(&self) -> RepoResult<Vec<StateId>> {
        let mut tips = Vec::new();
        let remotes_dir = self.astvcs_dir().join("refs/remotes");
        if !remotes_dir.is_dir() {
            return Ok(tips);
        }
        for remote_entry in
            fs::read_dir(&remotes_dir).map_err(|e| RepoError::from_io("read remotes", e))?
        {
            let remote_entry =
                remote_entry.map_err(|e| RepoError::from_io("read remote entry", e))?;
            if !remote_entry
                .file_type()
                .map_err(|e| RepoError::from_io("remote file type", e))?
                .is_dir()
            {
                continue;
            }
            let remote_name = remote_entry.file_name().to_string_lossy().to_string();
            for branch_entry in fs::read_dir(remote_entry.path())
                .map_err(|e| RepoError::from_io("read remote branches", e))?
            {
                let branch_entry =
                    branch_entry.map_err(|e| RepoError::from_io("read branch entry", e))?;
                if !branch_entry
                    .file_type()
                    .map_err(|e| RepoError::from_io("branch file type", e))?
                    .is_file()
                {
                    continue;
                }
                let branch = branch_entry.file_name().to_string_lossy().to_string();
                if let Some(state_id) = self.read_remote_ref_unlocked(&remote_name, &branch)? {
                    tips.push(state_id);
                }
            }
        }
        Ok(tips)
    }

    fn has_timeline_unlocked(&self, state_id: &StateId) -> bool {
        self.astvcs_dir()
            .join("timeline")
            .join(format!("{state_id}.json"))
            .is_file()
    }

    /// Remove unreachable blobs and optionally unreachable state history.
    /// Blob deletion runs only when `prune` is true; state history deletion only
    /// when `prune_history` is true. Default is dry-run for both tiers.
    pub fn gc(&self, prune: bool, prune_history: bool) -> RepoResult<GcReport> {
        let _lock = self.repo_lock()?;
        let reach = self.reachability_unlocked()?;
        let store = self.blobs_store();
        let on_disk = store.list_all_ids()?;
        let mut report = GcReport {
            blobs_examined: on_disk.len(),
            blobs_removed: 0,
            bytes_reclaimed: 0,
            pruned: prune,
            states_examined: 0,
            states_removed: 0,
            history_bytes_reclaimable: 0,
            history_pruned: prune_history,
        };

        for id in on_disk {
            if reach.blobs.contains(&id) {
                continue;
            }
            report.blobs_removed += 1;
            if prune {
                report.bytes_reclaimed += store.remove(&id)?;
            } else {
                report.bytes_reclaimed += store.file_size(&id).unwrap_or(0);
            }
        }

        let state_ids = list_on_disk_state_ids(&self.astvcs_dir())?;
        report.states_examined = state_ids.len();
        for state_id in state_ids {
            if state_id == ROOT_STATE_ID || reach.states.contains(&state_id) {
                continue;
            }
            report.states_removed += 1;
            let bytes = state_history_bytes(&self.astvcs_dir(), &state_id);
            report.history_bytes_reclaimable += bytes;
            if prune_history {
                remove_state_history_files(&self.astvcs_dir(), &state_id)?;
            }
        }

        Ok(report)
    }

    /// Pack loose blobs into compressed pack files and remove loose copies.
    pub fn repack(&self) -> RepoResult<super::RepackReport> {
        let _lock = self.repo_lock()?;
        self.blobs_store().repack().map_err(RepoError::from_message)
    }

    /// Check repository consistency. With `FsckOptions::repair` or `prune_refs`,
    /// applies conservative automatic fixes under the repo lock, then re-checks.
    pub fn fsck(&self, options: FsckOptions) -> RepoResult<FsckReport> {
        let _lock = self.repo_lock()?;
        let mut repairs = Vec::new();
        if options.repair || options.prune_refs {
            repairs.extend(apply_fsck_repairs(self, &options)?);
        }
        let findings = collect_fsck_findings(self)?;
        Ok(FsckReport { findings, repairs })
    }
}

fn collect_fsck_findings(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let mut findings = Vec::new();

    findings.extend(check_head_branch(repo)?);
    findings.extend(check_format_version(repo)?);
    findings.extend(check_refs(repo)?);
    findings.extend(check_timeline_and_blobs(repo)?);
    findings.extend(check_orphaned_state_manifests(repo)?);
    findings.extend(check_pack_integrity(repo)?);
    findings.extend(check_orphan_temps(repo.root_path())?);

    let head_result = repo.head_state_unlocked();
    let reach = repo.reachability_unlocked().ok();
    match head_result {
        Ok(head) => {
            if let Ok(head_manifest) = repo.load_manifest_unlocked(&head)
                && let Ok(index) = read_index(repo)
                && let Some(reach) = reach
            {
                findings.extend(check_index(&head, &head_manifest, &index, &reach)?);
            }
        }
        Err(e) => {
            if findings
                .iter()
                .any(|f| matches!(f.kind, FsckKind::HeadBranchMissing))
                && let Ok(index) = read_index(repo)
                && !index.is_empty()
            {
                findings.push(FsckFinding {
                    kind: FsckKind::IndexInconsistent,
                    detail: format!(
                        "HEAD is invalid ({e}); index.json has {} entr(y/ies)",
                        index.len()
                    ),
                });
            } else if !findings
                .iter()
                .any(|f| matches!(f.kind, FsckKind::HeadBranchMissing))
            {
                findings.push(FsckFinding {
                    kind: FsckKind::IndexInconsistent,
                    detail: format!("cannot resolve HEAD: {e}"),
                });
            }
        }
    }

    findings.sort_by(|a, b| {
        a.kind
            .label()
            .cmp(b.kind.label())
            .then(a.detail.cmp(&b.detail))
    });
    Ok(findings)
}

fn apply_fsck_repairs(repo: &Repo, options: &FsckOptions) -> RepoResult<Vec<FsckRepair>> {
    let mut repairs = Vec::new();

    if options.prune_refs {
        repairs.extend(prune_dangling_refs(repo)?);
    }

    if options.repair {
        let head_findings = check_head_branch(repo)?;
        let head_branch_missing = head_findings
            .iter()
            .any(|f| matches!(f.kind, FsckKind::HeadBranchMissing));
        if head_branch_missing && !repo.list_branches_unlocked()?.is_empty() {
            return Err(RepoError::integrity_check(
                "fsck --repair refused: HEAD names a missing branch while other branches exist; \
                 update HEAD manually",
            ));
        }

        if let Ok(head) = repo.head_state_unlocked()
            && repo.has_timeline_unlocked(&head)
            && let Ok(head_manifest) = repo.load_manifest_unlocked(&head)
            && let Ok(index) = read_index(repo)
        {
            let reach = repo.reachability_unlocked().ok();
            if index_inconsistent_with_head(&head, &head_manifest, &index, reach.as_ref()) {
                repo.repair_index_from_head_unlocked(&head)?;
                repairs.push(FsckRepair {
                    kind: FsckRepairKind::IndexRewritten,
                    detail: format!("rewrote index.json from HEAD state {head}"),
                });
            }
        }

        for path in
            atomic::find_stray_temp_files(repo.root_path()).map_err(RepoError::from_message)?
        {
            fs::remove_file(&path).map_err(|e| RepoError::from_io("remove stray temp", e))?;
            repairs.push(FsckRepair {
                kind: FsckRepairKind::OrphanTempRemoved,
                detail: path.display().to_string(),
            });
        }
    }

    repairs.sort_by(|a, b| {
        a.kind
            .label()
            .cmp(b.kind.label())
            .then(a.detail.cmp(&b.detail))
    });
    Ok(repairs)
}

fn index_inconsistent_with_head(
    head: &StateId,
    head_manifest: &ManifestMap,
    index: &HashMap<String, IndexEntry>,
    reach: Option<&Reachability>,
) -> bool {
    if let Some(reach) = reach
        && !reach.states.contains(head)
    {
        return true;
    }
    index
        .iter()
        .any(|(path, entry)| entry.state_id != *head || !head_manifest.contains_key(path))
}

fn prune_dangling_refs(repo: &Repo) -> RepoResult<Vec<FsckRepair>> {
    let mut repairs = Vec::new();
    for branch in repo.list_branches_unlocked()? {
        if repo.has_timeline_unlocked(&branch.state_id) {
            continue;
        }
        let path = repo.astvcs_dir().join("refs/heads").join(&branch.name);
        if path.is_file() {
            fs::remove_file(&path).map_err(|e| RepoError::from_io("remove dangling ref", e))?;
            repairs.push(FsckRepair {
                kind: FsckRepairKind::DanglingRefPruned,
                detail: format!(
                    "removed refs/heads/{} (pointed to {} with no timeline entry)",
                    branch.name, branch.state_id
                ),
            });
        }
    }

    for tag in repo.list_tags_unlocked()? {
        if repo.has_timeline_unlocked(&tag.state_id) {
            continue;
        }
        let path = repo.astvcs_dir().join("refs/tags").join(&tag.name);
        if path.is_file() {
            fs::remove_file(&path).map_err(|e| RepoError::from_io("remove dangling tag", e))?;
            repairs.push(FsckRepair {
                kind: FsckRepairKind::DanglingRefPruned,
                detail: format!(
                    "removed refs/tags/{} (pointed to {} with no timeline entry)",
                    tag.name, tag.state_id
                ),
            });
        }
    }

    let remotes_dir = repo.astvcs_dir().join("refs/remotes");
    if remotes_dir.is_dir() {
        for remote_entry in
            fs::read_dir(&remotes_dir).map_err(|e| RepoError::from_io("read remotes", e))?
        {
            let remote_entry =
                remote_entry.map_err(|e| RepoError::from_io("read remote entry", e))?;
            if !remote_entry
                .file_type()
                .map_err(|e| RepoError::from_io("remote file type", e))?
                .is_dir()
            {
                continue;
            }
            let remote = remote_entry.file_name().to_string_lossy().to_string();
            for branch_entry in fs::read_dir(remote_entry.path())
                .map_err(|e| RepoError::from_io("read remote branches", e))?
            {
                let branch_entry =
                    branch_entry.map_err(|e| RepoError::from_io("read branch entry", e))?;
                if !branch_entry
                    .file_type()
                    .map_err(|e| RepoError::from_io("branch file type", e))?
                    .is_file()
                {
                    continue;
                }
                let branch = branch_entry.file_name().to_string_lossy().to_string();
                let text = fs::read_to_string(branch_entry.path())
                    .map_err(|e| RepoError::from_io("read remote ref", e))?;
                let state_id = text.trim().to_string();
                if repo.has_timeline_unlocked(&state_id) {
                    continue;
                }
                fs::remove_file(branch_entry.path())
                    .map_err(|e| RepoError::from_io("remove dangling remote ref", e))?;
                repairs.push(FsckRepair {
                    kind: FsckRepairKind::DanglingRefPruned,
                    detail: format!(
                        "removed refs/remotes/{remote}/{branch} (pointed to {state_id} with no timeline entry)"
                    ),
                });
            }
        }
    }

    Ok(repairs)
}

fn read_index(repo: &Repo) -> RepoResult<HashMap<String, IndexEntry>> {
    let path = repo.astvcs_dir().join("index.json");
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path).map_err(|e| RepoError::from_io("read index", e))?;
    serde_json::from_str(&text).map_err(|e| RepoError::other(format!("parse index: {e}")))
}

fn check_format_version(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let path = repo.astvcs_dir().join(crate::store::repo::CONFIG_FILE);
    let config: crate::store::repo::RepoConfig = crate::store::repo::read_json_unlocked(&path)?;
    if config.format_version > CURRENT_FORMAT_VERSION {
        Ok(vec![FsckFinding {
            kind: FsckKind::UnknownFormatVersion,
            detail: format!(
                "config.json format_version {} is newer than supported version {}",
                config.format_version, CURRENT_FORMAT_VERSION
            ),
        }])
    } else {
        Ok(vec![])
    }
}

fn check_head_branch(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let mut findings = Vec::new();
    let head_path = repo.astvcs_dir().join("HEAD");
    let text = fs::read_to_string(&head_path).map_err(|e| e.to_string())?;
    let line = text.trim();
    if is_state_id(line) {
        return Ok(findings);
    }
    let ref_path = repo.astvcs_dir().join("refs/heads").join(line);
    if !ref_path.is_file() {
        findings.push(FsckFinding {
            kind: FsckKind::HeadBranchMissing,
            detail: format!("HEAD names branch '{line}' but refs/heads/{line} is missing"),
        });
    }
    Ok(findings)
}

fn check_refs(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let mut findings = Vec::new();
    for branch in repo.list_branches_unlocked()? {
        if !repo.has_timeline_unlocked(&branch.state_id) {
            findings.push(FsckFinding {
                kind: FsckKind::DanglingRef,
                detail: format!(
                    "refs/heads/{} points to {} with no timeline entry",
                    branch.name, branch.state_id
                ),
            });
        }
    }
    for tag in repo.list_tags_unlocked()? {
        if !repo.has_timeline_unlocked(&tag.state_id) {
            findings.push(FsckFinding {
                kind: FsckKind::DanglingRef,
                detail: format!(
                    "refs/tags/{} points to {} with no timeline entry",
                    tag.name, tag.state_id
                ),
            });
        }
    }
    let remotes_dir = repo.astvcs_dir().join("refs/remotes");
    if remotes_dir.is_dir() {
        for remote_entry in fs::read_dir(&remotes_dir).map_err(|e| e.to_string())? {
            let remote_entry = remote_entry.map_err(|e| e.to_string())?;
            if !remote_entry
                .file_type()
                .map_err(|e| e.to_string())?
                .is_dir()
            {
                continue;
            }
            let remote = remote_entry.file_name().to_string_lossy().to_string();
            for branch_entry in fs::read_dir(remote_entry.path()).map_err(|e| e.to_string())? {
                let branch_entry = branch_entry.map_err(|e| e.to_string())?;
                if !branch_entry
                    .file_type()
                    .map_err(|e| e.to_string())?
                    .is_file()
                {
                    continue;
                }
                let branch = branch_entry.file_name().to_string_lossy().to_string();
                let text = fs::read_to_string(branch_entry.path()).map_err(|e| e.to_string())?;
                let state_id = text.trim().to_string();
                if !repo.has_timeline_unlocked(&state_id) {
                    findings.push(FsckFinding {
                        kind: FsckKind::DanglingRef,
                        detail: format!(
                            "refs/remotes/{remote}/{branch} points to {state_id} with no timeline entry"
                        ),
                    });
                }
            }
        }
    }
    Ok(findings)
}

fn list_on_disk_state_ids(astvcs_dir: &Path) -> RepoResult<std::collections::HashSet<StateId>> {
    use std::collections::HashSet;
    let mut ids = HashSet::new();
    for subdir in ["timeline", "states"] {
        let dir = astvcs_dir.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&dir).map_err(|e| RepoError::from_io("read state dir", e))? {
            let entry = entry.map_err(|e| RepoError::from_io("read state entry", e))?;
            if !entry
                .file_type()
                .map_err(|e| RepoError::from_io("state file type", e))?
                .is_file()
            {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(state_id) = name.strip_suffix(".json") else {
                continue;
            };
            ids.insert(state_id.to_string());
        }
    }
    Ok(ids)
}

fn state_history_path(astvcs_dir: &Path, subdir: &str, state_id: &StateId) -> std::path::PathBuf {
    astvcs_dir.join(subdir).join(format!("{state_id}.json"))
}

fn state_history_bytes(astvcs_dir: &Path, state_id: &StateId) -> u64 {
    let mut bytes = 0;
    for subdir in ["timeline", "states"] {
        let path = state_history_path(astvcs_dir, subdir, state_id);
        if let Ok(meta) = path.metadata() {
            bytes += meta.len();
        }
    }
    bytes
}

fn remove_state_history_files(astvcs_dir: &Path, state_id: &StateId) -> RepoResult<()> {
    for subdir in ["timeline", "states"] {
        let path = state_history_path(astvcs_dir, subdir, state_id);
        if path.is_file() {
            fs::remove_file(&path).map_err(|e| RepoError::from_io("remove state history", e))?;
        }
    }
    Ok(())
}

fn check_orphaned_state_manifests(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let mut findings = Vec::new();
    let states_dir = repo.astvcs_dir().join("states");
    if !states_dir.is_dir() {
        return Ok(findings);
    }
    for entry in fs::read_dir(&states_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(state_id) = name.strip_suffix(".json") else {
            continue;
        };
        if !repo.has_timeline_unlocked(&state_id.to_string()) {
            findings.push(FsckFinding {
                kind: FsckKind::OrphanedStateManifest,
                detail: format!("states/{state_id}.json has no timeline entry"),
            });
        }
    }
    Ok(findings)
}

fn check_timeline_and_blobs(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let mut findings = Vec::new();
    let timeline_dir = repo.astvcs_dir().join("timeline");
    if !timeline_dir.is_dir() {
        return Ok(findings);
    }
    for entry in fs::read_dir(&timeline_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(state_id) = name.strip_suffix(".json") else {
            continue;
        };
        let state_id = state_id.to_string();
        let manifest = match repo.load_manifest_unlocked(&state_id) {
            Ok(m) => m,
            Err(e) => {
                findings.push(FsckFinding {
                    kind: FsckKind::MissingStateManifest,
                    detail: format!("state {state_id}: {e}"),
                });
                continue;
            }
        };
        for (path, entry) in manifest {
            if !repo.blobs_store().contains(&entry.blob) {
                findings.push(FsckFinding {
                    kind: FsckKind::MissingBlob,
                    detail: format!("state {state_id} path {path}: blob {} missing", entry.blob),
                });
            }
        }
    }
    Ok(findings)
}

fn check_pack_integrity(repo: &Repo) -> RepoResult<Vec<FsckFinding>> {
    let packs = PackStore::new(repo.astvcs_dir());
    let problems = packs
        .verify_all_entries()
        .map_err(RepoError::from_message)?;
    Ok(problems
        .into_iter()
        .map(|detail| FsckFinding {
            kind: FsckKind::PackCorrupt,
            detail,
        })
        .collect())
}

fn check_index(
    head: &StateId,
    head_manifest: &ManifestMap,
    index: &HashMap<String, IndexEntry>,
    reach: &Reachability,
) -> Result<Vec<FsckFinding>, String> {
    let mut findings = Vec::new();
    if !reach.states.contains(head) {
        findings.push(FsckFinding {
            kind: FsckKind::IndexInconsistent,
            detail: format!("HEAD state {head} is not reachable from any ref tip"),
        });
    }
    for (path, entry) in index {
        if entry.state_id != *head {
            findings.push(FsckFinding {
                kind: FsckKind::IndexInconsistent,
                detail: format!(
                    "index[{path}] state_id {} differs from HEAD {head}",
                    entry.state_id
                ),
            });
        }
        if !head_manifest.contains_key(path) {
            findings.push(FsckFinding {
                kind: FsckKind::IndexInconsistent,
                detail: format!("index tracks {path} but HEAD manifest has no such path"),
            });
        }
    }
    Ok(findings)
}

fn check_orphan_temps(root: &Path) -> RepoResult<Vec<FsckFinding>> {
    let mut findings = Vec::new();
    for path in atomic::find_orphan_temp_files(root).map_err(RepoError::from_message)? {
        findings.push(FsckFinding {
            kind: FsckKind::OrphanTempFile,
            detail: path.display().to_string(),
        });
    }
    Ok(findings)
}

fn is_state_id(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Repo;
    use crate::store::error::RepoErrorKind;
    use crate::store::reachability::ROOT_STATE_ID;
    use tempfile::TempDir;

    fn sample_repo() -> (TempDir, Repo) {
        let dir = TempDir::new().unwrap();
        let repo = Repo::init_with_identity(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn gc_no_unreachable_is_noop() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        let report = repo.gc(false, false).unwrap();
        assert_eq!(report.blobs_removed, 0);
        assert!(report.format_output().contains("no unreachable"));
    }

    #[test]
    fn gc_preserves_remote_tracking_blobs() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("kept", None).unwrap();
        repo.checkout_branch("kept").unwrap();
        fs::write(dir.path().join("note.txt"), "remote kept\n").unwrap();
        let kept_tip = repo.commit("kept commit").unwrap().state_id;
        repo.create_branch("orphan", None).unwrap();
        repo.checkout_branch("orphan").unwrap();
        fs::write(dir.path().join("orphan.txt"), "drop me\n").unwrap();
        let _orphan_tip = repo.commit("orphan commit").unwrap().state_id;
        repo.checkout_branch("main").unwrap();
        fs::create_dir_all(repo.astvcs_dir().join("refs/remotes/origin")).unwrap();
        repo.write_remote_ref("origin", "kept", &kept_tip).unwrap();
        repo.remove_branch("kept").unwrap();
        repo.remove_branch("orphan").unwrap();

        let kept_blob = repo
            .load_manifest(&kept_tip)
            .unwrap()
            .get("note.txt")
            .map(|e| e.blob.clone())
            .unwrap();
        assert!(repo.blobs_store().contains(&kept_blob));

        let report = repo.gc(true, false).unwrap();
        assert!(report.blobs_removed >= 1);
        assert!(repo.blobs_store().contains(&kept_blob));
        assert!(!repo.blobs_store().list_all_ids().unwrap().is_empty());
    }

    #[test]
    fn gc_twice_is_idempotent() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("temp", None).unwrap();
        repo.checkout_branch("temp").unwrap();
        fs::write(dir.path().join("temp.txt"), "temporary\n").unwrap();
        repo.commit("temp only").unwrap();
        repo.checkout_branch("main").unwrap();
        repo.remove_branch("temp").unwrap();

        let first = repo.gc(true, false).unwrap();
        let second = repo.gc(true, false).unwrap();
        assert!(first.blobs_removed >= 1);
        assert_eq!(second.blobs_removed, 0);
        assert!(second.format_output().contains("nothing to prune"));
    }

    #[test]
    fn fsck_clean_repository() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        let report = repo.fsck(FsckOptions::default()).unwrap();
        assert!(report.is_clean());
        assert!(report.format_output().contains("repository ok"));
    }

    #[test]
    fn gc_preserves_packed_blobs() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("kept", None).unwrap();
        repo.checkout_branch("kept").unwrap();
        fs::write(dir.path().join("note.txt"), "remote kept\n").unwrap();
        let kept_tip = repo.commit("kept commit").unwrap().state_id;
        repo.create_branch("orphan", None).unwrap();
        repo.checkout_branch("orphan").unwrap();
        fs::write(dir.path().join("orphan.txt"), "drop me\n").unwrap();
        let _orphan_tip = repo.commit("orphan commit").unwrap().state_id;
        repo.checkout_branch("main").unwrap();
        fs::create_dir_all(repo.astvcs_dir().join("refs/remotes/origin")).unwrap();
        repo.write_remote_ref("origin", "kept", &kept_tip).unwrap();
        repo.remove_branch("kept").unwrap();
        repo.remove_branch("orphan").unwrap();

        let kept_blob = repo
            .load_manifest(&kept_tip)
            .unwrap()
            .get("note.txt")
            .map(|e| e.blob.clone())
            .unwrap();

        repo.repack().unwrap();
        let shard = &kept_blob[..2];
        let loose_path = repo
            .astvcs_dir()
            .join("blobs")
            .join(shard)
            .join(format!("{kept_blob}.json"));
        assert!(!loose_path.exists());
        assert!(repo.blobs_store().contains(&kept_blob));

        let report = repo.gc(true, false).unwrap();
        assert!(report.blobs_removed >= 1);
        assert!(repo.blobs_store().contains(&kept_blob));
        assert!(!repo.blobs_store().list_all_ids().unwrap().is_empty());
    }

    #[test]
    fn fsck_clean_after_repack() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn hi() {}\n").unwrap();
        repo.commit("second").unwrap();
        repo.repack().unwrap();
        let report = repo.fsck(FsckOptions::default()).unwrap();
        assert!(report.is_clean());
    }

    #[test]
    fn gc_prune_history_idempotent() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("temp", None).unwrap();
        repo.checkout_branch("temp").unwrap();
        fs::write(dir.path().join("temp.txt"), "temporary\n").unwrap();
        repo.commit("temp only").unwrap();
        repo.checkout_branch("main").unwrap();
        repo.remove_branch("temp").unwrap();

        let first = repo.gc(true, true).unwrap();
        let second = repo.gc(true, true).unwrap();
        assert!(first.states_removed >= 1);
        assert_eq!(second.states_removed, 0);
        assert!(
            second
                .format_output()
                .contains("no unreachable history to prune")
        );
    }

    #[test]
    fn gc_preserves_unreachable_states_until_prune_history() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("base").unwrap();
        repo.create_branch("temp", None).unwrap();
        repo.checkout_branch("temp").unwrap();
        fs::write(dir.path().join("temp.txt"), "keep until prune\n").unwrap();
        let orphan_tip = repo.commit("temp only").unwrap().state_id;
        repo.checkout_branch("main").unwrap();
        repo.remove_branch("temp").unwrap();

        assert!(repo.load_timeline_entry(&orphan_tip).is_ok());
        assert!(repo.load_manifest(&orphan_tip).is_ok());

        let dry = repo.gc(false, false).unwrap();
        assert!(dry.states_removed >= 1);
        assert!(repo.load_timeline_entry(&orphan_tip).is_ok());

        repo.gc(false, true).unwrap();
        assert!(repo.load_timeline_entry(&orphan_tip).is_err());
        assert!(repo.load_manifest(&orphan_tip).is_err());
        assert!(repo.checkout_state(&orphan_tip).is_err());
    }

    #[test]
    fn gc_prune_history_does_not_remove_reachable_states() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let tip = repo.commit("base").unwrap().state_id;
        repo.create_branch("temp", None).unwrap();
        repo.checkout_branch("temp").unwrap();
        fs::write(dir.path().join("temp.txt"), "orphan\n").unwrap();
        repo.commit("temp only").unwrap();
        repo.checkout_branch("main").unwrap();
        repo.remove_branch("temp").unwrap();

        let report = repo.gc(true, true).unwrap();
        assert!(report.states_removed >= 1);
        assert!(repo.load_timeline_entry(&tip).is_ok());
        assert!(repo.load_manifest(&tip).is_ok());
        assert!(repo.load_timeline_entry(&ROOT_STATE_ID.to_string()).is_ok());
    }

    #[test]
    fn fsck_repair_fixes_index_inconsistency() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        let head = repo.head_state().unwrap();
        fs::write(
            dir.path().join(".astvcs/index.json"),
            r#"{
  "main.rs": {
    "state_id": "0000000000000000000000000000000000000000000000000000000000000000",
    "content_kind": "text"
  }
}"#,
        )
        .unwrap();

        let before = repo.fsck(FsckOptions::default()).unwrap();
        assert!(!before.is_clean());
        assert!(
            before
                .findings
                .iter()
                .any(|f| matches!(f.kind, FsckKind::IndexInconsistent))
        );

        let repaired = repo
            .fsck(FsckOptions {
                repair: true,
                prune_refs: false,
            })
            .unwrap();
        assert!(
            repaired
                .repairs
                .iter()
                .any(|r| matches!(r.kind, FsckRepairKind::IndexRewritten))
        );

        let after = repo.fsck(FsckOptions::default()).unwrap();
        assert!(after.is_clean());

        let index = read_index(&repo).unwrap();
        assert_eq!(index.get("main.rs").unwrap().state_id, head);
    }

    #[test]
    fn fsck_repair_refuses_ambiguous_head() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        fs::write(dir.path().join(".astvcs/HEAD"), "ghost\n").unwrap();

        let report = repo.fsck(FsckOptions::default()).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.kind, FsckKind::HeadBranchMissing))
        );

        let err = repo
            .fsck(FsckOptions {
                repair: true,
                prune_refs: false,
            })
            .unwrap_err();
        assert_eq!(err.kind, RepoErrorKind::IntegrityCheck);
        assert!(err.message.contains("fsck --repair refused"));
    }

    #[test]
    fn fsck_prune_refs_removes_dangling_ref() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        fs::write(
            dir.path().join(".astvcs/refs/heads/dangling"),
            format!("{}\n", "f".repeat(64)),
        )
        .unwrap();

        let before = repo.fsck(FsckOptions::default()).unwrap();
        assert!(
            before
                .findings
                .iter()
                .any(|f| matches!(f.kind, FsckKind::DanglingRef))
        );
        assert!(dir.path().join(".astvcs/refs/heads/dangling").is_file());
        assert!(dir.path().join(".astvcs/refs/heads/main").is_file());

        let pruned = repo
            .fsck(FsckOptions {
                repair: false,
                prune_refs: true,
            })
            .unwrap();
        assert!(
            pruned
                .repairs
                .iter()
                .any(|r| matches!(r.kind, FsckRepairKind::DanglingRefPruned))
        );
        assert!(!dir.path().join(".astvcs/refs/heads/dangling").exists());
        assert!(dir.path().join(".astvcs/refs/heads/main").is_file());

        let after = repo.fsck(FsckOptions::default()).unwrap();
        assert!(after.is_clean());
    }

    #[test]
    fn fsck_repair_combined_scenario() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.path().join("note.txt"), "data\n").unwrap();
        repo.commit("init").unwrap();
        fs::write(
            dir.path().join(".astvcs/index.json"),
            r#"{
  "main.rs": {
    "state_id": "0000000000000000000000000000000000000000000000000000000000000000",
    "content_kind": "text"
  }
}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join(".astvcs/refs/heads/dangling"),
            format!("{}\n", "f".repeat(64)),
        )
        .unwrap();
        fs::write(dir.path().join("note.txt"), "data\n").unwrap();
        fs::write(dir.path().join("note.txt.astvcs-tmp"), "partial").unwrap();

        let report = repo
            .fsck(FsckOptions {
                repair: true,
                prune_refs: true,
            })
            .unwrap();
        assert!(report.is_clean(), "{}", report.format_output());
    }

    #[test]
    fn fsck_warns_on_unknown_format_version() {
        let (dir, repo) = sample_repo();
        let config_path = dir.path().join(".astvcs").join("config.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        value["format_version"] = serde_json::json!(99);
        fs::write(&config_path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let report = repo.fsck(FsckOptions::default()).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f.kind, FsckKind::UnknownFormatVersion)),
            "{}",
            report.format_output()
        );
        assert!(!report.is_clean());
    }
}
