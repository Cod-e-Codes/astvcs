use crate::store::scan_cache::{self, DirStat, ScanCache};
use ignore::WalkBuilder;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const ASTVCS_DIR: &str = ".astvcs";
const GIT_DIR: &str = ".git";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Full,
    Incremental,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanMetrics {
    pub mode: Option<ScanMode>,
    pub paths_statted: usize,
    pub paths_reused: usize,
    pub dirs_walked: usize,
    pub dirs_pruned: usize,
}

thread_local! {
    static LAST_SCAN_METRICS: RefCell<ScanMetrics> = RefCell::new(ScanMetrics::default());
}

/// Metrics from the most recent working-tree scan on this thread.
pub fn last_scan_metrics() -> ScanMetrics {
    LAST_SCAN_METRICS.with(|m| m.borrow().clone())
}

fn record_metrics(metrics: ScanMetrics) {
    LAST_SCAN_METRICS.with(|m| *m.borrow_mut() = metrics);
}

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

/// Collect tracked files under `root`, honoring ignore rules (always a full walk).
#[cfg(test)]
pub fn scan_working_files(root: &Path) -> Result<ScanReport, String> {
    let (report, _) = scan_working_with_cache(root, None, true, "")?;
    Ok(report)
}

/// Scan the working tree, optionally reusing `cache` for an incremental walk.
///
/// When `full_scan` is true, or the cache is absent or invalid for `head_state_id`,
/// performs a complete walk and rebuilds the cache snapshot.
pub fn scan_working_with_cache(
    root: &Path,
    cache: Option<&ScanCache>,
    full_scan: bool,
    head_state_id: &str,
) -> Result<(ScanReport, ScanCache), String> {
    let use_incremental =
        !full_scan && cache.is_some_and(|c| c.is_valid_for(head_state_id) && !c.paths.is_empty());
    if use_incremental {
        scan_working_incremental(root, cache.unwrap(), head_state_id)
    } else {
        scan_working_full(root, head_state_id)
    }
}

fn scan_working_full(root: &Path, head_state_id: &str) -> Result<(ScanReport, ScanCache), String> {
    let mut files = HashSet::new();
    let mut skipped = Vec::new();
    let mut new_cache = ScanCache::new(head_state_id);
    let mut metrics = ScanMetrics {
        mode: Some(ScanMode::Full),
        ..ScanMetrics::default()
    };

    if let Ok(dir_stat) = scan_cache::stat_dir(root) {
        new_cache.dirs.insert(String::new(), dir_stat);
        metrics.dirs_walked += 1;
    }

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
        let path = entry.path();
        if let Some(file_type) = entry.file_type() {
            if file_type.is_dir() {
                let rel = rel_path(root, path)?;
                if let Ok(dir_stat) = scan_cache::stat_dir(path) {
                    new_cache.dirs.insert(rel, dir_stat);
                    metrics.dirs_walked += 1;
                }
                continue;
            }
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
        } else {
            continue;
        }

        let rel = rel_path(root, path)?;
        if entry.file_type().is_some_and(|t| t.is_symlink()) {
            if let Ok(stat) = scan_cache::stat_path(path) {
                new_cache.paths.insert(rel.clone(), stat);
                metrics.paths_statted += 1;
            }
            files.insert(rel);
            continue;
        }

        match classify_file(path) {
            Ok(()) => {
                if let Ok(stat) = scan_cache::stat_path(path) {
                    new_cache.paths.insert(rel.clone(), stat);
                    metrics.paths_statted += 1;
                }
                files.insert(rel);
            }
            Err(reason) => {
                skipped.push(SkippedPath { path: rel, reason });
            }
        }
    }

    record_metrics(metrics);
    Ok((ScanReport { files, skipped }, new_cache))
}

fn scan_working_incremental(
    root: &Path,
    cache: &ScanCache,
    head_state_id: &str,
) -> Result<(ScanReport, ScanCache), String> {
    let mut files = HashSet::new();
    let mut skipped = Vec::new();
    let mut new_cache = ScanCache::new(head_state_id);
    let mut metrics = ScanMetrics {
        mode: Some(ScanMode::Incremental),
        ..ScanMetrics::default()
    };

    let mut checked_paths = HashSet::new();
    for path in cache.paths.keys() {
        checked_paths.insert(path.clone());
    }

    for path in &checked_paths {
        let full = root.join(path);
        match scan_cache::stat_path(&full) {
            Ok(current) => {
                metrics.paths_statted += 1;
                if let Some(cached) = cache.paths.get(path)
                    && scan_cache::path_stat_unchanged(cached, &current)
                {
                    files.insert(path.clone());
                    new_cache.paths.insert(path.clone(), current);
                    metrics.paths_reused += 1;
                    continue;
                }
                if current.is_symlink {
                    files.insert(path.clone());
                    new_cache.paths.insert(path.clone(), current);
                    continue;
                }
                match classify_file(&full) {
                    Ok(()) => {
                        files.insert(path.clone());
                        if let Ok(stat) = scan_cache::stat_path(&full) {
                            new_cache.paths.insert(path.clone(), stat);
                        }
                    }
                    Err(reason) => {
                        skipped.push(SkippedPath {
                            path: path.clone(),
                            reason,
                        });
                    }
                }
            }
            Err(_) => {
                metrics.paths_statted += 1;
            }
        }
    }

    new_cache.dirs = cache.dirs.clone();
    if let Ok(root_stat) = scan_cache::stat_dir(root) {
        new_cache.dirs.insert(String::new(), root_stat);
    }

    let root_buf = root.to_path_buf();
    let cached_dirs = cache.dirs.clone();
    let dirs_walked = Arc::new(AtomicUsize::new(0));
    let dirs_pruned = Arc::new(AtomicUsize::new(0));
    let dirs_walked_filter = Arc::clone(&dirs_walked);
    let dirs_pruned_filter = Arc::clone(&dirs_pruned);
    let root_for_filter = root_buf.clone();

    let walker = WalkBuilder::new(&root_buf)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .add_custom_ignore_filename(".astvcsignore")
        .filter_entry(move |entry| {
            if is_repo_metadata(entry.path()) {
                return false;
            }
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                let Ok(rel) = rel_path(&root_for_filter, entry.path()) else {
                    return true;
                };
                if let Ok(current) = scan_cache::stat_dir(entry.path()) {
                    dirs_walked_filter.fetch_add(1, Ordering::Relaxed);
                    if let Some(cached) = cached_dirs.get(&rel)
                        && scan_cache::dir_stat_unchanged(cached, &current)
                    {
                        dirs_pruned_filter.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
            }
            true
        })
        .build();

    for result in walker {
        let entry = result.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if let Ok(rel) = rel_path(&root_buf, path)
                && let Ok(dir_stat) = scan_cache::stat_dir(path)
            {
                new_cache.dirs.insert(rel, dir_stat);
            }
            continue;
        }
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let rel = rel_path(&root_buf, path)?;
        if files.contains(&rel) {
            continue;
        }
        if file_type.is_symlink() {
            if let Ok(stat) = scan_cache::stat_path(path) {
                new_cache.paths.insert(rel.clone(), stat);
                metrics.paths_statted += 1;
            }
            files.insert(rel);
            continue;
        }
        match classify_file(path) {
            Ok(()) => {
                if let Ok(stat) = scan_cache::stat_path(path) {
                    new_cache.paths.insert(rel.clone(), stat);
                    metrics.paths_statted += 1;
                }
                files.insert(rel);
            }
            Err(reason) => {
                skipped.push(SkippedPath { path: rel, reason });
            }
        }
    }

    metrics.dirs_walked += dirs_walked.load(Ordering::Relaxed);
    metrics.dirs_pruned += dirs_pruned.load(Ordering::Relaxed);
    refresh_parent_dir_stats(root, &files, &mut new_cache.dirs);

    record_metrics(metrics);
    Ok((ScanReport { files, skipped }, new_cache))
}

fn refresh_parent_dir_stats(
    root: &Path,
    files: &HashSet<String>,
    dirs: &mut HashMap<String, DirStat>,
) {
    for path in files {
        let mut current = Path::new(path);
        while let Some(parent) = current.parent() {
            if parent.as_os_str().is_empty() {
                if let Ok(stat) = scan_cache::stat_dir(root) {
                    dirs.insert(String::new(), stat);
                }
                break;
            }
            let rel = parent.to_string_lossy().replace('\\', "/");
            if let Ok(stat) = scan_cache::stat_dir(&root.join(&rel)) {
                dirs.insert(rel, stat);
            }
            current = parent;
        }
    }
}

/// Re-stat index paths that were not covered by the cache snapshot alone.
pub fn merge_index_paths_into_scan(
    root: &Path,
    index_paths: &HashSet<String>,
    report: &mut ScanReport,
    cache: &mut ScanCache,
    metrics: &mut ScanMetrics,
) -> Result<(), String> {
    for path in index_paths {
        if report.files.contains(path) {
            continue;
        }
        let full = root.join(path);
        match scan_cache::stat_path(&full) {
            Ok(stat) if stat.is_symlink => {
                metrics.paths_statted += 1;
                report.files.insert(path.clone());
                cache.paths.insert(path.clone(), stat);
            }
            Ok(_) => match classify_file(&full) {
                Ok(()) => {
                    metrics.paths_statted += 1;
                    if let Ok(stat) = scan_cache::stat_path(&full) {
                        cache.paths.insert(path.clone(), stat);
                    }
                    report.files.insert(path.clone());
                }
                Err(reason) => {
                    metrics.paths_statted += 1;
                    report.skipped.push(SkippedPath {
                        path: path.clone(),
                        reason,
                    });
                }
            },
            Err(_) => {
                metrics.paths_statted += 1;
            }
        }
    }
    Ok(())
}

fn rel_path(root: &Path, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "/"))
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
    use std::thread;
    use std::time::Duration;
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

    #[test]
    fn incremental_scan_reuses_unchanged_paths() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for i in 0..50 {
            fs::write(
                root.join(format!("file{i}.rs")),
                format!("fn f{i}() {{}}\n"),
            )
            .unwrap();
        }

        let head = "0".repeat(64);
        let (report, cache) = scan_working_with_cache(root, None, true, &head).unwrap();
        assert_eq!(report.files.len(), 50);
        assert_eq!(last_scan_metrics().mode, Some(ScanMode::Full));

        let (report2, _) = scan_working_with_cache(root, Some(&cache), false, &head).unwrap();
        assert_eq!(report2.files, report.files);
        let metrics = last_scan_metrics();
        assert_eq!(metrics.mode, Some(ScanMode::Incremental));
        assert!(metrics.paths_reused >= 50, "metrics: {metrics:?}");
    }

    #[test]
    fn incremental_scan_detects_touched_file() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for i in 0..20 {
            fs::write(
                root.join(format!("file{i}.rs")),
                format!("fn f{i}() {{}}\n"),
            )
            .unwrap();
        }

        let head = "0".repeat(64);
        let (_, cache) = scan_working_with_cache(root, None, true, &head).unwrap();
        fs::write(root.join("file0.rs"), "fn f0() { let x = 1; }\n").unwrap();
        thread::sleep(Duration::from_millis(10));

        let (report, _) = scan_working_with_cache(root, Some(&cache), false, &head).unwrap();
        assert!(report.files.contains("file0.rs"));
        let metrics = last_scan_metrics();
        assert_eq!(metrics.mode, Some(ScanMode::Incremental));
        assert!(metrics.paths_reused >= 19, "metrics: {metrics:?}");
    }

    #[test]
    fn full_scan_flag_bypasses_cache() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();

        let head = "0".repeat(64);
        let (_, cache) = scan_working_with_cache(root, None, true, &head).unwrap();
        let (_, _) = scan_working_with_cache(root, Some(&cache), true, &head).unwrap();
        assert_eq!(last_scan_metrics().mode, Some(ScanMode::Full));
    }

    #[test]
    fn incremental_scan_finds_new_file_in_changed_dir() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg").join("a.rs"), "fn a() {}\n").unwrap();

        let head = "0".repeat(64);
        let (_, cache) = scan_working_with_cache(root, None, true, &head).unwrap();

        fs::write(root.join("pkg").join("b.rs"), "fn b() {}\n").unwrap();

        let (report, _) = scan_working_with_cache(root, Some(&cache), false, &head).unwrap();
        assert!(report.files.contains("pkg/a.rs"));
        assert!(report.files.contains("pkg/b.rs"));
    }

    #[test]
    fn head_mismatch_forces_full_scan() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();

        let head = "0".repeat(64);
        let (_, cache) = scan_working_with_cache(root, None, true, &head).unwrap();
        let other = "1".repeat(64);
        let (_, _) = scan_working_with_cache(root, Some(&cache), false, &other).unwrap();
        assert_eq!(last_scan_metrics().mode, Some(ScanMode::Full));
    }
}
