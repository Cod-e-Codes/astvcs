use ignore::WalkBuilder;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

pub const ASTVCS_DIR: &str = ".astvcs";
const GIT_DIR: &str = ".git";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedPath {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanReport {
    pub files: HashSet<String>,
    pub skipped: Vec<SkippedPath>,
}

/// Collect tracked files under `root`, honoring ignore rules.
pub fn scan_working_files(root: &Path) -> Result<ScanReport, String> {
    let mut files = HashSet::new();
    let mut skipped = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .add_custom_ignore_filename(".astvcsignore")
        .filter_entry(|entry| !is_repo_metadata(entry.path()))
        .build();

    for result in walker {
        let entry = result.map_err(|e| e.to_string())?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        match classify_file(path) {
            Ok(()) => {
                files.insert(rel);
            }
            Err(reason) => {
                skipped.push(SkippedPath { path: rel, reason });
            }
        }
    }
    Ok(ScanReport { files, skipped })
}

fn is_repo_metadata(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::Normal(name) if name == ASTVCS_DIR || name == GIT_DIR
        )
    })
}

fn classify_file(path: &Path) -> Result<(), String> {
    fs::read(path).map_err(|e| format!("read error: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn respects_gitignore() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "target/\nignored.txt\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target").join("build.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("ignored.txt"), "skip\n").unwrap();
        fs::write(root.join("tracked.rs"), "fn tracked() {}\n").unwrap();

        let report = scan_working_files(root).unwrap();
        assert!(report.files.contains("tracked.rs"));
        assert!(!report.files.contains("target/build.rs"));
        assert!(!report.files.contains("ignored.txt"));
    }

    #[test]
    fn respects_astvcsignore() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".astvcsignore"), "vendor/\n").unwrap();
        fs::create_dir_all(root.join("vendor")).unwrap();
        fs::write(root.join("vendor").join("dep.rs"), "fn dep() {}\n").unwrap();
        fs::write(root.join("app.rs"), "fn app() {}\n").unwrap();

        let report = scan_working_files(root).unwrap();
        assert!(report.files.contains("app.rs"));
        assert!(!report.files.contains("vendor/dep.rs"));
    }

    #[test]
    fn skips_astvcs_and_git_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".astvcs").join("blobs")).unwrap();
        fs::write(root.join(".astvcs").join("HEAD"), "main\n").unwrap();
        fs::create_dir_all(root.join(".git").join("objects")).unwrap();
        fs::write(root.join(".git").join("config"), "[core]\n").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let report = scan_working_files(root).unwrap();
        assert!(report.files.contains("main.rs"));
        assert!(!report.files.iter().any(|p| p.contains(".astvcs")));
        assert!(!report.files.iter().any(|p| p.contains(".git/")));
    }

    #[test]
    fn tracks_binary_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("image.png"), [0x89, 0x50, 0x4E, 0x47, 0, 0]).unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let report = scan_working_files(root).unwrap();
        assert!(report.files.contains("main.rs"));
        assert!(report.files.contains("image.png"));
        assert!(report.skipped.is_empty());
    }
}
