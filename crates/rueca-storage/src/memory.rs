//! `memory://<name>`: an in-process store shared by name.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::SystemTime;

use anyhow::Context;
use bytes::Bytes;

use crate::{ObjectMeta, ObjectVersion, PutError, Storage, Versioned, check_path};

#[derive(Clone)]
struct Entry {
    bytes: Bytes,
    e_tag: String,
    modified: SystemTime,
}

#[derive(Default)]
struct Objects {
    map: BTreeMap<String, Entry>,
    writes: u64,
}

/// Objects in a `Mutex<BTreeMap>`. [`MemoryStorage::named`] returns one
/// instance per name for the whole process. E-tags are a per-store write
/// counter, so every write gets a new one.
#[derive(Default)]
pub struct MemoryStorage {
    objects: Mutex<Objects>,
}

fn registry() -> &'static Mutex<HashMap<String, Arc<MemoryStorage>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<MemoryStorage>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

impl MemoryStorage {
    /// The process-wide store called `name`, created empty on first use.
    pub fn named(name: &str) -> Arc<MemoryStorage> {
        let mut reg = registry().lock().unwrap_or_else(PoisonError::into_inner);
        reg.entry(name.to_string()).or_default().clone()
    }

    /// A private store, not registered under any name.
    pub fn new() -> MemoryStorage {
        MemoryStorage::default()
    }

    fn lock(&self) -> MutexGuard<'_, Objects> {
        self.objects.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read(&self, path: &str) -> anyhow::Result<Option<Entry>> {
        check_path(path)?;
        Ok(self.lock().map.get(path).cloned())
    }

    fn write(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        check_path(path)?;
        let mut objects = self.lock();
        let current = objects.map.get(path).map(|e| e.e_tag.as_str());
        match (expected, current) {
            (None, Some(_)) => return Err(PutError::AlreadyExists),
            (Some(_), None) => return Err(PutError::Precondition),
            (Some(v), Some(cur)) if v.e_tag.as_deref() != Some(cur) => {
                return Err(PutError::Precondition);
            }
            _ => {}
        }
        objects.writes += 1;
        let e_tag = format!("\"m{:016x}\"", objects.writes);
        let entry = Entry { bytes, e_tag: e_tag.clone(), modified: SystemTime::now() };
        objects.map.insert(path.to_string(), entry);
        Ok(ObjectVersion { e_tag: Some(e_tag), version: None })
    }
}

fn version(e: &Entry) -> ObjectVersion {
    ObjectVersion { e_tag: Some(e.e_tag.clone()), version: None }
}

impl Storage for MemoryStorage {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        Ok(self.read(path)?.map(|e| Versioned { version: version(&e), value: e.bytes }))
    }

    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        let got = self.get(path)?;
        Ok(got.filter(|v| v.version.e_tag.is_none() || v.version.e_tag != known.e_tag))
    }

    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        let e = self.read(path)?.with_context(|| format!("{path}: no such object"))?;
        let len = e.bytes.len();
        let end = usize::try_from(range.end).unwrap_or(usize::MAX).min(len);
        let start = usize::try_from(range.start).unwrap_or(usize::MAX).min(end);
        Ok(e.bytes.slice(start..end))
    }

    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        let e = self.read(path)?.with_context(|| format!("{path}: no such object"))?;
        let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut tmp = tempfile::Builder::new().prefix(".download-").tempfile_in(dir)?;
        tmp.write_all(&e.bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(dest).map_err(|err| err.error)?;
        Ok(version(&e))
    }

    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        self.write(path, bytes, None)
    }

    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        let bytes = fs::read(src).with_context(|| format!("reading {}", src.display()))?;
        self.write(path, Bytes::from(bytes), None)
    }

    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        self.write(path, bytes, expected)
    }

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        let objects = self.lock();
        let found = objects
            .map
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, e)| ObjectMeta {
                path: k.clone(),
                size: e.bytes.len() as u64,
                version: version(e),
                last_modified: Some(e.modified),
            })
            .collect();
        Ok(found)
    }

    fn delete(&self, path: &str) -> anyhow::Result<()> {
        check_path(path)?;
        self.lock().map.remove(path);
        Ok(())
    }

    fn local_path(&self, _path: &str) -> Option<PathBuf> {
        None
    }
}
