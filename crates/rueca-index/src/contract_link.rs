//! The organization link layer, contracts (RFC 0006 §3.3.2, phase 4): the
//! contract edges of one consumer entry into its dependencies, from the
//! segments' contract tables alone (no packs, no resolver).
//!
//! - **Rows.** Every row of the consumer's contract table is matched
//!   against the `Provides` rows of its dependencies' default entries
//!   **and of its own segment**, with the matcher of the in-snapshot
//!   resolver (`rueca_model::contract::best_matches`): exact > template >
//!   method wildcard, same bucket.
//! - **Edges** (`meta.strategy = Contract`, `reversed = false`):
//!   a `Consumes` row → a code provider: `CALLS`; → a spec provider (an
//!   OpenAPI operation, a `.proto` RPC method): `CALLS`; a code `Provides`
//!   row → a spec provider: `IMPLEMENTS`. Code and spec providers are
//!   matched separately (reaching a handler and the operation it
//!   implements is not ambiguous); spec rows never link out.
//! - **Ambiguity across repositories.** The best quality and the number of
//!   providers sharing it are computed over the union of the consumer's
//!   own providers and every dependency's: a consumer with a matching
//!   handler in its own repository and another in a dependency is
//!   ambiguous, so the link edge gets `AMBIGUOUS_CONFIDENCE` (0.5); a
//!   better match in its own repository (an exact route against a
//!   dependency's template) leaves no link edge at all. The in-snapshot
//!   edge of the consumer's own segment keeps the confidence its
//!   resolver gave it (it cannot see other repositories).
//! - **Targets.** Providers that are tests or in test files are not
//!   candidates (as for package edges); a row never matches its own node.
//!   Nodes are the segments' own (contract rows are stored per segment),
//!   so nothing is mapped. Evidence: the consumer row's `(file, span)`.

use std::collections::{BTreeMap, HashMap};

use rueca_model::contract::{best_matches, match_bucket, match_confidence};
use rueca_model::{ContractKind, ContractRow, ContractSide, EdgeKind, EdgeMeta, NodeId, Strategy};
use rueca_segment::Segment;
use rueca_segment::org::{LinkEdge, LinkInput};

/// Where a provider row is: `None` = the consumer's own segment, `Some(k)`
/// = dependency `k`.
type Side = Option<usize>;

/// Provider rows by [`match_bucket`]: (side, node, key).
type Group<'a> = HashMap<(ContractKind, String), Vec<(Side, NodeId, &'a str)>>;

/// Code and spec providers of the consumer and its dependencies.
#[derive(Default)]
struct Providers<'a> {
    code: Group<'a>,
    spec: Group<'a>,
}

impl<'a> Providers<'a> {
    fn collect(consumer: &'a Segment, deps: &'a [LinkInput<'a>]) -> Providers<'a> {
        let mut p = Providers::default();
        p.add(None, consumer);
        for (k, d) in deps.iter().enumerate() {
            p.add(Some(k), d.segment);
        }
        p
    }

    fn add(&mut self, side: Side, seg: &'a Segment) {
        for r in seg.contracts().iter().filter(|r| r.side == ContractSide::Provides && !is_test(seg, r.node)) {
            let group = if r.is_spec() { &mut self.spec } else { &mut self.code };
            group.entry(match_bucket(r.kind, &r.key)).or_default().push((side, r.node, r.key.as_str()));
        }
    }
}

/// Whether node `n` of `seg` is a test or in a test file.
fn is_test(seg: &Segment, n: NodeId) -> bool {
    let node = seg.node(n);
    node.is_test || node.file.is_some_and(|f| rueca_segment::is_test_path(&seg.file(f).path))
}

/// The contract link edges of `consumer` into `deps` (`LinkEdge::dep`
/// indexes `deps`), one per (src, dep, dst, kind), the most confident.
pub(crate) fn contract_edges(consumer: &Segment, deps: &[LinkInput<'_>]) -> Vec<LinkEdge> {
    let providers = Providers::collect(consumer, deps);
    let mut best: BTreeMap<(NodeId, u32, NodeId, EdgeKind), LinkEdge> = BTreeMap::new();
    for r in consumer.contracts() {
        let (groups, kind) = match r.side {
            ContractSide::Consumes => (vec![&providers.code, &providers.spec], EdgeKind::Calls),
            ContractSide::Provides if !r.is_spec() => (vec![&providers.spec], EdgeKind::Implements),
            ContractSide::Provides => continue,
        };
        for group in groups {
            for e in row_edges(r, group, kind) {
                let slot = best.entry((e.src, e.dep, e.dst, e.kind)).or_insert(e);
                if e.meta.confidence > slot.meta.confidence {
                    *slot = e;
                }
            }
        }
    }
    best.into_values().collect()
}

/// The edges of row `r` to the best providers of `group` that are in a
/// dependency (see the module docs for ambiguity).
fn row_edges(r: &ContractRow, group: &Group<'_>, kind: EdgeKind) -> Vec<LinkEdge> {
    let candidates = group
        .get(&match_bucket(r.kind, &r.key))
        .into_iter()
        .flatten()
        .filter(|(side, node, _)| side.is_some() || *node != r.node)
        .map(|&(side, node, key)| ((side, node), key));
    let (ids, best) = best_matches(r.kind, &r.key, candidates);
    let Some(quality) = best else { return Vec::new() };
    let meta = EdgeMeta {
        confidence: match_confidence(quality, ids.len()),
        strategy: Strategy::Contract,
        evidence: Some((r.file, r.span)),
    };
    ids.into_iter()
        .filter_map(|(side, dst)| side.map(|k| (k, dst)))
        .map(|(k, dst)| LinkEdge { src: r.node, dep: k as u32, dst, kind, meta, reversed: false })
        .collect()
}
