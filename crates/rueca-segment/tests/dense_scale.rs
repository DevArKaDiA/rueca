//! The exhaustive dense scan at organization scale (RFC 0007 §6.4, phase
//! 3): is an ANN index needed? Run with
//! `cargo test -p rueca-segment --release --test dense_scale -- --ignored --nocapture`
//! (`RUECA_SCALE_SEGMENTS`, `RUECA_SCALE_ROWS`, `RUECA_SCALE_DIMS`: default
//! 21 × 50,000 × 1,024, about 1.1 GB of int8 rows in memory).
//!
//! Per query: every segment's top 100 by the scan kernel of
//! `Vectors::top_n`, merged into one top 100; once segment after segment on
//! one thread, once one thread per segment (the per-repository parallel
//! scan). Random rows: the kernel's cost does not depend on the values.

use std::time::Instant;

use rueca_segment::vectors::Int8Rows;

const TOP: usize = 100;

/// A merged ranking: (segment, node, score).
type Merged = Vec<(usize, u32, f32)>;
/// A scan of every segment for one query.
type Scan = fn(&[Matrix], &[f32]) -> Merged;
const QUERIES: usize = 20;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// One synthetic segment's matrix.
struct Matrix {
    nodes: Vec<u32>,
    scales: Vec<f32>,
    data: Vec<u8>,
    dims: usize,
}

impl Matrix {
    fn random(rng: &mut fastrand::Rng, rows: usize, dims: usize) -> Matrix {
        let mut data = vec![0u8; rows * dims];
        rng.fill(&mut data);
        Matrix {
            nodes: (0..rows as u32).collect(),
            scales: (0..rows).map(|_| 0.004 + 0.004 * rng.f32()).collect(),
            data,
            dims,
        }
    }

    fn rows(&self) -> Int8Rows<'_> {
        Int8Rows { nodes: &self.nodes, scales: &self.scales, data: bytemuck::cast_slice(&self.data), dims: self.dims }
    }
}

/// The merged top `TOP` of per-segment lists: (score desc, segment, node).
fn merge(lists: Vec<Vec<(u32, f32)>>) -> Merged {
    let mut all: Vec<(usize, u32, f32)> =
        lists.into_iter().enumerate().flat_map(|(s, l)| l.into_iter().map(move |(n, x)| (s, n, x))).collect();
    all.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    all.truncate(TOP);
    all
}

fn single(segs: &[Matrix], q: &[f32]) -> Merged {
    merge(segs.iter().map(|m| m.rows().top_n(q, TOP, |_| true)).collect())
}

fn parallel(segs: &[Matrix], q: &[f32]) -> Merged {
    let lists = std::thread::scope(|s| {
        let handles: Vec<_> = segs.iter().map(|m| s.spawn(move || m.rows().top_n(q, TOP, |_| true))).collect();
        handles.into_iter().map(|h| h.join().expect("scan thread")).collect()
    });
    merge(lists)
}

/// (median, p95) of milliseconds.
fn percentiles(mut ms: Vec<f64>) -> (f64, f64) {
    ms.sort_by(f64::total_cmp);
    let at = |p: f64| ms[((ms.len() - 1) as f64 * p).round() as usize];
    (at(0.5), at(0.95))
}

fn measure(segs: &[Matrix], queries: &[Vec<f32>], scan: Scan) -> (f64, f64) {
    let _ = scan(segs, &queries[0]);
    let ms = queries
        .iter()
        .map(|q| {
            let t = Instant::now();
            std::hint::black_box(scan(segs, q));
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    percentiles(ms)
}

#[test]
#[ignore = "measurement"]
fn exhaustive_scan_at_organization_scale() {
    let (n_segs, rows, dims) =
        (env("RUECA_SCALE_SEGMENTS", 21), env("RUECA_SCALE_ROWS", 50_000), env("RUECA_SCALE_DIMS", 1024));
    let mut rng = fastrand::Rng::with_seed(7);
    let t = Instant::now();
    let segs: Vec<Matrix> = (0..n_segs).map(|_| Matrix::random(&mut rng, rows, dims)).collect();
    let queries: Vec<Vec<f32>> = (0..QUERIES)
        .map(|_| {
            let v: Vec<f32> = (0..dims).map(|_| rng.f32() - 0.5).collect();
            rueca_segment::vectors::truncate_normalize(&v, dims)
        })
        .collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!(
        "{n_segs} segments × {rows} rows × {dims} dims ({:.2} GB int8) built in {:.1} s; {threads} hardware threads",
        (n_segs * rows * dims) as f64 / 1e9,
        t.elapsed().as_secs_f64()
    );
    assert_eq!(single(&segs, &queries[0]), parallel(&segs, &queries[0]), "both scans rank alike");
    for k in [1, n_segs.div_ceil(4), n_segs.div_ceil(2), n_segs] {
        let part = &segs[..k];
        let total = (k * rows) as f64;
        let (s50, s95) = measure(part, &queries, single);
        let (p50, p95) = measure(part, &queries, parallel);
        println!(
            "{k:>3} segments, {:>9} vectors: single thread p50 {s50:7.1} ms p95 {s95:7.1} ms ({:.1} ns/row); \
             per-segment threads p50 {p50:6.1} ms p95 {p95:6.1} ms",
            k * rows,
            s50 * 1e6 / total,
        );
    }
}
