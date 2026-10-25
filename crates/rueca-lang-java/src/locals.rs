//! Local-variable scope table driven by the vendored `queries/locals.scm`.
//!
//! The extractor asks two questions about an identifier in a method body:
//! **is it a local variable** (parameter, local, `for` / `catch` / lambda /
//! resource / pattern variable), so that it is never reported as a
//! reference to a field or a class, and **which type was it declared
//! with**, so that `x.m()` can be reported with the typed receiver
//! `$type:T` (RFC 0003 §9.2).
//!
//! The query (adapted from nvim-treesitter and bloop; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`). This module runs it once per file with a
//! match limit and a deadline, attaches every binding to its innermost scope
//! and answers lookups by walking from the innermost scope containing a use
//! outwards. Java rules the query cannot express:
//!
//! - A local is visible from its declaration on (no hoisting): a binding
//!   counts only if it starts before the use.
//! - Record components (`record P(int x)`) are fields, not locals: the
//!   extractor records them as members, so they are skipped here.
//! - The declared type comes from the declaration: the `type` of a local,
//!   parameter, enhanced-`for` variable or resource, the single type of a
//!   `catch`, the type of a pattern; `var` takes the class of a
//!   `new T(…)` or a cast initializer. Arrays, primitives and untyped
//!   lambda parameters have no type.
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

use crate::types::{class_of_type, suffix_type};

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

/// Compiles `queries/locals.scm` against tree-sitter-java (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_java::LANGUAGE.into();
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
    /// Declared class (`Foo`, `Map.Entry`, `com.x.Foo`), generic arguments
    /// removed; `None` for primitives, arrays, `var` without a `new` / cast
    /// initializer, and untyped lambda parameters.
    pub(crate) class: Option<String>,
    /// Declared type in overload-suffix form (`int`, `String`, `byte[]`,
    /// `List`), primitives and arrays included; for `var`, the class of a
    /// `new` / cast initializer. Used to choose overloads by argument type.
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

fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// The class a `var` initializer constructs or casts to.
fn initializer_class(value: Node<'_>, source: &[u8]) -> Option<String> {
    let mut v = value;
    while v.kind() == "parenthesized_expression" {
        v = v.named_child(0)?;
    }
    match v.kind() {
        "object_creation_expression" => {
            let has_body =
                (0..v.named_child_count()).filter_map(|i| v.named_child(i as u32)).any(|c| c.kind() == "class_body");
            if has_body {
                return None;
            }
            class_of_type(v.child_by_field_name("type")?, source)
        }
        "cast_expression" => class_of_type(v.child_by_field_name("type")?, source),
        _ => None,
    }
}

/// Declared class of a binding identifier, from its declaration.
fn binding_class(def: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = def.parent()?;
    let declared = |ty: Option<Node<'_>>, value: Option<Node<'_>>| -> Option<String> {
        let ty = ty?;
        if ty.kind() == "type_identifier" && text(ty, source) == "var" {
            return initializer_class(value?, source);
        }
        class_of_type(ty, source)
    };
    match parent.kind() {
        "variable_declarator" => {
            if parent.child_by_field_name("dimensions").is_some() {
                return None;
            }
            let decl = parent.parent()?;
            if decl.kind() != "local_variable_declaration" {
                return None;
            }
            declared(decl.child_by_field_name("type"), parent.child_by_field_name("value"))
        }
        "formal_parameter" => {
            if parent.child_by_field_name("dimensions").is_some() {
                return None;
            }
            class_of_type(parent.child_by_field_name("type")?, source)
        }
        "enhanced_for_statement" => {
            if parent.child_by_field_name("dimensions").is_some() {
                return None;
            }
            declared(parent.child_by_field_name("type"), None)
        }
        "resource" => declared(parent.child_by_field_name("type"), parent.child_by_field_name("value")),
        "catch_formal_parameter" => {
            let ct = (0..parent.named_child_count())
                .filter_map(|i| parent.named_child(i as u32))
                .find(|c| c.kind() == "catch_type")?;
            let types: Vec<Node<'_>> = (0..ct.named_child_count()).filter_map(|i| ct.named_child(i as u32)).collect();
            match types[..] {
                [one] => class_of_type(one, source),
                _ => None,
            }
        }
        "instanceof_expression" => class_of_type(parent.child_by_field_name("right")?, source),
        "type_pattern" | "record_pattern_component" => {
            let ty = (0..parent.named_child_count())
                .filter_map(|i| parent.named_child(i as u32))
                .find(|c| c.kind() != "identifier" && c.id() != def.id())?;
            class_of_type(ty, source)
        }
        _ => None,
    }
}

/// Declared type of a binding identifier in overload-suffix form
/// ([`Binding::ty`]).
fn binding_type(def: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = def.parent()?;
    let with_dims = |ty: Node<'_>, dims: Option<Node<'_>>| -> Option<String> {
        let t = text(ty, source);
        if t == "var" {
            return None;
        }
        let mut s = suffix_type(t);
        if let Some(d) = dims {
            s.push_str(&text(d, source).split_whitespace().collect::<String>());
        }
        Some(s)
    };
    match parent.kind() {
        "variable_declarator" => {
            let decl = parent.parent()?;
            match decl.kind() {
                "local_variable_declaration" => {
                    let ty = decl.child_by_field_name("type")?;
                    if text(ty, source) == "var" {
                        return binding_class(def, source).map(|c| suffix_type(&c));
                    }
                    with_dims(ty, parent.child_by_field_name("dimensions"))
                }
                // `String... args` is a `String[]` inside the method.
                "spread_parameter" => {
                    let ty = (0..decl.named_child_count()).filter_map(|i| decl.named_child(i as u32)).find(|c| {
                        !matches!(c.kind(), "modifiers" | "variable_declarator" | "annotation" | "marker_annotation")
                    })?;
                    with_dims(ty, None).map(|t| format!("{t}[]"))
                }
                _ => None,
            }
        }
        "formal_parameter" => with_dims(parent.child_by_field_name("type")?, parent.child_by_field_name("dimensions")),
        "enhanced_for_statement" => {
            with_dims(parent.child_by_field_name("type")?, parent.child_by_field_name("dimensions"))
        }
        _ => binding_class(def, source).map(|c| suffix_type(&c)),
    }
}

/// Record components are fields (members), not locals.
fn is_record_component(def: Node<'_>) -> bool {
    def.parent()
        .filter(|p| p.kind() == "formal_parameter")
        .and_then(|p| p.parent())
        .and_then(|params| params.parent())
        .is_some_and(|owner| owner.kind() == "record_declaration")
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
                    Role::Def => {
                        if c.node.kind() != "identifier" || is_record_component(c.node) {
                            continue;
                        }
                        defs.push(Binding {
                            name: text(c.node, source).to_string(),
                            pos: c.node.start_byte() as u32,
                            class: binding_class(c.node, source),
                            ty: binding_type(c.node, source),
                        });
                    }
                    Role::Ignore => {}
                }
            }
        }
    }
    let incomplete = cursor.did_exceed_match_limit() || timed_out;

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
    let mut locals = Locals { scopes: table, incomplete };
    defs.sort_by_key(|d| d.pos);
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
class A {
  void m(int p, String... rest) {
    int x = 1;
    for (int i = 0; i < 3; i++) {}
    for (String s : rest) {}
    try (var in = new java.io.StringReader("")) {} catch (RuntimeException e) {}
    java.util.function.BiFunction<Integer, Integer, Integer> f = (a, b) -> a + b;
    java.util.function.Function<Integer, Integer> g = y -> y;
    Object o = null;
    if (o instanceof String str) {}
    switch (o) {
      case Integer n -> {}
      case Pt(int px, int py) -> {}
      default -> {}
    }
    switch (x) { case 1: break; }
  }
  record Pt(int px, int py) { Pt {} }
  A() {}
}
"#;

    fn parse(src: &str) -> Tree {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_java::LANGUAGE.into()).unwrap();
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
        for needle in [
            "SPDX-License-Identifier: Apache-2.0",
            "29212d826054bb2948fa9d3a99eca9bf6a050eb2",
            "431e9e82c5a293c40f22aadb3615c3aa387af82e",
            "Adapted for Rueca",
        ] {
            assert!(LOCALS_SCM.contains(needle), "missing `{needle}` in locals.scm header");
        }
    }

    fn at(src: &str, needle: &str) -> u32 {
        src.find(needle).unwrap_or_else(|| panic!("{needle}")) as u32
    }

    #[test]
    fn bindings_scopes_and_types() {
        let src = "class A {\n  Foo field;\n  void m(Bar b, int n, String[] arr) {\n    var v = new Baz<String>();\n    Qux q = make();\n    for (Item it : items) { it.go(); }\n    try { } catch (IOException e) { e.log(); } catch (A | B ab) { ab.x(); }\n    Runnable r = () -> { int inner = 1; };\n    if (!(o instanceof Pat pat)) return;\n    pat.use();\n    b.call(); q.call(); v.call(); field.call();\n  }\n  void other() { b.call(); }\n}\n";
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        let class = |name: &str, needle: &str| locals.lookup(name, at(src, needle)).map(|b| b.class.clone());
        assert_eq!(class("b", "b.call(); q"), Some(Some("Bar".into())));
        assert_eq!(class("n", "b.call(); q"), Some(None));
        assert_eq!(class("arr", "b.call(); q"), Some(None));
        assert_eq!(class("v", "v.call()"), Some(Some("Baz".into())));
        assert_eq!(class("q", "q.call()"), Some(Some("Qux".into())));
        assert_eq!(class("it", "it.go()"), Some(Some("Item".into())));
        assert_eq!(class("e", "e.log()"), Some(Some("IOException".into())));
        assert_eq!(class("ab", "ab.x()"), Some(None));
        assert_eq!(class("pat", "pat.use()"), Some(Some("Pat".into())));
        // Fields are not locals; a loop variable is not visible after the loop;
        // another method does not see `b`; lambda locals stay in the lambda.
        assert_eq!(class("field", "field.call()"), None);
        assert_eq!(class("it", "b.call(); q"), None);
        assert_eq!(class("b", "b.call(); }\n}"), None);
        assert_eq!(class("inner", "pat.use()"), None);
        assert!(!locals.incomplete);
    }

    #[test]
    fn use_before_declaration_is_not_local() {
        let src = "class A { void m() { x.go(); Foo x = null; x.go(); } }";
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        assert!(!locals.is_local("x", at(src, "x.go(); Foo")));
        assert!(locals.is_local("x", at(src, "x.go(); }")));
    }

    #[test]
    fn record_components_are_not_locals() {
        let src = "record P(Foo x) { void m() { x.go(); } }\nclass B { void n() { x.go(); } }";
        let tree = parse(src);
        let locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        assert!(!locals.is_local("x", at(src, "x.go(); } }\nclass")));
        assert!(!locals.is_local("x", at(src, "x.go(); } }\n").max(at(src, "n() { x") + 8)));
    }
}
