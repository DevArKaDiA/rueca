//! The store's advisory writer lock, `<store>/.lock` (RFC 0008 §5.5).
//!
//! An exclusive [`File::lock`] on one file per store serializes every
//! read-modify-write of the store's mutable files (`catalog.json`,
//! `packs/index.rcpi`, `orgs/<name>/org.json`) and organization garbage
//! collection, across threads and processes. Readers never take it. The
//! lock is not reentrant: a holder that asks for it again waits for itself
//! until the timeout.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::{Result, SegmentError};

/// The lock file, directly under the store directory.
pub(crate) const LOCK_FILE: &str = ".lock";

/// How long a writer waits for the lock before failing.
pub(crate) const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest pause between two attempts.
const MAX_PAUSE: Duration = Duration::from_millis(50);

/// The held store lock; released when dropped (or when the process exits).
///
/// The file holds the holder's process id, so a writer that times out can
/// name it.
#[derive(Debug)]
pub struct StoreLock {
    file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl StoreLock {
    /// Takes the lock of the store at `store`, waiting at most
    /// [`LOCK_TIMEOUT`].
    pub(crate) fn acquire(store: &Path) -> Result<StoreLock> {
        Self::acquire_within(store, LOCK_TIMEOUT)
    }

    /// Takes the lock, waiting at most `timeout`: `File::lock` has no
    /// timeout, so this polls `try_lock` with a growing pause.
    pub(crate) fn acquire_within(store: &Path, timeout: Duration) -> Result<StoreLock> {
        fs::create_dir_all(store)?;
        let path = store.join(LOCK_FILE);
        let mut file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path)?;
        let started = Instant::now();
        let mut pause = Duration::from_millis(1);
        loop {
            match file.try_lock() {
                Ok(()) => {
                    record_holder(&mut file);
                    return Ok(StoreLock { file });
                }
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => {
                    return Err(SegmentError::Other(format!("locking {}: {e}", path.display())));
                }
            }
            if started.elapsed() >= timeout {
                return Err(timed_out(&path, &mut file, timeout));
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(MAX_PAUSE);
        }
    }
}

/// Best effort: writes this process's id into the held lock file.
fn record_holder(file: &mut File) {
    let pid = std::process::id().to_string();
    let _ = file.set_len(0).and_then(|_| file.rewind()).and_then(|_| file.write_all(pid.as_bytes()));
}

/// The timeout error: names the lock file and, when readable, the holder.
fn timed_out(path: &Path, file: &mut File, timeout: Duration) -> SegmentError {
    let mut holder = String::new();
    let _ = file.rewind().and_then(|_| file.read_to_string(&mut holder));
    let holder = holder.trim();
    let by = if holder.is_empty() { String::new() } else { format!(" (held by process {holder})") };
    SegmentError::Other(format!(
        "timed out after {} s waiting for the store lock {}{by}: another rueca process is writing to this store",
        timeout.as_secs_f32(),
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_times_out_naming_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let held = StoreLock::acquire(dir.path()).unwrap();
        let err = StoreLock::acquire_within(dir.path(), Duration::from_millis(100)).unwrap_err().to_string();
        let path = dir.path().join(LOCK_FILE);
        assert!(err.contains(&path.display().to_string()), "{err}");
        #[cfg(unix)]
        assert!(err.contains(&format!("held by process {}", std::process::id())), "{err}");
        drop(held);
        // Released on drop: the next writer gets it at once.
        let again = StoreLock::acquire_within(dir.path(), Duration::from_millis(100)).unwrap();
        drop(again);
    }

    #[test]
    fn the_lock_serializes_threads() {
        let dir = tempfile::tempdir().unwrap();
        let inside = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..20 {
                        let _l = StoreLock::acquire(dir.path()).unwrap();
                        assert_eq!(inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
                        std::thread::sleep(Duration::from_micros(200));
                        inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
    }
}
