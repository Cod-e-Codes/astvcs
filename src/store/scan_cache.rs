use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const SCAN_CACHE_FILE: &str = "scan-cache.json";
pub const SCAN_CACHE_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathStat {
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    pub size: u64,
    pub is_symlink: bool,
    /// Unix permission bits (`mode & 0o777`) when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirStat {
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    /// Immediate child entry count (includes hidden names; excludes `.` and `..`).
    #[serde(default)]
    pub child_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedEntry {
    pub stat: PathStat,
    /// SHA-256 hex digest of on-disk bytes (symlink target text for links).
    pub bytes_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    pub version: u32,
    pub head_state_id: String,
    pub paths: HashMap<String, PathStat>,
    pub dirs: HashMap<String, DirStat>,
    /// Paths whose content matched HEAD at the last status or commit scan.
    #[serde(default)]
    pub verified: HashMap<String, VerifiedEntry>,
}

impl ScanCache {
    pub fn new(head_state_id: impl Into<String>) -> Self {
        Self {
            version: SCAN_CACHE_VERSION,
            head_state_id: head_state_id.into(),
            paths: HashMap::new(),
            dirs: HashMap::new(),
            verified: HashMap::new(),
        }
    }

    pub fn is_valid_for(&self, head_state_id: &str) -> bool {
        self.version == SCAN_CACHE_VERSION && self.head_state_id == head_state_id
    }
}

pub fn scan_cache_path(astvcs_dir: &Path) -> PathBuf {
    astvcs_dir.join(SCAN_CACHE_FILE)
}

pub fn load_scan_cache(astvcs_dir: &Path) -> Result<Option<ScanCache>, String> {
    let path = scan_cache_path(astvcs_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let cache: ScanCache = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(Some(cache))
}

pub fn save_scan_cache(astvcs_dir: &Path, cache: &ScanCache) -> Result<(), String> {
    crate::store::atomic::write_atomic_json(&scan_cache_path(astvcs_dir), cache)
}

pub fn invalidate_scan_cache(astvcs_dir: &Path) -> Result<(), String> {
    let path = scan_cache_path(astvcs_dir);
    if path.is_file() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// True when metadata and raw bytes still match a prior verified snapshot.
pub fn path_verified_unchanged(root: &Path, path: &str, verified: &VerifiedEntry) -> bool {
    let full = root.join(path);
    let Ok(current_stat) = stat_path(&full) else {
        return false;
    };
    if !path_stat_unchanged(&verified.stat, &current_stat) {
        return false;
    }
    match hash_path_bytes(&full) {
        Ok(current_hash) => current_hash == verified.bytes_hash,
        Err(_) => false,
    }
}

pub fn verify_entry_for_path(root: &Path, path: &str) -> Result<VerifiedEntry, String> {
    let full = root.join(path);
    let stat = stat_path(&full)?;
    let bytes_hash = hash_path_bytes(&full)?;
    Ok(VerifiedEntry { stat, bytes_hash })
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn hash_path_bytes(path: &Path) -> Result<String, String> {
    if path.is_symlink() {
        let target = fs::read_link(path).map_err(|e| e.to_string())?;
        return Ok(hash_bytes(target.as_os_str().as_encoded_bytes()));
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    Ok(hash_bytes(&bytes))
}

pub fn stat_path(path: &Path) -> Result<PathStat, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let modified = meta.modified().map_err(|e| e.to_string())?;
    let (mtime_secs, mtime_nanos) = system_time_parts(modified);
    #[cfg(unix)]
    let unix_mode = {
        use std::os::unix::fs::PermissionsExt;
        if meta.is_file() || meta.is_symlink() {
            Some(meta.permissions().mode() & 0o7777)
        } else {
            None
        }
    };
    #[cfg(not(unix))]
    let unix_mode = None;
    Ok(PathStat {
        mtime_secs,
        mtime_nanos,
        size: meta.len(),
        is_symlink: meta.file_type().is_symlink(),
        unix_mode,
    })
}

pub fn stat_dir(path: &Path) -> Result<DirStat, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    let modified = meta.modified().map_err(|e| e.to_string())?;
    let (mtime_secs, mtime_nanos) = system_time_parts(modified);
    let child_count = count_dir_children(path)?;
    Ok(DirStat {
        mtime_secs,
        mtime_nanos,
        child_count,
    })
}

pub fn count_dir_children(path: &Path) -> Result<u32, String> {
    let mut count = 0u32;
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        entry.map_err(|e| e.to_string())?;
        count = count.saturating_add(1);
    }
    Ok(count)
}

pub fn dir_stat_unchanged(cached: &DirStat, current: &DirStat) -> bool {
    cached.mtime_secs == current.mtime_secs
        && cached.mtime_nanos == current.mtime_nanos
        && cached.child_count == current.child_count
}

pub fn path_stat_unchanged(cached: &PathStat, current: &PathStat) -> bool {
    cached.mtime_secs == current.mtime_secs
        && cached.mtime_nanos == current.mtime_nanos
        && cached.size == current.size
        && cached.is_symlink == current.is_symlink
        && cached.unix_mode == current.unix_mode
}

fn system_time_parts(time: SystemTime) -> (u64, u32) {
    use std::time::UNIX_EPOCH;
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    (duration.as_secs(), duration.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::new("abc");
        cache.paths.insert(
            "main.rs".into(),
            PathStat {
                mtime_secs: 1,
                mtime_nanos: 2,
                size: 3,
                is_symlink: false,
                unix_mode: None,
            },
        );
        save_scan_cache(dir.path(), &cache).unwrap();
        let loaded = load_scan_cache(dir.path()).unwrap().unwrap();
        assert_eq!(loaded, cache);
    }

    #[test]
    fn invalidate_removes_cache_file() {
        let dir = TempDir::new().unwrap();
        save_scan_cache(dir.path(), &ScanCache::new("x")).unwrap();
        invalidate_scan_cache(dir.path()).unwrap();
        assert!(load_scan_cache(dir.path()).unwrap().is_none());
    }

    #[test]
    fn verified_detects_content_change_with_unchanged_stat() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("note.txt");
        fs::write(&path, "alpha\n").unwrap();
        let before = verify_entry_for_path(dir.path(), "note.txt").unwrap();
        fs::write(&path, "beta\n").unwrap();
        let stale = VerifiedEntry {
            stat: before.stat,
            bytes_hash: before.bytes_hash,
        };
        assert!(!path_verified_unchanged(dir.path(), "note.txt", &stale));
    }

    #[test]
    fn dir_stat_requires_matching_child_count() {
        let cached = DirStat {
            mtime_secs: 1,
            mtime_nanos: 0,
            child_count: 2,
        };
        let current = DirStat {
            mtime_secs: 1,
            mtime_nanos: 0,
            child_count: 3,
        };
        assert!(!dir_stat_unchanged(&cached, &current));
    }
}
