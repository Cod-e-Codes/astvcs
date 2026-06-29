use super::error::{RepoError, RepoResult};
use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "repo.lock";

thread_local! {
    static LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Cached lock file for this thread. Unlocked (but kept open) between commands.
    static LOCK_STATE: RefCell<Option<(PathBuf, File)>> = const { RefCell::new(None) };
}

pub(crate) fn lock_held() -> bool {
    LOCK_DEPTH.with(|depth| depth.get() > 0)
}

/// Release the OS advisory lock while keeping the lock file descriptor open.
///
/// Sets thread-local lock depth to zero without closing the cached handle.
/// Used before running client hooks that may invoke `astvcs` as a subprocess.
pub(crate) fn suspend_repo_lock() -> RepoResult<()> {
    if !lock_held() {
        return Err(RepoError::other(
            "cannot suspend repository lock: lock not held on this thread",
        ));
    }
    LOCK_DEPTH.with(|depth| depth.set(0));
    LOCK_STATE.with(|state| {
        if let Some((_, file)) = state.borrow().as_ref() {
            file.unlock()
                .map_err(|e| RepoError::other(format!("unlock repo lock: {e}")))?;
        }
        Ok(())
    })
}

/// Re-acquire the repository lock after [`suspend_repo_lock`].
pub(crate) fn resume_repo_lock(astvcs_dir: &Path) -> RepoResult<RepoLockGuard> {
    let path = astvcs_dir.join(LOCK_FILE);
    open_lock_file(&path)?;
    try_lock_cached(&path)?;
    LOCK_DEPTH.with(|depth| depth.set(1));
    Ok(RepoLockGuard { path })
}

/// Exclusive repository lock held for the duration of one logical command.
///
/// Uses OS advisory whole-file locking (`flock` / `LockFileEx`). The lock is
/// released when the outermost guard drops or the process exits; there are no
/// stale marker files. Reentrant on the same thread so nested repo calls do not
/// deadlock. The OS lock file descriptor is cached per thread and unlocked (not
/// closed) between outermost acquisitions so Linux does not fail reopening the
/// same lock file with `WouldBlock` after a prior guard dropped.
#[derive(Debug)]
pub struct RepoLockGuard {
    path: PathBuf,
}

impl RepoLockGuard {
    pub fn acquire(astvcs_dir: &Path) -> RepoResult<Self> {
        let path = astvcs_dir.join(LOCK_FILE);
        if LOCK_DEPTH.with(|depth| depth.get()) > 0 {
            LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
            return Ok(Self { path });
        }

        open_lock_file(&path)?;
        try_lock_cached(&path)?;

        LOCK_DEPTH.with(|depth| depth.set(1));
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn open_lock_file(path: &Path) -> RepoResult<()> {
    LOCK_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let need_new = match state.as_ref() {
            None => true,
            Some((stored, _)) => stored != path,
        };
        if !need_new {
            return Ok(());
        }
        if let Some((_, old)) = state.take() {
            let _ = old.unlock();
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RepoError::from_io("create lock dir", e))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| RepoError::other(format!("open lock file {}: {e}", path.display())))?;
        *state = Some((path.to_path_buf(), file));
        Ok(())
    })
}

fn try_lock_cached(path: &Path) -> RepoResult<()> {
    LOCK_STATE.with(|state| {
        let state = state.borrow();
        let Some((stored, file)) = state.as_ref() else {
            return Err(RepoError::other(format!(
                "open lock file {}: missing handle",
                path.display()
            )));
        };
        if stored != path {
            return Err(RepoError::other(format!(
                "open lock file {}: path mismatch with cached handle",
                path.display()
            )));
        }
        file.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => RepoError::lock_contention(format!(
                "repository is locked by another process; cannot acquire {}",
                path.display()
            )),
            TryLockError::Error(e) => RepoError::other(format!("lock {}: {e}", path.display())),
        })
    })
}

impl Drop for RepoLockGuard {
    fn drop(&mut self) {
        LOCK_DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            if next == 0 {
                LOCK_STATE.with(|state| {
                    if let Some((_, file)) = state.borrow().as_ref() {
                        let _ = file.unlock();
                    }
                });
            }
        });
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
    fn sequential_acquire_after_release_on_same_thread() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();

        {
            let _guard = RepoLockGuard::acquire(&astvcs).unwrap();
        }
        {
            let _guard = RepoLockGuard::acquire(&astvcs).unwrap();
        }
        {
            let _guard = RepoLockGuard::acquire(&astvcs).unwrap();
        }
    }

    #[test]
    fn outer_guard_may_drop_before_inner_without_releasing_lock() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();
        let astvcs = Arc::new(astvcs);
        let barrier = Arc::new(Barrier::new(2));

        let astvcs_a = Arc::clone(&astvcs);
        let barrier_a = Arc::clone(&barrier);
        let holder = thread::spawn(move || {
            let outer = RepoLockGuard::acquire(&astvcs_a).unwrap();
            let inner = RepoLockGuard::acquire(&astvcs_a).unwrap();
            drop(outer);
            barrier_a.wait();
            thread::sleep(std::time::Duration::from_millis(100));
            drop(inner);
        });

        barrier.wait();
        let err = RepoLockGuard::acquire(&astvcs).unwrap_err();
        assert!(
            err.contains("repository is locked by another process"),
            "{err}"
        );
        holder.join().unwrap();

        let _again = RepoLockGuard::acquire(&astvcs).unwrap();
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

    #[test]
    fn suspend_and_resume_releases_for_subprocess() {
        let dir = TempDir::new().unwrap();
        let astvcs = dir.path().join(".astvcs");
        std::fs::create_dir_all(&astvcs).unwrap();

        let _outer = RepoLockGuard::acquire(&astvcs).unwrap();
        suspend_repo_lock().unwrap();
        let _sub = RepoLockGuard::acquire(&astvcs).unwrap();
        drop(_sub);
        let _again = resume_repo_lock(&astvcs).unwrap();
    }
}
