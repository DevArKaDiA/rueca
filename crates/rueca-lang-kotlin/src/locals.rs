//! Local scope table driven by the vendored `queries/locals.scm`.
//!
//! The extractor asks two questions about an identifier in a body: **is it
//! a local** (parameter, `val` / `var`, `for` / `catch` / lambda / `when`
//! subject variable), so that it is never reported as a reference to a
//! property or a class, and **which class does it hold**, so that `x.m()`
//! can be reported with the typed receiver `$type:T` (RFC 0003 §9.2).
//!
//! The query (adapted from nvim-treesitter; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`). This module runs it once per file with a
//! match limit and a deadline, attaches every binding to its innermost scope
//! and answers lookups by walking from the innermost scope containing a use
//! outwards. Kotlin rules the query cannot express:
//!
//! - A local is visible from its declaration on (no hoisting).
//! - Properties of classes and files and `val` / `var` primary-constructor
//!   parameters are members: the extractor records them, so they are
//!   skipped here.
//! - The class comes from the declared type (`x: Foo`, `e: IOException`),
//!   else from the initializer: a call of a name that looks like a class
//!   (`val x = Foo(…)`, `a.b.Foo(…)`), a cast (`x as Foo`), a string
//!   literal (`String`). A call of a lower-case name (`val x = make()`) is
//!   kept as [`Binding::init_call`] for the extractor, which knows the
//!   return types of the file's functions.
//!
//! If the cursor hits its match limit or the deadline the table is partial:
//! affected identifiers are simply treated as non-local and untyped.

use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rueca_extract::ExtractError;
use rueca_extract::tree_sitter::{
    Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree,
};

use crate::types::{class_of_type, declared_type, is_type_like, named_children, suffix_type, text};

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scope,
    Def,
    Ignore,
}

fn role_of(name: &str) -> Role {
    match name {
        "local.scope" => Role::Scope,
        n if n == "local.definition" || n.starts_with("local.definition.") => Role::Def,
        _ => Role::Ignore,
    }
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

/// The grammar every query and parse of this crate uses.
pub(crate) fn language() -> rueca_extract::tree_sitter::Language {
    tree_sitter_kotlin_sg::LANGUAGE.into()
}

/// Compiles `queries/locals.scm` against the Kotlin grammar (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let query = Query::new(&language(), LOCALS_SCM).map_err(|e| format!("locals.scm: {e}"))?;
            let roles = query.capture_names().iter().map(|n| role_of(n)).collect();
            Ok(LocalsQuery { query, roles })
        })
        .as_ref()
        .map_err(|e| ExtractError::Query(e.clone()))
}

/// One local binding.
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    pub(crate) name: String,
    /// Start byte of the binding identifier.
    pos: u32,
    /// Class held (`Foo`, `a.b.Foo`), generic arguments removed.
    pub(crate) class: Option<String>,
    /// Declared type in overload-suffix form (`Int`, `String`), for
    /// argument types.
    pub(crate) ty: Option<String>,
    /// Name of the lower-case function an untyped initializer calls
    /// (`val x = make()`).
    pub(crate) init_call: Option<String>,
    /// Byte range of the initializer of an untyped binding, for the
    /// extractor to type it as a receiver (`val b = A.builder()`).
    pub(crate) init: Option<(usize, usize)>,
}

struct ScopeInfo {
    start: u32,
    end: u32,
    parent: Option<usize>,
    bindings: Vec<Binding>,
}

/// Per-file local table.
#[derive(Default)]
pub(crate) struct Locals {
    /// Sorted by start byte (outer scope first on ties).
    scopes: Vec<ScopeInfo>,
}

impl Locals {
    /// Innermost scope containing byte `pos`.
    fn innermost(&self, pos: u32) -> Option<usize> {
        let idx = self.scopes.partition_point(|s| s.start <= pos);
        let mut cur = idx.checked_sub(1)?;
        loop {
            let s = &self.scopes[cur];
            if s.start <= pos && pos < s.end.max(s.start + 1) {
                return Some(cur);
            }
            cur = s.parent?;
        }
    }

    /// The local binding of `name` visible at byte `pos`, if any.
    pub(crate) fn lookup(&self, name: &str, pos: u32) -> Option<&Binding> {
        let mut cur = self.innermost(pos);
        while let Some(i) = cur {
            let s = &self.scopes[i];
            if let Some(b) = s.bindings.iter().rev().find(|b| b.name == name && b.pos <= pos) {
                return Some(b);
            }
            cur = s.parent;
        }
        None
    }

    /// Whether `name` is a local at byte `pos`.
    pub(crate) fn is_local(&self, name: &str, pos: u32) -> bool {
        self.lookup(name, pos).is_some()
    }
}

/// Skips parentheses and `!!` around an expression.
fn strip_expr(mut v: Node<'_>) -> Node<'_> {
    for _ in 0..8 {
        match v.kind() {
            "parenthesized_expression" | "postfix_expression" => match v.named_child(0) {
                Some(c) => v = c,
                None => return v,
            },
            _ => return v,
        }
    }
    v
}

/// What an initializer expression says about the value's class: `(class,
/// lower-case callee)`.
pub(crate) fn initializer(value: Node<'_>, source: &[u8]) -> (Option<String>, Option<String>) {
    let v = strip_expr(value);
    match v.kind() {
        "call_expression" => match call_class(v, source) {
            Some(c) => (Some(c), None),
            None => match call_path(v, source) {
                Some(p) if !p.contains('.') => (None, Some(p)),
                _ => (None, None),
            },
        },
        "as_expression" => (declared_type(v).and_then(|t| class_of_type(t, source)), None),
        "string_literal" => (Some("String".to_string()), None),
        _ => (None, None),
    }
}

/// Standard-library calls whose result's class does not depend on the
/// receiver.
const STD_RESULTS: &[(&str, &str)] = &[
    ("emptyList", "List"),
    ("emptyMap", "Map"),
    ("emptySet", "Set"),
    ("joinToString", "String"),
    ("listOf", "List"),
    ("mapOf", "Map"),
    ("mutableListOf", "MutableList"),
    ("mutableMapOf", "MutableMap"),
    ("mutableSetOf", "MutableSet"),
    ("setOf", "Set"),
    ("toList", "List"),
    ("toMap", "Map"),
    ("toMutableList", "MutableList"),
    ("toMutableSet", "MutableSet"),
    ("toSet", "Set"),
    ("toString", "String"),
    ("trimIndent", "String"),
    ("trimMargin", "String"),
];

/// The class a standard-library call named `name` returns.
fn std_result(name: &str) -> Option<String> {
    STD_RESULTS.iter().find(|(n, _)| *n == name).map(|(_, c)| c.to_string())
}

/// The class a call evaluates to when the call shows it: a construction
/// (`Foo(…)`, `a.b.Foo(…)`, a name that looks like a class) or a
/// standard-library call whose result does not depend on its receiver
/// (`x.toString()`, `listOf(…)`).
pub(crate) fn call_class(call: Node<'_>, source: &[u8]) -> Option<String> {
    let callee = call.named_child(0)?;
    let last = match callee.kind() {
        "navigation_expression" => callee.named_child(1).and_then(|n| n.named_child(0)).map(|n| text(n, source)),
        "simple_identifier" => Some(text(callee, source)),
        _ => None,
    };
    if let Some(c) = last.and_then(std_result) {
        return Some(c);
    }
    let p = call_path(call, source)?;
    is_type_like(p.rsplit('.').next().unwrap_or(&p)).then_some(p)
}

/// The callee of a call written as a name path: `Foo(…)` → `Foo`,
/// `a.b.Foo(…)` → `a.b.Foo`, `make<T>()` → `make`.
pub(crate) fn call_path(call: Node<'_>, source: &[u8]) -> Option<String> {
    let callee = call.named_child(0)?;
    name_path(callee, source)
}

/// A navigation chain of plain identifiers as text (`a.b.C`).
pub(crate) fn name_path(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_identifier" | "type_identifier" => Some(text(node, source).to_string()),
        "navigation_expression" => {
            let head = name_path(node.named_child(0)?, source)?;
            let suffix = node.named_child(1)?;
            let id = suffix.named_child(0).filter(|c| c.kind() == "simple_identifier")?;
            if text(suffix, source).starts_with("?.") {
                return None;
            }
            Some(format!("{head}.{}", text(id, source)))
        }
        _ => None,
    }
}

/// Whether a `variable_declaration` belongs to a property of a class, an
/// object or the file (a member, not a local).
fn is_member_property(var: Node<'_>) -> bool {
    let Some(mut decl) = var.parent() else { return false };
    if decl.kind() == "multi_variable_declaration" {
        let Some(p) = decl.parent() else { return false };
        decl = p;
    }
    decl.kind() == "property_declaration"
        && decl.parent().is_some_and(|p| matches!(p.kind(), "class_body" | "source_file" | "enum_class_body"))
}

/// The initializer of the property or `when` subject declaring `var`.
fn initializer_of(var: Node<'_>) -> Option<Node<'_>> {
    let parent = var.parent()?;
    if !matches!(parent.kind(), "property_declaration" | "when_subject") {
        return None;
    }
    named_children(parent).into_iter().find(|c| {
        c.start_byte() > var.end_byte()
            && !matches!(c.kind(), "property_delegate" | "getter" | "setter" | "type_constraints")
            && !c.kind().ends_with("comment")
    })
}

/// The binding a `@local.definition` capture introduces.
fn def_binding(node: Node<'_>, source: &[u8]) -> Option<Binding> {
    if node.kind() != "simple_identifier" {
        return None;
    }
    let parent = node.parent()?;
    let mut init = None;
    let (class, ty, init_call) = match parent.kind() {
        "variable_declaration" => {
            if is_member_property(parent) {
                return None;
            }
            let value = initializer_of(parent);
            init = value.filter(|_| declared_type(parent).is_none()).map(|v| (v.start_byte(), v.end_byte()));
            binding_types(parent, value, source)
        }
        // `val` / `var` constructor parameters are properties.
        "class_parameter" if crate::types::child_of_kind(parent, "binding_pattern_kind").is_some() => return None,
        "parameter" | "class_parameter" | "catch_block" | "parameter_with_optional_type" => {
            binding_types(parent, None, source)
        }
        _ => (None, None, None),
    };
    Some(Binding { name: text(node, source).to_string(), pos: node.start_byte() as u32, class, ty, init_call, init })
}

type Types = (Option<String>, Option<String>, Option<String>);

/// Class, suffix type and initializer callee of a declaration.
fn binding_types(decl: Node<'_>, init: Option<Node<'_>>, source: &[u8]) -> Types {
    if let Some(t) = declared_type(decl) {
        return (class_of_type(t, source), Some(suffix_type(t, source)), None);
    }
    let Some(v) = init else { return (None, None, None) };
    let (class, call) = initializer(v, source);
    let ty = class.as_ref().map(|c| c.rsplit('.').next().unwrap_or(c).to_string());
    (class, ty, call)
}

/// The scope table of `(start, end)` scope ranges: sorted, deduplicated and
/// linked to their innermost enclosing scope.
fn scope_table(mut scopes: Vec<(u32, u32)>) -> Vec<ScopeInfo> {
    scopes.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    scopes.dedup();
    let mut table: Vec<ScopeInfo> = Vec::with_capacity(scopes.len());
    let mut stack: Vec<usize> = Vec::new();
    for (start, end) in scopes {
        while let Some(&top) = stack.last() {
            if table[top].end >= end && table[top].start <= start {
                break;
            }
            stack.pop();
        }
        let parent = stack.last().copied();
        table.push(ScopeInfo { start, end, parent, bindings: Vec::new() });
        stack.push(table.len() - 1);
    }
    table
}

/// Runs the locals query over `tree` and builds the scope table.
pub(crate) fn build(tree: &Tree, source: &[u8], max_time: Duration) -> Result<Locals, ExtractError> {
    let lq = locals_query()?;
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    let deadline = Instant::now() + max_time;
    let mut progress = |_: &QueryCursorState| {
        if Instant::now() > deadline { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    };
    let options = QueryCursorOptions::new().progress_callback(&mut progress);
    let mut scopes: Vec<(u32, u32)> = Vec::new();
    let mut defs: Vec<Binding> = Vec::new();
    {
        let mut matches = cursor.matches_with_options(&lq.query, tree.root_node(), source, options);
        while let Some(m) = matches.next() {
            for c in m.captures() {
                match lq.roles[c.index as usize] {
                    Role::Scope => scopes.push((c.node.start_byte() as u32, c.node.end_byte() as u32)),
                    Role::Def => defs.extend(def_binding(c.node, source)),
                    Role::Ignore => {}
                }
            }
        }
    }
    let mut locals = Locals { scopes: scope_table(scopes) };
    defs.sort_by_key(|d| d.pos);
    defs.dedup_by_key(|d| d.pos);
    for d in defs {
        if let Some(s) = locals.innermost(d.pos) {
            locals.scopes[s].bindings.push(d);
        }
    }
    Ok(locals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rueca_extract::tree_sitter::Parser;

    /// One snippet per pattern of `locals.scm`: every pattern must match it.
    pub(crate) const GOLDEN: &str = r#"
class A(val p: Int, plain: String) {
  constructor(s: String) : this(1, s)
  var v: Int = 0
    set(value) { field = value }
  init { println(plain) }
  fun m(a: Foo, b: Int) {
    val x = Foo()
    val (d, e) = pair
    for (i in 0..3) {}
    while (true) {}
    do {} while (false)
    try {} catch (ex: IOException) {}
    listOf(1).map { y: Int -> y }
    val f = fun(z: Int): Int = z
    if (x is Foo) {}
    when (val w = make()) { else -> "${w}" }
  }
  val g get() = 1
}
enum class E { A { }, B }
"#;

    fn parse(src: &str) -> Tree {
        let mut p = Parser::new();
        p.set_language(&language()).unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query().expect("locals.scm compiles");
        let names = lq.query.capture_names();
        assert!(names.contains(&"local.scope"));
        assert_eq!(lq.roles.len(), names.len());
    }

    #[test]
    fn every_pattern_matches_the_golden_corpus() {
        let lq = locals_query().unwrap();
        let tree = parse(GOLDEN);
        assert!(!tree.root_node().has_error(), "golden corpus parses");
        let mut cursor = QueryCursor::new();
        let mut seen = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN.as_bytes());
        while let Some(m) = matches.next() {
            seen[m.pattern_index] = true;
        }
        let missing: Vec<usize> = seen.iter().enumerate().filter(|(_, s)| !**s).map(|(i, _)| i).collect();
        assert!(missing.is_empty(), "patterns without a match: {missing:?}");
    }

    #[test]
    fn bindings_and_classes() {
        let tree = parse(GOLDEN);
        let l = build(&tree, GOLDEN.as_bytes(), Duration::from_secs(5)).unwrap();
        let at = |needle: &str| GOLDEN.find(needle).unwrap() as u32;
        let pos = at("val (d, e)");
        assert_eq!(l.lookup("x", pos).and_then(|b| b.class.clone()).as_deref(), Some("Foo"));
        assert_eq!(l.lookup("a", pos).and_then(|b| b.class.clone()).as_deref(), Some("Foo"));
        assert_eq!(l.lookup("b", pos).and_then(|b| b.ty.clone()).as_deref(), Some("Int"));
        assert!(l.lookup("d", at("for (i")).is_some());
        // Properties and `val` constructor parameters are members.
        assert!(l.lookup("v", pos).is_none());
        assert!(l.lookup("p", pos).is_none());
        assert!(l.lookup("plain", at("println(plain)")).is_some());
        let w = at("${w}");
        assert_eq!(l.lookup("w", w).and_then(|b| b.init_call.clone()).as_deref(), Some("make"));
        assert_eq!(l.lookup("ex", at("{} catch") + 30).and_then(|b| b.class.clone()), None);
    }
}
