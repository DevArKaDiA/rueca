//! `file:///abs/dir`: Rueca's own single-host backend over `std::fs`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bytes::Bytes;
use tempfile::NamedTempFile;

use crate::{ObjectMeta, ObjectVersion, PutError, Storage, Versioned, blake3_hex, check_path};

/// The advisory lock file, shared with a local store's writers.
const LOCK_FILE: &str = ".lock";
/// Scratch directory for temporary files (same filesystem as the objects).
const TMP_DIR: &str = ".tmp";
/// A writer waiting longer than this for the lock fails.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Objects are files under `root`; writes are temporary file + rename under
/// an exclusive lock on `root/.lock`; e-tags are BLAKE3 hex digests of the
/// content. Single-host by design (RFC 0008 §5.5).
pub struct FileStorage {
    root: PathBuf,
}

/// Held while a write checks and renames; released on drop (closing the file
/// releases the lock).
struct LockGuard {
    _file: File,
}

impl FileStorage {
    /// Opens (creating it if needed) the store directory `root`, which must
    /// be absolute.
    pub fn open(root: &Path) -> anyhow::Result<FileStorage> {
        if !root.is_absolute() {
            bail!("file store {} must be an absolute path", root.display());
        }
        fs::create_dir_all(root.join(TMP_DIR)).with_context(|| format!("creating file store {}", root.display()))?;
        Ok(FileStorage { root: root.to_path_buf() })
    }

    /// The store directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object(&self, path: &str) -> anyhow::Result<PathBuf> {
        check_path(path)?;
        let first = path.split('/').next().unwrap_or_default();
        if first == LOCK_FILE || first == TMP_DIR {
            bail!("object path `{path}` is reserved by the file store");
        }
        Ok(self.root.join(path))
    }

    fn lock(&self) -> anyhow::Result<LockGuard> {
        let lock_path = self.root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening {}", lock_path.display()))?;
        let start = Instant::now();
        let mut pause = Duration::from_millis(1);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(LockGuard { _file: file }),
                Err(TryLockError::WouldBlock) if start.elapsed() < LOCK_TIMEOUT => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(50));
                }
                Err(TryLockError::WouldBlock) => bail!(
                    "timed out after {}s waiting for the store lock {}",
                    LOCK_TIMEOUT.as_secs(),
                    lock_path.display()
                ),
                Err(TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("locking {}", lock_path.display()));
                }
            }
        }
    }

    fn temp(&self) -> io::Result<NamedTempFile> {
        tempfile::Builder::new().prefix(".put-").tempfile_in(self.root.join(TMP_DIR))
    }

    /// Writes `bytes` to a temporary file, fsynced.
    fn stage_bytes(&self, bytes: &[u8]) -> io::Result<(NamedTempFile, String)> {
        let mut tmp = self.temp()?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        Ok((tmp, blake3_hex(&blake3::hash(bytes))))
    }

    /// Copies `src` to a temporary file, fsynced, hashing on the way.
    fn stage_file(&self, src: &Path) -> anyhow::Result<(NamedTempFile, String)> {
        let mut input = File::open(src).with_context(|| format!("opening {}", src.display()))?;
        let mut tmp = self.temp()?;
        let e_tag = copy_hashing(&mut input, tmp.as_file_mut())?;
        tmp.as_file().sync_all()?;
        Ok((tmp, e_tag))
    }

    /// Under the lock: checks the precondition, then renames `tmp` to `path`.
    fn commit(
        &self,
        path: &str,
        staged: (NamedTempFile, String),
        expected: Option<&ObjectVersion>,
    ) -> Result<ObjectVersion, PutError> {
        let (tmp, e_tag) = staged;
        let dest = self.object(path)?;
        let _guard = self.lock()?;
        check_precondition(&dest, expected)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        tmp.persist(&dest).map_err(|e| PutError::from(e.error))?;
        if let Some(parent) = dest.parent() {
            sync_dir(parent);
        }
        Ok(ObjectVersion { e_tag: Some(e_tag), version: None })
    }

    fn read_object(&self, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let file = self.object(path)?;
        match fs::read(&file) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }

    fn open_object(&self, path: &str) -> anyhow::Result<File> {
        let file = self.object(path)?;
        File::open(&file).with_context(|| format!("{path}: no such object ({})", file.display()))
    }

    fn walk(&self, dir: &Path, prefix: &str, out: &mut Vec<ObjectMeta>) -> anyhow::Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        for entry in entries {
            let entry = entry?;
            let full = entry.path();
            let Some(rel) = self.relative(&full) else { continue };
            let meta = entry.metadata()?;
            if meta.is_dir() {
                // Descend only where the prefix can still match.
                let as_dir = format!("{rel}/");
                if as_dir.starts_with(prefix) || prefix.starts_with(&as_dir) {
                    self.walk(&full, prefix, out)?;
                }
            } else if rel.starts_with(prefix) {
                out.push(ObjectMeta {
                    path: rel,
                    size: meta.len(),
                    version: ObjectVersion::default(),
                    last_modified: meta.modified().ok(),
                });
            }
        }
        Ok(())
    }

    /// `full` relative to the root as an object path; `None` for the lock
    /// file, the scratch directory and non-UTF-8 names.
    fn relative(&self, full: &Path) -> Option<String> {
        let rel = full.strip_prefix(&self.root).ok()?;
        let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
        let parts = parts?;
        if matches!(parts.first(), Some(&LOCK_FILE | &TMP_DIR)) {
            return None;
        }
        Some(parts.join("/"))
    }
}

fn check_precondition(dest: &Path, expected: Option<&ObjectVersion>) -> Result<(), PutError> {
    let Some(expected) = expected else {
        return match fs::symlink_metadata(dest) {
            Ok(_) => Err(PutError::AlreadyExists),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        };
    };
    let current = match File::open(dest) {
        Ok(mut f) => copy_hashing(&mut f, &mut io::sink())?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PutError::Precondition),
        Err(e) => return Err(e.into()),
    };
    if expected.e_tag.as_deref() == Some(current.as_str()) { Ok(()) } else { Err(PutError::Precondition) }
}

/// Copies `input` to `output`, returning the BLAKE3 hex digest of the bytes.
fn copy_hashing(input: &mut impl Read, output: &mut impl Write) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
    Ok(blake3_hex(&hasher.finalize()))
}

/// Best-effort fsync of a directory so a completed rename survives a crash.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn versioned(bytes: Vec<u8>) -> Versioned<Bytes> {
    let e_tag = blake3_hex(&blake3::hash(&bytes));
    Versioned { value: Bytes::from(bytes), version: ObjectVersion { e_tag: Some(e_tag), version: None } }
}

impl Storage for FileStorage {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        Ok(self.read_object(path)?.map(versioned))
    }

    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        Ok(self.get(path)?.filter(|v| v.version.e_tag != known.e_tag))
    }

    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        let mut file = self.open_object(path)?;
        let size = file.metadata()?.len();
        let end = range.end.min(size);
        let start = range.start.min(end);
        file.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.take(end - start).read_to_end(&mut buf)?;
        Ok(Bytes::from(buf))
    }

    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        let mut input = self.open_object(path)?;
        let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut tmp = tempfile::Builder::new().prefix(".download-").tempfile_in(dir)?;
        let e_tag = copy_hashing(&mut input, tmp.as_file_mut())?;
        tmp.as_file().sync_all()?;
        tmp.persist(dest).map_err(|e| e.error)?;
        Ok(ObjectVersion { e_tag: Some(e_tag), version: None })
    }

    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        self.object(path)?;
        let staged = self.stage_bytes(&bytes)?;
        self.commit(path, staged, None)
    }

    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        // Not worth copying a file that is already there (checked again
        // under the lock).
        check_precondition(&self.object(path)?, None)?;
        let staged = self.stage_file(src)?;
        self.commit(path, staged, None)
    }

    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        self.object(path)?;
        let staged = self.stage_bytes(&bytes)?;
        self.commit(path, staged, expected)
    }

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        let mut out = Vec::new();
        self.walk(&self.root, prefix, &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn delete(&self, path: &str) -> anyhow::Result<()> {
        let file = self.object(path)?;
        let _guard = self.lock()?;
        match fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", file.display())),
        }
    }

    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.object(path).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_paths_and_hidden_internals() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStorage::open(dir.path()).unwrap();
        assert!(store.put_new(".lock", Bytes::from_static(b"x")).is_err());
        assert!(store.put_new(".tmp/x", Bytes::from_static(b"x")).is_err());
        store.put_new("a/b", Bytes::from_static(b"x")).unwrap();
        let listed: Vec<_> = store.list("").unwrap().into_iter().map(|m| m.path).collect();
        assert_eq!(listed, vec!["a/b".to_string()]);
        assert_eq!(store.local_path("a/b").unwrap(), dir.path().join("a/b"));
    }

    #[test]
    fn e_tag_is_blake3_of_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStorage::open(dir.path()).unwrap();
        let v = store.put_new("x", Bytes::from_static(b"hello")).unwrap();
        assert_eq!(v.e_tag.unwrap(), blake3::hash(b"hello").to_hex().to_string());
        assert!(FileStorage::open(Path::new("relative")).is_err());
    }
}
