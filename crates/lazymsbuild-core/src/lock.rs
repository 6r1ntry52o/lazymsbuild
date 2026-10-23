//! `build.lock`: one build or sync at a time per repository, whether it was started from the TUI
//! or from the command line.
//!
//! The lock is an operating-system file lock, so it is released when the process ends, however it
//! ends. Windows file locks also block reading the locked file, so what the running build is doing
//! is read from `status.json` instead.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A held build lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct BuildLock {
    _file: File,
    path: PathBuf,
}

impl BuildLock {
    /// Takes the lock, or returns `None` when another process holds it.
    pub fn try_acquire(path: &Path) -> Result<Option<BuildLock>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(BuildLock { _file: file, path: path.to_path_buf() })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err).with_context(|| format!("cannot lock {}", path.display())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.lock");
        let first = BuildLock::try_acquire(&path).unwrap().expect("first lock");
        assert!(BuildLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(BuildLock::try_acquire(&path).unwrap().is_some());
    }
}
