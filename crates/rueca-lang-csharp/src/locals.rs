//! Local-variable scope table driven by the vendored `queries/locals.scm`.
//!
//! The extractor asks two questions about an identifier in a body: **is it
//! a local variable** (parameter, local, `foreach` / `catch` / lambda /
//! pattern / `out var` variable, primary-constructor parameter), so that it
//! is never reported as a reference to a field, a property or a type, and
//! **which type was it declared with**, so that `x.M()` can be reported
//! with the typed receiver `$type:T` (RFC 0003 §9.2).
//!
//! The query (adapted from nvim-treesitter; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`). This module runs it once per file with a
//! match limit and a deadline, attaches every binding to its innermost scope
//! and answers lookups by walking from the innermost scope containing a use
//! outwards. C# rules the query cannot express:
//!
//! - A local is visible from its declaration on (no hoisting): a binding
//!   counts only if it starts before the use. Parameters precede the body.
//! - Record parameters (`record P(int X)`) are properties, not locals: the
//!   extractor records them as members, so they are skipped here.
//! - The declared type comes from the declaration: the `type` of a local,
//!   parameter, `foreach` variable, `catch` declaration, `out` declaration
//!   or declaration pattern; `var` takes the class of a `new T(…)`, a cast
//!   or an `as T` initializer. Arrays, keyword value types and implicit
//!   lambda parameters have no class.
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

use crate::types::{class_of_type, suffix_type, text};

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

/// Compiles `queries/locals.scm` against tree-sitter-c-sharp (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_c_sharp::LANGUAGE.into();
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
    pub(crate) name: String,
    /// Start byte of the binding identifier.
    pos: u32,
    /// Declared class (`Foo`, `Outer.Inner`, `System.IO.Stream`), generic
    /// arguments removed; `None` for value keywords, arrays, `var` without
    /// a `new` / cast / `as` initializer, and implicit lambda parameters.
    pub(crate) class: Option<String>,
    /// Declared type in overload-suffix form (`int`, `string`, `byte[]`,
    /// `List`), keywords and arrays included; for `var`, the class of a
    /// `new` / cast / `as` initializer. Used to choose overloads.
    pub(crate) ty: Option<String>,
}

struct ScopeInfo {
    start: u32,
    end: u32,
    parent: Option<usize>,
    bindings: Vec<Binding>,
}

/// Per-file local-variable table.
#[derive(Default)]
pub(crate) struct Locals {
    /// Sorted by start byte (outer scope first on ties).
    scopes: Vec<ScopeInfo>,
    /// True if the cursor hit [`MATCH_LIMIT`] or the deadline.
    #[allow(dead_code)]
    pub(crate) incomplete: bool,
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

    /// Whether `name` is a local variable at byte `pos`.
    pub(crate) fn is_local(&self, name: &str, pos: u32) -> bool {
        self.lookup(name, pos).is_some()
    }
}

/// Skips parentheses and the null-forgiving `!` around an expression.
fn unwrap_expr(mut v: Node<'_>) -> Option<Node<'_>> {
    while matches!(v.kind(), "parenthesized_expression" | "postfix_unary_expression") {
        v = v.named_child(0)?;
    }
    Some(v)
}

/// The class a `var` initializer constructs, casts to or converts with `as`.
fn initializer_class(value: Node<'_>, source: &[u8]) -> Option<String> {
    let v = unwrap_expr(value)?;
    match v.kind() {
        "object_creation_expression" | "cast_expression" => class_of_type(v.child_by_field_name("type")?, source),
        "as_expression" => class_of_type(v.child_by_field_name("right")?, source),
        _ => None,
    }
}

/// The initializer expression of a `variable_declarator` (its last named
/// child that is not the name, a tuple pattern or an array size).
fn declarator_value(declarator: Node<'_>) -> Option<Node<'_>> {
    let name = declarator.child_by_field_name("name");
    let mut cursor = declarator.walk();
    declarator
        .named_children(&mut cursor)
        .filter(|c| Some(*c) != name && !matches!(c.kind(), "tuple_pattern" | "bracketed_argument_list"))
        .last()
}

/// A declared type node: its class, or for `var` the initializer's.
fn declared(ty: Option<Node<'_>>, value: Option<Node<'_>>, source: &[u8]) -> Option<String> {
    let ty = ty?;
    if ty.kind() == "implicit_type" {
        return initializer_class(value?, source);
    }
    class_of_type(ty, source)
}

/// Declared class of a binding identifier, from its declaration.
fn binding_class(def: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = def.parent()?;
    match parent.kind() {
        "variable_declarator" => {
            let decl = parent.parent()?;
            declared(decl.child_by_field_name("type"), declarator_value(parent), source)
        }
        "parameter"
        | "declaration_expression"
        | "declaration_pattern"
        | "recursive_pattern"
        | "catch_declaration"
        | "parameter_list" => class_of_type(parent.child_by_field_name("type")?, source),
        "foreach_statement" => {
            let ty = parent.child_by_field_name("type")?;
            (ty.kind() != "implicit_type").then(|| class_of_type(ty, source)).flatten()
        }
        _ => None,
    }
}

/// Declared type of a binding identifier in overload-suffix form
/// ([`Binding::ty`]).
fn binding_type(def: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = def.parent()?;
    let ty = match parent.kind() {
        "variable_declarator" => parent.parent()?.child_by_field_name("type")?,
        "parameter" | "declaration_expression" | "declaration_pattern" | "catch_declaration" | "foreach_statement" => {
            parent.child_by_field_name("type")?
        }
        // `params T[] xs`: the array inside the method.
        "parameter_list" => parent.child_by_field_name("type")?,
        _ => return None,
    };
    if ty.kind() == "implicit_type" {
        return binding_class(def, source).map(|c| suffix_type(&c));
    }
    Some(suffix_type(text(ty, source)))
}

/// Record parameters are properties (members), not locals.
fn is_record_parameter(def: Node<'_>) -> bool {
    def.parent()
        .filter(|p| p.kind() == "parameter")
        .and_then(|p| p.parent())
        .and_then(|params| params.parent())
        .is_some_and(|owner| owner.kind() == "record_declaration")
}

/// The binding a `@local.definition` capture introduces: identifiers (and
/// implicit lambda parameters) only, record parameters excluded.
fn def_binding(node: Node<'_>, source: &[u8]) -> Option<Binding> {
    if !matches!(node.kind(), "identifier" | "implicit_parameter") || is_record_parameter(node) {
        return None;
    }
    Some(Binding {
        name: text(node, source).to_string(),
        pos: node.start_byte() as u32,
        class: binding_class(node, source),
        ty: binding_type(node, source),
    })
}

/// The scope table of `(start, end)` scope ranges: sorted, deduplicated and
/// linked to their innermost enclosing scope.
fn scope_table(mut scopes: Vec<(u32, u32)>) -> Vec<ScopeInfo> {
    // Outer scopes first on equal starts, so parents precede children.
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
    let mut timed_out = false;
    let mut progress = |_: &QueryCursorState| {
        if Instant::now() > deadline {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
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
    let incomplete = cursor.did_exceed_match_limit() || timed_out;
    let mut locals = Locals { scopes: scope_table(scopes), incomplete };
    defs.sort_by_key(|d| d.pos);
    defs.dedup_by(|a, b| a.pos == b.pos);
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
class A(int primary) {
  void M(int p, params string[] rest) {
    int x = 1;
    var (a, b) = (1, 2);
    for (int i = 0; i < 3; i++) {}
    foreach (var s in rest) {}
    foreach (var (k, v) in pairs) {}
    using (var r = new Reader()) {}
    try { } catch (Exception e) { }
    Func<int, int> f = y => y;
    Func<int, int, int> g = (int u, int w) => u + w;
    Action h = delegate (int z) { };
    if (int.TryParse("1", out var parsed)) {}
    if (o is string str) {}
    if (o is Point { X: 1 } pt) {}
    var t = o switch { (var q, var r2) => 1, _ => 2 };
    switch (x) { case 1: break; }
    var qs = from c in items let d = c select d;
    int Local() => 1;
  }
  unsafe void U() { fixed (int* ptr = arr) {} }
  A() {}
  ~A() {}
  int this[int ix] { get { return ix; } }
  public static A operator +(A l, A r) => l;
  public static implicit operator int(A a) => 0;
}
struct S(int sp) {}
"#;

    fn parse(src: &str) -> Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_c_sharp::LANGUAGE.into()).unwrap();
        p.parse(src, None).unwrap()
    }

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query().expect("locals.scm compiles");
        let names = lq.query.capture_names();
        assert!(names.contains(&"local.scope"));
        assert!(names.iter().any(|n| n.starts_with("local.definition")));
        assert_eq!(lq.roles.len(), names.len());
    }

    #[test]
    fn every_locals_pattern_matches_the_golden_corpus() {
        let tree = parse(GOLDEN);
        assert!(!tree.root_node().has_error(), "golden corpus must parse cleanly");
        let lq = locals_query().unwrap();
        let mut seen = vec![false; lq.query.pattern_count()];
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN.as_bytes());
        while let Some(m) = matches.next() {
            seen[m.pattern_index] = true;
        }
        let missing: Vec<usize> = seen.iter().enumerate().filter(|(_, s)| !**s).map(|(i, _)| i).collect();
        assert!(missing.is_empty(), "patterns without a match in the golden corpus: {missing:?}");
    }

    #[test]
    fn query_header_records_provenance() {
        for needle in
            ["SPDX-License-Identifier: Apache-2.0", "29212d826054bb2948fa9d3a99eca9bf6a050eb2", "Adapted for Rueca"]
        {
            assert!(LOCALS_SCM.contains(needle), "missing `{needle}` in locals.scm header");
        }
    }

    fn at(src: &str, needle: &str) -> u32 {
        src.find(needle).unwrap_or_else(|| panic!("{needle}")) as u32
    }

    #[test]
    fn bindings_scopes_and_types() {
        let src = "class A {\n  Foo field;\n  void M(Bar b, int n, string[] arr) {\n    var v = new Baz<string>();\n    Qux q = Make();\n    var w = o as Wid;\n    foreach (Item it in items) { it.Go(); }\n    try { } catch (IOException e) { e.Log(); }\n    Action r = () => { int inner = 1; };\n    if (o is Pat pat) { }\n    pat.Use();\n    Try(out Res res); res.Ok();\n    b.Call(); q.Call(); v.Call(); w.Call(); field.Call();\n  }\n  void Other() { b.Call(); }\n}\n";
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        let class = |name: &str, needle: &str| locals.lookup(name, at(src, needle)).map(|b| b.class.clone());
        assert_eq!(class("b", "b.Call(); q"), Some(Some("Bar".into())));
        assert_eq!(class("n", "b.Call(); q"), Some(None));
        assert_eq!(class("arr", "b.Call(); q"), Some(None));
        assert_eq!(class("v", "v.Call()"), Some(Some("Baz".into())));
        assert_eq!(class("q", "q.Call()"), Some(Some("Qux".into())));
        assert_eq!(class("w", "w.Call()"), Some(Some("Wid".into())));
        assert_eq!(class("it", "it.Go()"), Some(Some("Item".into())));
        assert_eq!(class("e", "e.Log()"), Some(Some("IOException".into())));
        assert_eq!(class("pat", "pat.Use()"), Some(Some("Pat".into())));
        assert_eq!(class("res", "res.Ok()"), Some(Some("Res".into())));
        // Fields are not locals; a loop variable is not visible after the loop;
        // another method does not see `b`; lambda locals stay in the lambda.
        assert_eq!(class("field", "field.Call()"), None);
        assert_eq!(class("it", "b.Call(); q"), None);
        assert_eq!(class("b", "b.Call(); }\n}"), None);
        assert_eq!(class("inner", "pat.Use()"), None);
        assert!(!locals.incomplete);
        let ty = |name: &str, needle: &str| locals.lookup(name, at(src, needle)).and_then(|b| b.ty.clone());
        assert_eq!(ty("n", "b.Call(); q").as_deref(), Some("int"));
        assert_eq!(ty("arr", "b.Call(); q").as_deref(), Some("string[]"));
        assert_eq!(ty("v", "v.Call()").as_deref(), Some("Baz"));
    }

    #[test]
    fn use_before_declaration_is_not_local() {
        let src = "class A { void M() { x.Go(); Foo x = null; x.Go(); } }";
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        assert!(!locals.is_local("x", at(src, "x.Go(); Foo")));
        assert!(locals.is_local("x", at(src, "x.Go(); }")));
    }

    #[test]
    fn record_parameters_are_not_locals_primary_constructor_parameters_are() {
        let src = "record P(Foo X) { void M() { X.Go(); } }\nclass C(Bar y) { void N() { y.Go(); } }";
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        assert!(!locals.is_local("X", at(src, "X.Go()")));
        assert_eq!(locals.lookup("y", at(src, "y.Go()")).and_then(|b| b.class.clone()).as_deref(), Some("Bar"));
    }
}
