//! Graph signals of `search_symbols` (RFC 0007 §7.3, §7.4), applied to the
//! first-stage pool (BM25 or fused candidates, priors and routing applied)
//! in every mode, before the reranker:
//!
//! - **Centrality prior**: each candidate's score is multiplied by
//!   `((0.25 + p) / 1.25)^β`, `p` the percentile of its distinct in-degree
//!   over `CALLS` and `IMPORTS` in its snapshot
//!   ([`rueca_segment::Segment::centrality`]). β = 0 (off) by default. A
//!   multiplication commutes with fusion, so this is the prior of §7.4
//!   applied where the other priors are; in lexical mode it re-scores the
//!   BM25 top N (the in-index priors are applied before top-k).
//! - **Graph expansion**: the 1-hop neighbours of the top `seeds`
//!   candidates join the pool. A neighbour inherits its seed's score
//!   without the seed's own multipliers, times `decay`, times its own
//!   prior, routing boost and centrality prior; a neighbour already in the
//!   pool keeps the higher of its score and the inherited one. Edge sets
//!   ([`ExpandEdges`]): `CALLS` both ways (callers, callees), the RFC's
//!   "contains" as Rueca's `DEFINES` both ways (members and container;
//!   Rueca's `CONTAINS` links folders and files, which search does not
//!   return), `IMPORTS` both ways. In organization mode `CALLS` and
//!   `IMPORTS` also follow link edges across repositories, seen through
//!   the call's view and visibility, when the other end's repository is
//!   selected. Neighbours pass the call's filters (labels, language,
//!   `path_glob`); at most `per_seed` per seed and `total` per query are
//!   added, in seed order, then callers, callees, members, container,
//!   importers, imported, each by node id, link edges after a kind's local
//!   edges.

use std::collections::HashMap;
use std::sync::Arc;

use rueca_embed::RetrievalConfig;
use rueca_model::{EdgeKind, NodeId};
use rueca_segment::org::LinkDir;
use rueca_segment::{SymbolQuery, centrality_prior};

use crate::refset::Key;
use crate::{Engine, Selection};

/// One ranked candidate: (view, node, score).
pub(crate) type Hit = (usize, NodeId, f64);

/// The edges graph expansion follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExpandEdges {
    /// No expansion (the default until the phase 2 sweep decides).
    #[default]
    Off,
    /// Callers and callees.
    Calls,
    /// Calls, members and the container.
    CallsContains,
    /// Calls, containment and imports.
    CallsContainsImports,
}

impl ExpandEdges {
    /// Parses `off`, `calls`, `calls+contains`, `calls+contains+imports`.
    #[must_use]
    pub fn parse(s: &str) -> Option<ExpandEdges> {
        match s.trim().to_ascii_lowercase().replace(' ', "").as_str() {
            "" | "off" | "none" => Some(ExpandEdges::Off),
            "calls" => Some(ExpandEdges::Calls),
            "calls+contains" | "contains+calls" => Some(ExpandEdges::CallsContains),
            "calls+contains+imports" => Some(ExpandEdges::CallsContainsImports),
            _ => None,
        }
    }

    /// The configuration spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ExpandEdges::Off => "off",
            ExpandEdges::Calls => "calls",
            ExpandEdges::CallsContains => "calls+contains",
            ExpandEdges::CallsContainsImports => "calls+contains+imports",
        }
    }

    /// Edge kinds followed, in neighbour order.
    fn kinds(self) -> &'static [EdgeKind] {
        match self {
            ExpandEdges::Off => &[],
            ExpandEdges::Calls => &[EdgeKind::Calls],
            ExpandEdges::CallsContains => &[EdgeKind::Calls, EdgeKind::Defines],
            ExpandEdges::CallsContainsImports => &[EdgeKind::Calls, EdgeKind::Defines, EdgeKind::Imports],
        }
    }
}

/// Graph expansion and the centrality prior: `RUECA_EXPAND`,
/// `RUECA_EXPAND_DECAY`, `RUECA_EXPAND_SEEDS`, `RUECA_EXPAND_PER_SEED`,
/// `RUECA_EXPAND_TOTAL`, `RUECA_CENTRALITY_BETA`, else `[retrieval]` of
/// `<store>/rueca.toml`, else off, decay 0.5, 10 seeds, 20 per seed, 100
/// in all, β 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphParams {
    /// Edge set.
    pub expand: ExpandEdges,
    /// Top candidates whose neighbours join.
    pub seeds: usize,
    /// A neighbour inherits its seed's score times this, in `[0, 1]`.
    pub decay: f32,
    /// Neighbours added per seed, at most.
    pub per_seed: usize,
    /// Neighbours added per query, at most.
    pub total: usize,
    /// Exponent of the centrality prior (0: off).
    pub centrality_beta: f32,
}

impl Default for GraphParams {
    fn default() -> Self {
        GraphParams { expand: ExpandEdges::Off, seeds: 10, decay: 0.5, per_seed: 20, total: 100, centrality_beta: 0.0 }
    }
}

/// A non-empty environment variable.
pub(crate) fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

impl GraphParams {
    /// The parameters in effect: environment over `config` over defaults.
    /// Invalid values are ignored.
    #[must_use]
    pub fn resolve(config: &RetrievalConfig) -> GraphParams {
        let d = GraphParams::default();
        let num = |k: &str, c: Option<usize>| env(k).and_then(|v| v.parse().ok()).or(c);
        let real = |k: &str, c: Option<f32>| env(k).and_then(|v| v.parse().ok()).or(c);
        let expand = env("RUECA_EXPAND").or_else(|| config.expand.clone());
        GraphParams {
            expand: expand.as_deref().and_then(ExpandEdges::parse).unwrap_or(d.expand),
            seeds: num("RUECA_EXPAND_SEEDS", config.expand_seeds).unwrap_or(d.seeds),
            decay: real("RUECA_EXPAND_DECAY", config.expand_decay)
                .filter(|x| (0.0..=1.0).contains(x))
                .unwrap_or(d.decay),
            per_seed: num("RUECA_EXPAND_PER_SEED", config.expand_per_seed).unwrap_or(d.per_seed),
            total: num("RUECA_EXPAND_TOTAL", config.expand_total).unwrap_or(d.total),
            centrality_beta: real("RUECA_CENTRALITY_BETA", config.centrality_beta)
                .filter(|x| x.is_finite() && *x >= 0.0)
                .unwrap_or(d.centrality_beta),
        }
    }

    /// Whether either signal changes a ranking.
    #[must_use]
    pub fn active(&self) -> bool {
        self.expansion() || self.centrality_beta != 0.0
    }

    fn expansion(&self) -> bool {
        self.expand != ExpandEdges::Off && self.seeds > 0 && self.total > 0 && self.per_seed > 0
    }
}

/// What a candidate's raw score is multiplied by: prior, routing boost and
/// centrality prior, per view.
struct Weights {
    priors: Vec<Arc<[f32]>>,
    centrality: Vec<Option<Arc<[f32]>>>,
    boost: Vec<f32>,
    beta: f32,
}

impl Weights {
    fn new(sel: &Selection, boost: &[f32], beta: f32) -> Weights {
        Weights {
            priors: sel.views.iter().map(|v| v.seg.priors()).collect(),
            centrality: sel.views.iter().map(|v| (beta != 0.0).then(|| v.seg.centrality())).collect(),
            boost: (0..sel.views.len()).map(|i| boost.get(i).copied().unwrap_or(1.0)).collect(),
            beta,
        }
    }

    fn centrality(&self, (vi, n): Key) -> f32 {
        let p = self.centrality[vi].as_ref().and_then(|c| c.get(n as usize).copied());
        p.map_or(1.0, |p| centrality_prior(p, self.beta))
    }

    /// Prior × routing × centrality of a candidate.
    fn of(&self, key: Key) -> f64 {
        let prior = self.priors[key.0].get(key.1 as usize).copied().unwrap_or(1.0);
        f64::from(prior) * f64::from(self.boost[key.0]) * f64::from(self.centrality(key))
    }
}

/// Score descending, then view, then node.
pub(crate) fn sort_hits(hits: &mut [Hit]) {
    hits.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
}

/// The pool being expanded: candidates, where each sits, and counters.
struct Pool<'p> {
    hits: &'p mut Vec<Hit>,
    at: HashMap<Key, usize>,
    added: usize,
}

impl Pool<'_> {
    /// Offers neighbour `key` with score `s`: raises a pooled candidate,
    /// else adds it when the seed and the query have room. Returns whether
    /// it was added.
    fn offer(&mut self, key: Key, s: f64, room: bool) -> bool {
        if let Some(&i) = self.at.get(&key) {
            self.hits[i].2 = self.hits[i].2.max(s);
            return false;
        }
        if !room {
            return false;
        }
        self.at.insert(key, self.hits.len());
        self.hits.push((key.0, key.1, s));
        self.added += 1;
        true
    }
}

impl Engine {
    /// The centrality prior and graph expansion over a first-stage pool
    /// sorted by score (`boost`: the routing boost per view). Returns how
    /// many neighbours joined.
    pub(crate) fn graph_stage(
        &self,
        sel: &Selection,
        base: &SymbolQuery,
        hits: &mut Vec<Hit>,
        boost: &[f32],
    ) -> anyhow::Result<usize> {
        let g = &self.graph;
        if !g.active() {
            return Ok(0);
        }
        let w = Weights::new(sel, boost, g.centrality_beta);
        if g.centrality_beta != 0.0 {
            hits.iter_mut().for_each(|h| h.2 *= f64::from(w.centrality((h.0, h.1))));
            sort_hits(hits);
        }
        if !g.expansion() {
            return Ok(0);
        }
        let added = self.expand(sel, base, hits, &w)?;
        sort_hits(hits);
        Ok(added)
    }

    /// Adds the neighbours of the top seeds (see the module docs).
    fn expand(&self, sel: &Selection, base: &SymbolQuery, hits: &mut Vec<Hit>, w: &Weights) -> anyhow::Result<usize> {
        let g = &self.graph;
        let filters = sel.views.iter().map(|v| v.seg.symbol_filter(base)).collect::<Result<Vec<_>, _>>()?;
        let seeds: Vec<Hit> = hits.iter().take(g.seeds).copied().collect();
        let at = hits.iter().enumerate().map(|(i, h)| ((h.0, h.1), i)).collect();
        let mut pool = Pool { hits, at, added: 0 };
        for (vi, n, score) in seeds {
            let m = w.of((vi, n));
            if m <= 0.0 || score <= 0.0 {
                continue;
            }
            let inherited = score / m * f64::from(g.decay);
            let mut mine = 0;
            for key in self.neighbours_of(sel, (vi, n))? {
                if key == (vi, n) || !filters[key.0](key.1) {
                    continue;
                }
                let room = mine < g.per_seed && pool.added < g.total;
                mine += usize::from(pool.offer(key, inherited * w.of(key), room));
            }
        }
        Ok(pool.added)
    }

    /// The 1-hop neighbours of `key` over the configured edges, in the
    /// module docs' order, each once.
    fn neighbours_of(&self, sel: &Selection, (vi, n): Key) -> anyhow::Result<Vec<Key>> {
        let seg = &sel.views[vi].seg;
        let mut out: Vec<Key> = Vec::new();
        for &kind in self.graph.expand.kinds() {
            out.extend(seg.in_edges(n, Some(kind)).iter().map(|e| (vi, e.other)));
            out.extend(seg.out_edges(n, Some(kind)).iter().map(|e| (vi, e.other)));
            if kind != EdgeKind::Defines {
                out.extend(link_neighbours(sel, (vi, n), kind)?);
            }
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|k| seen.insert(*k));
        Ok(out)
    }
}

/// Organization mode: the other ends of `key`'s link edges of `kind`, both
/// ways, in the selected repositories.
fn link_neighbours(sel: &Selection, (vi, n): Key, kind: EdgeKind) -> anyhow::Result<Vec<Key>> {
    let (Some(org), Some(at)) = (sel.org.as_ref(), sel.views[vi].org_slot) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for dir in [LinkDir::In, LinkDir::Out] {
        for e in org.index.link_edges(at, n, dir, Some(kind), &org.visible, &org.view)? {
            if let Some(other) = sel.views.iter().position(|v| v.org_slot == Some(e.slot)) {
                out.push((other, e.node));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_sets_parse_and_print() {
        for e in [ExpandEdges::Off, ExpandEdges::Calls, ExpandEdges::CallsContains, ExpandEdges::CallsContainsImports] {
            assert_eq!(ExpandEdges::parse(e.as_str()), Some(e));
        }
        assert_eq!(ExpandEdges::parse("Calls + Contains"), Some(ExpandEdges::CallsContains));
        assert_eq!(ExpandEdges::parse("imports"), None);
        assert_eq!(ExpandEdges::CallsContainsImports.kinds().len(), 3);
    }

    #[test]
    fn params_resolve_from_config() {
        let keys = [
            "RUECA_EXPAND",
            "RUECA_EXPAND_DECAY",
            "RUECA_EXPAND_SEEDS",
            "RUECA_EXPAND_PER_SEED",
            "RUECA_EXPAND_TOTAL",
            "RUECA_CENTRALITY_BETA",
        ];
        if keys.iter().any(|k| std::env::var_os(k).is_some()) {
            return;
        }
        assert_eq!(GraphParams::resolve(&RetrievalConfig::default()), GraphParams::default());
        assert!(!GraphParams::default().active());
        let cfg = RetrievalConfig {
            expand: Some("calls".into()),
            expand_decay: Some(0.25),
            expand_seeds: Some(5),
            centrality_beta: Some(0.5),
            ..Default::default()
        };
        let p = GraphParams::resolve(&cfg);
        assert_eq!((p.expand, p.decay, p.seeds, p.centrality_beta), (ExpandEdges::Calls, 0.25, 5, 0.5));
        assert!(p.active());
        let bad = RetrievalConfig {
            expand: Some("everything".into()),
            expand_decay: Some(2.0),
            centrality_beta: Some(-1.0),
            ..Default::default()
        };
        assert_eq!(GraphParams::resolve(&bad), GraphParams::default());
    }

    #[test]
    fn offers_raise_or_add_within_room() {
        let mut hits = vec![(0, 1, 1.0), (0, 2, 0.4)];
        let at = hits.iter().enumerate().map(|(i, h)| ((h.0, h.1), i)).collect();
        let mut pool = Pool { hits: &mut hits, at, added: 0 };
        // A pooled candidate keeps the higher score, room or not.
        assert!(!pool.offer((0, 2), 0.5, false));
        assert!(!pool.offer((0, 1), 0.5, true));
        assert!(pool.offer((1, 7), 0.3, true));
        assert!(!pool.offer((1, 8), 0.3, false));
        assert!(!pool.offer((1, 7), 0.2, true));
        assert_eq!(pool.added, 1);
        sort_hits(&mut hits);
        assert_eq!(hits, vec![(0, 1, 1.0), (0, 2, 0.5), (1, 7, 0.3)]);
    }
}
