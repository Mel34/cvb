use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LockError {
    Locked,
    Io(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Locked => write!(
                f,
                "CVB: project is currently locked by another surgery operation (E-LOCK)"
            ),
            LockError::Io(msg) => write!(f, "CVB: lock file I/O error: {msg}"),
        }
    }
}

impl std::error::Error for LockError {}

/// An exclusive advisory project lock released automatically on drop or process exit.
#[derive(Debug)]
pub(crate) struct ProjectLock {
    path: PathBuf,
    file: Option<File>,
}

impl ProjectLock {
    /// Attempts to acquire an exclusive, non-blocking lock on `project.lock`.
    pub(crate) fn acquire(project_dir: &Path) -> Result<Self, LockError> {
        let lock_path = project_dir.join("project.lock");

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);

        let file = options
            .open(&lock_path)
            .map_err(|e| LockError::Io(e.to_string()))?;

        #[cfg(unix)]
        {
            let ret = unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
            };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(LockError::Locked);
                }
                return Err(LockError::Io(err.to_string()));
            }
        }

        Ok(ProjectLock {
            path: lock_path,
            file: Some(file),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            #[cfg(unix)]
            unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
            drop(file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn test_dir(label: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "cvb-lock-{label}-{}",
            std::process::id()
        ));
        if base.exists() {
            fs::remove_dir_all(&base).unwrap();
        }
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn test_lock_acquire_and_release() {
        let base = test_dir("basic");

        let lock = ProjectLock::acquire(&base);
        assert!(lock.is_ok(), "first acquire should succeed");
        drop(lock);

        let lock_again = ProjectLock::acquire(&base);
        assert!(lock_again.is_ok(), "acquire after drop should succeed");
        drop(lock_again);

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn test_non_blocking_rejection_when_locked() {
        let base = test_dir("contend");
        let lock1 = ProjectLock::acquire(&base).unwrap();

        match ProjectLock::acquire(&base) {
            Err(LockError::Locked) => {}
            other => panic!("expected LockError::Locked, got: {other:?}"),
        }

        drop(lock1);

        let lock3 = ProjectLock::acquire(&base);
        assert!(lock3.is_ok(), "acquire after holder dropped must succeed");
        drop(lock3);

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_lock_identity_stays_stable_when_target_inode_is_replaced() {
        use std::os::unix::fs::MetadataExt;

        let base = test_dir("stable-target");
        let target = base.join("target.txt");
        let replacement = base.join("target.new");
        fs::write(&target, "original target\n").unwrap();

        let lock = ProjectLock::acquire(&base).unwrap();
        let lock_path = lock.path().to_path_buf();
        let lock_before = fs::metadata(&lock_path).unwrap();
        let lock_identity_before = (lock_before.dev(), lock_before.ino());
        let target_inode_before = fs::metadata(&target).unwrap().ino();

        fs::write(&replacement, "replacement target\n").unwrap();
        fs::rename(&replacement, &target).unwrap();

        let target_inode_after = fs::metadata(&target).unwrap().ino();
        assert_ne!(
            target_inode_before, target_inode_after,
            "the test must replace the target with a different inode"
        );

        let lock_after = fs::metadata(&lock_path).unwrap();
        assert_eq!(
            lock_identity_before,
            (lock_after.dev(), lock_after.ino()),
            "project.lock identity must remain stable across target replacement"
        );
        assert!(
            matches!(ProjectLock::acquire(&base), Err(LockError::Locked)),
            "a competing operation must still be excluded while the original lock is held"
        );

        drop(lock);
        let lock_after_release = ProjectLock::acquire(&base);
        assert!(
            lock_after_release.is_ok(),
            "the lock must be acquirable after the original holder releases it"
        );
        drop(lock_after_release);

        fs::remove_dir_all(base).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn test_project_lock_rejects_competing_process() {
        const CHILD_DIR_ENV: &str = "CVB_TEST_PROJECT_LOCK_CHILD_DIR";

        if let Ok(dir) = std::env::var(CHILD_DIR_ENV) {
            assert!(
                matches!(ProjectLock::acquire(Path::new(&dir)), Err(LockError::Locked)),
                "child process must be rejected while the parent holds project.lock"
            );
            return;
        }

        use std::process::Command;

        let base = test_dir("process-contend");
        let lock = ProjectLock::acquire(&base).unwrap();
        let test_name = format!(
            "{}::test_project_lock_rejects_competing_process",
            module_path!()
        );

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD_DIR_ENV, &base)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child lock contention test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn test_lock_file_permissions() {
        let base = test_dir("perms");
        let lock = ProjectLock::acquire(&base).unwrap();

        #[cfg(unix)]
        {
            let meta = fs::metadata(lock.path()).unwrap();
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o600,
                "new project.lock file must have mode 0600"
            );
        }

        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }
}
