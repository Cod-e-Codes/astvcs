use std::cell::Cell;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "repo.lock";

thread_local! {
    static LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn lock_held() -> bool {
    LOCK_DEPTH.with(|depth| depth.get() > 0)
}

/// Exclusive repository lock held for the duration of one logical command.
///
/// Uses OS advisory whole-file locking (`flock` / `LockFileEx`). The lock is
/// released when the guard drops or the process exits; there are no stale
/// marker files. Reentrant on the same thread so nested repo calls do not
/// deadlock.
#[derive(Debug)]
pub struct RepoLockGuard {
    _file: Option<File>,
    path: PathBuf,
}

impl RepoLockGuard {
    pub fn acquire(astvcs_dir: &Path) -> Result<Self, String> {
        let path = astvcs_dir.join(LOCK_FILE);
        if LOCK_DEPTH.with(|depth| depth.get()) > 0 {
            LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
            return Ok(Self { _file: None, path });
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("open lock file {}: {e}", path.display()))?;

        file.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => format!(
                "repository is locked by another process; cannot acquire {}",
                path.display()
            ),
            TryLockError::Error(e) => format!("lock {}: {e}", path.display()),
        })?;

        LOCK_DEPTH.with(|depth| depth.set(1));
        Ok(Self {
            _file: Some(file),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RepoLockGuard {
    fn drop(&mut self) {
        LOCK_DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
        });
        // `file` drops here, releasing the OS advisory lock on the last guard.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    #[test]
    fn reentrant_lock_on_same_thread() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();

        let outer = RepoLockGuard::acquire(&astvcs).unwrap();
        let inner = RepoLockGuard::acquire(&astvcs).unwrap();
        drop(inner);
        drop(outer);
    }

    #[test]
    fn second_process_fails_fast_while_lock_held() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();
        let astvcs = Arc::new(astvcs);
        let barrier = Arc::new(Barrier::new(2));

        let astvcs_a = Arc::clone(&astvcs);
        let barrier_a = Arc::clone(&barrier);
        let holder = thread::spawn(move || {
            let _guard = RepoLockGuard::acquire(&astvcs_a).unwrap();
            barrier_a.wait();
            thread::sleep(std::time::Duration::from_millis(100));
        });

        barrier.wait();
        let err = RepoLockGuard::acquire(&astvcs).unwrap_err();
        assert!(
            err.contains("repository is locked by another process"),
            "{err}"
        );
        assert!(err.contains("repo.lock"), "{err}");
        holder.join().unwrap();

        let _again = RepoLockGuard::acquire(&astvcs).unwrap();
    }

    #[test]
    fn lock_released_after_holder_dropped() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();

        {
            let _guard = RepoLockGuard::acquire(&astvcs).unwrap();
        }
        let _guard = RepoLockGuard::acquire(&astvcs).unwrap();
    }
}
