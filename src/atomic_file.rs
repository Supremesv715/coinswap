//! Shared file-locking and atomic JSON-writing helpers.

use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write},
    path::Path,
};

/// An exclusive lock on a file, held while this value is alive.
///
/// The lock belongs to the operating system, so it is released when this value
/// is dropped and also if the process dies while holding it.
pub(crate) struct FileLock {
    /// Holding the handle is the lock; closing it releases.
    _file: File,
}

impl FileLock {
    /// Block until the lock at `lock_path` is held.
    ///
    /// The sentinel file is created if absent and never removed: deleting it
    /// would let another process lock a fresh inode under the same path while
    /// this one still holds the old one.
    pub(crate) fn acquire(lock_path: &Path) -> io::Result<Self> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;

        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                log::info!("Waiting for lock on {}", lock_path.display());
                file.lock()?;
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }

        Ok(Self { _file: file })
    }
}

pub(crate) fn read_json<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }

    let content = std::fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(io::Error::other)
}

pub(crate) fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    write_bytes_atomically(path, json.as_bytes())
}

/// Durably replace `path` with `contents` without exposing a partially-written file.
///
/// `std::fs::rename` cannot replace an existing destination on Windows. A named
/// temporary file uses the platform's replace operation there while retaining
/// rename-over semantics on Unix.
pub(crate) fn write_bytes_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut temporary_file = tempfile::NamedTempFile::new_in(parent)?;
    temporary_file.write_all(contents)?;
    temporary_file.as_file().sync_all()?;
    temporary_file.persist(path).map_err(|error| error.error)?;

    // Syncing a directory makes the rename durable on Unix. Windows does not
    // permit opening directories with `File::open`; MoveFileExW has already
    // completed the replacement before `persist` returns.
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_bytes_atomically;

    #[test]
    fn atomic_write_replaces_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.cbor");
        std::fs::write(&path, b"old").unwrap();

        write_bytes_atomically(&path, b"new").unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
}
