use crate::frontend::FileContent;
use crate::store::atomic;
use crate::trace;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub type BlobId = String;

const BLOBS_DIR: &str = "blobs";

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(astvcs_root: impl AsRef<Path>) -> Self {
        Self {
            root: astvcs_root.as_ref().join(BLOBS_DIR),
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())
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
        let path = self.blob_path(&id);
        if path.exists() {
            trace::notice(format!("blob {id}: already stored (deduplicated)"));
            return Ok(id);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(content).map_err(|e| e.to_string())?;
        atomic::write_atomic(&path, &bytes).map_err(|e| e.to_string())?;
        trace::notice(format!("blob {id}: wrote {} bytes", bytes.len()));
        Ok(id)
    }

    pub fn read(&self, id: &BlobId) -> Result<FileContent, String> {
        let path = self.blob_path(id);
        let bytes = fs::read(&path).map_err(|e| format!("blob {id}: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    pub fn contains(&self, id: &BlobId) -> bool {
        self.blob_path(id).exists()
    }

    pub fn read_bytes(&self, id: &BlobId) -> Result<Vec<u8>, String> {
        let path = self.blob_path(id);
        fs::read(&path).map_err(|e| format!("blob {id}: {e}"))
    }

    /// Store pre-serialized blob bytes when the id matches the content hash.
    pub fn write_bytes(&self, id: &BlobId, bytes: &[u8]) -> Result<(), String> {
        let computed = {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            hex::encode(hasher.finalize())
        };
        if computed != *id {
            return Err(format!("blob id mismatch: expected {id}, got {computed}"));
        }
        self.ensure_dirs()?;
        let path = self.blob_path(id);
        if path.exists() {
            trace::notice(format!("blob {id}: already stored (deduplicated)"));
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        atomic::write_atomic(&path, bytes).map_err(|e| e.to_string())?;
        trace::notice(format!("blob {id}: imported {} bytes", bytes.len()));
        Ok(())
    }

    fn blob_path(&self, id: &BlobId) -> PathBuf {
        let shard = if id.len() >= 2 { &id[..2] } else { "00" };
        self.root.join(shard).join(format!("{id}.json"))
    }
}

pub fn hash_manifest(manifest: &std::collections::HashMap<String, BlobId>) -> String {
    let mut paths: Vec<_> = manifest.keys().collect();
    paths.sort();
    let mut hasher = Sha256::new();
    for path in paths {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(manifest.get(path).unwrap().as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::TextBlob;
    use tempfile::TempDir;

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
    fn manifest_hash_is_stable() {
        let mut m = std::collections::HashMap::new();
        m.insert("a.rs".into(), "abc".into());
        m.insert("b.py".into(), "def".into());
        let h1 = hash_manifest(&m);
        let h2 = hash_manifest(&m);
        assert_eq!(h1, h2);
    }
}
