//! Contracts inside one snapshot (RFC 0006 phase 4; keys and matching in
//! `rueca_model::contract`).
//!
//! - Every `FileFacts::contracts` hint becomes a row of the contract table
//!   (`ResolvedGraph::contracts`), placed on its owner definition's node, or
//!   on the file node when it has no owner (or the owner is out of range).
//! - Edges, all `Strategy::Contract`:
//!   - `Consumes` → `Provides` from code (a route handler): `CALLS`;
//!   - `Consumes` → `Provides` from a spec (an OpenAPI `Route`, a `.proto`
//!     `RpcMethod`; framework in `SPEC_FRAMEWORKS`): `CALLS`;
//!   - `Provides` from code → `Provides` from a spec whose key matches (the
//!     spec as provider): `IMPLEMENTS`.
//!
//!   Spec-to-spec and code-to-code providers never link. Code and spec
//!   providers are matched separately: a consumer that reaches a handler and
//!   the spec operation it implements is not ambiguous.
//! - Confidence: the best `MatchQuality` among the providers of one group
//!   (exact 0.9, template 0.85, method wildcard 0.7); when several distinct
//!   provider nodes share that best quality, each edge gets
//!   `AMBIGUOUS_CONFIDENCE` (0.5). Evidence: the consumer's span; for
//!   `IMPLEMENTS`, the handler's hint span.
//! - A node never links to itself; edges are deduplicated by
//!   `(src, dst, kind)` like every resolver edge. Consumers that match
//!   nothing get no edge (they stay in the table, dangling).

use rueca_model::contract::{AMBIGUOUS_CONFIDENCE, match_bucket as bucket, match_confidence};
use rueca_model::{ContractKind, ContractRow, ContractSide};

use super::*;

/// One contract edge to add: `(src, dst, kind, meta)`.
pub(crate) type ContractEdge = (NodeId, NodeId, EdgeKind, EdgeMeta);

impl Resolver<'_> {
    /// The contract table: every hint of every file, in file then hint
    /// order, on its owner's node (else the file node).
    pub(crate) fn contract_rows(&self) -> Vec<ContractRow> {
        let mut rows = Vec::new();
        for (i, f) in self.files.iter().enumerate() {
            let Some(facts) = f.facts.as_ref() else { continue };
            for h in &facts.contracts {
                let owner = h.owner.and_then(|o| self.def_node[i].get(o as usize).copied());
                rows.push(ContractRow {
                    node: owner.unwrap_or(self.file_node[i]),
                    side: h.side,
                    kind: h.kind,
                    key: h.key.clone(),
                    raw: h.raw.clone(),
                    file: i as FileId,
                    span: h.span,
                    framework: h.framework.clone(),
                });
            }
        }
        rows
    }
}

/// Provider rows (indexes into the table) by [`bucket`].
type Providers = HashMap<(ContractKind, String), Vec<usize>>;

/// Code and spec providers of a table, bucketed.
fn providers(rows: &[ContractRow]) -> (Providers, Providers) {
    let (mut code, mut spec) = (Providers::new(), Providers::new());
    for (i, r) in rows.iter().enumerate().filter(|(_, r)| r.side == ContractSide::Provides) {
        let group = if r.is_spec() { &mut spec } else { &mut code };
        group.entry(bucket(r.kind, &r.key)).or_default().push(i);
    }
    (code, spec)
}

/// Distinct provider nodes matching `key` with the best quality, and that
/// quality's edge confidence (ambiguity applied). Excludes `exclude` (the
/// row's own node).
fn best_matches(
    rows: &[ContractRow],
    group: &Providers,
    kind: ContractKind,
    key: &str,
    exclude: NodeId,
) -> (Vec<NodeId>, f32) {
    let candidates = group
        .get(&bucket(kind, key))
        .into_iter()
        .flatten()
        .map(|&p| &rows[p])
        .filter(|p| p.node != exclude)
        .map(|p| (p.node, p.key.as_str()));
    let (nodes, best) = rueca_model::contract::best_matches(kind, key, candidates);
    let confidence = best.map_or(AMBIGUOUS_CONFIDENCE, |q| match_confidence(q, nodes.len()));
    (nodes, confidence)
}

/// The in-snapshot contract edges of a contract table (see the module docs).
pub(crate) fn contract_edges(rows: &[ContractRow]) -> Vec<ContractEdge> {
    let (code, spec) = providers(rows);
    let mut out = Vec::new();
    let both = [&code, &spec];
    for r in rows {
        let (groups, kind) = match r.side {
            ContractSide::Consumes => (&both[..], EdgeKind::Calls),
            ContractSide::Provides if !r.is_spec() => (&both[1..], EdgeKind::Implements),
            ContractSide::Provides => continue,
        };
        for group in groups {
            let (nodes, confidence) = best_matches(rows, group, r.kind, &r.key, r.node);
            let meta = EdgeMeta { confidence, strategy: Strategy::Contract, evidence: Some((r.file, r.span)) };
            out.extend(nodes.into_iter().map(|dst| (r.node, dst, kind, meta)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(node: NodeId, side: ContractSide, key: &str, framework: &str) -> ContractRow {
        ContractRow {
            node,
            side,
            kind: ContractKind::Http,
            key: key.into(),
            raw: key.into(),
            file: 0,
            span: Span { start_byte: node, end_byte: node + 1, ..Span::default() },
            framework: framework.into(),
        }
    }

    fn edges(rows: &[ContractRow]) -> Vec<(NodeId, NodeId, EdgeKind, f32)> {
        let mut v: Vec<_> = contract_edges(rows).into_iter().map(|(s, d, k, m)| (s, d, k, m.confidence)).collect();
        v.sort_by_key(|a| (a.0, a.1, a.2));
        v
    }

    #[test]
    fn exact_template_and_wildcard() {
        use ContractSide::*;
        let rows = [
            row(1, Provides, "GET /users/{}", "flask"),
            row(2, Consumes, "GET /users/{}", "requests"),
            row(3, Consumes, "GET /users/42", "requests"),
            row(4, Consumes, "* /users/7", "requests"),
        ];
        assert_eq!(
            edges(&rows),
            vec![(2, 1, EdgeKind::Calls, 0.9), (3, 1, EdgeKind::Calls, 0.85), (4, 1, EdgeKind::Calls, 0.7)]
        );
    }

    #[test]
    fn best_quality_wins_then_ambiguity() {
        use ContractSide::*;
        let rows = [
            row(1, Provides, "GET /users/{}", "flask"),
            row(2, Provides, "GET /users/me", "flask"),
            row(3, Consumes, "GET /users/me", "fetch"),
            row(4, Provides, "GET /items/{}", "express"),
            row(5, Provides, "GET /items/{}", "express"),
            row(6, Consumes, "GET /items/9", "fetch"),
            // Two hints of one handler are one provider node.
            row(7, Provides, "GET /a", "flask"),
            row(7, Provides, "* /a", "flask"),
            row(8, Consumes, "GET /a", "fetch"),
        ];
        assert_eq!(
            edges(&rows),
            vec![
                (3, 2, EdgeKind::Calls, 0.9),
                (6, 4, EdgeKind::Calls, 0.5),
                (6, 5, EdgeKind::Calls, 0.5),
                (8, 7, EdgeKind::Calls, 0.9),
            ]
        );
    }

    #[test]
    fn implements_and_spec_calls() {
        use ContractSide::*;
        let rows = [
            row(1, Provides, "GET /users/{}", "openapi"),
            row(2, Provides, "GET /users/{}", "flask"),
            row(3, Consumes, "GET /users/5", "requests"),
            row(4, Provides, "GET /users/{}", "proto"),
        ];
        // The consumer reaches the handler and both spec operations (two
        // spec nodes: ambiguous among specs, not against the handler).
        assert_eq!(
            edges(&rows),
            vec![
                (2, 1, EdgeKind::Implements, 0.5),
                (2, 4, EdgeKind::Implements, 0.5),
                (3, 1, EdgeKind::Calls, 0.5),
                (3, 2, EdgeKind::Calls, 0.85),
                (3, 4, EdgeKind::Calls, 0.5),
            ]
        );
        let rows = [row(1, Provides, "GET /users/{}", "openapi"), row(2, Provides, "GET /users/{}", "flask")];
        let e = contract_edges(&rows);
        assert_eq!(e.len(), 1);
        let (src, dst, kind, meta) = e[0];
        assert_eq!(
            (src, dst, kind, meta.confidence, meta.strategy),
            (2, 1, EdgeKind::Implements, 0.9, Strategy::Contract)
        );
        assert_eq!(meta.evidence, Some((0, rows[1].span)));
    }

    #[test]
    fn dangling_self_and_same_side() {
        use ContractSide::*;
        let rows = [
            row(1, Consumes, "GET /nowhere", "fetch"),
            row(2, Provides, "GET /a", "flask"),
            row(2, Consumes, "GET /a", "fetch"),
            row(3, Provides, "GET /b", "flask"),
            row(4, Provides, "GET /b", "flask"),
            row(5, Provides, "GET /c", "openapi"),
            row(6, Provides, "GET /c", "proto"),
            row(7, Consumes, "POST /b", "fetch"),
        ];
        assert_eq!(edges(&rows), vec![]);
    }

    #[test]
    fn rpc_package_wildcard() {
        let mut p = row(1, ContractSide::Provides, "helloworld.Greeter/SayHello", "proto");
        p.kind = ContractKind::Rpc;
        let mut c = row(2, ContractSide::Consumes, "Greeter/SayHello", "grpc-python");
        c.kind = ContractKind::Rpc;
        let mut h = row(3, ContractSide::Provides, "Greeter/SayHello", "grpc-python");
        h.kind = ContractKind::Rpc;
        assert_eq!(
            edges(&[p, c, h]),
            vec![(2, 1, EdgeKind::Calls, 0.85), (2, 3, EdgeKind::Calls, 0.9), (3, 1, EdgeKind::Implements, 0.85)]
        );
    }
}
