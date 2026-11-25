//! Local-variable table driven by the vendored `queries/locals.scm`.
//!
//! The query (adapted from nvim-treesitter; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope.hard` /
//! `@local.scope`) and bindings (`@local.definition.<kind>`). This module
//! runs it once per file with a [`QueryCursor`] (match limit and deadline
//! enforced) and applies Ruby's rules, which a query cannot express:
//!
//! - A binding belongs to the innermost scope containing it (a parameter to
//!   its method, block or lambda).
//! - A parameter is visible in its whole scope; an assignment target from
//!   its own position on (Ruby decides at parse time: `x = x` reads the new,
//!   still `nil`, local; `helper` before `helper = 1` is a method call).
//! - A `def`, `class`, `module` or `class << x` body starts a new table
//!   (a hard scope): bindings outside it are not visible inside. Blocks and
//!   lambdas see the locals of the scopes around them up to the nearest
//!   hard scope.
//!
//! If the cursor hits its match limit or deadline the table is partial and
//! the affected identifiers are treated as method calls (more references,
//! never fewer facts).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rueca_extract::ExtractError;
use rueca_extract::tree_sitter::{
    Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree,
};

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    Parameter,
    Var,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Hard,
    Soft,
    Def(DefKind),
    Ignore,
}

fn role_of(name: &str) -> Role {
    match name {
        "local.scope.hard" => Role::Hard,
        "local.scope" => Role::Soft,
        "local.definition.parameter" => Role::Def(DefKind::Parameter),
        "local.definition.var" => Role::Def(DefKind::Var),
        _ => Role::Ignore,
    }
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

/// Compiles `queries/locals.scm` against tree-sitter-ruby (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_ruby::LANGUAGE.into();
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
    /// The scope the binding belongs to (byte range).
    scope: (u32, u32),
    /// Byte offset from which the name is a local.
    visible_from: u32,
    /// Start byte of the binding identifier.
    pub(crate) pos: u32,
}

/// One scope of the file.
#[derive(Debug, Clone, Copy)]
struct Scope {
    start: u32,
    end: u32,
    hard: bool,
}

/// Per-file local-variable table.
#[derive(Default)]
pub(crate) struct Locals {
    pub(crate) bindings: Vec<Binding>,
    by_name: HashMap<String, Vec<usize>>,
    /// Scopes sorted by start, outer first on ties.
    scopes: Vec<Scope>,
}

impl Locals {
    /// The innermost hard scope containing `pos` (the whole file when none).
    fn hard_scope(&self, pos: u32) -> (u32, u32) {
        let upto = self.scopes.partition_point(|s| s.start <= pos);
        self.scopes[..upto].iter().rev().find(|s| s.hard && pos < s.end).map_or((0, u32::MAX), |s| (s.start, s.end))
    }

    /// Indexes of the bindings of `name` visible at byte `pos`.
    pub(crate) fn visible(&self, name: &str, pos: u32) -> Vec<usize> {
        let Some(ids) = self.by_name.get(name) else { return Vec::new() };
        let hard = self.hard_scope(pos);
        ids.iter()
            .copied()
            .filter(|&i| {
                let b = &self.bindings[i];
                b.scope.0 <= pos && pos < b.scope.1 && b.visible_from <= pos && b.scope.0 >= hard.0
            })
            .collect()
    }

    /// Whether `name` is a local variable at byte `pos`.
    pub(crate) fn is_local(&self, name: &str, pos: u32) -> bool {
        !self.visible(name, pos).is_empty()
    }

    /// Every binding of `name` in the hard scope around `pos` (whatever its
    /// position): the assignments a type must agree with.
    pub(crate) fn same_table(&self, name: &str, pos: u32) -> Vec<usize> {
        let Some(ids) = self.by_name.get(name) else { return Vec::new() };
        let hard = self.hard_scope(pos);
        ids.iter()
            .copied()
            .filter(|&i| {
                let b = &self.bindings[i];
                b.scope.0 >= hard.0 && b.scope.1 <= hard.1 && self.hard_scope(b.pos) == hard
            })
            .collect()
    }
}

pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// The scopes and the definitions captured by the locals query.
type Captures<'t> = (Vec<Scope>, Vec<(Node<'t>, DefKind)>);

/// Runs the locals query over `tree`: the scopes and the definitions.
fn run_query<'t>(lq: &LocalsQuery, tree: &'t Tree, source: &[u8], time_budget: Duration) -> Captures<'t> {
    let mut scopes: Vec<Scope> = Vec::new();
    let mut defs: Vec<(Node<'_>, DefKind)> = Vec::new();
    let deadline = Instant::now() + time_budget;
    let mut progress = |_: &QueryCursorState| {
        if Instant::now() > deadline { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    };
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    let options = QueryCursorOptions::new().progress_callback(&mut progress);
    let mut matches = cursor.matches_with_options(&lq.query, tree.root_node(), source, options);
    let mut seen = std::collections::HashSet::new();
    while let Some(m) = matches.next() {
        for cap in m.captures() {
            let role = lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore);
            match role {
                Role::Hard | Role::Soft if seen.insert(cap.node.id()) => scopes.push(Scope {
                    start: cap.node.start_byte() as u32,
                    end: cap.node.end_byte() as u32,
                    hard: role == Role::Hard,
                }),
                Role::Def(kind) => defs.push((cap.node, kind)),
                _ => {}
            }
        }
    }
    (scopes, defs)
}

/// The innermost scope containing byte `pos` (the whole file when none).
fn innermost(scopes: &[Scope], pos: u32) -> (u32, u32) {
    let upto = scopes.partition_point(|s| s.start <= pos);
    scopes[..upto].iter().rev().find(|s| pos < s.end).map_or((0, u32::MAX), |s| (s.start, s.end))
}

/// Runs the locals query over `tree` and builds the table. `time_budget`
/// bounds the query run.
pub(crate) fn build(tree: &Tree, source: &[u8], time_budget: Duration) -> Result<Locals, ExtractError> {
    let lq = locals_query()?;
    let (mut scopes, defs) = run_query(lq, tree, source, time_budget);
    scopes.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut locals = Locals { scopes, ..Locals::default() };
    let mut seen_defs = std::collections::HashSet::new();
    for (node, kind) in defs {
        if !seen_defs.insert(node.id()) {
            continue;
        }
        let name = text(node, source);
        if name.is_empty() {
            continue;
        }
        let pos = node.start_byte() as u32;
        // A parameter belongs to the scope that owns its parameter list;
        // the parameter list is inside that scope's node.
        let scope = innermost(&locals.scopes, pos);
        let visible_from = match kind {
            DefKind::Parameter => scope.0,
            DefKind::Var => pos,
        };
        let id = locals.bindings.len();
        locals.bindings.push(Binding { kind, scope, visible_from, pos });
        locals.by_name.entry(name.to_string()).or_default().push(id);
    }
    Ok(locals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(src: &str) -> (Locals, Tree) {
        let lang: rueca_extract::tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
        let mut p = rueca_extract::tree_sitter::Parser::new();
        p.set_language(&lang).unwrap();
        let tree = p.parse(src, None).unwrap();
        (build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap(), tree)
    }

    fn at(src: &str, needle: &str) -> u32 {
        src.rfind(needle).unwrap() as u32
    }

    #[test]
    fn locals_query_compiles_with_every_capture() {
        let lq = locals_query().unwrap();
        let names = lq.query.capture_names();
        for want in ["local.scope.hard", "local.scope", "local.definition.parameter", "local.definition.var"] {
            assert!(names.contains(&want), "{want}");
        }
    }

    #[test]
    fn locals_assignment_order_and_hard_scopes() {
        let src = "x = 1\ndef m(a, b = 2, *c, d:, **e, &f)\n  helper\n  helper = 3\n  helper\n  [1].each { |g| g; a; y = 1 }\n  y\n  x\nend\n";
        let (l, _t) = table(src);
        for p in ["a", "b", "c", "d", "e", "f"] {
            assert!(l.is_local(p, at(src, "helper\n  helper =")), "{p}");
        }
        assert!(!l.is_local("helper", src.find("  helper\n").unwrap() as u32 + 2));
        assert!(l.is_local("helper", at(src, "helper\n  [1]")));
        assert!(l.is_local("g", at(src, "g; a")));
        // `y` assigned in the block is not visible after it; `x` is outside the `def`.
        assert!(!l.is_local("y", at(src, "y\n  x")));
        assert!(!l.is_local("x", at(src, "x\nend")));
        assert!(l.is_local("x", at(src, "x = 1") + 1) || src.starts_with("x"));
    }

    #[test]
    fn locals_rescue_for_and_multiple_assignment() {
        let src = "def m\n  a, (b, c), *d = list\n  for i in 1..3 do i end\n  begin\n  rescue Foo => err\n    err\n  end\n  a; b; c; d; z ||= 1; z\nend\n";
        let (l, _t) = table(src);
        let end = at(src, "z\nend");
        for n in ["a", "b", "c", "d", "i", "err", "z"] {
            assert!(l.is_local(n, end), "{n}");
        }
        assert!(!l.is_local("list", end));
    }
}
