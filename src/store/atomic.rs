use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Suffix for in-progress atomic writes. Stray files with this suffix are
/// removed at the start of each locked command when the canonical file exists.
pub const TEMP_SUFFIX: &str = ".astvcs-tmp";

fn temp_path(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    final_path.with_file_name(format!("{file_name}{TEMP_SUFFIX}"))
}

/// Write `contents` to `path` atomically via same-directory rename.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = temp_path(path);
    {
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(contents).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })
}

pub fn write_atomic_text(path: &Path, text: &str) -> Result<(), String> {
    write_atomic(path, text.as_bytes())
}

pub fn write_atomic_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_atomic_text(path, &text)
}

/// List `.astvcs-tmp` files whose canonical target path already exists.
pub fn find_stray_temp_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut stray = Vec::new();
    find_stray_temps_dir(root, &mut stray)?;
    stray.sort();
    Ok(stray)
}

/// Remove leftover temp files from a prior crash. Only deletes a temp when the
/// canonical target path already exists (or the temp has no inferable target).
pub fn cleanup_stray_temp_files(root: &Path) -> Result<(), String> {
    for path in find_stray_temp_files(root)? {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn find_stray_temps_dir(dir: &Path, stray: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            if is_repo_metadata_dir(&path) {
                continue;
            }
            find_stray_temps_dir(&path, stray)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(TEMP_SUFFIX) {
            continue;
        }
        let canonical_name = name.strip_suffix(TEMP_SUFFIX).unwrap_or(name);
        let canonical = path.with_file_name(canonical_name);
        if canonical.exists() {
            stray.push(path);
        }
    }
    Ok(())
}

fn is_repo_metadata_dir(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == ".git" || name == ".astvcs")
}

/// List `.astvcs-tmp` files whose canonical target path does not exist.
pub fn find_orphan_temp_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut orphans = Vec::new();
    find_orphan_temps_dir(root, &mut orphans)?;
    orphans.sort();
    Ok(orphans)
}

fn find_orphan_temps_dir(dir: &Path, orphans: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            if is_repo_metadata_dir(&path) {
                continue;
            }
            find_orphan_temps_dir(&path, orphans)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(TEMP_SUFFIX) {
            continue;
        }
        let canonical_name = name.strip_suffix(TEMP_SUFFIX).unwrap_or(name);
        let canonical = path.with_file_name(canonical_name);
        if !canonical.exists() {
            orphans.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_atomic_replaces_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.txt");
        write_atomic_text(&path, "old").unwrap();
        write_atomic_text(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn cleanup_removes_stray_temp_when_canonical_exists() {
        let dir = TempDir::new().unwrap();
        let canonical = dir.path().join("file.txt");
        fs::write(&canonical, "ok").unwrap();
        fs::write(dir.path().join(format!("file.txt{TEMP_SUFFIX}")), "partial").unwrap();
        cleanup_stray_temp_files(dir.path()).unwrap();
        assert!(!dir.path().join(format!("file.txt{TEMP_SUFFIX}")).exists());
        assert_eq!(fs::read_to_string(&canonical).unwrap(), "ok");
    }

    #[test]
    fn cleanup_leaves_orphan_temp_without_canonical() {
        let dir = TempDir::new().unwrap();
        let orphan = dir.path().join(format!("missing.txt{TEMP_SUFFIX}"));
        fs::write(&orphan, "partial").unwrap();
        cleanup_stray_temp_files(dir.path()).unwrap();
        assert!(orphan.exists());
    }
}
