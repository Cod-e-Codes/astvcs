use crate::frontend::FileContent;
use crate::store::atomic;
use crate::store::pack::{PackStore, RepackReport, verify_blob_hash};
use crate::trace;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub type BlobId = String;

const BLOBS_DIR: &str = "blobs";

pub struct BlobStore {
    astvcs_root: PathBuf,
}

impl BlobStore {
    pub fn new(astvcs_root: impl AsRef<Path>) -> Self {
        Self {
            astvcs_root: astvcs_root.as_ref().to_path_buf(),
        }
    }

    fn blobs_root(&self) -> PathBuf {
        self.astvcs_root.join(BLOBS_DIR)
    }

    fn packs(&self) -> PackStore {
        PackStore::new(&self.astvcs_root)
    }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        fs::create_dir_all(self.blobs_root()).map_err(|e| e.to_string())?;
        self.packs().ensure_dirs()
    }

    pub fn hash_content(content: &FileContent) -> Result<BlobId, String> {
        let bytes = serde_json::to_vec(content).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Ok(hex::encode(hasher.finalize()))
    }

    pub fn write(&self, content: &FileContent) -> Result<BlobId, String> {
        self.ensure_dirs()?;
        let id = Self::hash_content(content)?;
        if self.contains(&id) {
            trace::notice(format!("blob {id}: already stored (deduplicated)"));
            return Ok(id);
        }
        let path = self.blob_path(&id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(content).map_err(|e| e.to_string())?;
        atomic::write_atomic(&path, &bytes).map_err(|e| e.to_string())?;
        trace::notice(format!("blob {id}: wrote {} bytes", bytes.len()));
        Ok(id)
    }

    pub fn read(&self, id: &BlobId) -> Result<FileContent, String> {
        let bytes = self.read_bytes(id)?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub fn contains(&self, id: &BlobId) -> bool {
        self.blob_path(id).exists() || self.packs().contains(id)
    }

    pub fn read_bytes(&self, id: &BlobId) -> Result<Vec<u8>, String> {
        let path = self.blob_path(id);
        if path.is_file() {
            return fs::read(&path).map_err(|e| format!("blob {id}: {e}"));
        }
        self.packs().read_bytes(id)
    }

    /// Store pre-serialized blob bytes when the id matches the content hash.
    pub fn write_bytes(&self, id: &BlobId, bytes: &[u8]) -> Result<(), String> {
        verify_blob_hash(id, bytes)?;
        self.ensure_dirs()?;
        if self.contains(id) {
            trace::notice(format!("blob {id}: already stored (deduplicated)"));
            return Ok(());
        }
        let path = self.blob_path(id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        atomic::write_atomic(&path, bytes).map_err(|e| e.to_string())?;
        trace::notice(format!("blob {id}: imported {} bytes", bytes.len()));
        Ok(())
    }

    /// Returns on-disk byte length when the blob exists (loose file or packed entry).
    pub fn file_size(&self, id: &BlobId) -> Result<u64, String> {
        let path = self.blob_path(id);
        if path.is_file() {
            return fs::metadata(&path)
                .map(|m| m.len())
                .map_err(|e| e.to_string());
        }
        self.packs().on_disk_size(id)
    }

    fn blob_path(&self, id: &BlobId) -> PathBuf {
        let shard = if id.len() >= 2 { &id[..2] } else { "00" };
        self.blobs_root().join(shard).join(format!("{id}.json"))
    }

    /// List every blob id stored on disk (loose and packed).
    pub fn list_all_ids(&self) -> Result<Vec<BlobId>, String> {
        self.ensure_dirs()?;
        let mut ids = Vec::new();
        list_blob_ids_recursive(&self.blobs_root(), &mut ids)?;
        for packed in self.packs().list_ids()? {
            if !ids.iter().any(|id| id == &packed) {
                ids.push(packed);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Remove a blob from loose storage and/or the pack index. Returns bytes reclaimed.
    pub fn remove(&self, id: &BlobId) -> Result<u64, String> {
        let mut reclaimed = 0u64;
        let path = self.blob_path(id);
        if path.is_file() {
            reclaimed += fs::metadata(&path).map_err(|e| e.to_string())?.len();
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        reclaimed += self.packs().remove(id)?;
        Ok(reclaimed)
    }

    /// Pack all loose blobs into pack files and remove the loose copies.
    pub fn repack(&self) -> Result<RepackReport, String> {
        self.ensure_dirs()?;
        let loose_ids = list_loose_ids(&self.blobs_root())?;
        if loose_ids.is_empty() {
            return Ok(RepackReport {
                blobs_packed: 0,
                loose_removed: 0,
                bytes_before: 0,
                bytes_after: 0,
            });
        }

        let mut blobs = Vec::with_capacity(loose_ids.len());
        let mut bytes_before = 0u64;
        for id in &loose_ids {
            let path = self.blob_path(id);
            let bytes = fs::read(&path).map_err(|e| format!("read loose blob {id}: {e}"))?;
            verify_blob_hash(id, &bytes)?;
            bytes_before += bytes.len() as u64;
            blobs.push((id.clone(), bytes));
        }
        blobs.sort_by(|a, b| a.0.cmp(&b.0));

        let store = self.packs();
        let lookup = |id: &str| {
            blobs
                .iter()
                .find(|(blob_id, _)| blob_id == id)
                .map(|(_, bytes)| bytes.clone())
        };
        let mut report = store.pack_loose_blobs(&blobs, &lookup)?;

        let mut loose_removed = 0usize;
        for id in &loose_ids {
            let path = self.blob_path(id);
            if path.is_file() {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
                loose_removed += 1;
            }
        }

        report.loose_removed = loose_removed;
        report.bytes_before = bytes_before;
        Ok(report)
    }
}

fn list_loose_ids(blobs_root: &Path) -> Result<Vec<BlobId>, String> {
    let mut ids = Vec::new();
    list_blob_ids_recursive(blobs_root, &mut ids)?;
    ids.sort();
    Ok(ids)
}

fn list_blob_ids_recursive(dir: &Path, out: &mut Vec<BlobId>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            list_blob_ids_recursive(&path, out)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            out.push(id.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::TextBlob;
    use tempfile::TempDir;

    #[test]
    fn roundtrip_binary_blob() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path());
        let content = FileContent::Binary(crate::frontend::BinaryBlob::new(vec![
            0x89, 0x50, 0x4E, 0x47, 0, 0,
        ]));
        let id = store.write(&content).unwrap();
        let loaded = store.read(&id).unwrap();
        assert_eq!(content, loaded);
    }

    #[test]
    fn deduplicates_identical_content() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path());
        let content = FileContent::Text(TextBlob::new("hello".into()));
        let a = store.write(&content).unwrap();
        let b = store.write(&content).unwrap();
        assert_eq!(a, b);
        assert!(store.contains(&a));
    }

    #[test]
    fn roundtrip_blob() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path());
        let content = FileContent::Text(TextBlob::new("data\n".into()));
        let id = store.write(&content).unwrap();
        let loaded = store.read(&id).unwrap();
        assert_eq!(content, loaded);
    }

    #[test]
    fn repack_preserves_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path());
        let a = store
            .write(&FileContent::Text(TextBlob::new("alpha\n".into())))
            .unwrap();
        let b = store
            .write(&FileContent::Text(TextBlob::new("beta\n".into())))
            .unwrap();
        let report = store.repack().unwrap();
        assert_eq!(report.blobs_packed, 2);
        assert_eq!(report.loose_removed, 2);
        assert!(!store.blob_path(&a).exists());
        assert!(!store.blob_path(&b).exists());
        assert!(store.contains(&a));
        assert!(store.contains(&b));
        assert_eq!(
            store.read(&a).unwrap(),
            FileContent::Text(TextBlob::new("alpha\n".into()))
        );
        assert_eq!(
            store.read(&b).unwrap(),
            FileContent::Text(TextBlob::new("beta\n".into()))
        );
        assert_eq!(store.list_all_ids().unwrap().len(), 2);
    }
}
