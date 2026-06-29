use crate::store::atomic;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const PACKS_DIR: &str = "packs";
const INDEX_FILE: &str = "index.json";
const PACK_MAGIC: &[u8; 8] = b"ASTVCSPK";
const PACK_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackEncoding {
    Zstd,
    DeltaZstd,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackIndexEntry {
    pub pack_file: String,
    pub offset: u64,
    pub length: u32,
    pub compressed_length: u32,
    pub encoding: PackEncoding,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackIndex {
    pub version: u32,
    pub entries: HashMap<String, PackIndexEntry>,
}

impl PackIndex {
    pub fn empty() -> Self {
        Self {
            version: 1,
            entries: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepackReport {
    pub blobs_packed: usize,
    pub loose_removed: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl RepackReport {
    pub fn format_output(&self) -> String {
        format!(
            "repack: packed {} blob(s); removed {} loose file(s); {} -> {} on disk\n",
            self.blobs_packed,
            self.loose_removed,
            format_bytes(self.bytes_before),
            format_bytes(self.bytes_after)
        )
    }
}

pub struct PackStore {
    root: PathBuf,
}

impl PackStore {
    pub fn new(astvcs_root: impl AsRef<Path>) -> Self {
        Self {
            root: astvcs_root.as_ref().join(PACKS_DIR),
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    pub fn load_index(&self) -> Result<PackIndex, String> {
        let path = self.index_path();
        if !path.is_file() {
            return Ok(PackIndex::empty());
        }
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    pub fn save_index(&self, index: &PackIndex) -> Result<(), String> {
        self.ensure_dirs()?;
        atomic::write_atomic_json(&self.index_path(), index).map_err(|e| e.to_string())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.load_index()
            .map(|index| index.entries.contains_key(id))
            .unwrap_or(false)
    }

    pub fn list_ids(&self) -> Result<Vec<String>, String> {
        let index = self.load_index()?;
        let mut ids: Vec<String> = index.entries.keys().cloned().collect();
        ids.sort();
        Ok(ids)
    }

    pub fn on_disk_size(&self, id: &str) -> Result<u64, String> {
        let index = self.load_index()?;
        let entry = index
            .entries
            .get(id)
            .ok_or_else(|| format!("packed blob {id} not in index"))?;
        Ok(u64::from(entry.compressed_length))
    }

    pub fn remove(&self, id: &str) -> Result<u64, String> {
        let mut index = self.load_index()?;
        reencode_dependents(&self.root, id, &mut index)?;
        let Some(entry) = index.entries.remove(id) else {
            return Ok(0);
        };
        self.save_index(&index)?;
        Ok(u64::from(entry.compressed_length))
    }

    pub fn read_bytes(&self, id: &str) -> Result<Vec<u8>, String> {
        let index = self.load_index()?;
        let entry = index
            .entries
            .get(id)
            .ok_or_else(|| format!("packed blob {id} not in index"))?;
        let pack_path = self.root.join(&entry.pack_file);
        let mut file =
            File::open(&pack_path).map_err(|e| format!("open pack {}: {e}", entry.pack_file))?;
        file.seek(SeekFrom::Start(entry.offset))
            .map_err(|e| e.to_string())?;
        let mut compressed = vec![0u8; entry.compressed_length as usize];
        file.read_exact(&mut compressed)
            .map_err(|e| format!("read pack entry {id}: {e}"))?;
        let raw = match entry.encoding {
            PackEncoding::Zstd => decompress_zstd(&compressed)?,
            PackEncoding::DeltaZstd => {
                let base_id = entry
                    .base_id
                    .as_deref()
                    .ok_or_else(|| format!("packed blob {id}: delta entry missing base_id"))?;
                let base = self.read_bytes(base_id)?;
                let delta = decompress_zstd(&compressed)?;
                apply_delta(&base, &delta)?
            }
        };
        verify_blob_hash(id, &raw)?;
        Ok(raw)
    }

    /// Pack loose blobs into a new pack file and update the index atomically.
    pub fn pack_loose_blobs(
        &self,
        blobs: &[(String, Vec<u8>)],
        base_lookup: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Result<RepackReport, String> {
        if blobs.is_empty() {
            return Ok(RepackReport {
                blobs_packed: 0,
                loose_removed: 0,
                bytes_before: 0,
                bytes_after: 0,
            });
        }

        self.ensure_dirs()?;
        let mut index = self.load_index()?;
        let pack_name = next_pack_name(&self.root)?;
        let pack_path = self.root.join(&pack_name);
        let mut pack_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&pack_path)
            .map_err(|e| e.to_string())?;

        write_pack_header(&mut pack_file)?;

        let mut bytes_before = 0u64;
        let mut bytes_after = 0u64;
        let mut packed = 0usize;

        for (id, raw) in blobs {
            verify_blob_hash(id, raw)?;
            bytes_before += raw.len() as u64;

            let base_id = pick_delta_base(id, blobs);
            let base_bytes = base_id
                .as_deref()
                .and_then(|base| base_lookup(base).or_else(|| self.read_bytes(base).ok()));

            let (encoding, compressed, stored_base) =
                encode_blob(raw, base_id.as_deref(), base_bytes.as_deref())?;

            let offset = pack_file
                .seek(SeekFrom::End(0))
                .map_err(|e| e.to_string())?;
            pack_file
                .write_all(&compressed)
                .map_err(|e| e.to_string())?;
            pack_file.sync_all().map_err(|e| e.to_string())?;

            let entry = PackIndexEntry {
                pack_file: pack_name.clone(),
                offset,
                length: raw.len() as u32,
                compressed_length: compressed.len() as u32,
                encoding,
                base_id: stored_base,
            };
            index.entries.insert(id.clone(), entry);
            bytes_after += compressed.len() as u64;
            packed += 1;
        }

        self.save_index(&index)?;
        Ok(RepackReport {
            blobs_packed: packed,
            loose_removed: 0,
            bytes_before,
            bytes_after,
        })
    }

    pub fn verify_all_entries(&self) -> Result<Vec<String>, String> {
        let index = self.load_index()?;
        let mut problems = Vec::new();
        for (id, entry) in &index.entries {
            let pack_path = self.root.join(&entry.pack_file);
            if !pack_path.is_file() {
                problems.push(format!(
                    "packed blob {id}: pack file {} missing",
                    entry.pack_file
                ));
                continue;
            }
            match self.read_bytes(id) {
                Ok(bytes) => {
                    if let Err(e) = verify_blob_hash(id, &bytes) {
                        problems.push(format!("packed blob {id}: {e}"));
                    }
                }
                Err(e) => problems.push(format!("packed blob {id}: {e}")),
            }
        }
        problems.sort();
        Ok(problems)
    }
}

fn write_pack_header(file: &mut File) -> Result<(), String> {
    file.write_all(PACK_MAGIC).map_err(|e| e.to_string())?;
    file.write_all(&PACK_VERSION.to_le_bytes())
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn next_pack_name(packs_dir: &Path) -> Result<String, String> {
    let mut max = 0u32;
    if packs_dir.is_dir() {
        for entry in fs::read_dir(packs_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num) = name
                .strip_prefix("pack-")
                .and_then(|s| s.strip_suffix(".pack"))
                .and_then(|s| s.parse::<u32>().ok())
            {
                max = max.max(num);
            }
        }
    }
    Ok(format!("pack-{:04}.pack", max + 1))
}

fn pick_delta_base(id: &str, batch: &[(String, Vec<u8>)]) -> Option<String> {
    let shard = id.get(..2).unwrap_or("");
    batch
        .iter()
        .filter(|(other, _)| other.as_str() < id && other.get(..2) == Some(shard))
        .map(|(other, _)| other.as_str())
        .max()
        .map(str::to_string)
}

fn encode_blob(
    raw: &[u8],
    base_id: Option<&str>,
    base_bytes: Option<&[u8]>,
) -> Result<(PackEncoding, Vec<u8>, Option<String>), String> {
    let zstd_only = compress_zstd(raw)?;
    let mut best = (PackEncoding::Zstd, zstd_only, None);

    if let (Some(base_id), Some(base)) = (base_id, base_bytes)
        && let Ok(delta) = build_delta(base, raw)
        && let Ok(compressed_delta) = compress_zstd(&delta)
        && compressed_delta.len() < best.1.len()
    {
        best = (
            PackEncoding::DeltaZstd,
            compressed_delta,
            Some(base_id.to_string()),
        );
    }

    Ok(best)
}

fn build_delta(base: &[u8], new: &[u8]) -> Result<Vec<u8>, String> {
    let prefix = common_prefix_len(base, new);
    let suffix = common_suffix_len(base, new, prefix);
    let middle = &new[prefix..new.len().saturating_sub(suffix)];
    let mut out = Vec::with_capacity(8 + middle.len());
    out.extend_from_slice(&(prefix as u32).to_le_bytes());
    out.extend_from_slice(&(suffix as u32).to_le_bytes());
    out.extend_from_slice(middle);
    Ok(out)
}

fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    if delta.len() < 8 {
        return Err("delta payload too short".into());
    }
    let prefix = u32::from_le_bytes(delta[0..4].try_into().unwrap()) as usize;
    let suffix = u32::from_le_bytes(delta[4..8].try_into().unwrap()) as usize;
    if prefix > base.len() || suffix > base.len().saturating_sub(prefix) {
        return Err("delta prefix/suffix out of range".into());
    }
    let middle = &delta[8..];
    let mut out = Vec::with_capacity(prefix + middle.len() + suffix);
    out.extend_from_slice(&base[..prefix]);
    out.extend_from_slice(middle);
    out.extend_from_slice(&base[base.len() - suffix..]);
    Ok(out)
}

fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

fn common_suffix_len(a: &[u8], b: &[u8], prefix: usize) -> usize {
    let a_rest = &a[prefix..];
    let b_rest = &b[prefix..];
    let limit = a_rest.len().min(b_rest.len());
    (0..limit)
        .take_while(|&i| a_rest[a_rest.len() - 1 - i] == b_rest[b_rest.len() - 1 - i])
        .count()
}

fn compress_zstd(data: &[u8]) -> Result<Vec<u8>, String> {
    zstd::encode_all(data, 3).map_err(|e| e.to_string())
}

fn decompress_zstd(data: &[u8]) -> Result<Vec<u8>, String> {
    zstd::decode_all(data).map_err(|e| e.to_string())
}

pub fn verify_blob_hash(id: &str, bytes: &[u8]) -> Result<(), String> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let computed = hex::encode(hasher.finalize());
    if computed != id {
        return Err(format!("hash mismatch: expected {id}, got {computed}"));
    }
    Ok(())
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

fn reencode_dependents(
    packs_root: &Path,
    base_id: &str,
    index: &mut PackIndex,
) -> Result<(), String> {
    let dependents: Vec<String> = index
        .entries
        .iter()
        .filter(|(_, entry)| entry.base_id.as_deref() == Some(base_id))
        .map(|(id, _)| id.clone())
        .collect();
    for dep_id in dependents {
        let raw = read_entry_bytes(packs_root, index, &dep_id)?;
        let entry = index
            .entries
            .get(&dep_id)
            .cloned()
            .ok_or_else(|| format!("packed blob {dep_id}: missing during reencode"))?;
        let compressed = compress_zstd(&raw)?;
        let updated = PackIndexEntry {
            encoding: PackEncoding::Zstd,
            compressed_length: compressed.len() as u32,
            base_id: None,
            ..entry
        };
        let pack_path = packs_root.join(&updated.pack_file);
        let mut file = OpenOptions::new()
            .append(true)
            .open(&pack_path)
            .map_err(|e| e.to_string())?;
        let offset = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        file.write_all(&compressed).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        index
            .entries
            .insert(dep_id, PackIndexEntry { offset, ..updated });
    }
    Ok(())
}

fn read_entry_bytes(packs_root: &Path, index: &PackIndex, id: &str) -> Result<Vec<u8>, String> {
    let entry = index
        .entries
        .get(id)
        .ok_or_else(|| format!("packed blob {id} not in index"))?;
    let pack_path = packs_root.join(&entry.pack_file);
    let mut file =
        File::open(&pack_path).map_err(|e| format!("open pack {}: {e}", entry.pack_file))?;
    file.seek(SeekFrom::Start(entry.offset))
        .map_err(|e| e.to_string())?;
    let mut compressed = vec![0u8; entry.compressed_length as usize];
    file.read_exact(&mut compressed)
        .map_err(|e| format!("read pack entry {id}: {e}"))?;
    match entry.encoding {
        PackEncoding::Zstd => decompress_zstd(&compressed),
        PackEncoding::DeltaZstd => {
            let base_id = entry
                .base_id
                .as_deref()
                .ok_or_else(|| format!("packed blob {id}: delta entry missing base_id"))?;
            let base_entry = index
                .entries
                .get(base_id)
                .ok_or_else(|| format!("packed blob {id}: base {base_id} missing"))?;
            let mut base_file =
                File::open(packs_root.join(&base_entry.pack_file)).map_err(|e| e.to_string())?;
            base_file
                .seek(SeekFrom::Start(base_entry.offset))
                .map_err(|e| e.to_string())?;
            let mut base_compressed = vec![0u8; base_entry.compressed_length as usize];
            base_file
                .read_exact(&mut base_compressed)
                .map_err(|e| e.to_string())?;
            let base = match base_entry.encoding {
                PackEncoding::Zstd => decompress_zstd(&base_compressed)?,
                PackEncoding::DeltaZstd => {
                    return Err(format!(
                        "packed blob {id}: nested delta bases are not supported"
                    ));
                }
            };
            let delta = decompress_zstd(&compressed)?;
            apply_delta(&base, &delta)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn delta_roundtrip_similar_json() {
        let base = br#"{"kind":"text","lines":["fn main() {}"]}"#;
        let new = br#"{"kind":"text","lines":["fn main() { println!(\"hi\"); }"]}"#;
        let delta = build_delta(base, new).unwrap();
        let restored = apply_delta(base, &delta).unwrap();
        assert_eq!(restored, new);
    }

    #[test]
    fn pack_loose_and_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = PackStore::new(dir.path());
        let bytes = br#"{"kind":"text","lines":["hello"]}"#;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let id = hex::encode(hasher.finalize());
        let report = store
            .pack_loose_blobs(&[(id.clone(), bytes.to_vec())], &|_| None)
            .unwrap();
        assert_eq!(report.blobs_packed, 1);
        let loaded = store.read_bytes(&id).unwrap();
        assert_eq!(loaded, bytes);
    }

    #[test]
    fn encode_blob_prefers_delta_when_smaller() {
        let base = vec![b'a'; 10_000];
        let mut new = base.clone();
        new.extend_from_slice(b"extra");
        let (encoding, _, base_id) = encode_blob(&new, Some("base"), Some(&base)).expect("encode");
        assert_eq!(encoding, PackEncoding::DeltaZstd);
        assert_eq!(base_id.as_deref(), Some("base"));
    }

    #[test]
    fn pack_batch_roundtrip_with_optional_delta() {
        let dir = TempDir::new().unwrap();
        let store = PackStore::new(dir.path());
        let base_bytes = br#"{"kind":"text","lines":["version one"]}"#.to_vec();
        let new_bytes = br#"{"kind":"text","lines":["version two"]}"#.to_vec();
        let mut hasher = Sha256::new();
        hasher.update(&base_bytes);
        let base_id = hex::encode(hasher.finalize());
        hasher = Sha256::new();
        hasher.update(&new_bytes);
        let new_id = hex::encode(hasher.finalize());

        store
            .pack_loose_blobs(
                &[
                    (base_id.clone(), base_bytes.clone()),
                    (new_id.clone(), new_bytes.clone()),
                ],
                &|id| {
                    if id == base_id {
                        Some(base_bytes.clone())
                    } else {
                        None
                    }
                },
            )
            .unwrap();

        assert_eq!(store.read_bytes(&base_id).unwrap(), base_bytes);
        assert_eq!(store.read_bytes(&new_id).unwrap(), new_bytes);
    }
}
