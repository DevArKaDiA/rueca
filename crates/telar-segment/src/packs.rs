//! Content packs (`packs/<id>.tlpack`) and the store-level blob index
//! (`packs/index.tlpi`). See the crate docs for the byte layout.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::{Result, SegmentError};

pub(crate) const PACK_MAGIC: &[u8; 8] = b"TELARPAK";
pub(crate) const PACK_END_MAGIC: &[u8; 8] = b"TLPAKEND";
pub(crate) const PACK_VERSION: u32 = 1;
const PACK_HEADER_LEN: usize = 16;
/// `footer_offset u64 | blake3(footer) [32] | end magic [8]`.
const PACK_TRAILER_LEN: usize = 8 + 32 + 8;
/// `sha [20] | offset u64 | compressed_len u64 | raw_len u64`.
const PACK_ENTRY_LEN: usize = 20 + 8 + 8 + 8;
pub(crate) const INDEX_MAGIC: &[u8; 8] = b"TELARPXI";
pub(crate) const INDEX_FILE: &str = "index.tlpi";
pub(crate) const ZSTD_LEVEL: i32 = 3;

pub(crate) type Sha = [u8; 20];

pub(crate) fn parse_sha(hex: &str) -> Option<Sha> {
    let hex = hex.as_bytes();
    if hex.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (hex[2 * i] as char).to_digit(16)?;
        let lo = (hex[2 * i + 1] as char).to_digit(16)?;
        *o = (hi * 16 + lo) as u8;
    }
    Some(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlobLoc {
    pub pack: u32,
    pub offset: u64,
    pub clen: u64,
    pub raw_len: u64,
}

#[derive(Serialize, Deserialize)]
struct IndexFile {
    packs: Vec<String>,
    entries: Vec<(Sha, BlobLoc)>,
}

/// In-memory blob index over every pack in `<store>/packs`.
#[derive(Default)]
pub(crate) struct BlobIndex {
    pub packs: Vec<String>,
    pub map: HashMap<Sha, BlobLoc>,
}

fn corrupt(msg: impl Into<String>) -> SegmentError {
    SegmentError::Corrupt(msg.into())
}

fn list_packs(dir: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    match fs::read_dir(dir) {
        Ok(rd) => {
            for ent in rd {
                let name = ent?.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".tlpack") {
                    out.insert(stem.to_string());
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(out)
}

/// Reads and validates a pack footer: `(sha, offset, clen, raw_len)` entries.
fn read_pack_footer(bytes: &[u8]) -> Result<Vec<(Sha, u64, u64, u64)>> {
    if bytes.len() < PACK_HEADER_LEN + PACK_TRAILER_LEN || &bytes[..8] != PACK_MAGIC {
        return Err(corrupt("bad pack header"));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != PACK_VERSION {
        return Err(SegmentError::Version(version));
    }
    let t = bytes.len() - PACK_TRAILER_LEN;
    if &bytes[t + 40..t + 48] != PACK_END_MAGIC {
        return Err(corrupt("bad pack trailer"));
    }
    let footer_offset = u64::from_le_bytes(bytes[t..t + 8].try_into().unwrap());
    if footer_offset < PACK_HEADER_LEN as u64 || footer_offset > t as u64 {
        return Err(corrupt("bad pack footer offset"));
    }
    let footer = &bytes[footer_offset as usize..t];
    if blake3::hash(footer).as_bytes() != &bytes[t + 8..t + 40] {
        return Err(corrupt("pack footer checksum mismatch"));
    }
    if footer.len() < 4 {
        return Err(corrupt("short pack footer"));
    }
    let count = u32::from_le_bytes(footer[..4].try_into().unwrap()) as usize;
    if footer.len() != 4 + count * PACK_ENTRY_LEN {
        return Err(corrupt("pack footer length"));
    }
    let mut out = Vec::with_capacity(count);
    for c in footer[4..].as_chunks::<PACK_ENTRY_LEN>().0 {
        let sha: Sha = c[..20].try_into().unwrap();
        let off = u64::from_le_bytes(c[20..28].try_into().unwrap());
        let clen = u64::from_le_bytes(c[28..36].try_into().unwrap());
        let raw = u64::from_le_bytes(c[36..44].try_into().unwrap());
        if off < PACK_HEADER_LEN as u64 || off.checked_add(clen).is_none_or(|end| end > footer_offset) {
            return Err(corrupt("pack entry out of bounds"));
        }
        out.push((sha, off, clen, raw));
    }
    Ok(out)
}

fn map_file(path: &Path) -> Result<Arc<Mmap>> {
    let f = File::open(path)?;
    // SAFETY: packs are immutable once renamed into place.
    Ok(Arc::new(unsafe { Mmap::map(&f)? }))
}

impl BlobIndex {
    /// Loads `<packs>/index.tlpi` if it is valid and lists exactly the packs on
    /// disk; otherwise rebuilds the index by scanning pack footers (corrupt
    /// packs are skipped). Returns `(index, rebuilt)`.
    pub fn load(packs_dir: &Path) -> Result<(BlobIndex, bool)> {
        let on_disk = list_packs(packs_dir)?;
        if let Some(idx) = Self::read_index_file(&packs_dir.join(INDEX_FILE)) {
            let listed: BTreeSet<String> = idx.packs.iter().cloned().collect();
            if listed == on_disk {
                return Ok((idx, false));
            }
        }
        let mut idx = BlobIndex::default();
        for name in on_disk {
            let Ok(map) = map_file(&packs_dir.join(format!("{name}.tlpack"))) else { continue };
            let Ok(entries) = read_pack_footer(&map) else { continue };
            let pack = idx.packs.len() as u32;
            idx.packs.push(name);
            for (sha, offset, clen, raw_len) in entries {
                idx.map.entry(sha).or_insert(BlobLoc { pack, offset, clen, raw_len });
            }
        }
        Ok((idx, true))
    }

    fn read_index_file(path: &Path) -> Option<BlobIndex> {
        let bytes = fs::read(path).ok()?;
        if bytes.len() < 16 + 32 || &bytes[..8] != INDEX_MAGIC {
            return None;
        }
        if u32::from_le_bytes(bytes[8..12].try_into().ok()?) != PACK_VERSION {
            return None;
        }
        let (payload, hash) = bytes[16..].split_at(bytes.len() - 16 - 32);
        if blake3::hash(payload).as_bytes() != hash {
            return None;
        }
        let file: IndexFile = postcard::from_bytes(payload).ok()?;
        if file.entries.iter().any(|(_, l)| l.pack as usize >= file.packs.len()) {
            return None;
        }
        Some(BlobIndex { packs: file.packs, map: file.entries.into_iter().collect() })
    }

    /// Atomically rewrites `<packs>/index.tlpi` (entries sorted by sha).
    pub fn save(&self, packs_dir: &Path, tmp_dir: &Path) -> Result<()> {
        let mut entries: Vec<(Sha, BlobLoc)> = self.map.iter().map(|(k, v)| (*k, *v)).collect();
        entries.sort_unstable_by_key(|(k, _)| *k);
        let payload = postcard::to_stdvec(&IndexFile { packs: self.packs.clone(), entries })
            .map_err(|e| SegmentError::Other(e.to_string()))?;
        let mut bytes = Vec::with_capacity(payload.len() + 48);
        bytes.extend_from_slice(INDEX_MAGIC);
        bytes.extend_from_slice(&PACK_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(blake3::hash(&payload).as_bytes());
        crate::store::atomic_write(&packs_dir.join(INDEX_FILE), tmp_dir, &bytes)
    }
}

/// Appends zstd frames to a new pack in `tmp/`; `finish` publishes it.
pub(crate) struct PackWriter {
    out: BufWriter<File>,
    tmp_path: PathBuf,
    pos: u64,
    entries: Vec<(Sha, u64, u64, u64)>,
}

impl PackWriter {
    pub fn create(tmp_dir: &Path, id: &str) -> Result<PackWriter> {
        let tmp_path = tmp_dir.join(format!("{id}.tlpack.tmp"));
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&tmp_path)?);
        out.write_all(PACK_MAGIC)?;
        out.write_all(&PACK_VERSION.to_le_bytes())?;
        out.write_all(&0u32.to_le_bytes())?;
        Ok(PackWriter { out, tmp_path, pos: PACK_HEADER_LEN as u64, entries: Vec::new() })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends one already-compressed frame. Returns its offset.
    pub fn append(&mut self, sha: Sha, frame: &[u8], raw_len: u64) -> Result<u64> {
        let off = self.pos;
        self.out.write_all(frame)?;
        self.pos += frame.len() as u64;
        self.entries.push((sha, off, frame.len() as u64, raw_len));
        Ok(off)
    }

    /// Writes footer + trailer, fsyncs and renames the pack into `packs_dir`.
    /// Returns the bytes written.
    pub fn finish(mut self, packs_dir: &Path, id: &str) -> Result<u64> {
        let mut footer = Vec::with_capacity(4 + self.entries.len() * PACK_ENTRY_LEN);
        footer.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (sha, off, clen, raw) in &self.entries {
            footer.extend_from_slice(sha);
            footer.extend_from_slice(&off.to_le_bytes());
            footer.extend_from_slice(&clen.to_le_bytes());
            footer.extend_from_slice(&raw.to_le_bytes());
        }
        let footer_offset = self.pos;
        self.out.write_all(&footer)?;
        self.out.write_all(&footer_offset.to_le_bytes())?;
        self.out.write_all(blake3::hash(&footer).as_bytes())?;
        self.out.write_all(PACK_END_MAGIC)?;
        let file = self.out.into_inner().map_err(|e| SegmentError::Io(e.into_error()))?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp_path, packs_dir.join(format!("{id}.tlpack")))?;
        crate::store::sync_dir(packs_dir);
        Ok(footer_offset + footer.len() as u64 + PACK_TRAILER_LEN as u64)
    }

    pub fn abort(self) {
        let path = self.tmp_path.clone();
        drop(self);
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn compress(raw: &[u8]) -> Result<Vec<u8>> {
    let mut enc = zstd::bulk::Compressor::new(ZSTD_LEVEL)?;
    enc.include_checksum(true)?;
    enc.include_contentsize(true)?;
    Ok(enc.compress(raw)?)
}

/// Read side: blob index loaded lazily, packs mmapped lazily.
pub(crate) struct PackReader {
    packs_dir: PathBuf,
    index: std::sync::OnceLock<std::result::Result<BlobIndex, String>>,
    maps: Mutex<HashMap<u32, Arc<Mmap>>>,
}

impl PackReader {
    pub fn new(store: &Path) -> PackReader {
        PackReader { packs_dir: store.join("packs"), index: std::sync::OnceLock::new(), maps: Mutex::new(HashMap::new()) }
    }

    /// Starts from an already-loaded index (used by the writer).
    pub fn with_index(store: &Path, index: BlobIndex) -> PackReader {
        let r = PackReader::new(store);
        let _ = r.index.set(Ok(index));
        r
    }

    fn index(&self) -> Result<&BlobIndex> {
        self.index
            .get_or_init(|| BlobIndex::load(&self.packs_dir).map(|(i, _)| i).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| SegmentError::Other(format!("loading blob index: {e}")))
    }

    pub fn into_index(self) -> Option<BlobIndex> {
        self.index.into_inner().and_then(|r| r.ok())
    }

    pub fn read(&self, sha_hex: &str) -> Result<Vec<u8>> {
        let sha = parse_sha(sha_hex).ok_or_else(|| SegmentError::NotFound(format!("blob {sha_hex} (invalid sha)")))?;
        let index = self.index()?;
        let loc = *index.map.get(&sha).ok_or_else(|| SegmentError::NotFound(format!("blob {sha_hex} not in any pack")))?;
        let map = {
            let mut maps = self.maps.lock().unwrap_or_else(|p| p.into_inner());
            match maps.get(&loc.pack) {
                Some(m) => m.clone(),
                None => {
                    let name = index.packs.get(loc.pack as usize).ok_or_else(|| corrupt("blob index pack id"))?;
                    let m = map_file(&self.packs_dir.join(format!("{name}.tlpack")))?;
                    maps.insert(loc.pack, m.clone());
                    m
                }
            }
        };
        let end = loc.offset.checked_add(loc.clen).ok_or_else(|| corrupt("blob bounds"))?;
        let frame = map.get(loc.offset as usize..end as usize).ok_or_else(|| corrupt(format!("blob {sha_hex} out of pack bounds")))?;
        if loc.raw_len > (1u64 << 34) {
            return Err(corrupt("blob raw length implausible"));
        }
        let raw = zstd::bulk::decompress(frame, loc.raw_len as usize)
            .map_err(|e| corrupt(format!("blob {sha_hex}: zstd: {e}")))?;
        if raw.len() as u64 != loc.raw_len {
            return Err(corrupt(format!("blob {sha_hex}: length mismatch")));
        }
        Ok(raw)
    }
}
