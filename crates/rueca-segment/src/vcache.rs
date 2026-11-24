//! The store vector cache (RFC 0007 §5.5): full-width `f16` vectors keyed
//! by their vector key, shared by every segment, worktree and repository
//! of a store (and across stores with `RUECA_VECTOR_DIR`).
//!
//! ```text
//! <vectors>/<embed key>/meta.json   {"format": 1, "dims": D, "encoding": "f16"}
//! <vectors>/<embed key>/<xx>.keys   [u8; 32] vector keys, append-only
//! <vectors>/<embed key>/<xx>.f16    [u16; D] rows (IEEE half, little-endian), same order
//! ```
//!
//! `<vectors>` is `$RUECA_VECTOR_DIR` when set, else `<store>/vectors`.
//! `<xx>` is the first byte of the key in hex: 256 shards, each a pair of
//! append-only files, so a lookup reads only the shards its keys fall in.
//! The directory is named by the embed key, not the model key: entries are
//! full width and unquantized, so every Matryoshka width and encoding a
//! segment chooses reads the same entry.
//!
//! # Writers and readers
//!
//! - **Writers** (any number of processes, e.g. indexers of different
//!   repositories sharing a store) take an exclusive advisory lock on the
//!   shard's `.keys` file (`File::lock`, `flock` on Unix) for the whole
//!   append. Under the lock they repair the shard (below), skip keys the
//!   shard already holds, append the rows to `.f16`, fsync, then append
//!   the keys to `.keys` and fsync. A key is therefore never visible before
//!   its row.
//! - **Readers** take no lock: a shard holds `min(keys, rows)` complete
//!   entries, counted from the file lengths, so an append in progress is
//!   invisible and a torn tail is ignored.
//! - **Repair**: a shard whose lengths disagree (a crash between the two
//!   appends, or a partial write) is truncated to its complete entries by
//!   the next writer. Rows carry no checksum: the cache holds derived data
//!   that a `rueca index` re-embeds if it is deleted.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::vectors::{f16_to_f32, f32_to_f16};
use crate::{Result, SegmentError};

/// Environment variable that moves the cache out of the store.
pub const VECTOR_DIR_ENV: &str = "RUECA_VECTOR_DIR";

/// Cache layout version (`meta.json`).
const CACHE_FORMAT: u32 = 1;

/// A 32-byte vector key.
pub type VectorKeyBytes = [u8; 32];

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct CacheMeta {
    format: u32,
    dims: usize,
    encoding: String,
}

/// The vectors of one embed key.
#[derive(Debug, Clone)]
pub struct VectorCache {
    dir: PathBuf,
    dims: usize,
}

/// Where a store's cache lives: `$RUECA_VECTOR_DIR`, else `<store>/vectors`.
pub fn vector_root(store: &Path) -> PathBuf {
    match std::env::var_os(VECTOR_DIR_ENV) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => store.join("vectors"),
    }
}

fn shard_of(key: &VectorKeyBytes) -> u8 {
    key[0]
}

fn other(msg: impl Into<String>) -> SegmentError {
    SegmentError::Other(msg.into())
}

impl VectorCache {
    /// Opens (creating it if needed) the cache of `embed_key` under `root`
    /// for vectors of `dims` components.
    ///
    /// # Errors
    ///
    /// I/O errors, or an existing cache of another width or layout.
    pub fn create(root: &Path, embed_key: &str, dims: usize) -> Result<VectorCache> {
        if dims == 0 || embed_key.is_empty() || !embed_key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(other(format!("invalid vector cache key {embed_key:?} / width {dims}")));
        }
        let dir = root.join(embed_key);
        fs::create_dir_all(&dir)?;
        let want = CacheMeta { format: CACHE_FORMAT, dims, encoding: "f16".into() };
        let meta_path = dir.join("meta.json");
        match read_meta(&meta_path)? {
            Some(have) if have == want => {}
            Some(have) => {
                return Err(other(format!(
                    "vector cache {} holds {} × {} vectors (format {}), not {dims} × f16",
                    dir.display(),
                    have.dims,
                    have.encoding,
                    have.format
                )));
            }
            None => {
                let bytes = serde_json::to_vec(&want).map_err(|e| other(e.to_string()))?;
                let mut tmp = tempfile::Builder::new().prefix(".meta-").tempfile_in(&dir)?;
                tmp.write_all(&bytes)?;
                tmp.as_file().sync_all()?;
                tmp.persist(&meta_path).map_err(|e| SegmentError::Io(e.error))?;
                crate::store::sync_dir(&dir);
            }
        }
        Ok(VectorCache { dir, dims })
    }

    /// Opens the cache of `embed_key` under `root` for reading; `None` when
    /// it does not exist.
    ///
    /// # Errors
    ///
    /// An unreadable or invalid `meta.json`.
    pub fn open(root: &Path, embed_key: &str) -> Result<Option<VectorCache>> {
        let dir = root.join(embed_key);
        Ok(read_meta(&dir.join("meta.json"))?.map(|m| VectorCache { dir, dims: m.dims }))
    }

    /// Components per vector.
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// The cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn paths(&self, shard: u8) -> (PathBuf, PathBuf) {
        (self.dir.join(format!("{shard:02x}.keys")), self.dir.join(format!("{shard:02x}.f16")))
    }

    fn row_bytes(&self) -> u64 {
        self.dims as u64 * 2
    }

    /// The complete entries of a shard (no lock): its keys, at most as many
    /// as it has rows.
    fn shard_keys(&self, shard: u8) -> Result<Vec<VectorKeyBytes>> {
        let (kp, fp) = self.paths(shard);
        let keys = match fs::read(&kp) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let rows = fs::metadata(&fp).map(|m| m.len() / self.row_bytes()).unwrap_or(0);
        let n = ((keys.len() / 32) as u64).min(rows) as usize;
        Ok(keys[..n * 32].as_chunks::<32>().0.to_vec())
    }

    /// Every key in the cache with its (shard, row): the indexer's lookup.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn index(&self) -> Result<CacheIndex> {
        let mut map = HashMap::new();
        for shard in 0..=255u8 {
            for (row, k) in self.shard_keys(shard)?.into_iter().enumerate() {
                map.entry(k).or_insert((shard, row as u32));
            }
        }
        Ok(CacheIndex { map })
    }

    /// The rows of `keys` (full width, `f32`), `None` for keys not cached;
    /// reads only the shards the keys fall in.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn get(&self, keys: &[VectorKeyBytes]) -> Result<Vec<Option<Vec<f32>>>> {
        let mut by_shard: HashMap<u8, Vec<usize>> = HashMap::new();
        for (i, k) in keys.iter().enumerate() {
            by_shard.entry(shard_of(k)).or_default().push(i);
        }
        let mut out = vec![None; keys.len()];
        let mut shards: Vec<u8> = by_shard.keys().copied().collect();
        shards.sort_unstable();
        for shard in shards {
            let rows: HashMap<VectorKeyBytes, u32> =
                self.shard_keys(shard)?.into_iter().enumerate().rev().map(|(row, k)| (k, row as u32)).collect();
            let wanted: Vec<(usize, u32)> =
                by_shard[&shard].iter().filter_map(|&i| rows.get(&keys[i]).map(|&r| (i, r))).collect();
            if wanted.is_empty() {
                continue;
            }
            let mut f = File::open(self.paths(shard).1)?;
            for (i, row) in wanted {
                out[i] = Some(self.read_row(&mut f, row)?);
            }
        }
        Ok(out)
    }

    /// Reads rows located by [`VectorCache::index`].
    ///
    /// # Errors
    ///
    /// I/O errors (a row the index lists is always complete).
    pub fn read_located(&self, locs: &[(u8, u32)]) -> Result<Vec<Vec<f32>>> {
        let mut files: HashMap<u8, File> = HashMap::new();
        let mut out = Vec::with_capacity(locs.len());
        for &(shard, row) in locs {
            let f = match files.entry(shard) {
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => v.insert(File::open(self.paths(shard).1)?),
            };
            out.push(self.read_row(f, row)?);
        }
        Ok(out)
    }

    fn read_row(&self, f: &mut File, row: u32) -> Result<Vec<f32>> {
        let mut buf = vec![0u8; self.dims * 2];
        f.seek(SeekFrom::Start(u64::from(row) * self.row_bytes()))?;
        f.read_exact(&mut buf)?;
        Ok(buf.as_chunks::<2>().0.iter().map(|c| f16_to_f32(u16::from_le_bytes(*c))).collect())
    }

    /// Appends `entries` (full-width vectors) shard by shard under the
    /// shard lock, skipping keys already present. Returns how many entries
    /// were written.
    ///
    /// # Errors
    ///
    /// I/O errors, or a vector of the wrong width.
    pub fn append(&self, entries: &[(VectorKeyBytes, &[f32])]) -> Result<usize> {
        if let Some((_, v)) = entries.iter().find(|(_, v)| v.len() != self.dims) {
            return Err(other(format!("vector of {} components in a {}-wide cache", v.len(), self.dims)));
        }
        let mut by_shard: HashMap<u8, Vec<usize>> = HashMap::new();
        for (i, (k, _)) in entries.iter().enumerate() {
            by_shard.entry(shard_of(k)).or_default().push(i);
        }
        let mut shards: Vec<u8> = by_shard.keys().copied().collect();
        shards.sort_unstable();
        let mut written = 0;
        for shard in shards {
            let rows: Vec<(VectorKeyBytes, &[f32])> = by_shard[&shard].iter().map(|&i| entries[i]).collect();
            written += self.append_shard(shard, &rows)?;
        }
        Ok(written)
    }

    fn append_shard(&self, shard: u8, rows: &[(VectorKeyBytes, &[f32])]) -> Result<usize> {
        let (kp, fp) = self.paths(shard);
        let open = |p: &Path| OpenOptions::new().read(true).write(true).create(true).truncate(false).open(p);
        let mut keys_file = open(&kp)?;
        keys_file.lock()?;
        let mut data_file = open(&fp)?;
        let n = self.repair(&keys_file, &data_file)?;
        let mut have = vec![0u8; n as usize * 32];
        keys_file.seek(SeekFrom::Start(0))?;
        keys_file.read_exact(&mut have)?;
        let mut seen: HashSet<VectorKeyBytes> = have.as_chunks::<32>().0.iter().copied().collect();
        let mut key_bytes = Vec::new();
        let mut row_bytes = Vec::new();
        for (k, v) in rows {
            if seen.insert(*k) {
                key_bytes.extend_from_slice(k);
                row_bytes.extend(v.iter().flat_map(|x| f32_to_f16(*x).to_le_bytes()));
            }
        }
        if key_bytes.is_empty() {
            return Ok(0);
        }
        data_file.seek(SeekFrom::Start(n * self.row_bytes()))?;
        data_file.write_all(&row_bytes)?;
        data_file.sync_data()?;
        keys_file.seek(SeekFrom::Start(n * 32))?;
        keys_file.write_all(&key_bytes)?;
        keys_file.sync_data()?;
        Ok(key_bytes.len() / 32)
    }

    /// Truncates a shard to its complete entries (caller holds the lock);
    /// returns their count.
    fn repair(&self, keys_file: &File, data_file: &File) -> Result<u64> {
        let (kl, dl) = (keys_file.metadata()?.len(), data_file.metadata()?.len());
        let n = (kl / 32).min(dl / self.row_bytes());
        if kl != n * 32 || dl != n * self.row_bytes() {
            tracing::warn!(
                cache = %self.dir.display(),
                key_bytes = kl,
                row_bytes = dl,
                kept = n,
                "vector cache shard lengths disagree; truncated to its complete entries"
            );
            keys_file.set_len(n * 32)?;
            data_file.set_len(n * self.row_bytes())?;
        }
        Ok(n)
    }
}

fn read_meta(path: &Path) -> Result<Option<CacheMeta>> {
    match fs::read(path) {
        Ok(b) => {
            serde_json::from_slice(&b).map(Some).map_err(|e| SegmentError::Corrupt(format!("{}: {e}", path.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Every key of a cache with its location, from [`VectorCache::index`].
#[derive(Debug, Default)]
pub struct CacheIndex {
    map: HashMap<VectorKeyBytes, (u8, u32)>,
}

impl CacheIndex {
    /// The (shard, row) of `key`.
    pub fn get(&self, key: &VectorKeyBytes) -> Option<(u8, u32)> {
        self.map.get(key).copied()
    }

    /// Entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EK: &str = "00ff";

    fn key(first: u8, n: u8) -> VectorKeyBytes {
        let mut k = [n; 32];
        k[0] = first;
        k
    }

    fn row(x: f32, dims: usize) -> Vec<f32> {
        (0..dims).map(|i| x + i as f32 / 8.0).collect()
    }

    #[test]
    fn round_trip_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let c = VectorCache::create(dir.path(), EK, 4).unwrap();
        let (a, b) = (row(0.5, 4), row(-1.0, 4));
        assert_eq!(c.append(&[(key(1, 1), &a), (key(2, 2), &b), (key(1, 1), &a)]).unwrap(), 2);
        assert_eq!(c.append(&[(key(1, 1), &a)]).unwrap(), 0);
        let got = c.get(&[key(1, 1), key(3, 3), key(2, 2)]).unwrap();
        assert_eq!(got, vec![Some(a.clone()), None, Some(b.clone())]);
        let idx = c.index().unwrap();
        assert_eq!(idx.len(), 2);
        let rows = c.read_located(&[idx.get(&key(2, 2)).unwrap()]).unwrap();
        assert_eq!(rows, vec![b]);
        // Reopened read-only, same content; another width is refused.
        let r = VectorCache::open(dir.path(), EK).unwrap().unwrap();
        assert_eq!(r.dims(), 4);
        assert!(VectorCache::create(dir.path(), EK, 8).is_err());
        assert!(VectorCache::open(dir.path(), "abcd").unwrap().is_none());
    }

    #[test]
    fn two_repositories_share_entries() {
        // Two indexers of different repositories embed the same text: the
        // second finds the first's entry and appends nothing.
        let dir = tempfile::tempdir().unwrap();
        let v = row(0.25, 8);
        let first = VectorCache::create(dir.path(), EK, 8).unwrap();
        assert_eq!(first.append(&[(key(9, 9), &v)]).unwrap(), 1);
        let second = VectorCache::create(dir.path(), EK, 8).unwrap();
        assert!(second.index().unwrap().get(&key(9, 9)).is_some());
        assert_eq!(second.append(&[(key(9, 9), &v)]).unwrap(), 0);
        let handles: Vec<_> = (0..4u8)
            .map(|t| {
                let c = second.clone();
                std::thread::spawn(move || {
                    for i in 0..20u8 {
                        let r = row(f32::from(i), 8);
                        c.append(&[(key(7, i), &r), (key(t, i), &r)]).unwrap();
                    }
                })
            })
            .collect();
        handles.into_iter().for_each(|h| h.join().unwrap());
        let idx = second.index().unwrap();
        assert_eq!(idx.len(), 1 + 20 + 4 * 20);
        let got = second.get(&[key(7, 5)]).unwrap();
        assert_eq!(got[0].as_deref(), Some(row(5.0, 8).as_slice()));
    }

    #[test]
    fn torn_shard_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let c = VectorCache::create(dir.path(), EK, 4).unwrap();
        let a = row(1.0, 4);
        c.append(&[(key(5, 1), &a), (key(5, 2), &a)]).unwrap();
        let (kp, fp) = c.paths(5);
        // A crash after the row append: one row without its key, plus half a key.
        let mut d = OpenOptions::new().append(true).open(&fp).unwrap();
        d.write_all(&[0u8; 8]).unwrap();
        let mut k = OpenOptions::new().append(true).open(&kp).unwrap();
        k.write_all(&[5u8; 16]).unwrap();
        // Readers see the complete entries only.
        assert_eq!(c.index().unwrap().len(), 2);
        // The next writer truncates the tail and appends after it.
        let b = row(-2.0, 4);
        assert_eq!(c.append(&[(key(5, 3), &b)]).unwrap(), 1);
        assert_eq!(fs::metadata(&kp).unwrap().len(), 3 * 32);
        assert_eq!(fs::metadata(&fp).unwrap().len(), 3 * 8);
        assert_eq!(c.get(&[key(5, 3), key(5, 2)]).unwrap(), vec![Some(b), Some(a)]);
    }
}
