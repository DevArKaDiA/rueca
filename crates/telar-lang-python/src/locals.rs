//! Local-binding table driven by the vendored `queries/locals.scm`.
//!
//! The query (adapted from nvim-treesitter and bloop, see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`), bindings
//! (`@local.definition.<kind>`), `global` / `nonlocal` declarations
//! (`@local.global`, `@local.nonlocal`) and every identifier use
//! (`@local.reference`). This module runs it once per file with a
//! [`QueryCursor`] (match limit and deadline enforced) and applies Python's
//! scope rules — the part a query cannot express — to decide, for every
//! reference identifier, whether it names a local binding:
//!
//! - A name bound anywhere in a function / lambda / comprehension body is
//!   local to that whole scope (no positional shadowing; Python hoists).
//! - Definition captures may be whole pattern nodes (`a, (b, *c)`); every
//!   identifier inside them is bound, except inside attribute / subscript
//!   sub-expressions (`self.x = …`, `d[k] = …` bind nothing).
//! - `def` / `class` names bind in the scope *enclosing* the definition (the
//!   `#set! … "parent"` predicate stripped from the nvim query).
//! - `:=` binds in the nearest non-comprehension scope (PEP 572).
//! - Class-body bindings are visible only to code directly in the class body,
//!   except PEP 695 type parameters, which nested methods also see.
//! - Parameter defaults, annotations, return types, class bases and keyword
//!   arguments (the "header" before a scope's body) are evaluated in the
//!   enclosing scope, but see the definition's own type parameters.
//! - `global x` resolves `x` at module level; `nonlocal x` skips the
//!   declaring scope's own bindings.
//! - Imports are bindings but never "local": reads of imported names are
//!   reported so the resolver can follow the import.
//! - At module level only loop / comprehension / `with` / `except` / walrus /
//!   `match` targets count as local; module variables, functions, classes and
//!   imports are cross-definition references worth reporting.
//!
//! Known gaps (documented in the crate docs): the first iterable of a
//! comprehension is resolved in the comprehension scope rather than the
//! enclosing one, and `del x` does not unbind.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use telar_extract::ExtractError;
use telar_extract::tree_sitter::{Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree};

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep. The
/// locals patterns are shallow, so legitimate files stay far below it.
pub(crate) const MATCH_LIMIT: u32 = 1024;

/// Upper bound on identifiers collected from one definition pattern node.
const MAX_PATTERN_NODES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    Function,
    Type,
    TypeParameter,
    Parameter,
    Var,
    Iter,
    Alias,
    Walrus,
    Case,
    Import,
}

impl DefKind {
    fn from_suffix(s: &str) -> DefKind {
        match s {
            "function" | "method" => DefKind::Function,
            "type" | "class" => DefKind::Type,
            "type_parameter" => DefKind::TypeParameter,
            "parameter" => DefKind::Parameter,
            "iter" => DefKind::Iter,
            "alias" => DefKind::Alias,
            "walrus" => DefKind::Walrus,
            "case" => DefKind::Case,
            "import" => DefKind::Import,
            // `var`, `field` and any other nvim-style kind: a plain binding.
            _ => DefKind::Var,
        }
    }

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// Kinds that make a module-level name local (not worth a `Read`).
const MODULE_LOCAL: u16 = (1 << DefKind::Iter as u16)
    | (1 << DefKind::Alias as u16)
    | (1 << DefKind::Walrus as u16)
    | (1 << DefKind::Case as u16);
const IMPORT_BIT: u16 = 1 << DefKind::Import as u16;
const TYPE_PARAM_BIT: u16 = 1 << DefKind::TypeParameter as u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scope,
    Def(DefKind),
    Global,
    Nonlocal,
    Reference,
    Ignore,
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

fn role_of(name: &str) -> Role {
    match name {
        "local.scope" => Role::Scope,
        "local.reference" => Role::Reference,
        "local.global" => Role::Global,
        "local.nonlocal" => Role::Nonlocal,
        _ => match name.strip_prefix("local.definition") {
            Some("") => Role::Def(DefKind::Var),
            Some(rest) => match rest.strip_prefix('.') {
                Some(kind) => Role::Def(DefKind::from_suffix(kind)),
                None => Role::Ignore,
            },
            None => Role::Ignore,
        },
    }
}

/// Compiles `queries/locals.scm` against tree-sitter-python (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_python::LANGUAGE.into();
            let query = Query::new(&language, LOCALS_SCM).map_err(|e| format!("locals.scm: {e}"))?;
            let roles = query.capture_names().iter().map(|n| role_of(n)).collect();
            Ok(LocalsQuery { query, roles })
        })
        .as_ref()
        .map_err(|e| ExtractError::Query(e.clone()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ScopeKind {
    Module,
    Class,
    Function,
    Lambda,
    Comprehension,
}

struct ScopeInfo {
    start: u32,
    end: u32,
    /// Start of the body; references before it (the "header") are evaluated
    /// in the enclosing scope.
    body_start: u32,
    node_id: usize,
    kind: ScopeKind,
    parent: Option<usize>,
    /// Name → bit set of [`DefKind`]s bound in this scope.
    names: HashMap<String, u16>,
    globals: HashSet<String>,
    nonlocals: HashSet<String>,
}

enum PointKind {
    /// A binding; `owner` is the def/class node for names bound in the parent.
    Def { kind: DefKind, owner: Option<usize> },
    Global,
    Nonlocal,
    Reference,
}

struct Point {
    pos: u32,
    name: String,
    kind: PointKind,
}

/// Per-file scope table. Resolution is lazy: only identifiers the extractor
/// asks about are resolved, and each walk is capped at `max_hops` scopes
/// (`Budget::max_depth`), so adversarially nested input stays linear.
#[derive(Default)]
pub(crate) struct Locals {
    scopes: Vec<ScopeInfo>,
    module: Option<usize>,
    /// Reference identifier start byte → (innermost scope, name).
    refs: HashMap<u32, (usize, String)>,
    max_hops: usize,
    /// True if the cursor hit [`MATCH_LIMIT`] or the deadline; the table is
    /// then partial and unknown identifiers are treated as non-local.
    /// Not part of `FileFacts` (the contract has no field for it); kept for
    /// tests and diagnostics.
    #[allow(dead_code)]
    pub(crate) incomplete: bool,
}

impl Locals {
    fn resolution_at(&self, pos: u32) -> Resolution {
        match self.refs.get(&pos) {
            Some((scope, name)) => self.resolve(*scope, pos, name),
            None => Resolution::NotLocal,
        }
    }

    /// Whether `ident` (an identifier node) names a local binding of its
    /// scope or of an enclosing function scope.
    pub(crate) fn is_local(&self, ident: Node<'_>) -> bool {
        self.resolution_at(ident.start_byte() as u32) != Resolution::NotLocal
    }

    /// Whether `ident` names a PEP 695 type parameter.
    pub(crate) fn is_type_parameter(&self, ident: Node<'_>) -> bool {
        self.resolution_at(ident.start_byte() as u32) == Resolution::TypeParameter
    }

    fn resolve(&self, scope: usize, pos: u32, name: &str) -> Resolution {
        let scopes = &self.scopes;
        let mut s = scope;
        // Header references (defaults, annotations, bases) are evaluated in
        // the enclosing scope but see the definition's own type parameters.
        if pos < scopes[s].body_start && scopes[s].kind != ScopeKind::Module {
            if scopes[s].names.get(name).is_some_and(|b| b & TYPE_PARAM_BIT != 0) {
                return Resolution::TypeParameter;
            }
            match scopes[s].parent {
                Some(p) => s = p,
                None => return Resolution::NotLocal,
            }
        }
        let mut innermost = true;
        for _ in 0..=self.max_hops {
            let sc = &scopes[s];
            if sc.globals.contains(name) {
                return self.module_resolution(name);
            }
            if !sc.nonlocals.contains(name)
                && let Some(&bits) = sc.names.get(name)
            {
                match sc.kind {
                    ScopeKind::Module => {
                        return if bits & MODULE_LOCAL != 0 { Resolution::Local } else { Resolution::NotLocal };
                    }
                    ScopeKind::Class if !innermost => {
                        if bits & TYPE_PARAM_BIT != 0 {
                            return Resolution::TypeParameter;
                        }
                    }
                    _ => {
                        return if bits & TYPE_PARAM_BIT != 0 {
                            Resolution::TypeParameter
                        } else if bits & !IMPORT_BIT != 0 {
                            Resolution::Local
                        } else {
                            Resolution::NotLocal
                        };
                    }
                }
            }
            innermost = false;
            match sc.parent {
                Some(p) => s = p,
                None => return Resolution::NotLocal,
            }
        }
        Resolution::NotLocal
    }

    /// Resolution of a `global` name: the module scope's bindings only.
    fn module_resolution(&self, name: &str) -> Resolution {
        match self.module.and_then(|m| self.scopes[m].names.get(name)) {
            Some(bits) if bits & MODULE_LOCAL != 0 => Resolution::Local,
            _ => Resolution::NotLocal,
        }
    }
}

/// Every identifier bound by a target / pattern node.
fn bound_identifiers<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    let mut stack = vec![node];
    let mut visited = 0;
    while let Some(n) = stack.pop() {
        visited += 1;
        if visited > MAX_PATTERN_NODES {
            return;
        }
        match n.kind() {
            "identifier" => out.push(n),
            "attribute" | "subscript" | "type" | "keyword_identifier" => {}
            _ => {
                let mut cursor = n.walk();
                let children: Vec<Node<'t>> = n.named_children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
        }
    }
}

fn scope_kind(node: Node<'_>) -> ScopeKind {
    match node.kind() {
        "module" => ScopeKind::Module,
        "class_definition" => ScopeKind::Class,
        "function_definition" => ScopeKind::Function,
        "lambda" => ScopeKind::Lambda,
        _ => ScopeKind::Comprehension,
    }
}

/// Runs the locals query over `tree` and resolves every reference.
/// `time_budget` bounds the query run; `max_hops` bounds each resolution walk.
pub(crate) fn build(tree: &Tree, source: &[u8], time_budget: Duration, max_hops: usize) -> Result<Locals, ExtractError> {
    let lq = locals_query()?;
    let mut scopes: Vec<ScopeInfo> = Vec::new();
    let mut points: Vec<Point> = Vec::new();

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
    let text = |n: Node<'_>| String::from_utf8_lossy(&source[n.start_byte()..n.end_byte()]).into_owned();
    {
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches = cursor.matches_with_options(&lq.query, tree.root_node(), source, options);
        let mut seen_scopes = HashSet::new();
        let mut ids = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                let node = cap.node;
                match lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore) {
                    Role::Scope => {
                        if seen_scopes.insert(node.id()) {
                            let body_start = node
                                .child_by_field_name("body")
                                .map(|b| b.start_byte())
                                .unwrap_or(node.start_byte()) as u32;
                            scopes.push(ScopeInfo {
                                start: node.start_byte() as u32,
                                end: node.end_byte() as u32,
                                body_start,
                                node_id: node.id(),
                                kind: scope_kind(node),
                                parent: None,
                                names: HashMap::new(),
                                globals: HashSet::new(),
                                nonlocals: HashSet::new(),
                            });
                        }
                    }
                    Role::Def(kind) => {
                        ids.clear();
                        bound_identifiers(node, &mut ids);
                        let owner = match kind {
                            DefKind::Function | DefKind::Type => node.parent().map(|p| p.id()),
                            _ => None,
                        };
                        for id in &ids {
                            points.push(Point {
                                pos: id.start_byte() as u32,
                                name: text(*id),
                                kind: PointKind::Def { kind, owner },
                            });
                        }
                    }
                    Role::Global | Role::Nonlocal | Role::Reference if node.kind() == "identifier" => {
                        let kind = match lq.roles[cap.index as usize] {
                            Role::Global => PointKind::Global,
                            Role::Nonlocal => PointKind::Nonlocal,
                            _ => PointKind::Reference,
                        };
                        points.push(Point { pos: node.start_byte() as u32, name: text(node), kind });
                    }
                    _ => {}
                }
            }
        }
    }
    let incomplete = timed_out || cursor.did_exceed_match_limit();

    // Scopes nest properly; order outer-first for the sweep.
    scopes.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)).then(a.kind.cmp(&b.kind)));
    // Stable sort keeps capture order among points at the same position.
    points.sort_by_key(|p| p.pos);

    // Sweep: attach every point to its innermost scope, linking scope parents.
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0;
    let mut refs: HashMap<u32, (usize, String)> = HashMap::new();
    for point in points {
        while next < scopes.len() && scopes[next].start <= point.pos {
            while stack.last().is_some_and(|&t| scopes[t].end <= scopes[next].start) {
                stack.pop();
            }
            scopes[next].parent = stack.last().copied();
            stack.push(next);
            next += 1;
        }
        while stack.last().is_some_and(|&t| scopes[t].end <= point.pos) {
            stack.pop();
        }
        let Some(&inner) = stack.last() else { continue };
        match point.kind {
            PointKind::Def { kind, owner } => {
                let mut s = inner;
                if owner == Some(scopes[s].node_id) {
                    s = scopes[s].parent.unwrap_or(s);
                }
                if kind == DefKind::Walrus {
                    while scopes[s].kind == ScopeKind::Comprehension {
                        match scopes[s].parent {
                            Some(p) => s = p,
                            None => break,
                        }
                    }
                }
                *scopes[s].names.entry(point.name).or_default() |= kind.bit();
            }
            PointKind::Global => {
                scopes[inner].globals.insert(point.name);
            }
            PointKind::Nonlocal => {
                scopes[inner].nonlocals.insert(point.name);
            }
            PointKind::Reference => {
                refs.entry(point.pos).or_insert((inner, point.name));
            }
        }
    }

    let module = scopes.iter().position(|sc| sc.kind == ScopeKind::Module);
    Ok(Locals { scopes, module, refs, max_hops, incomplete })
}

#[derive(Debug, PartialEq, Eq)]
enum Resolution {
    Local,
    TypeParameter,
    NotLocal,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture names the Rust side relies on. A grammar or query change that
    /// drops one of them must fail here.
    const REQUIRED_CAPTURES: &[&str] = &[
        "local.scope",
        "local.reference",
        "local.global",
        "local.nonlocal",
        "local.definition.function",
        "local.definition.type",
        "local.definition.type_parameter",
        "local.definition.parameter",
        "local.definition.var",
        "local.definition.iter",
        "local.definition.alias",
        "local.definition.walrus",
        "local.definition.case",
        "local.definition.import",
    ];

    /// Exercises every pattern of `locals.scm` at least once.
    const GOLDEN: &str = r#"
import a.b
import c as d
from e import f
class K[T](Base):
    attr = 1
    def m(self, p, q=1, r: int = 2, s: int, *args, **kw):
        global G
        nonlocal_test = 0
        def inner():
            nonlocal nonlocal_test
        for i in range(3):
            pass
        x += 1
        with open(p) as fh:
            pass
        if (w := 1):
            pass
        return [y for y in args], {z for z in kw}, {k: v for k, v in kw}, (g for g in kw)
    def typed[U](self, *a: U, **b: str):
        pass
fn = lambda u, v=1, *rest, **more: u
match fn:
    case Point(x=px) as pt:
        pass
    case [first, *others]:
        pass
"#;

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query().expect("locals.scm must compile against tree-sitter-python");
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
        let language = tree_sitter_python::LANGUAGE.into();
        let mut parser = telar_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(GOLDEN, None).unwrap();
        assert!(!tree.root_node().has_error(), "golden corpus must parse cleanly");
        let mut cursor = QueryCursor::new();
        let mut matched = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN.as_bytes());
        while let Some(m) = matches.next() {
            matched[m.pattern_index] = true;
        }
        let unmatched: Vec<(usize, usize)> = matched
            .iter()
            .enumerate()
            .filter(|(_, m)| !**m)
            .map(|(i, _)| (i, lq.query.start_byte_for_pattern(i)))
            .collect();
        let lines: Vec<String> = unmatched
            .iter()
            .map(|(i, b)| format!("pattern {i}: {}", LOCALS_SCM[*b..].lines().next().unwrap_or("")))
            .collect();
        assert!(unmatched.is_empty(), "patterns that no longer match (grammar node/field renamed?):\n{}", lines.join("\n"));
    }

    #[test]
    fn query_header_records_provenance() {
        assert!(LOCALS_SCM.starts_with("; SPDX-License-Identifier: Apache-2.0"));
        assert!(LOCALS_SCM.contains("29212d826054bb2948fa9d3a99eca9bf6a050eb2"));
        assert!(LOCALS_SCM.contains("431e9e82c5a293c40f22aadb3615c3aa387af82e"));
        // Neovim-only predicates are stripped (they may only appear in comments).
        let code = LOCALS_SCM.lines().filter(|l| !l.trim_start().starts_with(';'));
        assert!(code.clone().all(|l| !l.contains("#set!") && !l.contains("#make-range!")));
        assert!(code.clone().any(|l| l.contains("@local.scope")));
    }

    fn local_names(src: &str) -> Vec<String> {
        let language = tree_sitter_python::LANGUAGE.into();
        let mut parser = telar_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5), 512).unwrap();
        let mut out: Vec<String> = locals
            .refs
            .keys()
            .filter(|p| locals.resolution_at(**p) != Resolution::NotLocal)
            .map(|p| {
                let start = *p as usize;
                let len = src[start..].find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(src.len() - start);
                format!("{}@{}", &src[start..start + len], src[..start].matches('\n').count() + 1)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn hoisting_and_header_scoping() {
        // `x` is local in the whole body, even before its assignment; the
        // default `y=y` reads the enclosing `y`; the annotation sees T.
        let src = "def f[T](a: T, y=y):\n    print(x)\n    x = 1\n";
        // Parameter names in the header are bindings, not reads, so the table
        // does not list them; the annotation `T` and the type parameter are.
        assert_eq!(local_names(src), ["T@1", "T@1", "x@2", "x@3"]);
    }

    #[test]
    fn walrus_in_comprehension_binds_in_function() {
        let src = "def f(items):\n    [last := i for i in items]\n    return last\n";
        let names = local_names(src);
        assert!(names.contains(&"last@3".to_string()), "{names:?}");
    }

    #[test]
    fn global_and_nonlocal() {
        let src = "def f():\n    global G\n    G = 1\n    v = 0\n    def g():\n        nonlocal v\n        v = 2\n        return G, v\n";
        let names = local_names(src);
        assert!(!names.iter().any(|n| n.starts_with("G@")), "{names:?}");
        assert!(names.contains(&"v@8".to_string()), "{names:?}");
    }

    #[test]
    fn imports_are_bindings_but_not_local() {
        let src = "import os\ndef f():\n    import json\n    return json, os\n";
        let names = local_names(src);
        assert!(!names.iter().any(|n| n.starts_with("json@") || n.starts_with("os@")), "{names:?}");
    }

    #[test]
    fn class_scope_visibility() {
        let src = "class C[T]:\n    A = 1\n    B = A\n    def m(self) -> T:\n        return A, T\n";
        let names = local_names(src);
        assert!(names.contains(&"A@3".to_string()), "{names:?}");
        assert!(!names.contains(&"A@5".to_string()), "{names:?}");
        assert!(names.contains(&"T@4".to_string()) && names.contains(&"T@5".to_string()), "{names:?}");
    }

    #[test]
    fn match_limit_is_set_and_table_is_complete_for_normal_files() {
        let src = "def f(a):\n    return a\n";
        let language = tree_sitter_python::LANGUAGE.into();
        let mut parser = telar_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5), 512).unwrap();
        assert!(!locals.incomplete);
        let zero = build(&tree, src.as_bytes(), Duration::ZERO, 512).unwrap();
        // A zero time budget may cut the query short but never errors.
        let _ = zero.incomplete;
    }
}
