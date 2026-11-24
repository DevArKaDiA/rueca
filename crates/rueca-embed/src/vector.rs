//! Vector arithmetic and the deterministic batch order.

use std::cmp::Ordering;
use std::ops::Range;

/// Scales `v` to unit L2 norm in place; a zero vector stays zero.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    if norm > 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        let inv = (1.0 / norm) as f32;
        v.iter_mut().for_each(|x| *x *= inv);
    }
}

/// Matryoshka truncation: the first `dims` components, re-normalized.
/// `dims` at or above the length keeps the whole (normalized) vector.
#[must_use]
pub fn truncate_normalized(v: &[f32], dims: usize) -> Vec<f32> {
    let mut out = v[..dims.min(v.len())].to_vec();
    l2_normalize(&mut out);
    out
}

/// Dot product, the cosine of two unit vectors.
#[must_use]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Cosine similarity of two vectors of any norm (0 when either is zero).
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (na, nb) = (dot(a, a).sqrt(), dot(b, b).sqrt());
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot(a, b) / (na * nb) }
}

/// The order in which inputs are embedded (RFC 0007 §9): by token length,
/// then by BLAKE3 of the model input, then by position. Batches are filled
/// in this order, so equal inputs meet equal batch neighbours whatever
/// order the caller passed them in, and similar lengths share a batch.
///
/// `lens[i]` and `texts[i]` describe input `i`; the result lists input
/// positions.
#[must_use]
pub fn batch_order(texts: &[&str], lens: &[usize]) -> Vec<usize> {
    let hashes: Vec<blake3::Hash> = texts.iter().map(|t| blake3::hash(t.as_bytes())).collect();
    let mut order: Vec<usize> = (0..texts.len()).collect();
    order.sort_by(|&a, &b| compare(lens[a], &hashes[a], a, lens[b], &hashes[b], b));
    order
}

fn compare(la: usize, ha: &blake3::Hash, a: usize, lb: usize, hb: &blake3::Hash, b: usize) -> Ordering {
    la.cmp(&lb).then_with(|| ha.as_bytes().cmp(hb.as_bytes())).then(a.cmp(&b))
}

/// Puts results computed in `order` (see [`batch_order`]) back in input
/// order: `sorted[k]` belongs to input `order[k]`.
#[must_use]
pub fn restore_order<T>(sorted: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = std::iter::repeat_with(|| None).take(order.len()).collect();
    for (value, &pos) in sorted.into_iter().zip(order) {
        slots[pos] = Some(value);
    }
    slots.into_iter().flatten().collect()
}

/// Splits positions `0..lens.len()` of an already ordered list into
/// consecutive batches of at most `max_tokens` tokens and `max_seqs`
/// sequences. An input longer than `max_tokens` gets a batch of its own
/// (callers cut inputs to the budget first).
#[must_use]
pub fn pack_batches(lens: &[usize], max_tokens: usize, max_seqs: usize) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let (mut start, mut tokens) = (0, 0);
    for (i, &len) in lens.iter().enumerate() {
        let full = i > start && (tokens + len > max_tokens || i - start >= max_seqs);
        if full {
            batches.push(start..i);
            (start, tokens) = (i, 0);
        }
        tokens += len;
    }
    if start < lens.len() {
        batches.push(start..lens.len());
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_and_truncate() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);

        let full = [0.5_f32, 0.5, 0.5, 0.5];
        let t = truncate_normalized(&full, 2);
        assert_eq!(t.len(), 2);
        assert!((dot(&t, &t) - 1.0).abs() < 1e-6);
        assert!((t[0] - t[1]).abs() < 1e-6);
        assert_eq!(truncate_normalized(&full, 10).len(), 4);

        let mut zero = vec![0.0_f32; 3];
        l2_normalize(&mut zero);
        assert_eq!(zero, vec![0.0; 3]);
    }

    #[test]
    fn batches_respect_tokens_and_sequences() {
        assert_eq!(pack_batches(&[3, 3, 3, 3], 6, 10), vec![0..2, 2..4]);
        assert_eq!(pack_batches(&[1, 1, 1], 100, 2), vec![0..2, 2..3]);
        assert_eq!(pack_batches(&[9, 1], 4, 10), vec![0..1, 1..2]);
        assert!(pack_batches(&[], 4, 4).is_empty());
    }

    #[test]
    fn cosine_of_scaled_vectors_is_one() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 0.0]).abs() < f32::EPSILON);
    }

    #[test]
    fn batch_order_sorts_by_length_then_content_and_restores() {
        let texts = ["ccc", "a", "bb", "a", "zz"];
        let lens = [3, 1, 2, 1, 2];
        let order = batch_order(&texts, &lens);
        // Shortest first; the two equal "a" keep their relative order.
        assert_eq!(&order[..2], &[1, 3]);
        assert_eq!(order[4], 0);
        // The order depends on content, not on input position.
        let swapped = ["zz", "a", "bb", "a", "ccc"];
        let lens2 = [2, 1, 2, 1, 3];
        let order2 = batch_order(&swapped, &lens2);
        let seq1: Vec<&str> = order.iter().map(|&i| texts[i]).collect();
        let seq2: Vec<&str> = order2.iter().map(|&i| swapped[i]).collect();
        assert_eq!(seq1, seq2);

        let sorted: Vec<&str> = order.iter().map(|&i| texts[i]).collect();
        assert_eq!(restore_order(sorted, &order), texts.to_vec());
    }
}
