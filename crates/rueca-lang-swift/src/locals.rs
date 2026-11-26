//! Local scope table driven by the vendored `queries/locals.scm`.
//!
//! The extractor asks two questions about an identifier in a body: **is it
//! a local** (parameter, `let` / `var`, `for` / `if let` / `guard let` /
//! `catch` / `case let` / closure binding), so that a call of it is not a
//! reference to a function, and **which type does it hold**, so that
//! `x.m()` can be reported with the typed receiver `$type:T` (RFC 0003
//! §9.2).
//!
//! The query (adapted from nvim-treesitter; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`). This module runs it once per file with a
//! match limit and a deadline, attaches every binding to its innermost scope
//! and answers lookups by walking from the innermost scope containing a use
//! outwards. Swift rules the query cannot express:
//!
//! - A local is visible from its declaration on (no hoisting).
//! - Properties of types and files are members: the extractor records them,
//!   so they are skipped here; so is a parameter's external label.
//! - The type comes from the declared type (`x: Foo`, `(z: Foo) in`), else
//!   from the initializer: a call of a name that looks like a type
//!   (`let x = Foo(…)`, `Foo.Bar(…)`), a cast (`x as? Foo`), a literal
//!   (`"s"` → `String`, `[…]` → `Array`), or another local
//!   (`if let o = opt`, `if let opt`), followed when looked up. Any other
//!   initializer is kept as a byte range ([`Binding::init`]) for the
//!   extractor to type as a call result.
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

use crate::types::{class_of_type, declared_type, is_type_like, named_children, parameter_name, text};

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

/// How many local-to-local hops a type lookup follows.
const MAX_ALIAS: usize = 4;

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
    tree_sitter_swift::LANGUAGE.into()
}

/// Compiles `queries/locals.scm` against the Swift grammar (once per process).
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

/// What a binding's value says about its type.
#[derive(Debug, Clone, Default)]
pub(crate) struct Binding {
    pub(crate) name: String,
    /// Start byte of the binding identifier.
    pos: u32,
    /// Type held (`Foo`, `Foo.Bar`), generic arguments and optionality removed.
    pub(crate) class: Option<String>,
    /// Another local the value comes from (`if let o = opt`).
    alias: Option<String>,
    /// Element type of an array value (`xs: [Foo]` → `Foo`).
    element: Option<String>,
    /// The local whose elements this `for` variable iterates (`for x in xs`).
    element_of: Option<String>,
    /// Byte range of the initializer of an untyped binding, for the
    /// extractor to type it as a call result (`let x = make()`).
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

    /// The type `name` holds at byte `pos`, following local-to-local
    /// bindings (`if let o = opt`).
    pub(crate) fn class_of(&self, name: &str, pos: u32) -> Option<String> {
        let (mut name, mut pos) = (name.to_string(), pos);
        for _ in 0..MAX_ALIAS {
            let b = self.lookup(&name, pos)?;
            if b.class.is_some() {
                return b.class.clone();
            }
            if let Some(coll) = &b.element_of {
                return self.lookup(coll, b.pos.checked_sub(1)?)?.element.clone();
            }
            let next = b.alias.clone()?;
            // The shorthand `if let opt` rebinds the same name: look before it.
            pos = b.pos.checked_sub(1)?;
            name = next;
        }
        None
    }
}

/// Skips parentheses, `try`, `await`, `!` / `?` postfixes around an expression.
pub(crate) fn strip_expr(mut v: Node<'_>) -> Node<'_> {
    for _ in 0..8 {
        let inner = match v.kind() {
            "tuple_expression" if v.named_child_count() == 1 => v.named_child(0),
            "postfix_expression" | "await_expression" => v.named_child(0),
            "try_expression" => v.child_by_field_name("expr"),
            _ => None,
        };
        match inner {
            Some(c) => v = c,
            None => return v,
        }
    }
    v
}

/// A navigation chain of plain identifiers as text (`a.b.C`); `None` for
/// anything else (calls, subscripts, optional chaining).
pub(crate) fn name_path(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_identifier" | "type_identifier" => Some(text(node, source).trim_matches('`').to_string()),
        "user_type" => {
            let s = crate::types::strip_type_text(text(node, source));
            crate::types::is_name_path(&s).then_some(s)
        }
        "navigation_expression" => {
            let head = name_path(node.child_by_field_name("target")?, source)?;
            let suffix = node.child_by_field_name("suffix")?;
            let id = suffix.child_by_field_name("suffix").filter(|c| c.kind() == "simple_identifier")?;
            Some(format!("{head}.{}", text(id, source).trim_matches('`')))
        }
        _ => None,
    }
}

/// The callee of a call as a name path: `Foo(…)` → `Foo`, `a.B(…)` → `a.B`.
pub(crate) fn call_path(call: Node<'_>, source: &[u8]) -> Option<String> {
    name_path(operand_callee(call.named_child(0)?, source), source)
}

/// The callee a call applies to: the grammar attaches a call suffix to a
/// whole binary or prefix-operator expression (`a + b.f(1)` is parsed as
/// `(a + b.f)(1)`, `!x.isEmpty()` as `(!x.isEmpty)()`), whose right-most
/// operand is the callee.
pub(crate) fn operand_callee<'t>(callee: Node<'t>, source: &[u8]) -> Node<'t> {
    let mut cur = callee;
    for _ in 0..64 {
        let next = match cur.child_by_field_name("rhs") {
            Some(rhs) => Some(rhs),
            None if cur.kind() == "prefix_expression" && !text(cur, source).starts_with('.') => {
                cur.child_by_field_name("target")
            }
            None => None,
        };
        match next {
            Some(n) => cur = n,
            None => return cur,
        }
    }
    cur
}

/// The type a call evaluates to when the call shows it: a construction
/// (`Foo(…)`, `Foo.Bar(…)`, the last segment looking like a type).
pub(crate) fn call_class(call: Node<'_>, source: &[u8]) -> Option<String> {
    let p = call_path(call, source)?;
    is_type_like(p.rsplit('.').next().unwrap_or(&p)).then_some(p)
}

/// The type of a cast (`x as? T` → `T`).
pub(crate) fn cast_type(cast: Node<'_>, source: &[u8]) -> Option<String> {
    let t = named_children(cast).into_iter().rev().find(|c| crate::types::is_type_node(c.kind()))?;
    class_of_type(t, source)
}

/// What an initializer expression says about the value's type: the type,
/// or another local it comes from.
pub(crate) fn initializer(value: Node<'_>, source: &[u8]) -> (Option<String>, Option<String>) {
    let v = strip_expr(value);
    match v.kind() {
        "call_expression" => (call_class(v, source), None),
        "constructor_expression" => {
            (v.child_by_field_name("constructed_type").and_then(|t| class_of_type(t, source)), None)
        }
        "as_expression" => (cast_type(v, source), None),
        "line_string_literal" | "multi_line_string_literal" | "raw_string_literal" => {
            (Some("String".to_string()), None)
        }
        "array_literal" => (Some("Array".to_string()), None),
        "dictionary_literal" => (Some("Dictionary".to_string()), None),
        "simple_identifier" => (None, Some(text(v, source).to_string())),
        _ => (None, None),
    }
}

/// Whether a `property_declaration` is a member of a type or of the file.
fn is_member_property(decl: Node<'_>) -> bool {
    decl.parent()
        .is_some_and(|p| matches!(p.kind(), "class_body" | "source_file" | "enum_class_body" | "protocol_body"))
}

/// The initializer following the pattern `pat` in its declaration or
/// condition (`let x = v`, `if let x = v`).
fn value_after<'t>(decl: Node<'t>, pat: Node<'_>) -> Option<Node<'t>> {
    named_children(decl).into_iter().find(|c| {
        c.start_byte() > pat.end_byte()
            && !matches!(c.kind(), "type_annotation" | "statements" | "value_binding_pattern" | "pattern")
            && !c.kind().ends_with("comment")
    })
}

/// The binding a `@local.definition` capture introduces.
fn def_binding(node: Node<'_>, source: &[u8]) -> Option<Binding> {
    if node.kind() != "simple_identifier" {
        return None;
    }
    let parent = node.parent()?;
    let mut b = Binding {
        name: text(node, source).trim_matches('`').to_string(),
        pos: node.start_byte() as u32,
        ..Default::default()
    };
    match parent.kind() {
        "parameter" => {
            if parameter_name(parent) != Some(node) {
                return None;
            }
            let ty = declared_type(parent);
            b.class = ty.and_then(|t| class_of_type(t, source));
            b.element = ty.and_then(|t| element_type(t, source));
        }
        "lambda_parameter" => b.class = declared_type(parent).and_then(|t| class_of_type(t, source)),
        "pattern" => type_pattern_binding(parent, node, source, &mut b)?,
        "if_statement" | "guard_statement" | "while_statement" => {
            let (class, alias) = match value_after(parent, node) {
                Some(v) => initializer(v, source),
                None => (None, Some(b.name.clone())),
            };
            (b.class, b.alias) = (class, alias);
        }
        _ => {}
    }
    Some(b)
}

/// A binding inside a `pattern`: a `let` / `var` (typed by its annotation
/// or initializer), a `for` variable, a `catch let e as E` or a `case let`.
fn type_pattern_binding(pat: Node<'_>, id: Node<'_>, source: &[u8], b: &mut Binding) -> Option<()> {
    let mut top = pat;
    while let Some(p) = top.parent().filter(|p| p.kind() == "pattern") {
        top = p;
    }
    let Some(decl) = top.parent() else { return Some(()) };
    match decl.kind() {
        "property_declaration" => {
            if is_member_property(decl) {
                return None;
            }
            if top == pat && pat.child_by_field_name("bound_identifier") == Some(id) {
                property_types(decl, pat, source, b);
            }
        }
        "catch_block" => b.class = declared_type(top).and_then(|t| class_of_type(t, source)),
        "for_statement" if top == pat => {
            b.element_of = decl
                .child_by_field_name("collection")
                .filter(|c| c.kind() == "simple_identifier")
                .map(|c| text(c, source).to_string());
        }
        _ => {}
    }
    Some(())
}

/// The element type of an array type (`[Foo]`, `[Foo]?`).
fn element_type(ty: Node<'_>, source: &[u8]) -> Option<String> {
    let t = crate::types::unwrap_type(ty)?;
    if t.kind() != "array_type" {
        return None;
    }
    let el = named_children(t).into_iter().find(|c| crate::types::is_type_node(c.kind()))?;
    class_of_type(el, source)
}

/// Type of a `let x: T = v` / `let x = v` binding (the annotation or value
/// following this pattern in its declaration).
fn property_types(decl: Node<'_>, pat: Node<'_>, source: &[u8], b: &mut Binding) {
    let kids = named_children(decl);
    let at = kids.iter().position(|c| *c == pat).unwrap_or(0);
    let mut ann = None;
    let mut value = None;
    for c in &kids[at + 1..] {
        match c.kind() {
            "pattern" => break,
            "type_annotation" => ann = Some(*c),
            k if !k.ends_with("comment") && value.is_none() && k != "value_binding_pattern" => value = Some(*c),
            _ => {}
        }
    }
    if let Some(t) = ann.and_then(declared_type) {
        b.class = class_of_type(t, source);
        b.element = element_type(t, source);
        return;
    }
    if let Some(v) = value {
        (b.class, b.alias) = initializer(v, source);
        if b.class.is_none() && b.alias.is_none() {
            b.init = Some((v.start_byte(), v.end_byte()));
        }
    }
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
struct A {
  var p: Int = 0
  var c: Int { get { 1 } set { } }
  var w: Int = 0 { willSet { } didSet { } }
  init(x: Int) { }
  deinit { }
  subscript(i: Int) -> Int { i }
  func m(at a: Foo, _ b: Int, opt: Foo?) {
    let x = Foo()
    let (d, e) = pair
    for i in items { }
    for (k, v) in dict { }
    while let n = next() { }
    repeat { } while false
    if let o = opt { o.go() }
    if let opt { opt.go() }
    guard let g = opt as? Bar else { return }
    do { try f() } catch let err as MyError { }
    switch opt { case .some(let s): break
    default: break }
    items.map { (y: Foo) -> Int in y.v }
  }
}
protocol P { func r(z: Int) }
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
    fn bindings_and_types() {
        let tree = parse(GOLDEN);
        let l = build(&tree, GOLDEN.as_bytes(), Duration::from_secs(5)).unwrap();
        let at = |needle: &str| GOLDEN.find(needle).unwrap() as u32;
        let pos = at("let (d, e)");
        assert_eq!(l.class_of("x", pos).as_deref(), Some("Foo"));
        assert_eq!(l.class_of("a", pos).as_deref(), Some("Foo"));
        assert!(l.lookup("at", pos).is_none(), "external labels are not locals");
        assert!(l.lookup("p", pos).is_none(), "properties are members");
        assert!(l.lookup("d", at("for i")).is_some());
        assert_eq!(l.class_of("o", at("o.go()")).as_deref(), Some("Foo"));
        assert_eq!(l.class_of("opt", at("opt.go()")).as_deref(), Some("Foo"));
        assert_eq!(l.class_of("g", at("do {")).as_deref(), Some("Bar"));
        assert_eq!(l.class_of("y", at("y.v")).as_deref(), Some("Foo"));
        assert!(l.lookup("n", at("while let n") + 20).is_some());
        let t = parse("func f(xs: [Foo]) { for x in xs { x.go() } }");
        let src = "func f(xs: [Foo]) { for x in xs { x.go() } }";
        let l = build(&t, src.as_bytes(), Duration::from_secs(5)).unwrap();
        assert_eq!(l.class_of("x", src.find("x.go").unwrap() as u32).as_deref(), Some("Foo"));
    }
}
