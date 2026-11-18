//! Symbol vectors inside a segment (RFC 0007 §6): optional `vectors.*`
//! components, int8 with one scale per row, scanned exhaustively.
//!
//! | Component | Encoding | Content |
//! |---|---|---|
//! | `vectors.meta` | JSON | [`VectorsMeta`]: model record, keys, `dims`, `rows`, `encoding` (`i8`) |
//! | `vectors.nodes` | `[u32; V]` | node ids with a vector, strictly ascending |
//! | `vectors.keys` | `[[u8; 32]; V]` | vector keys (the store cache's keys, [`crate::vcache`]) |
//! | `vectors.scale` | `[f32; V]` | per-row scale: component = `i8 × scale` |
//! | `vectors.data` | `[i8; V × dims]` | rows, Matryoshka-truncated to `dims` and re-normalized before quantization |
//!
//! `vectors.meta` and `vectors.nodes` are checksummed when the segment
//! opens; `keys`, `scale` and `data` (the bulk) are checksummed on the first
//! [`crate::Segment::vectors`] call of the process, so lexical queries never
//! pay for them.

use std::ops::Range;

use rueca_model::NodeId;
use serde::{Deserialize, Serialize};

use crate::bundle::{Bundle, BundleWriter, SharedMmap};
use crate::{Result, SegmentError};

/// `vectors.meta`.
pub const META: &str = "vectors.meta";
/// `vectors.nodes`.
pub const NODES: &str = "vectors.nodes";
/// `vectors.keys`.
pub const KEYS: &str = "vectors.keys";
/// `vectors.scale`.
pub const SCALE: &str = "vectors.scale";
/// `vectors.data`.
pub const DATA: &str = "vectors.data";

/// Components checksummed lazily (on first use), not at open.
pub(crate) fn is_lazy(name: &str) -> bool {
    matches!(name, KEYS | SCALE | DATA)
}

/// Whether a component belongs to the vectors section.
pub(crate) fn is_vectors(name: &str) -> bool {
    name.starts_with("vectors.")
}

/// IEEE 754 half precision bits of `x`, rounded to nearest even
/// (overflow → infinity, NaN kept).
pub fn f32_to_f16(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        return sign | round_shift(m, shift) as u16;
    }
    let r = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    let up = rem > 0x1000 || (rem == 0x1000 && (r & 1) == 1);
    sign | (r + u32::from(up)) as u16
}

/// `m >> shift` rounded to nearest even.
fn round_shift(m: u32, shift: u32) -> u32 {
    let half = 1u32 << (shift - 1);
    let rem = m & ((1u32 << shift) - 1);
    let r = m >> shift;
    if rem > half || (rem == half && (r & 1) == 1) { r + 1 } else { r }
}

/// The `f32` value of IEEE 754 half precision bits.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    match (exp, man) {
        (0, 0) => f32::from_bits(sign),
        (0, _) => {
            let v = man as f32 * (1.0 / 16_777_216.0);
            if sign != 0 { -v } else { v }
        }
        (0x1f, 0) => f32::from_bits(sign | 0x7f80_0000),
        (0x1f, _) => f32::from_bits(sign | 0x7fc0_0000 | (man << 13)),
        _ => f32::from_bits(sign | ((exp + 112) << 23) | (man << 13)),
    }
}

/// The first `dims` components of `v`, L2-normalized (Matryoshka).
pub fn truncate_normalize(v: &[f32], dims: usize) -> Vec<f32> {
    let mut out = v[..dims.min(v.len())].to_vec();
    let norm = out.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    if norm > 0.0 {
        let inv = (1.0 / norm) as f32;
        out.iter_mut().for_each(|x| *x *= inv);
    }
    out
}

/// int8 quantization with one scale: `max |x| / 127` (1 for a zero row),
/// components rounded to nearest.
pub fn quantize(v: &[f32]) -> (f32, Vec<i8>) {
    let max = v.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
    let scale = if max > 0.0 && max.is_finite() { max / 127.0 } else { 1.0 };
    let q = v.iter().map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8).collect();
    (scale, q)
}

/// `vectors.meta`: what the vectors are and how to embed a query for them.
/// Fields Rueca reads are typed; the rest of the model record (runtime,
/// backend, batch sizes, …) is kept as is in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorsMeta {
    /// Preset that embedded the documents.
    pub preset: String,
    /// Embed key (hex): what a query embedder must share with the segment.
    pub embed_key: String,
    /// Model key (hex) of the stored vectors: embed key, `dims`, `i8`.
    pub model_key: String,
    /// sha256 of the weights.
    pub model_sha256: String,
    /// Stored width (≤ `max_dims`).
    pub dims: usize,
    /// Model width (the store cache's width).
    pub max_dims: usize,
    /// Always `i8`.
    pub encoding: String,
    /// Number of vectors.
    pub rows: usize,
    /// Version of the canonical text.
    pub text_builder_version: u32,
    /// Token budget per text.
    pub token_budget: usize,
    /// The rest of the model record.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Vectors to write into a segment, built row by row.
#[derive(Debug, Clone)]
pub struct VectorsWrite {
    meta: VectorsMeta,
    nodes: Vec<u32>,
    keys: Vec<[u8; 32]>,
    scale: Vec<f32>,
    data: Vec<i8>,
}

impl VectorsWrite {
    /// An empty set; `meta.rows` is set by [`VectorsWrite::push`].
    pub fn new(mut meta: VectorsMeta) -> VectorsWrite {
        meta.rows = 0;
        "i8".clone_into(&mut meta.encoding);
        VectorsWrite { meta, nodes: Vec::new(), keys: Vec::new(), scale: Vec::new(), data: Vec::new() }
    }

    /// Adds the vector of `node` (ascending order), given at any width ≥
    /// `meta.dims`: truncated to `dims`, re-normalized, quantized.
    ///
    /// # Errors
    ///
    /// Nodes out of order or a vector narrower than `dims`.
    pub fn push(&mut self, node: NodeId, key: [u8; 32], full: &[f32]) -> Result<()> {
        if self.nodes.last().is_some_and(|&last| last >= node) {
            return Err(SegmentError::Other(format!("vectors: node {node} out of order")));
        }
        if full.len() < self.meta.dims {
            return Err(SegmentError::Other(format!(
                "vectors: {} components, segment stores {}",
                full.len(),
                self.meta.dims
            )));
        }
        let (scale, q) = quantize(&truncate_normalize(full, self.meta.dims));
        self.nodes.push(node);
        self.keys.push(key);
        self.scale.push(scale);
        self.data.extend(q);
        self.meta.rows = self.nodes.len();
        Ok(())
    }

    /// The meta written with the vectors.
    pub fn meta(&self) -> &VectorsMeta {
        &self.meta
    }

    /// Rows so far.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no row was added.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub(crate) fn check_nodes(&self, node_count: usize) -> Result<()> {
        match self.nodes.last() {
            Some(&n) if n as usize >= node_count => {
                Err(SegmentError::Other(format!("vectors: node {n} beyond the {node_count} nodes of the segment")))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn write(&self, w: &mut BundleWriter) -> Result<()> {
        let meta = serde_json::to_vec(&self.meta).map_err(|e| SegmentError::Other(e.to_string()))?;
        w.add(META, &meta)?;
        w.add_pod(NODES, &self.nodes)?;
        w.add(KEYS, self.keys.as_flattened())?;
        w.add_pod(SCALE, &self.scale)?;
        w.add_pod(DATA, &self.data)
    }
}

/// The vectors of an opened segment.
pub struct Vectors {
    meta: VectorsMeta,
    map: SharedMmap,
    nodes: Range<usize>,
    keys: Range<usize>,
    scale: Range<usize>,
    data: Range<usize>,
}

fn corrupt(msg: impl Into<String>) -> SegmentError {
    SegmentError::Corrupt(msg.into())
}

impl Vectors {
    /// Reads the components (lengths and node ids checked; the lazy
    /// checksums are the caller's), `None` when the segment has none.
    pub(crate) fn open(bundle: &Bundle, node_count: usize) -> Result<Option<Vectors>> {
        let Ok(meta_bytes) = bundle.bytes(META) else { return Ok(None) };
        let meta: VectorsMeta = serde_json::from_slice(meta_bytes).map_err(|e| corrupt(format!("{META}: {e}")))?;
        if meta.encoding != "i8" || meta.dims == 0 {
            return Err(corrupt(format!("{META}: encoding {} × {} not supported", meta.encoding, meta.dims)));
        }
        let nodes = bundle.pod_range::<u32>(NODES)?;
        let keys = bundle.range(KEYS)?;
        let scale = bundle.pod_range::<f32>(SCALE)?;
        let data = bundle.pod_range::<i8>(DATA)?;
        let v = meta.rows;
        let ok = nodes.len() == v * 4
            && keys.len() == v * 32
            && scale.len() == v * 4
            && Some(data.len()) == v.checked_mul(meta.dims);
        if !ok {
            return Err(corrupt("vectors.* lengths disagree with vectors.meta"));
        }
        let ids: &[u32] = bytemuck::cast_slice(&bundle.map[nodes.clone()]);
        if ids.windows(2).any(|w| w[0] >= w[1]) || ids.last().is_some_and(|&n| n as usize >= node_count) {
            return Err(corrupt("vectors.nodes not ascending or out of range"));
        }
        Ok(Some(Vectors { meta, map: bundle.map.clone(), nodes, keys, scale, data }))
    }

    /// The record of the vectors.
    pub fn meta(&self) -> &VectorsMeta {
        &self.meta
    }

    /// Stored width.
    pub fn dims(&self) -> usize {
        self.meta.dims
    }

    /// Number of vectors.
    pub fn len(&self) -> usize {
        self.meta.rows
    }

    /// Whether there is no vector.
    pub fn is_empty(&self) -> bool {
        self.meta.rows == 0
    }

    /// Node ids with a vector, ascending (row order).
    pub fn nodes(&self) -> &[u32] {
        bytemuck::cast_slice(&self.map[self.nodes.clone()])
    }

    /// The vector key of a row.
    pub fn key(&self, row: usize) -> [u8; 32] {
        let b = &self.map[self.keys.clone()][row * 32..row * 32 + 32];
        b.try_into().expect("32-byte key")
    }

    fn scales(&self) -> &[f32] {
        bytemuck::cast_slice(&self.map[self.scale.clone()])
    }

    fn data(&self) -> &[i8] {
        bytemuck::cast_slice(&self.map[self.data.clone()])
    }

    /// The row of `node`, if it has a vector.
    pub fn row_of(&self, node: NodeId) -> Option<usize> {
        self.nodes().binary_search(&node).ok()
    }

    /// The dequantized row.
    pub fn row(&self, row: usize) -> Vec<f32> {
        let d = self.dims();
        let s = self.scales()[row];
        self.data()[row * d..(row + 1) * d].iter().map(|&x| f32::from(x) * s).collect()
    }

    /// Dot product of a row with `query` (`dims` components; the cosine for
    /// a unit query, up to quantization).
    pub fn score(&self, row: usize, query: &[f32]) -> f32 {
        let d = self.dims();
        dot_i8(&self.data()[row * d..(row + 1) * d], query) * self.scales()[row]
    }

    /// The `n` best rows for `query` among the nodes `keep` accepts, by an
    /// exhaustive scan: `(node, score)` by score descending, then node id.
    /// A query of the wrong width matches nothing.
    pub fn top_n(&self, query: &[f32], n: usize, keep: impl Fn(NodeId) -> bool) -> Vec<(NodeId, f32)> {
        let rows = Int8Rows { nodes: self.nodes(), scales: self.scales(), data: self.data(), dims: self.dims() };
        rows.top_n(query, n, keep)
    }
}

/// int8 rows with a per-row scale and their node ids: the matrix the
/// exhaustive dense scan reads (RFC 0007 §6.4), borrowed from a segment or
/// built in memory (the organization-scale measurement).
#[derive(Debug, Clone, Copy)]
pub struct Int8Rows<'a> {
    /// Node id of each row, ascending.
    pub nodes: &'a [u32],
    /// Scale of each row.
    pub scales: &'a [f32],
    /// `nodes.len() × dims` components, row-major.
    pub data: &'a [i8],
    /// Components per row.
    pub dims: usize,
}

impl Int8Rows<'_> {
    /// The `n` best rows for `query` among the nodes `keep` accepts:
    /// `(node, score)` by score descending, then node id. A query of the
    /// wrong width matches nothing.
    pub fn top_n(&self, query: &[f32], n: usize, keep: impl Fn(NodeId) -> bool) -> Vec<(NodeId, f32)> {
        let d = self.dims;
        if query.len() != d || n == 0 || d == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(NodeId, f32)> = self
            .nodes
            .iter()
            .zip(self.data.chunks_exact(d))
            .zip(self.scales)
            .filter(|((node, _), _)| keep(**node))
            .map(|((node, row), s)| (*node, dot_i8(row, query) * s))
            .collect();
        let order = |a: &(NodeId, f32), b: &(NodeId, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
        if scored.len() > n {
            scored.select_nth_unstable_by(n - 1, order);
            scored.truncate(n);
        }
        scored.sort_by(order);
        scored
    }
}

/// `Σ row[i] × q[i]`: 16 independent accumulators over fixed-size chunks,
/// a loop LLVM turns into SIMD multiply-adds.
fn dot_i8(row: &[i8], q: &[f32]) -> f32 {
    const L: usize = 16;
    let (rc, rt) = row.as_chunks::<L>();
    let (qc, qt) = q.as_chunks::<L>();
    let mut acc = [0.0_f32; L];
    for (r, x) in rc.iter().zip(qc) {
        for i in 0..L {
            acc[i] += f32::from(r[i]) * x[i];
        }
    }
    let tail: f32 = rt.iter().zip(qt).map(|(a, b)| f32::from(*a) * b).sum();
    acc.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_known_values_and_round_trip() {
        for (x, h) in [
            (0.0_f32, 0x0000_u16),
            (-0.0, 0x8000),
            (1.0, 0x3c00),
            (0.5, 0x3800),
            (-2.0, 0xc000),
            (65504.0, 0x7bff),
            (65520.0, 0x7c00),
            (f32::INFINITY, 0x7c00),
            (6e-8, 0x0001),
            (1e-8, 0x0000),
            (0.333_333_34, 0x3555),
        ] {
            assert_eq!(f32_to_f16(x), h, "{x}");
        }
        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());
        for h in 0..=u16::MAX {
            let x = f16_to_f32(h);
            if !x.is_nan() {
                assert_eq!(f32_to_f16(x), h, "{h:#06x}");
            }
        }
    }

    #[test]
    fn quantize_keeps_direction() {
        let v = truncate_normalize(&[3.0, -4.0, 0.0, 12.0], 2);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] + 0.8).abs() < 1e-6);
        let (s, q) = quantize(&v);
        assert_eq!(q[1], -127);
        assert!((f32::from(q[0]) * s - 0.6).abs() < 0.01);
        assert_eq!(quantize(&[0.0, 0.0]), (1.0, vec![0, 0]));
    }

    #[test]
    fn dot_matches_the_naive_sum() {
        let row: Vec<i8> = (0..37).map(|i| (i * 7 % 255 - 127) as i8).collect();
        let q: Vec<f32> = (0..37).map(|i| i as f32 / 10.0 - 1.0).collect();
        let naive: f32 = row.iter().zip(&q).map(|(a, b)| f32::from(*a) * b).sum();
        assert!((dot_i8(&row, &q) - naive).abs() < 1e-3);
    }
}
