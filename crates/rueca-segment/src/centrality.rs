//! Query-independent centrality (RFC 0007 §7.4): the percentile of a node's
//! distinct in-degree over `CALLS` and `IMPORTS` within its snapshot.
//!
//! Computed from the incoming CSR adjacency the first time a query asks for
//! it ([`crate::Segment::centrality`]), not at open and not at write time:
//! one pass over the incoming edges plus a sort of the degrees, a few
//! milliseconds on a 100k-node segment (`docs/how-it-works.md` §10), and
//! nothing at all while the prior is off (β = 0, the default).
//!
//! The population is every node a symbol search returns by default: files,
//! folders, modules and repositories (whose in-degree counts `IMPORTS`
//! between files) are left out and get percentile 1, so the prior never
//! moves them.

use rueca_model::{EdgeKind, Label, Node};

/// Labels outside the percentile population.
const CONTAINERS: [Label; 4] = [Label::File, Label::Folder, Label::Module, Label::Repository];

/// The incoming adjacency of a segment, borrowed from its mapped bundle.
pub(crate) struct InAdjacency<'a> {
    /// Row offsets, one per node plus one.
    pub offsets: &'a [u32],
    /// Source node of each incoming edge.
    pub sources: &'a [u32],
    /// Edge kind index of each incoming edge (rows sorted by kind, then source).
    pub kinds: &'a [u8],
}

impl InAdjacency<'_> {
    /// The sources of `node`'s incoming edges of `kind`, ascending.
    fn sources_of(&self, node: usize, kind: EdgeKind) -> &[u32] {
        let (Some(&a), Some(&b)) = (self.offsets.get(node), self.offsets.get(node + 1)) else { return &[] };
        let (a, b) = (a as usize, b as usize);
        let (Some(row), Some(src)) = (self.kinds.get(a..b), self.sources.get(a..b)) else { return &[] };
        let k = kind.index() as u8;
        let lo = row.partition_point(|&x| x < k);
        let hi = row.partition_point(|&x| x <= k);
        &src[lo..hi]
    }

    /// Distinct other nodes with a `CALLS` or `IMPORTS` edge into `node`.
    pub(crate) fn distinct_in_degree(&self, node: usize) -> u32 {
        merged_distinct(self.sources_of(node, EdgeKind::Calls), self.sources_of(node, EdgeKind::Imports), node)
    }
}

/// Distinct values of two ascending lists, `skip` left out.
fn merged_distinct(a: &[u32], b: &[u32], skip: usize) -> u32 {
    let (mut i, mut j, mut last, mut n) = (0, 0, None, 0_u32);
    while i < a.len() || j < b.len() {
        let x = match (a.get(i), b.get(j)) {
            (Some(&x), Some(&y)) if x <= y => {
                i += 1;
                x
            }
            (_, Some(&y)) => {
                j += 1;
                y
            }
            (Some(&x), None) => {
                i += 1;
                x
            }
            (None, None) => break,
        };
        if last != Some(x) && x as usize != skip {
            n += 1;
        }
        last = Some(x);
    }
    n
}

/// The centrality percentile `p ∈ [0, 1]` of every node: the share of the
/// population with a strictly smaller distinct in-degree, over the
/// population size minus one (the most referenced node gets 1, every node
/// without referrers 0). Containers get 1.
pub(crate) fn percentiles(nodes: &[Node], adj: &InAdjacency<'_>) -> Vec<f32> {
    let degree: Vec<Option<u32>> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (!CONTAINERS.contains(&n.label)).then(|| adj.distinct_in_degree(i)))
        .collect();
    let mut sorted: Vec<u32> = degree.iter().flatten().copied().collect();
    sorted.sort_unstable();
    #[allow(clippy::cast_precision_loss)]
    let denom = sorted.len().saturating_sub(1).max(1) as f32;
    degree
        .iter()
        .map(|d| match d {
            #[allow(clippy::cast_precision_loss)]
            Some(d) => sorted.partition_point(|&x| x < *d) as f32 / denom,
            None => 1.0,
        })
        .collect()
}

/// The prior of percentile `p` with exponent `beta`, RFC 0006's
/// routing-prior family: `((0.25 + p) / 1.25)^β`, 1 when β is 0.
#[must_use]
pub fn centrality_prior(p: f32, beta: f32) -> f32 {
    if beta == 0.0 {
        return 1.0;
    }
    ((0.25 + p.clamp(0.0, 1.0)) / 1.25).powf(beta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: Label) -> Node {
        Node {
            label,
            name: "x".into(),
            qualified_name: "x".into(),
            file: None,
            span: None,
            language: None,
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        }
    }

    #[test]
    fn distinct_sources_over_calls_and_imports() {
        let (imports, calls) = (EdgeKind::Imports.index() as u8, EdgeKind::Calls.index() as u8);
        let refs = EdgeKind::References.index() as u8;
        // Rows sorted by kind, then source. Node 0: imports from itself and
        // 2, calls from 1 and 2 (two call sites); node 1: a reference from
        // 0, which does not count.
        let adj = InAdjacency {
            offsets: &[0, 5, 6, 6],
            sources: &[0, 2, 1, 2, 2, 0],
            kinds: &[imports, imports, calls, calls, calls, refs],
        };
        assert_eq!(adj.distinct_in_degree(0), 2);
        assert_eq!(adj.distinct_in_degree(1), 0);
        assert_eq!(adj.distinct_in_degree(7), 0);
        assert_eq!(merged_distinct(&[1, 3, 5], &[2, 3, 6], 6), 4);
    }

    #[test]
    fn percentiles_rank_the_population_and_skip_containers() {
        let nodes = vec![node(Label::Function), node(Label::Function), node(Label::File), node(Label::Method)];
        let calls = EdgeKind::Calls.index() as u8;
        // Node 0 ← 1, 3; node 1 ← 3; node 2 (a file) ← 0; node 3 ← nothing.
        let adj = InAdjacency {
            offsets: &[0, 2, 3, 4, 4],
            sources: &[1, 3, 3, 0],
            kinds: &[calls, calls, calls, EdgeKind::Imports.index() as u8],
        };
        let p = percentiles(&nodes, &adj);
        assert_eq!(p, vec![1.0, 0.5, 1.0, 0.0]);
        assert!((centrality_prior(0.0, 1.0) - 0.2).abs() < 1e-6);
        assert!((centrality_prior(1.0, 2.0) - 1.0).abs() < 1e-6);
        assert!((centrality_prior(0.3, 0.0) - 1.0).abs() < f32::EPSILON);
    }
}
