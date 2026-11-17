//! Hybrid search (RFC 0007 §7): `search_symbols` in `lexical`, `hybrid` or
//! `semantic` mode.
//!
//! - **Candidates**: the BM25 top N (today's ranking, priors and routing
//!   included) and, per selected snapshot with vectors, the dense top N of
//!   an exhaustive int8 scan under the same filters (labels, language,
//!   `path_glob`, scope); N = [`FusionParams::n`] (at least the requested
//!   page).
//! - **Scores**: BM25 is recovered raw (the hit's score divided by its prior
//!   and routing boost); every candidate with a vector gets its cosine, from
//!   int8 and rescored from the store's `f16` cache when it is reachable.
//! - **Fusion** (§7.2): `s = (α·b̂ + (1 − α)·d̂) · prior · route`, `b̂` and
//!   `d̂` normalized ([`Norm`], min-max over each list's top N by default);
//!   BM25 is 0 outside its top N. A snapshot
//!   without vectors (or with another embed key) is scored lexically,
//!   `s = b̂ · prior · route` (§10), and listed in `retrieval.lexical`.
//!   `semantic` is `d̂ · prior · route` over the dense candidates only.
//!   Ties break by (snapshot, node).
//! - **Degradation** (§7.7): with no `mode`, hybrid when a selected snapshot
//!   has vectors and the model loads, else lexical (with a warning when a
//!   model failed); an explicit `hybrid` / `semantic` that cannot be served
//!   is an error naming `rueca index --embed` or `rueca models pull`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use rueca_embed::{EmbedOptions, Embedder, RetrievalConfig};
use rueca_model::NodeId;
use rueca_segment::vcache::{VectorCache, vector_root};
use rueca_segment::vectors::truncate_normalize;
use rueca_segment::{SymbolQuery, Vectors, VectorsMeta};
use serde_json::{Value, json};

use crate::args::SearchModeArg;
use crate::{Engine, Selection};

/// Dense candidates rescored from the `f16` cache, at most (RFC 0007 §6.2).
const RESCORE_TOP: usize = 200;

/// Score normalization before fusion (RFC 0007 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Norm {
    /// Min-max over each list's own top N (the default until phase 1
    /// tuning): a list of equal scores normalizes to 1, a value outside its
    /// list's range (a BM25 candidate's cosine) is clamped to `[0, 1]`.
    MinMax,
    /// By the theoretical minimum, the RFC's first choice: `b̂ = bm25 /
    /// max`, `d̂ = (cos + 1) / (max cos + 1)` over the candidates. Cosines
    /// of unrelated code sit well above −1, so it compresses the dense
    /// signal into a narrow band near 1.
    Theoretical,
}

/// Fusion parameters: `RUECA_FUSION_ALPHA`, `RUECA_FUSION_N`,
/// `RUECA_FUSION_NORM` (`minmax` / `theoretical`), else `[retrieval]`
/// of `<store>/rueca.toml`, else α 0.5, N 100, min-max.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusionParams {
    /// Weight of BM25, in `[0, 1]`.
    pub alpha: f32,
    /// Candidates per list.
    pub n: usize,
    /// Normalization.
    pub norm: Norm,
}

impl Default for FusionParams {
    fn default() -> Self {
        FusionParams { alpha: 0.5, n: 100, norm: Norm::MinMax }
    }
}

impl FusionParams {
    /// The parameters in effect: environment over `config` over defaults.
    /// Invalid values are ignored.
    pub fn resolve(config: &RetrievalConfig) -> FusionParams {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let d = FusionParams::default();
        let alpha = env("RUECA_FUSION_ALPHA").and_then(|v| v.parse().ok()).or(config.alpha);
        let n = env("RUECA_FUSION_N").and_then(|v| v.parse().ok()).or(config.fusion_n);
        let norm = env("RUECA_FUSION_NORM").or_else(|| config.normalization.clone());
        FusionParams {
            alpha: alpha.filter(|a: &f32| (0.0..=1.0).contains(a)).unwrap_or(d.alpha),
            n: n.filter(|n: &usize| *n > 0).unwrap_or(d.n),
            norm: match norm.as_deref() {
                Some("minmax" | "min-max") => Norm::MinMax,
                Some("theoretical") => Norm::Theoretical,
                _ => d.norm,
            },
        }
    }
}

/// Loads the query embedder for a segment's vectors.
pub trait QueryEmbedderLoader: Send + Sync {
    /// An embedder whose embed key equals `meta.embed_key`.
    ///
    /// # Errors
    ///
    /// Missing weights (naming `rueca models pull`), an unknown preset, a
    /// model that differs from the one that embedded the snapshot.
    fn load(&self, meta: &VectorsMeta) -> anyhow::Result<Arc<dyn Embedder>>;
}

/// The presets of `rueca-embed`, weights from the store's model cache.
#[derive(Debug, Clone)]
pub struct PresetQueryLoader {
    /// The store (its `models/`, or `$RUECA_MODEL_DIR`).
    pub store: PathBuf,
}

impl QueryEmbedderLoader for PresetQueryLoader {
    fn load(&self, meta: &VectorsMeta) -> anyhow::Result<Arc<dyn Embedder>> {
        let preset = rueca_embed::preset(&meta.preset)?;
        let opts = EmbedOptions {
            max_tokens: meta.token_budget,
            text_builder_version: meta.text_builder_version,
            ..EmbedOptions::default()
        };
        let cache = rueca_embed::ModelCache::for_store(&self.store);
        let e: Arc<dyn Embedder> = Arc::from(rueca_embed::backend::load(&cache, preset, &opts)?);
        anyhow::ensure!(
            e.meta().embed_key.to_string() == meta.embed_key,
            "the local `{}` model differs from the one that embedded the snapshot (embed key {}); \
             re-index with `rueca index --embed`",
            meta.preset,
            short(&meta.embed_key)
        );
        Ok(e)
    }
}

fn short(key: &str) -> &str {
    key.get(..8).unwrap_or(key)
}

/// `retrieval` of the envelope (RFC 0007 §8.2).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Retrieval {
    pub mode: &'static str,
    /// `preset:model-key-prefix` of the vectors used.
    pub model: Option<String>,
    pub alpha: Option<f32>,
    /// Snapshots (repository ids) ranked without vectors in a fused answer.
    pub lexical: Vec<String>,
    pub warnings: Vec<String>,
}

impl Retrieval {
    fn lexical(warnings: Vec<String>) -> Retrieval {
        Retrieval { mode: "lexical", warnings, ..Retrieval::default() }
    }

    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("mode".into(), json!(self.mode));
        if let Some(model) = &self.model {
            m.insert("model".into(), json!(model));
        }
        if let Some(a) = self.alpha {
            m.insert("alpha".into(), json!(crate::compact::sig3(f64::from(a))));
        }
        if !self.lexical.is_empty() {
            m.insert("lexical".into(), json!(self.lexical));
        }
        if !self.warnings.is_empty() {
            m.insert("warnings".into(), json!(self.warnings));
        }
        Value::Object(m)
    }
}

/// One fusion candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Candidate {
    pub view: usize,
    pub node: NodeId,
    /// Raw BM25, `None` outside the BM25 top N.
    pub bm25: Option<f32>,
    /// Cosine, `None` without a vector.
    pub cos: Option<f32>,
    /// Whether `cos` comes from the dense top N (min-max range).
    pub in_dense: bool,
    pub prior: f32,
    pub route: f32,
    /// The snapshot has no vectors for the active model: scored lexically.
    pub lexical_only: bool,
}

/// Min and max of an iterator of scores.
fn range(xs: impl Iterator<Item = f32>) -> Option<(f32, f32)> {
    xs.fold(None, |acc, x| match acc {
        None => Some((x, x)),
        Some((lo, hi)) => Some((lo.min(x), hi.max(x))),
    })
}

/// Scales `x` into `[0, 1]` by `range` (min-max); a degenerate range gives
/// 1 at or above its value, else 0.
fn minmax(x: f32, (lo, hi): (f32, f32)) -> f32 {
    if hi > lo {
        ((x - lo) / (hi - lo)).clamp(0.0, 1.0)
    } else if x >= hi {
        1.0
    } else {
        0.0
    }
}

/// The normalized (b̂, d̂) of every candidate.
fn normalized(cands: &[Candidate], norm: Norm) -> Vec<(f32, f32)> {
    let bm = range(cands.iter().filter_map(|c| c.bm25));
    match norm {
        Norm::Theoretical => {
            let max_b = bm.map_or(0.0, |r| r.1);
            let max_c = range(cands.iter().filter(|c| !c.lexical_only).filter_map(|c| c.cos)).map_or(-1.0, |r| r.1);
            cands
                .iter()
                .map(|c| {
                    let b = c.bm25.filter(|_| max_b > 0.0).map_or(0.0, |b| b / max_b);
                    let d = c.cos.filter(|_| max_c > -1.0).map_or(0.0, |x| (x + 1.0) / (max_c + 1.0));
                    (b, d)
                })
                .collect()
        }
        Norm::MinMax => {
            let dr = range(cands.iter().filter(|c| c.in_dense).filter_map(|c| c.cos));
            cands
                .iter()
                .map(|c| {
                    let b = c.bm25.zip(bm).map_or(0.0, |(x, r)| minmax(x, r));
                    let d = c.cos.zip(dr).map_or(0.0, |(x, r)| minmax(x, r));
                    (b, d)
                })
                .collect()
        }
    }
}

/// Convex fusion (RFC 0007 §7.2): `(view, node, score)` by score
/// descending, then view, then node.
pub(crate) fn fuse(cands: &[Candidate], alpha: f32, norm: Norm) -> Vec<(usize, NodeId, f64)> {
    let mut out: Vec<(usize, NodeId, f64)> = cands
        .iter()
        .zip(normalized(cands, norm))
        .map(|(c, (b, d))| {
            let s = if c.lexical_only { b } else { alpha * b + (1.0 - alpha) * d };
            (c.view, c.node, f64::from(s) * f64::from(c.prior) * f64::from(c.route))
        })
        .collect();
    out.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    out
}

/// The vectors of each selected view that the active model can serve.
struct Dense<'s> {
    vectors: Vec<Option<&'s Vectors>>,
    /// The query at each served view's width.
    queries: Vec<Option<Vec<f32>>>,
    meta: VectorsMeta,
}

/// Outcome of preparing the dense side.
enum Setup<'s> {
    Dense(Box<Dense<'s>>, Retrieval),
    Lexical(Retrieval),
}

/// `(view, node, score)` in rank order.
pub(crate) type RankedNodes = Vec<(usize, NodeId, f64)>;

/// A query embedder loaded once per process, or why it could not be.
pub(crate) type EmbedderSlot = Result<Arc<dyn Embedder>, String>;

impl Engine {
    /// The ranking of `search_symbols` in the requested mode, honoring
    /// `base.offset` / `base.limit`, and what the envelope says about it.
    pub(crate) fn rank_with_mode(
        &self,
        sel: &Selection,
        base: &SymbolQuery,
        mode: Option<SearchModeArg>,
    ) -> anyhow::Result<(RankedNodes, Retrieval)> {
        if mode == Some(SearchModeArg::Lexical) {
            return Ok((self.rank_symbols(sel, base)?, Retrieval::lexical(Vec::new())));
        }
        let semantic = mode == Some(SearchModeArg::Semantic);
        let (dense, mut retrieval) = match self.dense_setup(sel, &base.text, mode.is_some())? {
            Setup::Dense(d, r) => (d, r),
            Setup::Lexical(r) => return Ok((self.rank_symbols(sel, base)?, r)),
        };
        let limit = if base.limit == 0 { 20 } else { base.limit };
        let pool = self.fusion.n.max(base.offset.saturating_add(limit));
        let ranked = self.rank_symbols_routed(sel, &SymbolQuery { offset: 0, limit: pool, ..base.clone() })?;
        let cands = self.candidates(sel, base, &dense, &ranked, pool, semantic)?;
        let alpha = if semantic { 0.0 } else { self.fusion.alpha };
        retrieval.mode = if semantic { "semantic" } else { "hybrid" };
        retrieval.alpha = (!semantic).then_some(alpha);
        if semantic && !retrieval.lexical.is_empty() {
            let skipped = std::mem::take(&mut retrieval.lexical);
            retrieval.warnings.push(format!("not searched in semantic mode (no vectors): {}", skipped.join(", ")));
        }
        let fused = fuse(&cands, alpha, self.fusion.norm);
        Ok((fused.into_iter().skip(base.offset).take(limit).collect(), retrieval))
    }

    /// Which views have vectors for one model, that model loaded and the
    /// query embedded; or why the call is lexical.
    fn dense_setup<'s>(&self, sel: &'s Selection, text: &str, explicit: bool) -> anyhow::Result<Setup<'s>> {
        let mut warnings = Vec::new();
        let mut all: Vec<Option<&'s Vectors>> = Vec::with_capacity(sel.views.len());
        for v in &sel.views {
            match v.seg.vectors() {
                Ok(x) => all.push(x.filter(|x| !x.is_empty())),
                Err(e) if explicit => return Err(e.into()),
                Err(e) => {
                    warnings.push(format!("{}: vectors unreadable ({e})", v.repo()));
                    all.push(None);
                }
            }
        }
        let Some(meta) = active_model(&all) else {
            anyhow::ensure!(
                !explicit,
                "no vectors in the selected snapshots: index them with `rueca index --embed` (or use `mode: \"lexical\"`)"
            );
            return Ok(Setup::Lexical(Retrieval::lexical(warnings)));
        };
        let query = match self.query_embedder(&meta).and_then(|e| Ok(e.embed_query(text)?)) {
            Ok(q) => q,
            Err(e) if explicit => return Err(e.context("semantic search unavailable")),
            Err(e) => {
                warnings.push(format!("semantic search unavailable, ranked lexically: {e:#}"));
                return Ok(Setup::Lexical(Retrieval::lexical(warnings)));
            }
        };
        let vectors: Vec<Option<&Vectors>> =
            all.into_iter().map(|x| x.filter(|x| x.meta().embed_key == meta.embed_key)).collect();
        let queries = vectors.iter().map(|x| x.map(|x| truncate_normalize(&query, x.dims()))).collect();
        let lexical: Vec<String> =
            sel.views.iter().zip(&vectors).filter(|(_, x)| x.is_none()).map(|(v, _)| v.repo().to_string()).collect();
        let model = Some(format!("{}:{}", meta.preset, short(&meta.model_key)));
        let r = Retrieval { mode: "hybrid", model, alpha: None, lexical, warnings };
        Ok(Setup::Dense(Box::new(Dense { vectors, queries, meta }), r))
    }

    /// The process's embedder for `meta`'s embed key, loaded once (a
    /// failure is remembered too).
    fn query_embedder(&self, meta: &VectorsMeta) -> anyhow::Result<Arc<dyn Embedder>> {
        let mut map = self.embedders.lock().unwrap_or_else(|p| p.into_inner());
        let slot = map.entry(meta.embed_key.clone()).or_insert_with(|| {
            let t = std::time::Instant::now();
            let r = self.query_loader.load(meta).map_err(|e| format!("{e:#}"));
            tracing::debug!(preset = %meta.preset, ms = t.elapsed().as_millis() as u64, ok = r.is_ok(), "query model load");
            r
        });
        slot.clone().map_err(|e| anyhow::anyhow!(e))
    }

    /// BM25 and dense candidates with every score fusion needs.
    fn candidates(
        &self,
        sel: &Selection,
        base: &SymbolQuery,
        dense: &Dense<'_>,
        ranked: &crate::tools::Ranked,
        pool: usize,
        semantic: bool,
    ) -> anyhow::Result<Vec<Candidate>> {
        let views = &sel.views;
        let priors: Vec<Arc<[f32]>> = views.iter().map(|v| v.seg.priors()).collect();
        let mut by_key: BTreeMap<(usize, NodeId), Candidate> = BTreeMap::new();
        let blank = |view: usize, node: NodeId| Candidate {
            view,
            node,
            bm25: None,
            cos: None,
            in_dense: false,
            prior: priors[view].get(node as usize).copied().unwrap_or(1.0),
            route: ranked.boost[view],
            lexical_only: dense.vectors[view].is_none(),
        };
        if !semantic {
            for &(vi, n, score) in &ranked.hits {
                let mut c = blank(vi, n);
                let div = c.prior * c.route;
                c.bm25 = Some(if div > 0.0 { score as f32 / div } else { 0.0 });
                by_key.insert((vi, n), c);
            }
        }
        for (vi, v) in views.iter().enumerate() {
            let (Some(vecs), Some(q)) = (dense.vectors[vi], dense.queries[vi].as_deref()) else { continue };
            if !ranked.routed[vi] {
                continue;
            }
            let keep = v.seg.symbol_filter(base)?;
            for (n, cos) in vecs.top_n(q, pool, keep) {
                let c = by_key.entry((vi, n)).or_insert_with(|| blank(vi, n));
                c.cos = Some(cos);
                c.in_dense = true;
            }
        }
        self.exact_cosines(dense, &mut by_key);
        Ok(by_key.into_values().collect())
    }

    /// Cosines for BM25 candidates outside the dense list, then the best
    /// dense candidates rescored from the store's `f16` cache.
    fn exact_cosines(&self, dense: &Dense<'_>, cands: &mut BTreeMap<(usize, NodeId), Candidate>) {
        for c in cands.values_mut().filter(|c| c.cos.is_none()) {
            if let (Some(v), Some(q)) = (dense.vectors[c.view], dense.queries[c.view].as_deref()) {
                c.cos = v.row_of(c.node).map(|row| v.score(row, q));
            }
        }
        let Ok(Some(cache)) = VectorCache::open(&vector_root(&self.store_dir), &dense.meta.embed_key) else {
            return;
        };
        let mut order: Vec<(usize, NodeId, f32)> =
            cands.values().filter_map(|c| c.cos.map(|x| (c.view, c.node, x))).collect();
        order.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
        order.truncate(RESCORE_TOP.max(cands.values().filter(|c| c.bm25.is_some()).count()));
        let mut per_view: HashMap<usize, Vec<NodeId>> = HashMap::new();
        for (vi, n, _) in order {
            per_view.entry(vi).or_default().push(n);
        }
        for (vi, nodes) in per_view {
            let (Some(v), Some(q)) = (dense.vectors[vi], dense.queries[vi].as_deref()) else { continue };
            if let Ok(rescored) = rescore(&cache, v, q, &nodes) {
                for (n, cos) in rescored {
                    if let Some(c) = cands.get_mut(&(vi, n)) {
                        c.cos = Some(cos);
                    }
                }
            }
        }
    }
}

/// Full-precision cosines of `nodes` from the store cache (truncated to the
/// segment's width); nodes the cache lacks keep their int8 score.
fn rescore(cache: &VectorCache, v: &Vectors, q: &[f32], nodes: &[NodeId]) -> anyhow::Result<Vec<(NodeId, f32)>> {
    let rows: Vec<(NodeId, usize)> = nodes.iter().filter_map(|&n| v.row_of(n).map(|r| (n, r))).collect();
    let keys: Vec<[u8; 32]> = rows.iter().map(|&(_, r)| v.key(r)).collect();
    let full = cache.get(&keys)?;
    Ok(rows
        .iter()
        .zip(full)
        .filter_map(|(&(n, _), f)| {
            let f = truncate_normalize(&f?, v.dims());
            Some((n, f.iter().zip(q).map(|(a, b)| a * b).sum()))
        })
        .collect())
}

/// The vectors meta of the model most selected views share (ties: the
/// first view's), if any view has vectors.
fn active_model(all: &[Option<&Vectors>]) -> Option<VectorsMeta> {
    let mut counts: Vec<(&str, usize, &VectorsMeta)> = Vec::new();
    for v in all.iter().flatten() {
        let m = v.meta();
        match counts.iter_mut().find(|(k, _, _)| *k == m.embed_key) {
            Some(e) => e.1 += 1,
            None => counts.push((&m.embed_key, 1, m)),
        }
    }
    let best = counts.iter().map(|c| c.1).max()?;
    counts.into_iter().find(|c| c.1 == best).map(|c| c.2.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(view: usize, node: NodeId, bm25: Option<f32>, cos: Option<f32>) -> Candidate {
        Candidate { view, node, bm25, cos, in_dense: cos.is_some(), prior: 1.0, route: 1.0, lexical_only: false }
    }

    fn scores(v: &[(usize, NodeId, f64)]) -> Vec<(NodeId, f64)> {
        v.iter().map(|&(_, n, s)| (n, (s * 1000.0).round() / 1000.0)).collect()
    }

    #[test]
    fn convex_fusion_arithmetic() {
        // b̂ = bm25 / 10; d̂ = (cos + 1) / (0.8 + 1).
        let cands = [c(0, 1, Some(10.0), Some(0.8)), c(0, 2, Some(5.0), None), c(0, 3, None, Some(0.44))];
        let f = fuse(&cands, 0.5, Norm::Theoretical);
        // 1: 0.5·1 + 0.5·1 = 1; 2: 0.5·0.5 = 0.25; 3: 0.5·(1.44/1.8) = 0.4.
        assert_eq!(scores(&f), vec![(1, 1.0), (3, 0.4), (2, 0.25)]);
        // α = 1 is BM25 alone; α = 0 is dense alone.
        assert_eq!(scores(&fuse(&cands, 1.0, Norm::Theoretical)), vec![(1, 1.0), (2, 0.5), (3, 0.0)]);
        assert_eq!(scores(&fuse(&cands, 0.0, Norm::Theoretical))[..2], [(1, 1.0), (3, 0.8)]);
    }

    #[test]
    fn priors_and_routing_multiply() {
        let mut a = c(0, 1, Some(10.0), Some(0.8));
        a.prior = 0.5;
        let mut b = c(1, 1, Some(8.0), Some(0.8));
        b.route = 0.25;
        let f = fuse(&[a, b], 0.5, Norm::Theoretical);
        // a: 1 · 0.5; b: (0.5·0.8 + 0.5·1) · 0.25 = 0.225.
        assert_eq!(
            f.iter().map(|x| (x.0, (x.2 * 1000.0).round() / 1000.0)).collect::<Vec<_>>(),
            [(0, 0.5), (1, 0.225)]
        );
    }

    #[test]
    fn lexical_only_snapshots_use_bm25_alone() {
        let mut l = c(1, 7, Some(10.0), None);
        l.lexical_only = true;
        let d = c(0, 1, Some(5.0), Some(0.9));
        let f = fuse(&[d, l], 0.5, Norm::Theoretical);
        // l: b̂ = 1; d: 0.5·0.5 + 0.5·1 = 0.75.
        assert_eq!(scores(&f), vec![(7, 1.0), (1, 0.75)]);
    }

    #[test]
    fn normalization_edge_cases() {
        // One candidate: everything normalizes to 1, both ways.
        for norm in [Norm::Theoretical, Norm::MinMax] {
            let f = fuse(&[c(0, 4, Some(3.0), Some(0.2))], 0.5, norm);
            assert_eq!(scores(&f), vec![(4, 1.0)], "{norm:?}");
        }
        // Equal scores: equal fused scores, ties by node.
        for norm in [Norm::Theoretical, Norm::MinMax] {
            let f = fuse(&[c(0, 9, Some(2.0), Some(0.5)), c(0, 3, Some(2.0), Some(0.5))], 0.5, norm);
            assert_eq!(scores(&f), vec![(3, 1.0), (9, 1.0)], "{norm:?}");
        }
        // Min-max spreads each list over [0, 1]; missing is 0.
        let f = fuse(
            &[c(0, 1, Some(10.0), None), c(0, 2, Some(4.0), Some(0.3)), c(0, 3, None, Some(0.1))],
            0.5,
            Norm::MinMax,
        );
        assert_eq!(scores(&f), vec![(1, 0.5), (2, 0.5), (3, 0.0)]);
        // No BM25 at all, negative cosines: still ordered, finite.
        let f = fuse(&[c(0, 1, None, Some(-0.5)), c(0, 2, None, Some(-0.2))], 0.0, Norm::Theoretical);
        assert_eq!(f[0].1, 2);
        assert!(f.iter().all(|x| x.2.is_finite()));
        // Zero BM25 everywhere does not divide by zero.
        let f = fuse(&[c(0, 1, Some(0.0), None)], 1.0, Norm::Theoretical);
        assert_eq!(scores(&f), vec![(1, 0.0)]);
    }

    #[test]
    fn params_resolve_from_config() {
        let cfg = RetrievalConfig {
            alpha: Some(0.7),
            fusion_n: Some(50),
            normalization: Some("theoretical".into()),
            ..Default::default()
        };
        // The environment is not set for these keys in tests.
        if std::env::var_os("RUECA_FUSION_ALPHA").is_none()
            && std::env::var_os("RUECA_FUSION_N").is_none()
            && std::env::var_os("RUECA_FUSION_NORM").is_none()
        {
            assert_eq!(FusionParams::resolve(&cfg), FusionParams { alpha: 0.7, n: 50, norm: Norm::Theoretical });
            let bad = RetrievalConfig { alpha: Some(3.0), fusion_n: Some(0), ..Default::default() };
            assert_eq!(FusionParams::resolve(&bad), FusionParams::default());
        }
        let r = Retrieval { mode: "hybrid", model: Some("p:12345678".into()), alpha: Some(0.5), ..Default::default() };
        assert_eq!(r.to_json(), json!({"mode": "hybrid", "model": "p:12345678", "alpha": 0.5}));
        assert_eq!(Retrieval::lexical(vec![]).to_json(), json!({"mode": "lexical"}));
    }
}
