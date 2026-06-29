use crate::store::atomic::write_atomic_json;
use crate::store::blobs::{BlobId, BlobStore};
use crate::store::manifest::FileMode;
use crate::store::tracked::TrackedFile;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const STAGING_FILE: &str = "staging.json";

/// One staged path: blob metadata mirroring a manifest entry, or a deletion marker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedEntry {
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<BlobId>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_kind: String,
    #[serde(default)]
    pub mode: FileMode,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl StagedEntry {
    pub fn deletion() -> Self {
        Self {
            deleted: true,
            blob_id: None,
            content_kind: String::new(),
            mode: FileMode::Regular,
        }
    }

    pub fn from_tracked(blob_id: BlobId, content_kind: String, mode: FileMode) -> Self {
        Self {
            deleted: false,
            blob_id: Some(blob_id),
            content_kind,
            mode,
        }
    }
}

/// On-disk staging index. `active` is set on first `add` and stays true until cleared by fsck repair.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagingIndex {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub entries: HashMap<String, StagedEntry>,
}

impl StagingIndex {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn staging_in_use(&self) -> bool {
        self.active && !self.entries.is_empty()
    }
}

pub fn staging_path(astvcs_dir: &Path) -> PathBuf {
    astvcs_dir.join(STAGING_FILE)
}

pub fn load_staging(astvcs_dir: &Path) -> Result<StagingIndex, String> {
    let path = staging_path(astvcs_dir);
    if !path.is_file() {
        return Ok(StagingIndex::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(StagingIndex::default());
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub fn save_staging(astvcs_dir: &Path, index: &StagingIndex) -> Result<(), String> {
    write_atomic_json(&staging_path(astvcs_dir), index)
}

pub fn clear_staging_entries(index: &mut StagingIndex) {
    index.entries.clear();
}

/// Resolve a staged entry to a tracked file (None when staged deletion).
pub fn staged_to_tracked(
    store: &BlobStore,
    entry: &StagedEntry,
) -> Result<Option<TrackedFile>, String> {
    if entry.deleted {
        return Ok(None);
    }
    let blob_id = entry
        .blob_id
        .as_ref()
        .ok_or_else(|| "staged entry missing blob_id".to_string())?;
    let content = store.read(blob_id)?;
    Ok(Some(TrackedFile::new(content, entry.mode)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn deletion_entry_roundtrip() {
        let entry = StagedEntry::deletion();
        let json = serde_json::to_string(&entry).unwrap();
        let loaded: StagedEntry = serde_json::from_str(&json).unwrap();
        assert!(loaded.deleted);
        assert!(loaded.blob_id.is_none());
    }

    #[test]
    fn staging_index_save_load() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();
        let index = StagingIndex {
            active: true,
            entries: HashMap::from([(
                "a.txt".into(),
                StagedEntry::from_tracked("abc".into(), "text".into(), FileMode::Regular),
            )]),
        };
        save_staging(&astvcs, &index).unwrap();
        let loaded = load_staging(&astvcs).unwrap();
        assert_eq!(loaded, index);
    }
}
