//! Go-only resolver rule: implicit interface satisfaction (RFC 0003 §10.2,
//! §10.5).
//!
//! Go types implement interfaces without declaring it. After bases are
//! resolved, every project type of the Go family (`Struct`, `TypeAlias`)
//! whose method set — its own methods (attached through package scope) plus
//! those promoted from embedded types — covers the method names of a project
//! interface (its own methods plus embedded interfaces; at least one) gets:
//!
//! - `IMPLEMENTS` type → interface, `heuristic` 0.5;
//! - `OVERRIDES` from each covering method to the interface method with the
//!   same name, `heuristic` 0.5, so `trace` callers of an interface method
//!   with `edge_types: ["CALLS", "OVERRIDES"]` reaches the implementations.
//!
//! Names only: signatures are not compared, pointer and value receivers are
//! not distinguished. An interface that embeds one outside the project
//! (`fmt.Stringer`, `io.Reader`) is matched on the methods the project
//! declares, and only when there are at least two of them. Each interface
//! keeps at most [`MAX_IMPLEMENTERS`] implementers, the first by qualified
//! name, so a one-method interface with a common name (`String()`) cannot
//! flood the graph.

use super::*;

/// Implementers kept per interface.
pub(crate) const MAX_IMPLEMENTERS: usize = 64;

/// Confidence of implicit `IMPLEMENTS` / `OVERRIDES` edges.
const IMPLICIT_CONFIDENCE: f32 = 0.5;

impl Resolver<'_> {
    /// Method names of a type → method node, own first, then promoted
    /// through resolved bases (depth-first, depth ≤ 10).
    fn go_method_set(&self, t: NodeId) -> BTreeMap<String, NodeId> {
        let mut out = BTreeMap::new();
        let mut stack = vec![(t, 0usize)];
        let mut seen = Vec::new();
        while let Some((c, depth)) = stack.pop() {
            if depth > MAX_DEPTH || seen.contains(&c) {
                continue;
            }
            seen.push(c);
            if let Some(m) = self.members.get(&c) {
                for (name, slots) in m {
                    if let Some(n) = slots.callable
                        && self.nodes[n as usize].label == Label::Method
                    {
                        out.entry(name.clone()).or_insert(n);
                    }
                }
            }
            for &b in self.bases.get(&c).map_or(&[][..], |v| v.as_slice()).iter().rev() {
                stack.push((b, depth + 1));
            }
        }
        out
    }

    /// Adds the implicit `IMPLEMENTS` / `OVERRIDES` edges of Go types.
    pub(crate) fn go_implicit_interfaces(&mut self) {
        let (mut interfaces, mut types) = self.go_interfaces_and_types();
        if interfaces.is_empty() || types.is_empty() {
            return;
        }
        types.sort_unstable();
        types.dedup();
        // Method name → types that have it.
        let mut by_method: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
        let mut sets: HashMap<NodeId, BTreeMap<String, NodeId>> = HashMap::new();
        for &t in &types {
            let set = self.go_method_set(t);
            for name in set.keys() {
                by_method.entry(name.clone()).or_default().push(t);
            }
            sets.insert(t, set);
        }
        interfaces.sort_unstable();
        interfaces.dedup();
        let mut out: Vec<(NodeId, NodeId, EdgeKind, FileId, Span)> = Vec::new();
        for (iface, _) in interfaces {
            self.go_implementers(iface, &by_method, &sets, &mut out);
        }
        for (src, dst, kind, f, span) in out {
            let meta =
                EdgeMeta { confidence: IMPLICIT_CONFIDENCE, strategy: Strategy::Heuristic, evidence: Some((f, span)) };
            self.add_edge(src, dst, kind, meta);
        }
    }

    /// The Go interfaces (with their file) and the struct and alias types.
    fn go_interfaces_and_types(&self) -> (Vec<(NodeId, FileId)>, Vec<NodeId>) {
        let mut interfaces = Vec::new();
        let mut types = Vec::new();
        for i in 0..self.files.len() {
            if self.families[i] != Some(LanguageFamily::Go) {
                continue;
            }
            for (j, d) in self.defs(i as FileId).iter().enumerate() {
                let node = self.def_node[i][j];
                match d.label {
                    Label::Interface => interfaces.push((node, i as FileId)),
                    Label::Struct | Label::TypeAlias => types.push(node),
                    _ => {}
                }
            }
        }
        (interfaces, types)
    }

    /// The `IMPLEMENTS` and `OVERRIDES` edges of interface `iface`: the
    /// types having every method name of it (at most
    /// [`MAX_IMPLEMENTERS`], by qualified name).
    fn go_implementers(
        &self,
        iface: NodeId,
        by_method: &BTreeMap<String, Vec<NodeId>>,
        sets: &HashMap<NodeId, BTreeMap<String, NodeId>>,
        out: &mut Vec<(NodeId, NodeId, EdgeKind, FileId, Span)>,
    ) {
        // An interface embedding one outside the project (`fmt.Stringer`)
        // is matched on its known methods, of which it needs two.
        let wanted = self.go_method_set(iface);
        let needed = if self.type_is_open(iface) { 2 } else { 1 };
        if wanted.len() < needed {
            return;
        }
        // Types having every method name, starting from the rarest name.
        let mut names: Vec<&String> = wanted.keys().collect();
        names.sort_by_key(|n| by_method.get(*n).map_or(0, Vec::len));
        let Some(first) = by_method.get(names[0]) else { return };
        let mut cands: Vec<NodeId> =
            first.iter().copied().filter(|t| names.iter().all(|n| sets[t].contains_key(*n))).collect();
        cands.sort_by(|a, b| {
            self.nodes[*a as usize].qualified_name.cmp(&self.nodes[*b as usize].qualified_name).then(a.cmp(b))
        });
        cands.truncate(MAX_IMPLEMENTERS);
        for t in cands {
            let node = &self.nodes[t as usize];
            let (Some(f), Some(span)) = (node.file, node.span) else { continue };
            out.push((t, iface, EdgeKind::Implements, f, span));
            self.go_method_overrides(&wanted, &sets[&t], out);
        }
    }

    /// `OVERRIDES` from each method of an implementing type (`have`) to the
    /// interface method of the same name (`wanted`).
    fn go_method_overrides(
        &self,
        wanted: &BTreeMap<String, NodeId>,
        have: &BTreeMap<String, NodeId>,
        out: &mut Vec<(NodeId, NodeId, EdgeKind, FileId, Span)>,
    ) {
        for (name, &im) in wanted {
            let m = have[name];
            let mn = &self.nodes[m as usize];
            if m != im
                && let (Some(mf), Some(ms)) = (mn.file, mn.span)
            {
                out.push((m, im, EdgeKind::Overrides, mf, ms));
            }
        }
    }
}
