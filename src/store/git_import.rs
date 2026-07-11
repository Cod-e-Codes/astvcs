use crate::frontend::is_binary_payload;
use crate::store::repo::{CommitOptions, CommitOutcome, Repo};
use crate::store::scan_cache;
use crate::store::staging::{StagingIndex, clear_staging_entries};
use crate::trace;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

const GIT_MODE_SYMLINK: u32 = 120_000;
const GIT_MODE_SUBMODULE: u32 = 160_000;
const GIT_MODE_EXECUTABLE: u32 = 100_755;

/// One path entry from `git ls-tree -r HEAD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTreeEntry {
    pub mode: u32,
    pub object_type: String,
    pub object_id: String,
    pub path: String,
}

/// Validate that `git_path` is a git repository using a git subprocess.
pub fn validate_git_repo(git_path: &Path) -> Result<(), String> {
    git_command(git_path, &["rev-parse", "--git-dir"])?;
    Ok(())
}

/// List tracked blob paths at git HEAD.
pub fn list_head_tree(git_path: &Path) -> Result<Vec<GitTreeEntry>, String> {
    let output = git_command(git_path, &["ls-tree", "-r", "HEAD"])?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        entries.push(parse_ls_tree_line(line)?);
    }
    Ok(entries)
}

/// Parse one line of `git ls-tree` output (`<mode> <type> <object>\t<path>`).
pub fn parse_ls_tree_line(line: &str) -> Result<GitTreeEntry, String> {
    let line = line.trim_end();
    let (meta, path) = line
        .split_once('\t')
        .ok_or_else(|| format!("invalid ls-tree line (missing tab): {line}"))?;
    if path.is_empty() {
        return Err(format!("invalid ls-tree line (empty path): {line}"));
    }
    let parts: Vec<&str> = meta.split_whitespace().collect();
    if parts.len() != 3 {
        return Err(format!("invalid ls-tree line (expected 3 fields): {line}"));
    }
    let mode = parts[0]
        .parse::<u32>()
        .map_err(|_| format!("invalid ls-tree mode `{}` in line: {line}", parts[0]))?;
    Ok(GitTreeEntry {
        mode,
        object_type: parts[1].to_string(),
        object_id: parts[2].to_string(),
        path: path.to_string(),
    })
}

/// Import the git HEAD tree snapshot into the astvcs working tree and commit once.
pub fn import_git_snapshot(
    repo: &Repo,
    git_path: &Path,
    message: &str,
) -> Result<CommitOutcome, String> {
    let _lock = repo.repo_lock().map_err(|e| e.to_string())?;
    validate_git_repo(git_path)?;
    let entries = list_head_tree(git_path)?;

    let head = repo.head_state_unlocked().map_err(|e| e.to_string())?;
    let head_files = repo
        .load_state_files_unlocked(&head)
        .map_err(|e| e.to_string())?;

    let mut imported_paths = HashSet::new();
    for entry in &entries {
        if entry.path.starts_with(".astvcs/") || entry.path == ".astvcs" {
            trace::warn(format!(
                "import-git: skipped {:?} (astvcs metadata path)",
                entry.path
            ));
            continue;
        }
        if !is_safe_repo_relative_path(&entry.path) {
            trace::warn(format!(
                "import-git: skipped {:?} (unsafe relative path)",
                entry.path
            ));
            continue;
        }
        if entry.mode == GIT_MODE_SUBMODULE {
            trace::warn(format!(
                "import-git: skipped submodule {:?} (submodule import not supported in v1)",
                entry.path
            ));
            continue;
        }
        if entry.object_type != "blob" {
            trace::warn(format!(
                "import-git: skipped {:?} (unsupported git object type {})",
                entry.path, entry.object_type
            ));
            continue;
        }
        if entry.mode == GIT_MODE_SYMLINK {
            match import_symlink(repo.root_path(), git_path, entry) {
                Ok(()) => {
                    imported_paths.insert(entry.path.clone());
                }
                Err(reason) => {
                    trace::warn(format!("import-git: skipped {:?}: {reason}", entry.path))
                }
            }
            continue;
        }
        match import_blob_file(repo.root_path(), git_path, entry) {
            Ok(()) => {
                imported_paths.insert(entry.path.clone());
            }
            Err(reason) => trace::warn(format!("import-git: skipped {:?}: {reason}", entry.path)),
        }
    }

    for path in head_files.keys() {
        if !imported_paths.contains(path) {
            let full = repo.root_path().join(path);
            if full.is_file() || full.is_symlink() {
                remove_working_path(&full)?;
                trace::notice(format!("import-git: removed {path} (absent from git HEAD)"));
            }
        }
    }

    scan_cache::invalidate_scan_cache(&repo.astvcs_dir())?;

    let mut staging: StagingIndex = repo.load_staging_unlocked().map_err(|e| e.to_string())?;
    clear_staging_entries(&mut staging);
    staging.active = false;
    repo.save_staging_unlocked(&staging)
        .map_err(|e| e.to_string())?;

    repo.commit_with_options(
        message,
        CommitOptions {
            only_paths: Some(imported_paths),
            ..CommitOptions::default()
        },
    )
    .map_err(|e| e.to_string())
}

fn import_blob_file(repo_root: &Path, git_path: &Path, entry: &GitTreeEntry) -> Result<(), String> {
    let bytes = read_git_object(git_path, &entry.object_id)?;
    if is_binary_payload(&bytes) {
        return Err("binary file (NUL or invalid UTF-8)".into());
    }
    let full = repo_root.join(&entry.path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    remove_if_present(&full)?;
    fs::write(&full, &bytes).map_err(|e| e.to_string())?;
    if entry.mode == GIT_MODE_EXECUTABLE {
        set_executable_mode(&full)?;
    }
    Ok(())
}

fn import_symlink(repo_root: &Path, git_path: &Path, entry: &GitTreeEntry) -> Result<(), String> {
    let bytes = read_git_object(git_path, &entry.object_id)?;
    if is_binary_payload(&bytes) {
        return Err("symlink target is not valid UTF-8".into());
    }
    let target = String::from_utf8(bytes).expect("validated UTF-8 above");
    let full = repo_root.join(&entry.path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    remove_if_present(&full)?;
    create_symlink(&full, &target)?;
    Ok(())
}

fn read_git_object(git_path: &Path, object_id: &str) -> Result<Vec<u8>, String> {
    let output = git_command(git_path, &["cat-file", "blob", object_id])?;
    Ok(output.stdout)
}

fn git_command(git_path: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_path)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e} (is git on PATH?)"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(output)
}

fn is_safe_repo_relative_path(path: &str) -> bool {
    let p = Path::new(path);
    if p.is_absolute() {
        return false;
    }
    for component in p.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            _ => {}
        }
    }
    true
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    if path.is_symlink() || path.is_file() {
        remove_working_path(path)?;
    }
    Ok(())
}

fn remove_working_path(path: &Path) -> Result<(), String> {
    if path.is_symlink() {
        fs::remove_file(path).map_err(|e| e.to_string())
    } else if path.is_dir() {
        Err(format!(
            "refusing to remove directory at {}",
            path.display()
        ))
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

fn create_symlink(path: &Path, target: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, path).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_file;
        symlink_file(target, path).map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
fn set_executable_mode(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn set_executable_mode(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ls_tree_line_regular_file() {
        let entry = parse_ls_tree_line("100644 blob abc123\thello.txt").unwrap();
        assert_eq!(entry.mode, 100_644);
        assert_eq!(entry.object_type, "blob");
        assert_eq!(entry.object_id, "abc123");
        assert_eq!(entry.path, "hello.txt");
    }

    #[test]
    fn parse_ls_tree_line_executable_and_symlink() {
        let exe = parse_ls_tree_line("100755 blob deadbeef\tbin/run.sh").unwrap();
        assert_eq!(exe.mode, GIT_MODE_EXECUTABLE);
        assert_eq!(exe.path, "bin/run.sh");

        let link = parse_ls_tree_line("120000 blob cafebabe\tlink.txt").unwrap();
        assert_eq!(link.mode, GIT_MODE_SYMLINK);
        assert_eq!(link.path, "link.txt");
    }

    #[test]
    fn parse_ls_tree_line_nested_path_with_spaces() {
        let entry = parse_ls_tree_line("100644 blob ff00\tpath/with spaces/file name.rs").unwrap();
        assert_eq!(entry.path, "path/with spaces/file name.rs");
    }

    #[test]
    fn parse_ls_tree_line_rejects_missing_tab() {
        assert!(parse_ls_tree_line("100644 blob abc123 hello.txt").is_err());
    }

    #[test]
    fn is_safe_repo_relative_path_rejects_parent() {
        assert!(!is_safe_repo_relative_path("../secret.txt"));
        assert!(is_safe_repo_relative_path("src/lib.rs"));
    }
}
