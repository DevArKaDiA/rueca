//! Local store directories and the JSON catalog.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{CatalogEntry, Result, SegmentError, SegmentMeta, Store};

pub(crate) const CATALOG_FILE: &str = "catalog.json";

/// Creates `segments/`, `packs/` and `tmp/` under `dir`.
pub(crate) fn ensure_dirs(dir: &Path) -> Result<()> {
    for sub in ["segments", "packs", "tmp"] {
        fs::create_dir_all(dir.join(sub))?;
    }
    Ok(())
}

/// Writes `bytes` to a unique file in `tmp_dir`, fsyncs it and renames it over
/// `dest`. `tmp_dir` must be on the same filesystem as `dest`.
pub(crate) fn atomic_write(dest: &Path, tmp_dir: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = tempfile::Builder::new().prefix(".atomic-").tempfile_in(tmp_dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(dest).map_err(|e| SegmentError::Io(e.error))?;
    if let Some(parent) = dest.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Best-effort fsync of a directory so a completed rename survives a crash.
/// No-op where directories cannot be opened for syncing (Windows).
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Leftovers in `tmp/` older than this are from crashed writers.
const STALE_TMP: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Best-effort removal of scratch files and directories abandoned by crashed
/// writers (anything in `tmp/` not modified for [`STALE_TMP`]).
pub(crate) fn clean_stale_tmp(tmp_dir: &Path) {
    let Ok(rd) = fs::read_dir(tmp_dir) else { return };
    for ent in rd.flatten() {
        let stale = ent
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_TMP);
        if stale {
            let p = ent.path();
            let _ = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        }
    }
}

/// Canonicalizes `p`; for paths that do not exist, canonicalizes the nearest
/// existing ancestor and re-appends the rest. Relative paths are made absolute.
pub(crate) fn canonical(p: &Path) -> PathBuf {
    if let Ok(c) = fs::canonicalize(p) {
        return c;
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    };
    let mut rest = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(c) = fs::canonicalize(cur) {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return abs,
        }
    }
}

impl Store {
    pub(crate) fn open_impl(dir: &Path) -> Result<Store> {
        ensure_dirs(dir)?;
        Ok(Store { dir: dir.to_path_buf() })
    }

    pub(crate) fn default_dir_impl() -> PathBuf {
        if let Some(h) = std::env::var_os("TELAR_HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(h);
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        home.join(".cache").join("telar")
    }

    pub(crate) fn entries_impl(&self) -> Result<Vec<CatalogEntry>> {
        match fs::read(self.dir.join(CATALOG_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| SegmentError::Corrupt(format!("catalog.json: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, entries: &[CatalogEntry]) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(entries).map_err(|e| SegmentError::Other(e.to_string()))?;
        ensure_dirs(&self.dir)?;
        atomic_write(&self.dir.join(CATALOG_FILE), &self.dir.join("tmp"), &bytes)
    }

    pub(crate) fn register_impl(&self, root: &str, meta: &SegmentMeta) -> Result<()> {
        let root = canonical(Path::new(root)).to_string_lossy().into_owned();
        let mut entries = self.entries_impl()?;
        let mut replaced = Vec::new();
        entries.retain(|e| {
            if e.root == root {
                replaced.push(e.segment_id.clone());
                false
            } else {
                true
            }
        });
        entries.push(CatalogEntry {
            root,
            repo: meta.snapshot.repo.clone(),
            segment_id: meta.id.clone(),
            indexed_at: meta.snapshot.indexed_at,
        });
        entries.sort_by(|a, b| a.root.cmp(&b.root));
        self.save(&entries)?;
        // The catalog no longer points at the replaced segment, so deleting it
        // is best effort: a failure (e.g. a reader still holding it open on a
        // platform that forbids unlinking mapped files) only leaks a file.
        // Readers that already mapped it keep working on Unix.
        for old in replaced {
            if old != meta.id && !entries.iter().any(|e| e.segment_id == old) {
                let _ = fs::remove_file(self.segment_path(&old));
            }
        }
        Ok(())
    }

    pub(crate) fn entry_for_path_impl(&self, path: &Path) -> Result<Option<CatalogEntry>> {
        let path = canonical(path);
        Ok(self
            .entries_impl()?
            .into_iter()
            .filter(|e| path.starts_with(Path::new(&e.root)))
            .max_by_key(|e| Path::new(&e.root).components().count()))
    }
}
