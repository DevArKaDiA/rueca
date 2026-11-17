//! The reranker stage of `search_symbols` (RFC 0007 §7.5): behind `rerank:
//! true` (default from `[retrieval] rerank`, off), a cross-encoder scores
//! the top `rerank_k` candidates of the first stage (after the graph
//! signals) against the query. Each candidate's text is its canonical text
//! (the one its vector embeds, [`rueca_segment::canonical`]) cut to 256
//! tokens (`× 4` characters), rebuilt from the content packs at query time.
//!
//! **Final order**: the reranker's order for the top K (ties keep the
//! first-stage order), the first-stage order below. **Scores** stay
//! monotone with the order: a reranked candidate's score is the reranker's
//! relevance `p ∈ [0, 1]`; a candidate below K keeps its first-stage score
//! scaled under half the lowest reranked `p`.
//!
//! **Fallbacks** mirror the embedding side: an explicit `rerank: true` that
//! cannot load its model is an error (naming `rueca models import` /
//! `rueca models pull` and `RUECA_MODEL_DIR`); a reranker turned on by
//! configuration that cannot load leaves the first-stage order with a
//! warning in `retrieval.warnings`. A failed load is remembered for the
//! process.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use rueca_embed::{RerankOptions, Reranker, RetrievalConfig};
use serde_json::{Value, json};

use crate::graph::{Hit, env};
use crate::hybrid::Retrieval;
use crate::{Engine, Selection};

/// Tokens of a candidate's text (RFC 0007 §7.5).
pub const RERANK_TEXT_TOKENS: usize = 256;

/// The reranker stage's configuration: `RUECA_RERANK` (`1` / `0`),
/// `RUECA_RERANK_K`, `RUECA_RERANK_PRESET`, else `[retrieval]` `rerank`,
/// `rerank_k`, `rerank_preset`, else off, K = 50,
/// [`rueca_embed::DEFAULT_RERANK_PRESET`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerankParams {
    /// Whether a call that does not say reranks.
    pub default_on: bool,
    /// Candidates scored.
    pub k: usize,
    /// Reranker preset.
    pub preset: String,
}

impl Default for RerankParams {
    fn default() -> Self {
        RerankParams { default_on: false, k: 50, preset: rueca_embed::DEFAULT_RERANK_PRESET.to_string() }
    }
}

impl RerankParams {
    /// The parameters in effect: environment over `config` over defaults.
    /// Invalid values are ignored.
    #[must_use]
    pub fn resolve(config: &RetrievalConfig) -> RerankParams {
        let d = RerankParams::default();
        let on = env("RUECA_RERANK").and_then(|v| match v.as_str() {
            "1" | "true" | "on" => Some(true),
            "0" | "false" | "off" => Some(false),
            _ => None,
        });
        let k = env("RUECA_RERANK_K").and_then(|v| v.parse().ok()).or(config.rerank_k);
        RerankParams {
            default_on: on.or(config.rerank).unwrap_or(d.default_on),
            k: k.filter(|k: &usize| *k > 0).unwrap_or(d.k),
            preset: env("RUECA_RERANK_PRESET").or_else(|| config.rerank_preset.clone()).unwrap_or(d.preset),
        }
    }
}

/// Loads a reranker by preset.
pub trait RerankerLoader: Send + Sync {
    /// The reranker of `preset`.
    ///
    /// # Errors
    ///
    /// Missing weights, an unknown or non-reranker preset, runtime failures.
    fn load(&self, preset: &str) -> anyhow::Result<Arc<dyn Reranker>>;
}

/// The presets of `rueca-embed`, weights from the store's model cache.
#[derive(Debug, Clone)]
pub struct PresetRerankerLoader {
    /// The store (its `models/`, or `$RUECA_MODEL_DIR`).
    pub store: PathBuf,
}

impl RerankerLoader for PresetRerankerLoader {
    fn load(&self, preset: &str) -> anyhow::Result<Arc<dyn Reranker>> {
        let p = rueca_embed::preset(preset)?;
        let cache = rueca_embed::ModelCache::for_store(&self.store);
        Ok(Arc::from(rueca_embed::backend::load_reranker(&cache, p, &RerankOptions::default())?))
    }
}

/// A reranker loaded once per process, or why it could not be.
pub(crate) type RerankerSlot = Result<Arc<dyn Reranker>, String>;

/// A call's reranking: how many candidates, and whether the call asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RerankRequest {
    pub k: usize,
    pub explicit: bool,
}

/// `retrieval.reranker` of the envelope.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RerankInfo {
    pub preset: String,
    pub model_key: String,
    pub k: usize,
    pub ms: f64,
}

impl RerankInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "preset": self.preset,
            "model_key": self.model_key.get(..8).unwrap_or(&self.model_key),
            "k": self.k,
            "ms": (self.ms * 10.0).round() / 10.0,
        })
    }
}

/// The final order and scores of a reranked pool (see the module docs):
/// `hits` sorted by first-stage score, `scores` the reranker's for its
/// first `scores.len()` entries.
pub(crate) fn apply_rerank(hits: &mut [Hit], scores: &[f32]) {
    let k = scores.len().min(hits.len());
    let mut head: Vec<(Hit, f32)> = hits[..k].iter().copied().zip(scores.iter().copied()).collect();
    head.sort_by(|a, b| b.1.total_cmp(&a.1));
    let floor = head.last().map_or(0.0, |h| f64::from(h.1).max(0.0)) * 0.5;
    let top_rest = hits[k..].iter().map(|h| h.2).fold(0.0_f64, f64::max);
    for (slot, ((vi, n, _), p)) in hits.iter_mut().zip(head) {
        *slot = (vi, n, f64::from(p));
    }
    for h in &mut hits[k..] {
        h.2 = if top_rest > 0.0 { (h.2 / top_rest).max(0.0) * floor } else { 0.0 };
    }
}

impl Engine {
    /// Whether this call reranks, and how many candidates.
    pub(crate) fn rerank_request(&self, rerank: Option<bool>, k: Option<i64>) -> Option<RerankRequest> {
        let on = rerank.unwrap_or(self.rerank.default_on);
        let k = k.map_or(self.rerank.k, |k| usize::try_from(k.clamp(1, 200)).unwrap_or(self.rerank.k));
        on.then_some(RerankRequest { k, explicit: rerank == Some(true) })
    }

    /// Reranks the top of `hits` (sorted by first-stage score) in place.
    pub(crate) fn rerank_stage(
        &self,
        sel: &Selection,
        query: &str,
        hits: &mut [Hit],
        req: RerankRequest,
        retrieval: &mut Retrieval,
    ) -> anyhow::Result<()> {
        let reranker = match self.reranker() {
            Ok(r) => r,
            Err(e) if req.explicit => return Err(e.context("reranking unavailable")),
            Err(e) => {
                retrieval.warnings.push(format!("reranking unavailable, first-stage order kept: {e:#}"));
                return Ok(());
            }
        };
        let k = req.k.min(hits.len());
        let texts = candidate_texts(sel, &hits[..k])?;
        let docs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let started = Instant::now();
        let scores = reranker.rerank(query, &docs)?;
        anyhow::ensure!(scores.len() == k, "the reranker returned {} scores for {k} documents", scores.len());
        let ms = started.elapsed().as_secs_f64() * 1e3;
        apply_rerank(hits, &scores);
        let (preset, model_key) = (reranker.preset().to_string(), reranker.model_key().to_string());
        retrieval.reranker = Some(RerankInfo { preset, model_key, k, ms });
        Ok(())
    }

    /// The process's reranker for the configured preset, loaded once (a
    /// failure is remembered too).
    fn reranker(&self) -> anyhow::Result<Arc<dyn Reranker>> {
        let preset = &self.rerank.preset;
        let mut map = self.rerankers.lock().unwrap_or_else(|p| p.into_inner());
        let slot = map.entry(preset.clone()).or_insert_with(|| {
            let t = Instant::now();
            let r = self.rerank_loader.load(preset).map_err(|e| format!("{e:#}"));
            tracing::debug!(%preset, ms = t.elapsed().as_millis() as u64, ok = r.is_ok(), "reranker load");
            r
        });
        slot.clone().map_err(|e| anyhow::anyhow!(e))
    }
}

/// The canonical text of each candidate, read per snapshot.
fn candidate_texts(sel: &Selection, hits: &[Hit]) -> anyhow::Result<Vec<String>> {
    let mut by_view: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, h) in hits.iter().enumerate() {
        by_view.entry(h.0).or_default().push(i);
    }
    let mut out = vec![String::new(); hits.len()];
    for (vi, idx) in by_view {
        let nodes: Vec<_> = idx.iter().map(|&i| hits[i].1).collect();
        let texts = sel.views[vi].seg.canonical_texts(&nodes, RERANK_TEXT_TOKENS)?;
        for (i, t) in idx.into_iter().zip(texts) {
            out[i] = t;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reranked_head_then_scaled_tail() {
        let mut hits = vec![(0, 1, 9.0), (0, 2, 8.0), (0, 3, 7.0), (0, 4, 4.0), (0, 5, 2.0)];
        apply_rerank(&mut hits, &[0.1, 0.9, 0.1]);
        let order: Vec<u32> = hits.iter().map(|h| h.1).collect();
        // Reranker order on top (the tie keeps the first-stage order), then
        // the first stage.
        assert_eq!(order, vec![2, 1, 3, 4, 5]);
        let s: Vec<f64> = hits.iter().map(|h| h.2).collect();
        assert!(s.windows(2).all(|w| w[0] >= w[1]), "{s:?}");
        assert!((s[0] - 0.9).abs() < 1e-6 && (s[3] - 0.05).abs() < 1e-9 && (s[4] - 0.025).abs() < 1e-9);
        // K beyond the pool, and an empty pool.
        let mut one = vec![(1, 7, 3.0)];
        apply_rerank(&mut one, &[0.4]);
        assert_eq!(one, vec![(1, 7, f64::from(0.4_f32))]);
        apply_rerank(&mut [], &[]);
    }

    #[test]
    fn params_resolve_from_config() {
        if ["RUECA_RERANK", "RUECA_RERANK_K", "RUECA_RERANK_PRESET"].iter().any(|k| std::env::var_os(k).is_some()) {
            return;
        }
        assert_eq!(RerankParams::resolve(&RetrievalConfig::default()), RerankParams::default());
        let cfg = RetrievalConfig { rerank: Some(true), rerank_k: Some(20), ..Default::default() };
        let p = RerankParams::resolve(&cfg);
        assert_eq!((p.default_on, p.k), (true, 20));
        let r = RerankInfo { preset: "p".into(), model_key: "0123456789abcdef".into(), k: 20, ms: 12.34 };
        assert_eq!(r.to_json(), json!({"preset": "p", "model_key": "01234567", "k": 20, "ms": 12.3}));
    }
}
