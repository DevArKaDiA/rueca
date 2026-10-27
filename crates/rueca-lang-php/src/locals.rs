//! Variable-scope table driven by the vendored `queries/locals.scm`.
//!
//! PHP keeps variables (`$x`) syntactically apart from functions, classes
//! and constants, and the pack never reports variables as references. What
//! the scope table is for is **receiver typing**: deciding that `$user` in
//! `$user->save()` holds a `User` because every binding of `$user` in its
//! scope says so (a typed parameter `User $user`, `catch (User $user)`, or
//! `$user = new User(...)`).
//!
//! The query (adapted from nvim-treesitter and bloop; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`), bindings
//! (`@local.definition.<kind>`) and every variable use (`@local.reference`).
//! This module runs it once per file with a [`QueryCursor`] (match limit and
//! deadline enforced) and applies PHP's scope rules, which a query cannot
//! express:
//!
//! - Variables are function-scoped: blocks are not scopes, and a binding
//!   anywhere in a function applies to the whole function.
//! - Functions and methods do not see the enclosing scope; closures
//!   (`function () use ($x) {}`) see only what `use` captures, so a captured
//!   variable takes its type from the enclosing scope; arrow functions
//!   (`fn () => …`) see the enclosing scope automatically.
//! - A variable has a type only when *every* binding in its scope agrees on
//!   one class; any untyped binding (`$user = $repo->find()`, `global`,
//!   `foreach`) makes it unknown.
//!
//! If the cursor hits its match limit or deadline the table is partial and
//! affected variables are simply untyped.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rueca_extract::ExtractError;

use crate::text::{doc_tag_class, docblock_of};
use rueca_extract::tree_sitter::{
    Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree,
};

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

/// Upper bound on variables collected from one definition pattern node.
const MAX_PATTERN_NODES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    Parameter,
    Var,
    Catch,
    Global,
    /// Closure `use ($x)`: the enclosing scope's variable.
    Capture,
}

impl DefKind {
    fn from_suffix(s: &str) -> DefKind {
        match s {
            "parameter" => DefKind::Parameter,
            "catch" => DefKind::Catch,
            "global" => DefKind::Global,
            "capture" | "import" => DefKind::Capture,
            _ => DefKind::Var,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scope,
    Def(DefKind),
    Reference,
    Ignore,
}

fn role_of(name: &str) -> Role {
    match name {
        "local.scope" => Role::Scope,
        "local.reference" => Role::Reference,
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

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

/// Compiles `queries/locals.scm` against tree-sitter-php (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_php::LANGUAGE_PHP.into();
            let query = Query::new(&language, LOCALS_SCM).map_err(|e| format!("locals.scm: {e}"))?;
            let roles = query.capture_names().iter().map(|n| role_of(n)).collect();
            Ok(LocalsQuery { query, roles })
        })
        .as_ref()
        .map_err(|e| ExtractError::Query(e.clone()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ScopeKind {
    Program,
    Function,
    Closure,
    Arrow,
}

fn scope_kind(node: Node<'_>) -> ScopeKind {
    match node.kind() {
        "program" => ScopeKind::Program,
        "anonymous_function" => ScopeKind::Closure,
        "arrow_function" => ScopeKind::Arrow,
        _ => ScopeKind::Function,
    }
}

#[derive(Debug, Clone)]
struct Binding {
    kind: DefKind,
    /// Declared or constructed class, as written (`User`, `\App\User`).
    class: Option<String>,
}

struct ScopeInfo {
    start: u32,
    end: u32,
    kind: ScopeKind,
    parent: Option<usize>,
    /// Variable name (without `$`) → its bindings in this scope.
    bindings: HashMap<String, Vec<Binding>>,
}

/// Per-file variable-scope table.
#[derive(Default)]
pub(crate) struct Locals {
    scopes: Vec<ScopeInfo>,
    /// Variable use start byte → innermost scope.
    refs: HashMap<u32, usize>,
    max_hops: usize,
    /// True if the cursor hit [`MATCH_LIMIT`] or the deadline.
    #[allow(dead_code)]
    pub(crate) incomplete: bool,
}

/// Name of a `variable_name` node without `$` (`None` for `$$x`).
fn var_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "variable_name" {
        return None;
    }
    let mut cursor = node.walk();
    let name = node.named_children(&mut cursor).find(|c| c.kind() == "name")?;
    Some(String::from_utf8_lossy(&source[name.start_byte()..name.end_byte()]).into_owned())
}

/// Class written in a `type` node when it names exactly one class
/// (`User`, `?User`); unions, intersections and builtin types give `None`.
pub(crate) fn single_class(ty: Node<'_>, source: &[u8]) -> Option<String> {
    let named = match ty.kind() {
        "named_type" => ty,
        "optional_type" => {
            let mut cursor = ty.walk();
            let inner = ty.named_children(&mut cursor).find(|c| c.kind() != "comment")?;
            if inner.kind() != "named_type" {
                return None;
            }
            inner
        }
        _ => return None,
    };
    let text = String::from_utf8_lossy(&source[named.start_byte()..named.end_byte()]).into_owned();
    Some(text.split_whitespace().collect())
}

/// The class a binding node declares or constructs, if any.
fn binding_class(def: Node<'_>, kind: DefKind, source: &[u8]) -> Option<String> {
    let parent = def.parent()?;
    match kind {
        DefKind::Parameter => {
            let param = if parent.kind() == "by_ref" { parent.parent()? } else { parent };
            if param.kind() == "variadic_parameter" {
                return None;
            }
            match param.child_by_field_name("type") {
                Some(ty) => single_class(ty, source),
                // Untyped parameter: `@param Type $name` in the docblock.
                None => {
                    let function = param.parent()?.parent()?;
                    let doc = docblock_of(function, source)?;
                    let name = String::from_utf8_lossy(&source[def.start_byte()..def.end_byte()]).into_owned();
                    doc_tag_class(&doc, "@param", Some(&name))
                }
            }
        }
        DefKind::Catch => {
            let types = parent.child_by_field_name("type")?;
            let mut cursor = types.walk();
            let named: Vec<Node<'_>> = types.named_children(&mut cursor).filter(|c| c.kind() != "comment").collect();
            if named.len() != 1 {
                return None;
            }
            single_class(named[0], source)
        }
        DefKind::Var if def.kind() == "variable_name" && parent.kind() == "assignment_expression" => {
            let right = parent.child_by_field_name("right")?;
            if right.kind() != "object_creation_expression" {
                // `/** @var Cache $cache */` right before the statement.
                let stmt = parent.parent().filter(|s| s.kind() == "expression_statement")?;
                let doc = docblock_of(stmt, source)?;
                let name = String::from_utf8_lossy(&source[def.start_byte()..def.end_byte()]).into_owned();
                return doc_tag_class(&doc, "@var", Some(&name))
                    .or_else(|| (!doc.contains('$')).then(|| doc_tag_class(&doc, "@var", None)).flatten());
            }
            let mut cursor = right.walk();
            let class = right
                .named_children(&mut cursor)
                .find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"))?;
            Some(String::from_utf8_lossy(&source[class.start_byte()..class.end_byte()]).split_whitespace().collect())
        }
        _ => None,
    }
}

/// Every `variable_name` bound by a target node (a variable, `list(...)` /
/// `[...]` destructuring, a `foreach` key/value pair, a by-reference target).
fn bound_variables<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    let mut stack = vec![node];
    let mut visited = 0;
    while let Some(n) = stack.pop() {
        visited += 1;
        if visited > MAX_PATTERN_NODES {
            return;
        }
        match n.kind() {
            "variable_name" => out.push(n),
            "list_literal" | "pair" | "by_ref" | "array_element_initializer" => {
                let mut cursor = n.walk();
                let children: Vec<Node<'t>> = n.named_children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
            // Keys, subscripts, member accesses and nested expressions bind nothing.
            _ => {}
        }
    }
}

enum Point {
    Def { pos: u32, name: String, binding: Binding },
    Reference { pos: u32 },
}

impl Point {
    fn pos(&self) -> u32 {
        match self {
            Point::Def { pos, .. } | Point::Reference { pos } => *pos,
        }
    }
}

impl Locals {
    /// The class every binding of the variable `var` (a `variable_name`
    /// node) agrees on, as written in the source.
    pub(crate) fn class_of(&self, var: Node<'_>, source: &[u8]) -> Option<String> {
        let name = var_name(var, source)?;
        if name == "this" {
            return None;
        }
        let scope = *self.refs.get(&(var.start_byte() as u32))?;
        self.class_in(scope, &name, 0)
    }

    fn class_in(&self, scope: usize, name: &str, hops: usize) -> Option<String> {
        let mut s = scope;
        let mut hops = hops;
        loop {
            if hops > self.max_hops {
                return None;
            }
            hops += 1;
            let sc = &self.scopes[s];
            if let Some(bindings) = sc.bindings.get(name) {
                return self.agreed_class(sc.parent, bindings, name, hops);
            }
            match sc.kind {
                ScopeKind::Arrow => s = sc.parent?,
                _ => return None,
            }
        }
    }

    /// The class all `bindings` of `name` agree on (case-insensitive, leading
    /// `\` ignored); captures look it up in the `parent` scope.
    fn agreed_class(&self, parent: Option<usize>, bindings: &[Binding], name: &str, hops: usize) -> Option<String> {
        let mut agreed: Option<String> = None;
        for b in bindings {
            let class = match b.kind {
                DefKind::Capture => self.class_in(parent?, name, hops)?,
                DefKind::Global => return None,
                _ => b.class.clone()?,
            };
            match &agreed {
                Some(a) if !a.trim_start_matches('\\').eq_ignore_ascii_case(class.trim_start_matches('\\')) => {
                    return None;
                }
                Some(_) => {}
                None => agreed = Some(class),
            }
        }
        agreed
    }
}

/// Scopes and binding / reference points gathered from the query captures.
#[derive(Default)]
struct Collector<'t> {
    scopes: Vec<ScopeInfo>,
    points: Vec<Point>,
    seen_scopes: HashSet<usize>,
    vars: Vec<Node<'t>>,
}

impl<'t> Collector<'t> {
    fn capture(&mut self, role: Role, node: Node<'t>, source: &[u8]) {
        match role {
            Role::Scope => {
                if self.seen_scopes.insert(node.id()) {
                    self.scopes.push(ScopeInfo {
                        start: node.start_byte() as u32,
                        end: node.end_byte() as u32,
                        kind: scope_kind(node),
                        parent: None,
                        bindings: HashMap::new(),
                    });
                }
            }
            Role::Def(kind) => self.definitions(node, kind, source),
            Role::Reference if node.kind() == "variable_name" => {
                self.points.push(Point::Reference { pos: node.start_byte() as u32 });
            }
            _ => {}
        }
    }

    /// One `Def` point per variable the definition pattern binds.
    fn definitions(&mut self, node: Node<'t>, kind: DefKind, source: &[u8]) {
        self.vars.clear();
        bound_variables(node, &mut self.vars);
        for v in &self.vars {
            let Some(name) = var_name(*v, source) else { continue };
            let class = binding_class(*v, kind, source);
            self.points.push(Point::Def { pos: v.start_byte() as u32, name, binding: Binding { kind, class } });
        }
    }
}

/// Opens, in order, the scopes starting at or before `pos`, setting each
/// one's parent to the innermost open scope that still contains it.
fn open_scopes(scopes: &mut [ScopeInfo], stack: &mut Vec<usize>, next: &mut usize, pos: u32) {
    while *next < scopes.len() && scopes[*next].start <= pos {
        let start = scopes[*next].start;
        while stack.last().is_some_and(|&t| scopes[t].end <= start) {
            stack.pop();
        }
        scopes[*next].parent = stack.last().copied();
        stack.push(*next);
        *next += 1;
    }
}

/// Sweeps the sorted `points` over the sorted `scopes`: files each binding
/// under its innermost scope, sets every scope's parent and returns the
/// innermost scope of each reference.
fn assign_scopes(scopes: &mut [ScopeInfo], points: Vec<Point>) -> HashMap<u32, usize> {
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0;
    let mut refs: HashMap<u32, usize> = HashMap::new();
    for point in points {
        let pos = point.pos();
        open_scopes(scopes, &mut stack, &mut next, pos);
        while stack.last().is_some_and(|&t| scopes[t].end <= pos) {
            stack.pop();
        }
        let Some(&inner) = stack.last() else { continue };
        match point {
            Point::Def { name, binding, .. } => scopes[inner].bindings.entry(name).or_default().push(binding),
            Point::Reference { pos } => {
                refs.entry(pos).or_insert(inner);
            }
        }
    }
    // Scopes after the last point still need their parents for lookups.
    open_scopes(scopes, &mut stack, &mut next, u32::MAX);
    refs
}

/// Runs the locals query over `tree` and builds the scope table.
/// `time_budget` bounds the query run; `max_hops` bounds each lookup.
pub(crate) fn build(
    tree: &Tree,
    source: &[u8],
    time_budget: Duration,
    max_hops: usize,
) -> Result<Locals, ExtractError> {
    let lq = locals_query()?;
    let mut collector = Collector::default();

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
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                let role = lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore);
                collector.capture(role, cap.node, source);
            }
        }
    }
    let incomplete = timed_out || cursor.did_exceed_match_limit();
    let Collector { mut scopes, mut points, .. } = collector;

    // Scopes nest properly; order outer-first for the sweep.
    scopes.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)).then(a.kind.cmp(&b.kind)));
    // Stable sort keeps capture order among points at the same position.
    points.sort_by_key(Point::pos);

    let refs = assign_scopes(&mut scopes, points);
    Ok(Locals { scopes, refs, max_hops, incomplete })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture names the Rust side relies on.
    const REQUIRED_CAPTURES: &[&str] = &[
        "local.scope",
        "local.reference",
        "local.definition.parameter",
        "local.definition.capture",
        "local.definition.var",
        "local.definition.catch",
        "local.definition.global",
    ];

    /// Exercises every pattern of `locals.scm` at least once.
    const GOLDEN: &str = r#"<?php
function f(User $u, ...$rest) {
    $a = new A();
    $b =& $a;
    [$c, [$d]] = $rest;
    foreach ($u->items as $k => &$v) {}
    static $s = 0;
    global $g;
    try {} catch (E $e) {}
    $cl = function ($p) use ($u, &$a) { return $p; };
    $ar = fn ($q) => $q + $a;
}
class K {
    public function __construct(private R $r, public &$w) {}
}
"#;

    fn parse(src: &str) -> Tree {
        let language = tree_sitter_php::LANGUAGE_PHP.into();
        let mut parser = rueca_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query().expect("locals.scm must compile against tree-sitter-php");
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
        assert!(!tree.root_node().has_error(), "golden corpus must parse cleanly");
        let mut cursor = QueryCursor::new();
        let mut matched = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN.as_bytes());
        while let Some(m) = matches.next() {
            matched[m.pattern_index] = true;
        }
        let unmatched: Vec<String> = matched
            .iter()
            .enumerate()
            .filter(|(_, m)| !**m)
            .map(|(i, _)| {
                let b = lq.query.start_byte_for_pattern(i);
                format!("pattern {i}: {}", LOCALS_SCM[b..].lines().next().unwrap_or(""))
            })
            .collect();
        assert!(
            unmatched.is_empty(),
            "patterns that no longer match (grammar node/field renamed?):\n{}",
            unmatched.join("\n")
        );
    }

    #[test]
    fn query_header_records_provenance() {
        assert!(LOCALS_SCM.starts_with("; SPDX-License-Identifier: Apache-2.0"));
        assert!(LOCALS_SCM.contains("29212d826054bb2948fa9d3a99eca9bf6a050eb2"));
        assert!(LOCALS_SCM.contains("431e9e82c5a293c40f22aadb3615c3aa387af82e"));
        let code = LOCALS_SCM.lines().filter(|l| !l.trim_start().starts_with(';'));
        assert!(code.clone().all(|l| !l.contains("#set!")));
        assert!(code.clone().any(|l| l.contains("@local.scope")));
    }

    /// `(variable, class)` for every variable use whose class is known.
    fn typed(src: &str) -> Vec<(String, String)> {
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5), 512).unwrap();
        let mut out = Vec::new();
        let mut cursor = tree.walk();
        loop {
            let n = cursor.node();
            if n.kind() == "variable_name"
                && let Some(c) = locals.class_of(n, src.as_bytes())
            {
                let line = n.start_position().row + 1;
                out.push((format!("{}@{line}", &src[n.start_byte()..n.end_byte()]), c));
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    out.sort();
                    out.dedup();
                    return out;
                }
            }
        }
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        out.sort();
        out
    }

    #[test]
    fn parameters_catch_and_new_are_typed() {
        let src = "<?php\nfunction f(User $u, ?Repo $r, A|B $x) {\n$u->save();\n$r->find();\n$x->m();\n$n = new \\App\\Mail();\n$n->send();\ntry {} catch (E $e) { $e->log(); }\n}\n";
        assert_eq!(
            typed(src),
            pairs(&[
                ("$e@8", "E"),
                ("$n@6", "\\App\\Mail"),
                ("$n@7", "\\App\\Mail"),
                ("$r@2", "Repo"),
                ("$r@4", "Repo"),
                ("$u@2", "User"),
                ("$u@3", "User"),
            ])
        );
    }

    #[test]
    fn reassignment_and_scopes() {
        // Reassigned to something untyped: unknown. Functions do not see
        // the file scope; closures see captures; arrow functions see all.
        let src = "<?php\n$top = new Top();\nfunction g(User $u) {\n$u = $u->fresh();\n$u->save();\n$top->m();\n$c = function () use ($v) { return $v; };\n}\nfunction h(User $w) {\n$c = function () use ($w) { $w->a(); };\n$d = fn () => $w->b();\n$e = function () { $w->c(); };\n}\n";
        let got = typed(src);
        assert!(!got.iter().any(|(v, _)| v.starts_with("$u@")), "{got:?}");
        assert!(!got.iter().any(|(v, _)| v == "$top@6"), "{got:?}");
        assert!(got.contains(&("$top@2".to_string(), "Top".to_string())), "{got:?}");
        assert!(got.contains(&("$w@10".to_string(), "User".to_string())), "{got:?}");
        assert!(got.contains(&("$w@11".to_string(), "User".to_string())), "{got:?}");
        assert!(!got.iter().any(|(v, _)| v == "$w@12"), "{got:?}");
    }

    #[test]
    fn match_limit_is_set_and_table_is_complete_for_normal_files() {
        let tree = parse(GOLDEN);
        let locals = build(&tree, GOLDEN.as_bytes(), Duration::from_secs(5), 512).unwrap();
        assert!(!locals.incomplete);
        let zero = build(&tree, GOLDEN.as_bytes(), Duration::ZERO, 512).unwrap();
        let _ = zero.incomplete;
    }
}
