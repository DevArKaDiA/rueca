//! The `.tlseg` bundle container: header, 8-byte-aligned components, footer
//! directory with per-component BLAKE3 checksums, fixed-size trailer.
//!
//! See the crate docs for the exact byte layout.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::{Deref, Range};
use std::path::Path;
use std::sync::Arc;

use memmap2::Mmap;

use crate::{Result, SegmentError};

pub(crate) const SEG_MAGIC: &[u8; 8] = b"TELARSEG";
pub(crate) const SEG_END_MAGIC: &[u8; 8] = b"TLSEGEND";
pub(crate) const HEADER_LEN: usize = 16;
/// `footer_offset u64 | footer_len u64 | blake3(footer) [32] | end magic [8]`.
pub(crate) const TRAILER_LEN: usize = 8 + 8 + 32 + 8;
pub(crate) const ALIGN: u64 = 8;

/// Writes a bundle sequentially. Components are appended in call order.
pub(crate) struct BundleWriter {
    out: BufWriter<File>,
    pos: u64,
    dir: Vec<(String, u64, u64, [u8; 32])>,
}

impl BundleWriter {
    pub fn create(path: &Path, version: u32) -> Result<BundleWriter> {
        let file = File::create(path)?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        out.write_all(SEG_MAGIC)?;
        out.write_all(&version.to_le_bytes())?;
        out.write_all(&0u32.to_le_bytes())?;
        Ok(BundleWriter { out, pos: HEADER_LEN as u64, dir: Vec::new() })
    }

    fn pad(&mut self) -> Result<()> {
        let rem = self.pos % ALIGN;
        if rem != 0 {
            let n = (ALIGN - rem) as usize;
            self.out.write_all(&[0u8; 8][..n])?;
            self.pos += n as u64;
        }
        Ok(())
    }

    pub fn add(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        if self.dir.iter().any(|(n, ..)| n == name) {
            return Err(SegmentError::Other(format!("duplicate bundle component {name}")));
        }
        if name.len() > u16::MAX as usize {
            return Err(SegmentError::Other("component name too long".into()));
        }
        self.pad()?;
        let hash = *blake3::hash(bytes).as_bytes();
        self.out.write_all(bytes)?;
        self.dir.push((name.to_string(), self.pos, bytes.len() as u64, hash));
        self.pos += bytes.len() as u64;
        Ok(())
    }

    pub fn add_pod<T: bytemuck::Pod>(&mut self, name: &str, items: &[T]) -> Result<()> {
        self.add(name, bytemuck::cast_slice(items))
    }

    /// Writes footer and trailer, flushes and fsyncs. Returns the file size.
    pub fn finish(mut self) -> Result<u64> {
        self.pad()?;
        let mut footer = Vec::new();
        footer.extend_from_slice(&(self.dir.len() as u32).to_le_bytes());
        for (name, off, len, hash) in &self.dir {
            footer.extend_from_slice(&(name.len() as u16).to_le_bytes());
            footer.extend_from_slice(name.as_bytes());
            footer.extend_from_slice(&off.to_le_bytes());
            footer.extend_from_slice(&len.to_le_bytes());
            footer.extend_from_slice(hash);
        }
        let footer_offset = self.pos;
        self.out.write_all(&footer)?;
        self.out.write_all(&footer_offset.to_le_bytes())?;
        self.out.write_all(&(footer.len() as u64).to_le_bytes())?;
        self.out.write_all(blake3::hash(&footer).as_bytes())?;
        self.out.write_all(SEG_END_MAGIC)?;
        let total = footer_offset + footer.len() as u64 + TRAILER_LEN as u64;
        let file = self.out.into_inner().map_err(|e| SegmentError::Io(e.into_error()))?;
        file.sync_all()?;
        Ok(total)
    }
}

/// A memory map shared between the bundle reader and tantivy's `OwnedBytes`.
#[derive(Clone)]
pub(crate) struct SharedMmap(pub Arc<Mmap>);

impl Deref for SharedMmap {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0[..]
    }
}

// SAFETY: the bytes live in the `Mmap` behind an `Arc`; moving the `SharedMmap`
// does not move the mapping, so derefs stay valid for its whole lifetime.
unsafe impl stable_deref_trait::StableDeref for SharedMmap {}

/// An opened, fully validated bundle.
pub(crate) struct Bundle {
    pub map: SharedMmap,
    pub components: HashMap<String, Range<usize>>,
}

fn corrupt(msg: impl Into<String>) -> SegmentError {
    SegmentError::Corrupt(msg.into())
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}

impl Bundle {
    /// Maps `path` and validates magic, version, footer and every component
    /// checksum. Never panics on malformed input.
    pub fn open(path: &Path, version: u32) -> Result<Bundle> {
        let file = File::open(path)?;
        // SAFETY: segment files are immutable once renamed into place; Telar
        // never writes to a published `.tlseg`.
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_mmap(Arc::new(mmap), version)
    }

    pub fn from_mmap(mmap: Arc<Mmap>, version: u32) -> Result<Bundle> {
        let b: &[u8] = &mmap[..];
        if b.len() < HEADER_LEN || &b[..8] != SEG_MAGIC {
            return Err(corrupt("bad magic (not a .tlseg file)"));
        }
        let v = le_u32(b, 8).ok_or_else(|| corrupt("short header"))?;
        if v != version {
            return Err(SegmentError::Version(v));
        }
        if le_u32(b, 12) != Some(0) {
            return Err(corrupt("unknown header flags"));
        }
        if b.len() < HEADER_LEN + TRAILER_LEN {
            return Err(corrupt("file too short"));
        }
        let t = b.len() - TRAILER_LEN;
        if &b[t + 48..t + 56] != SEG_END_MAGIC {
            return Err(corrupt("bad trailer magic (truncated?)"));
        }
        let footer_offset = le_u64(b, t).unwrap_or(u64::MAX);
        let footer_len = le_u64(b, t + 8).unwrap_or(u64::MAX);
        let footer_end = footer_offset.checked_add(footer_len).ok_or_else(|| corrupt("footer bounds"))?;
        if footer_offset < HEADER_LEN as u64 || footer_end != t as u64 {
            return Err(corrupt("footer bounds"));
        }
        let footer = &b[footer_offset as usize..t];
        if blake3::hash(footer).as_bytes() != &b[t + 16..t + 48] {
            return Err(corrupt("footer checksum mismatch"));
        }
        let count = le_u32(footer, 0).ok_or_else(|| corrupt("footer"))? as usize;
        let mut at = 4usize;
        let mut components = HashMap::with_capacity(count.min(4096));
        for _ in 0..count {
            let name_len = footer
                .get(at..at + 2)
                .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
                .ok_or_else(|| corrupt("footer entry"))?;
            at += 2;
            let name = footer.get(at..at + name_len).ok_or_else(|| corrupt("footer entry name"))?;
            let name = std::str::from_utf8(name).map_err(|_| corrupt("footer entry name utf8"))?.to_string();
            at += name_len;
            let off = le_u64(footer, at).ok_or_else(|| corrupt("footer entry"))?;
            let len = le_u64(footer, at + 8).ok_or_else(|| corrupt("footer entry"))?;
            let hash = footer.get(at + 16..at + 48).ok_or_else(|| corrupt("footer entry"))?;
            at += 48;
            let end = off.checked_add(len).ok_or_else(|| corrupt("component bounds"))?;
            if off < HEADER_LEN as u64 || end > footer_offset {
                return Err(corrupt(format!("component {name} out of bounds")));
            }
            let range = off as usize..end as usize;
            if blake3::hash(&b[range.clone()]).as_bytes() != hash {
                return Err(corrupt(format!("checksum mismatch in component {name}")));
            }
            if components.insert(name.clone(), range).is_some() {
                return Err(corrupt(format!("duplicate component {name}")));
            }
        }
        if at != footer.len() {
            return Err(corrupt("trailing bytes in footer"));
        }
        Ok(Bundle { map: SharedMmap(mmap), components })
    }

    pub fn range(&self, name: &str) -> Result<Range<usize>> {
        self.components.get(name).cloned().ok_or_else(|| corrupt(format!("missing component {name}")))
    }

    pub fn bytes(&self, name: &str) -> Result<&[u8]> {
        let r = self.range(name)?;
        Ok(&self.map[r])
    }

    /// Range of a component holding a `T` array; validates alignment and length.
    pub fn pod_range<T: bytemuck::Pod>(&self, name: &str) -> Result<Range<usize>> {
        let r = self.range(name)?;
        bytemuck::try_cast_slice::<u8, T>(&self.map[r.clone()])
            .map_err(|e| corrupt(format!("component {name} is not a valid array: {e:?}")))?;
        Ok(r)
    }
}
