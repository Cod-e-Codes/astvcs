use crate::store::blobs::BlobId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// File type metadata tracked beside content-addressed blobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    #[default]
    Regular,
    Executable,
    Symlink,
}

impl FileMode {
    fn hash_tag(&self) -> &'static [u8] {
        match self {
            Self::Regular => b"regular",
            Self::Executable => b"executable",
            Self::Symlink => b"symlink",
        }
    }
}

/// One manifest path entry: blob id plus optional mode metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    pub blob: BlobId,
    pub mode: FileMode,
}

impl ManifestEntry {
    pub fn regular(blob: BlobId) -> Self {
        Self {
            blob,
            mode: FileMode::Regular,
        }
    }

    pub fn with_mode(blob: BlobId, mode: FileMode) -> Self {
        Self { blob, mode }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ManifestEntryRaw {
    Legacy(String),
    Extended {
        blob: String,
        #[serde(default)]
        mode: FileMode,
    },
}

impl From<ManifestEntryRaw> for ManifestEntry {
    fn from(raw: ManifestEntryRaw) -> Self {
        match raw {
            ManifestEntryRaw::Legacy(blob) => Self::regular(blob),
            ManifestEntryRaw::Extended { blob, mode } => Self { blob, mode },
        }
    }
}

impl<'de> Deserialize<'de> for ManifestEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(ManifestEntryRaw::deserialize(deserializer)?.into())
    }
}

impl Serialize for ManifestEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.mode == FileMode::Regular {
            serializer.serialize_str(&self.blob)
        } else {
            #[derive(Serialize)]
            struct Extended<'a> {
                blob: &'a str,
                mode: &'a FileMode,
            }
            Extended {
                blob: &self.blob,
                mode: &self.mode,
            }
            .serialize(serializer)
        }
    }
}

pub type ManifestMap = HashMap<String, ManifestEntry>;

/// Deserialize a manifest map from legacy string values or extended objects.
pub fn deserialize_manifest_map<'de, D>(deserializer: D) -> Result<ManifestMap, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: HashMap<String, ManifestEntryRaw> = HashMap::deserialize(deserializer)?;
    Ok(raw.into_iter().map(|(k, v)| (k, v.into())).collect())
}

/// Content-addressed state id from sorted manifest entries.
///
/// Regular-mode entries hash as `path + NUL + blob` only (legacy-compatible).
/// Non-regular modes append `NUL + mode tag` so mode-only edits change the state id.
pub fn hash_manifest(manifest: &ManifestMap) -> String {
    let mut paths: Vec<_> = manifest.keys().collect();
    paths.sort();
    let mut hasher = Sha256::new();
    for path in paths {
        let entry = manifest.get(path).unwrap();
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.blob.as_bytes());
        if entry.mode != FileMode::Regular {
            hasher.update([0]);
            hasher.update(entry.mode.hash_tag());
        }
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_hash_regular_matches_legacy_string_map() {
        let mut legacy = HashMap::new();
        legacy.insert("a.rs".into(), "abc".into());
        legacy.insert("b.py".into(), "def".into());

        let manifest: ManifestMap = legacy
            .into_iter()
            .map(|(k, v)| (k, ManifestEntry::regular(v)))
            .collect();

        let mut hasher = Sha256::new();
        let mut paths: Vec<_> = manifest.keys().collect();
        paths.sort();
        for path in paths {
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(manifest.get(path).unwrap().blob.as_bytes());
        }
        let legacy_hash = hex::encode(hasher.finalize());
        assert_eq!(hash_manifest(&manifest), legacy_hash);
    }

    #[test]
    fn manifest_hash_differs_on_executable_mode() {
        let blob: BlobId = "sameblob".into();
        let mut regular = ManifestMap::new();
        regular.insert("run.sh".into(), ManifestEntry::regular(blob.clone()));
        let mut executable = ManifestMap::new();
        executable.insert(
            "run.sh".into(),
            ManifestEntry::with_mode(blob, FileMode::Executable),
        );
        assert_ne!(hash_manifest(&regular), hash_manifest(&executable));
    }

    #[test]
    fn manifest_entry_roundtrip_serde() {
        let entry = ManifestEntry::with_mode("deadbeef".into(), FileMode::Executable);
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("executable"));
        let map = ManifestMap::from([("x.sh".into(), entry)]);
        let json = serde_json::to_string(&map).unwrap();
        let loaded: HashMap<String, ManifestEntryRaw> = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn legacy_string_manifest_deserializes() {
        let json = r#"{"main.rs":"abc123"}"#;
        let loaded: ManifestMap = serde_json::from_str(json).unwrap();
        assert_eq!(loaded["main.rs"].blob, "abc123");
        assert_eq!(loaded["main.rs"].mode, FileMode::Regular);
    }
}
