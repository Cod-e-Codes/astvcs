use crate::store::atomic;
use crate::store::error::{RepoError, RepoResult};
use crate::store::manifest::ManifestMap;
use crate::store::pack::PackStore;
use crate::store::reachability::{Reachability, reachable_from_tips};
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
}

impl GcReport {
    pub fn format_output(&self) -> String {
        if !self.pruned {
            if self.blobs_removed == 0 {
                return "gc dry-run: no unreachable blobs\n".into();
            }
            return format!(
                "gc dry-run: {} unreachable blob(s) (examined {}); would reclaim {}\n",
                self.blobs_removed,
                self.blobs_examined,
                format_bytes(self.bytes_reclaimed)
            );
        }
        if self.blobs_removed == 0 {
            return format!(
                "gc: examined {} blob(s); nothing to prune\n",
                self.blobs_examined
            );
        }
        format!(
            "gc: examined {} blob(s); removed {} unreachable; reclaimed {}\n",
            self.blobs_examined,
            self.blobs_removed,
            format_bytes(self.bytes_reclaimed)
        )
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
    PackCorrupt,
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
            Self::PackCorrupt => "pack corrupt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsckFinding {
    pub kind: FsckKind,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsckReport {
    pub findings: Vec<FsckFinding>,
}

impl FsckReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn format_output(&self) -> String {
        if self.is_clean() {
            return "fsck: repository ok\n".into();
        }
        let mut out = format!("fsck: {} issue(s) found\n", self.findings.len());
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

    /// Remove unreachable blobs. Dry-run unless `prune` is true.
    pub fn gc(&self, prune: bool) -> RepoResult<GcReport> {
        let _lock = self.repo_lock()?;
        let reach = self.reachability_unlocked()?;
        let store = self.blobs_store();
        let on_disk = store.list_all_ids()?;
        let mut report = GcReport {
            blobs_examined: on_disk.len(),
            blobs_removed: 0,
            bytes_reclaimed: 0,
            pruned: prune,
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

        Ok(report)
    }

    /// Pack loose blobs into compressed pack files and remove loose copies.
    pub fn repack(&self) -> RepoResult<super::RepackReport> {
        let _lock = self.repo_lock()?;
        self.blobs_store().repack().map_err(RepoError::from_message)
    }

    /// Check repository consistency without modifying anything.
    pub fn fsck(&self) -> RepoResult<FsckReport> {
        let _lock = self.repo_lock()?;
        let mut findings = Vec::new();

        findings.extend(check_head_branch(self)?);
        findings.extend(check_refs(self)?);
        findings.extend(check_timeline_and_blobs(self)?);
        findings.extend(check_pack_integrity(self)?);
        findings.extend(check_orphan_temps(self.root_path())?);

        let head_result = self.head_state_unlocked();
        let reach = self.reachability_unlocked().ok();
        match head_result {
            Ok(head) => {
                if let Ok(head_manifest) = self.load_manifest_unlocked(&head)
                    && let Ok(index) = read_index(self)
                    && let Some(reach) = reach
                {
                    findings.extend(check_index(&head, &head_manifest, &index, &reach)?);
                }
            }
            Err(e) => {
                if findings
                    .iter()
                    .any(|f| matches!(f.kind, FsckKind::HeadBranchMissing))
                    && let Ok(index) = read_index(self)
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
        Ok(FsckReport { findings })
    }
}

fn read_index(repo: &Repo) -> RepoResult<HashMap<String, IndexEntry>> {
    let path = repo.astvcs_dir().join("index.json");
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path).map_err(|e| RepoError::from_io("read index", e))?;
    serde_json::from_str(&text).map_err(|e| RepoError::other(format!("parse index: {e}")))
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
        let report = repo.gc(false).unwrap();
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

        let report = repo.gc(true).unwrap();
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

        let first = repo.gc(true).unwrap();
        let second = repo.gc(true).unwrap();
        assert!(first.blobs_removed >= 1);
        assert_eq!(second.blobs_removed, 0);
        assert!(second.format_output().contains("nothing to prune"));
    }

    #[test]
    fn fsck_clean_repository() {
        let (dir, repo) = sample_repo();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        repo.commit("init").unwrap();
        let report = repo.fsck().unwrap();
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

        let report = repo.gc(true).unwrap();
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
        let report = repo.fsck().unwrap();
        assert!(report.is_clean());
    }
}
