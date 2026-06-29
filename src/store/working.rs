use crate::frontend::{FileContent, SymlinkBlob, load_working_content};
use crate::store::manifest::FileMode;
use crate::store::tracked::TrackedFile;
use std::cell::RefCell;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

thread_local! {
    static LOAD_COUNT: RefCell<usize> = const { RefCell::new(0) };
}

/// Number of [`load_working_tracked`] calls on this thread since the last reset.
#[cfg(test)]
pub fn load_working_count() -> usize {
    LOAD_COUNT.with(|c| *c.borrow())
}

/// Reset the [`load_working_tracked`] counter (for tests).
#[cfg(test)]
pub fn reset_load_working_count() {
    LOAD_COUNT.with(|c| *c.borrow_mut() = 0);
}

/// Load a tracked path from the working tree (regular file, executable, or symlink).
pub fn load_working_tracked(root: &Path, rel: &str) -> Result<TrackedFile, String> {
    LOAD_COUNT.with(|c| *c.borrow_mut() += 1);
    let full = root.join(rel);
    if full.is_symlink() {
        let target = fs::read_link(&full).map_err(|e| format!("read symlink {rel}: {e}"))?;
        let target_str = target.to_string_lossy().into_owned();
        return Ok(TrackedFile::new(
            FileContent::Symlink(SymlinkBlob::new(target_str)),
            FileMode::Symlink,
        ));
    }
    let bytes = fs::read(&full).map_err(|e| format!("read {rel}: {e}"))?;
    let mode = detect_executable_mode(&full, &bytes);
    Ok(TrackedFile::new(load_working_content(rel, bytes), mode))
}

fn detect_executable_mode(path: &Path, bytes: &[u8]) -> FileMode {
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(path)
        && meta.permissions().mode() & 0o111 != 0
    {
        return FileMode::Executable;
    }
    #[cfg(windows)]
    if is_windows_shell_script_executable(path, bytes) {
        return FileMode::Executable;
    }
    let _ = bytes;
    FileMode::Regular
}

#[cfg(windows)]
fn is_windows_shell_script_executable(path: &Path, bytes: &[u8]) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("sh") | Some("bash") | Some("zsh")
    ) && bytes.starts_with(b"#!")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_executable_bit_on_unix() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("run.sh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
            let tracked = load_working_tracked(dir.path(), "run.sh").unwrap();
            assert_eq!(tracked.mode, FileMode::Executable);
        }
    }

    #[test]
    fn detects_shell_script_shebang_on_windows() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("run.sh");
        fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(windows)]
        {
            let tracked = load_working_tracked(dir.path(), "run.sh").unwrap();
            assert_eq!(tracked.mode, FileMode::Executable);
        }
    }
}
