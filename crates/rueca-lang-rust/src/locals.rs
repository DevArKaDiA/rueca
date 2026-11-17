//! Local-scope table driven by the vendored `queries/locals.scm`.
//!
//! The query (adapted from nvim-treesitter; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`). This module runs it once per file with a
//! [`QueryCursor`] (match limit and deadline enforced) and applies the Rust
//! rules a query cannot express:
//!
//! - A binding belongs to the innermost scope containing it. A `let`
//!   binding is visible from the end of its statement (`let x = x + 1`
//!   reads the outer `x`), an `if let` / `while let` binding from the end of
//!   its condition, a `for` binding in the loop body, parameters and match
//!   arm bindings in their whole function, closure or arm, a generic
//!   parameter in its whole item.
//! - In patterns, an identifier is a binding unless it is the path of a
//!   tuple-struct pattern (`Some` in `Some(x)`) or starts with an upper-case
//!   letter (`None`, `MAX`, a unit struct or variant: a path).
//!
//! Types of bindings are inferred afterwards by the extractor (see
//! `receivers.rs`) and stored here.
//!
//! If the cursor hits its match limit or deadline the table is partial and
//! affected identifiers are treated as non-local (more references, never
//! fewer facts).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rueca_extract::ExtractError;
use rueca_extract::tree_sitter::{
    Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree,
};

use crate::types::Ty;

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    Parameter,
    Var,
    TypeParameter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scope,
    Def(DefKind),
    Ignore,
}

fn role_of(name: &str) -> Role {
    if name == "local.scope" {
        return Role::Scope;
    }
    match name.strip_prefix("local.definition.") {
        Some("parameter") => Role::Def(DefKind::Parameter),
        Some("var") => Role::Def(DefKind::Var),
        Some("type_parameter") => Role::Def(DefKind::TypeParameter),
        _ => Role::Ignore,
    }
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

/// Compiles `queries/locals.scm` against tree-sitter-rust (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_rust::LANGUAGE.into();
            let query = Query::new(&language, LOCALS_SCM).map_err(|e| format!("locals.scm: {e}"))?;
            let roles = query.capture_names().iter().map(|n| role_of(n)).collect();
            Ok(LocalsQuery { query, roles })
        })
        .as_ref()
        .map_err(|e| ExtractError::Query(e.clone()))
}

/// One local binding.
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    pub(crate) kind: DefKind,
    /// Byte range where the binding is visible (its scope).
    start: u32,
    end: u32,
    visible_from: u32,
    /// Start byte of the identifier.
    pub(crate) pos: u32,
    /// Declared or inferred type (for a generic parameter: its first trait
    /// bound).
    pub(crate) ty: Option<Ty>,
    /// For an untyped `let` whose value is a call: the `$ret:` receiver of
    /// that call's result (see `returns.rs`), typed by the resolver.
    pub(crate) ret: Option<String>,
}

/// Per-file local-scope table.
#[derive(Default)]
pub(crate) struct Locals {
    pub(crate) bindings: Vec<Binding>,
    by_name: HashMap<String, Vec<usize>>,
}

pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

impl Locals {
    /// Index of the local binding of `name` visible at byte `pos`,
    /// innermost first.
    pub(crate) fn lookup_index(&self, name: &str, pos: u32) -> Option<usize> {
        let ids = self.by_name.get(name)?;
        let mut best: Option<usize> = None;
        for &i in ids {
            let b = &self.bindings[i];
            if b.start > pos || pos >= b.end || b.visible_from > pos {
                continue;
            }
            let better = best.is_none_or(|c| {
                let c = &self.bindings[c];
                (b.start, b.visible_from) > (c.start, c.visible_from)
            });
            if better {
                best = Some(i);
            }
        }
        best
    }

    /// The local binding of `name` visible at byte `pos`.
    pub(crate) fn lookup(&self, name: &str, pos: u32) -> Option<&Binding> {
        self.lookup_index(name, pos).map(|i| &self.bindings[i])
    }

    /// Bindings in visibility order (for type inference).
    pub(crate) fn inference_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.bindings.len()).collect();
        order.sort_by_key(|&i| (self.bindings[i].visible_from, self.bindings[i].pos));
        order
    }
}

/// Nearest ancestor of one of `kinds`, below `stop` (exclusive).
fn ancestor_until<'t>(node: Node<'t>, kinds: &[&str], stop: (u32, u32)) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if (n.start_byte() as u32, n.end_byte() as u32) == stop {
            return None;
        }
        if kinds.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Item kinds a generic parameter is visible in.
const GENERIC_OWNERS: &[&str] = &[
    "function_item",
    "function_signature_item",
    "impl_item",
    "trait_item",
    "struct_item",
    "enum_item",
    "union_item",
    "type_item",
    "associated_type",
];

/// Byte offset from which a pattern binding is visible within `scope`.
fn pattern_visible_from(node: Node<'_>, scope: (u32, u32)) -> u32 {
    const SITES: &[&str] = &["let_declaration", "let_condition", "for_expression", "match_arm", "closure_parameters"];
    let Some(site) = ancestor_until(node, SITES, scope).or_else(|| {
        // The scope node itself (`for`, a match arm) is the site.
        let mut cur = node.parent();
        while let Some(n) = cur {
            if (n.start_byte() as u32, n.end_byte() as u32) == scope {
                return Some(n);
            }
            cur = n.parent();
        }
        None
    }) else {
        return scope.0;
    };
    match site.kind() {
        "let_declaration" | "let_condition" => site.end_byte() as u32,
        "for_expression" => site.child_by_field_name("body").map_or(scope.0, |b| b.start_byte() as u32),
        _ => scope.0,
    }
}

/// Whether a pattern identifier is a path rather than a binding.
fn is_path_in_pattern(node: Node<'_>, name: &str) -> bool {
    if name.chars().next().is_some_and(char::is_uppercase) {
        return true;
    }
    let Some(parent) = node.parent() else { return false };
    parent.kind() == "tuple_struct_pattern" && parent.child_by_field_name("type").is_some_and(|t| t.id() == node.id())
}

/// The scopes and the definitions captured by the locals query.
type Captures<'t> = (Vec<(u32, u32)>, Vec<(Node<'t>, DefKind)>);

/// Runs the locals query over `tree`: the scopes, the definitions, and
/// whether the run was cut (deadline or match limit).
fn run_query<'t>(lq: &LocalsQuery, tree: &'t Tree, source: &[u8], time_budget: Duration) -> (Captures<'t>, bool) {
    let mut scopes: Vec<(u32, u32)> = Vec::new();
    let mut defs: Vec<(Node<'_>, DefKind)> = Vec::new();
    let deadline = Instant::now() + time_budget;
    let mut timed_out = false;
    let mut progress = |_: &QueryCursorState| {
        if Instant::now() > deadline {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    {
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches = cursor.matches_with_options(&lq.query, tree.root_node(), source, options);
        let mut seen = std::collections::HashSet::new();
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                match lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore) {
                    Role::Scope if seen.insert(cap.node.id()) => {
                        scopes.push((cap.node.start_byte() as u32, cap.node.end_byte() as u32));
                    }
                    Role::Def(kind) => defs.push((cap.node, kind)),
                    Role::Scope | Role::Ignore => {}
                }
            }
        }
    }
    let incomplete = timed_out || cursor.did_exceed_match_limit();
    ((scopes, defs), incomplete)
}

/// Scope ranges sorted by start (outer first on ties); scopes nest properly.
struct Scopes(Vec<(u32, u32)>);

impl Scopes {
    fn new(mut scopes: Vec<(u32, u32)>) -> Self {
        scopes.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        Scopes(scopes)
    }

    /// Innermost scope of a position: the containing scope with the latest
    /// start.
    fn innermost(&self, pos: u32) -> Option<(u32, u32)> {
        let upto = self.0.partition_point(|s| s.0 <= pos);
        self.0[..upto].iter().rev().find(|s| pos < s.1).copied()
    }
}

/// The scope range and visibility start of a captured definition; `None`
/// when it binds nothing.
fn binding_range(node: Node<'_>, kind: DefKind, name: &str, scopes: &Scopes) -> Option<(u32, u32, u32)> {
    if kind == DefKind::TypeParameter {
        let mut cur = node.parent();
        while let Some(n) = cur {
            if GENERIC_OWNERS.contains(&n.kind()) {
                return Some((n.start_byte() as u32, n.end_byte() as u32, n.start_byte() as u32));
            }
            cur = n.parent();
        }
        return None;
    }
    if kind == DefKind::Var && is_path_in_pattern(node, name) {
        return None;
    }
    let scope = scopes.innermost(node.start_byte() as u32)?;
    let from = match kind {
        DefKind::Var => pattern_visible_from(node, scope),
        _ => scope.0,
    };
    Some((scope.0, scope.1, from))
}

/// Runs the locals query over `tree` and builds the table. `time_budget`
/// bounds the query run.
pub(crate) fn build(tree: &Tree, source: &[u8], time_budget: Duration) -> Result<Locals, ExtractError> {
    let lq = locals_query()?;
    // A cut run (deadline, match limit) leaves the table partial: more
    // identifiers are references, never fewer facts.
    let ((scopes, defs), _cut) = run_query(lq, tree, source, time_budget);
    let scopes = Scopes::new(scopes);
    let mut locals = Locals::default();
    let mut seen_defs = std::collections::HashSet::new();
    for (node, kind) in defs {
        if !seen_defs.insert(node.id()) {
            continue;
        }
        let name = text(node, source);
        if name.is_empty() || name == "_" {
            continue;
        }
        let Some((start, end, visible_from)) = binding_range(node, kind, name, &scopes) else { continue };
        let id = locals.bindings.len();
        locals.bindings.push(Binding {
            kind,
            start,
            end,
            visible_from,
            pos: node.start_byte() as u32,
            ty: None,
            ret: None,
        });
        locals.by_name.entry(name.to_string()).or_default().push(id);
    }
    Ok(locals)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture names the Rust side relies on.
    const REQUIRED_CAPTURES: &[&str] =
        &["local.scope", "local.definition.parameter", "local.definition.var", "local.definition.type_parameter"];

    /// Exercises every pattern of `locals.scm` at least once.
    pub(crate) const GOLDEN: &str = r#"
fn f<T: Clone, const N: usize>(p: T, q: &[u8]) -> u32 {
    let a = 1;
    let (b, c) = (2, 3);
    if let Some(d) = opt {}
    if let e = 1 {}
    while let Some(Point { x, y: yy }) = it.next() {}
    for i in 0..10 { let _ = i; }
    match v {
        k => {}
        Wrap(m) => {}
        ref r => {}
        mut w => {}
        z @ 1..=3 => {}
        [s, ..] => {}
        o | B(o) => {}
        &h => {}
    }
    let cl = |u, t: u8| u + t;
    loop { break; }
    a
}
"#;

    fn parse(src: &str) -> Tree {
        let language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = rueca_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query().expect("locals.scm must compile against tree-sitter-rust");
        let names = lq.query.capture_names();
        for required in REQUIRED_CAPTURES {
            assert!(names.contains(required), "capture @{required} missing from locals.scm");
        }
        for (name, role) in names.iter().zip(&lq.roles) {
            assert_ne!(*role, Role::Ignore, "capture @{name} has no role");
        }
    }

    #[test]
    fn every_locals_pattern_matches_the_golden_corpus() {
        let lq = locals_query().unwrap();
        let tree = parse(GOLDEN);
        let mut cursor = QueryCursor::new();
        let mut hit = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN.as_bytes());
        while let Some(m) = matches.next() {
            hit[m.pattern_index] = true;
        }
        let missing: Vec<usize> = hit.iter().enumerate().filter(|(_, h)| !**h).map(|(i, _)| i).collect();
        assert!(missing.is_empty(), "patterns never matched: {missing:?}");
    }

    #[test]
    fn query_header_records_provenance() {
        assert!(LOCALS_SCM.starts_with("; SPDX-License-Identifier: Apache-2.0"));
        assert!(LOCALS_SCM.contains("29212d826054bb2948fa9d3a99eca9bf6a050eb2"));
    }

    fn names_at(src: &str, needle: &str) -> Vec<String> {
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        let pos = src.find(needle).unwrap() as u32;
        let mut out: Vec<String> = locals.by_name.keys().filter(|n| locals.lookup(n, pos).is_some()).cloned().collect();
        out.sort();
        out
    }

    #[test]
    fn scopes_and_visibility() {
        let src = "fn f(p: u8) { let x = x0; let y = x; g(y); }\nfn h() { q(); }";
        assert_eq!(names_at(src, "x0"), ["p"]);
        assert_eq!(names_at(src, "g(y)"), ["p", "x", "y"]);
        assert_eq!(names_at(src, "q()"), Vec::<String>::new());
        let src = "fn f() { for it in items(it0) { use_it(it); } if let Some(v) = o(v0) { w(v); } }";
        assert_eq!(names_at(src, "it0"), Vec::<String>::new());
        assert_eq!(names_at(src, "use_it"), ["it"]);
        assert_eq!(names_at(src, "v0"), Vec::<String>::new());
        assert_eq!(names_at(src, "w(v)"), ["v"]);
        // Paths in patterns are not bindings.
        let src = "fn f() { match x { None => a(), Some(v) => b(v), MAX => c() } }";
        assert_eq!(names_at(src, "b(v)"), ["v"]);
        assert_eq!(names_at(src, "c()"), Vec::<String>::new());
        let src = "fn g<T: Tr>(t: T) { t.m(); }";
        assert_eq!(names_at(src, "t.m"), ["T", "t"]);
    }
}
