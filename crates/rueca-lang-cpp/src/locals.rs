//! Local-scope table driven by the vendored `queries/c-locals.scm` and
//! `queries/cpp-locals.scm`.
//!
//! The table answers two questions for the reference walk:
//!
//! - **Is this name local?** A parameter, a local variable, a `for` / range
//!   `for` / `catch` / lambda binding, or a template type parameter. Locals
//!   are never reported as references (a call through a local function
//!   pointer, a read of a local), and a template's `T` is never reported as
//!   a project type.
//! - **What type does it declare?** `Foo* p`, `const Foo& f`, `Foo f(1)`,
//!   `auto f = new Foo` / `std::make_unique<Foo>()` give `Foo`, which the
//!   pack reports as a typed receiver `$type:Foo` for `p->m()` / `f.m()`.
//!
//! The query marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`); this module runs it once per file with a
//! [`QueryCursor`] (match limit and deadline enforced), nests the scopes and
//! applies C/C++ rules the query cannot express: a binding is visible from
//! its declaration to the end of its scope and in nested scopes, inner
//! bindings shadow outer ones, and names outside every scope (file,
//! namespace and class level) are definitions, not locals. If the cursor
//! hits its limits the table is partial and the affected names are treated
//! as non-local.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rueca_extract::ExtractError;
use rueca_extract::tree_sitter::{
    Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree,
};
use rueca_model::Language;

use crate::text::{collapse, text};

/// The vendored, adapted C locals query (also the base of the C++ query).
pub(crate) const C_LOCALS_SCM: &str = include_str!("../queries/c-locals.scm");
/// The vendored, adapted C++ additions (compiled after [`C_LOCALS_SCM`]).
pub(crate) const CPP_LOCALS_SCM: &str = include_str!("../queries/cpp-locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    Parameter,
    Var,
    /// A template type parameter.
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scope,
    Def(DefKind),
    Ignore,
}

fn role_of(name: &str) -> Role {
    match name {
        "local.scope" => Role::Scope,
        "local.definition.parameter" => Role::Def(DefKind::Parameter),
        "local.definition.type" => Role::Def(DefKind::Type),
        n if n.starts_with("local.definition") => Role::Def(DefKind::Var),
        _ => Role::Ignore,
    }
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

fn compile(language: &rueca_extract::tree_sitter::Language, source: &str) -> Result<LocalsQuery, String> {
    let query = Query::new(language, source).map_err(|e| format!("locals query: {e}"))?;
    let roles = query.capture_names().iter().map(|n| role_of(n)).collect();
    Ok(LocalsQuery { query, roles })
}

/// The C++ query text: the C base followed by the C++ additions.
pub(crate) fn cpp_query_text() -> String {
    format!("{C_LOCALS_SCM}\n{CPP_LOCALS_SCM}")
}

/// Compiles the locals query for `language` (once per process and grammar).
pub(crate) fn locals_query(language: Language) -> Result<&'static LocalsQuery, ExtractError> {
    static C: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    static CPP: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    let q = if language == Language::C {
        C.get_or_init(|| compile(&tree_sitter_c::LANGUAGE.into(), C_LOCALS_SCM))
    } else {
        CPP.get_or_init(|| compile(&tree_sitter_cpp::LANGUAGE.into(), &cpp_query_text()))
    };
    q.as_ref().map_err(|e| ExtractError::Query(e.clone()))
}

/// One binding of a name in a scope.
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    /// Start byte of the bound identifier.
    pub(crate) pos: u32,
    pub(crate) kind: DefKind,
    /// Declared type as written, without pointers, references and
    /// qualifiers (`Foo`, `ns::Foo<int>`); `None` for fundamental types,
    /// `auto` without a known initializer, and template type parameters.
    pub(crate) ty: Option<String>,
    /// Static type of the binding as an argument, for overload choice
    /// ([`arg_type_text`]): fundamental types included (`int`, `char*`,
    /// `string`); `None` for `auto`.
    pub(crate) arg: Option<String>,
}

/// A written type in the form the resolver compares with overload
/// parameters (`rueca-resolve`, `normalize_param_type`): qualifiers
/// (`const`, `struct`, …), template arguments, namespace qualifiers and
/// references dropped, words joined, `pointers` stars appended:
/// `const std::string&` → `string`, `unsigned int` → `unsignedint`.
pub(crate) fn arg_type_text(written: &str, pointers: usize) -> Option<String> {
    let mut plain = String::new();
    let mut depth = 0i32;
    for c in written.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = plain
        .split(|c: char| c.is_whitespace() || c == '&')
        .filter(|w| {
            !w.is_empty() && !matches!(*w, "const" | "volatile" | "struct" | "class" | "enum" | "union" | "typename")
        })
        .collect();
    let joined = words.concat();
    let stars = joined.chars().rev().take_while(|c| *c == '*').count() + pointers;
    let core = joined.trim_end_matches('*');
    let core = core.rsplit("::").next().unwrap_or(core);
    if core.is_empty() || matches!(core, "auto" | "decltype") || !core.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }
    Some(format!("{core}{}", "*".repeat(stars)))
}

struct ScopeInfo {
    start: u32,
    end: u32,
    parent: Option<usize>,
    bindings: HashMap<String, Vec<Binding>>,
}

/// Per-file local-scope table.
#[derive(Default)]
pub(crate) struct Locals {
    /// Ordered by start, outer scopes first for equal starts.
    scopes: Vec<ScopeInfo>,
    /// True if the cursor hit [`MATCH_LIMIT`] or the deadline.
    #[allow(dead_code)]
    pub(crate) incomplete: bool,
}

impl Locals {
    /// The binding of `name` visible at byte `pos`, if `name` is local there.
    pub(crate) fn lookup(&self, name: &str, pos: u32) -> Option<&Binding> {
        let idx = self.scopes.partition_point(|s| s.start <= pos).checked_sub(1)?;
        let mut cur = Some(idx);
        // Climb to the innermost scope that contains `pos`.
        while let Some(s) = cur {
            if self.scopes[s].end > pos {
                break;
            }
            cur = self.scopes[s].parent;
        }
        while let Some(s) = cur {
            let sc = &self.scopes[s];
            if let Some(bindings) = sc.bindings.get(name)
                && let Some(b) = bindings.iter().rev().find(|b| b.kind == DefKind::Type || b.pos <= pos)
            {
                return Some(b);
            }
            cur = sc.parent;
        }
        None
    }
}

/// Node kinds that wrap a declarator around the bound identifier.
const DECLARATOR_WRAPPERS: &[&str] = &[
    "pointer_declarator",
    "reference_declarator",
    "array_declarator",
    "parenthesized_declarator",
    "attributed_declarator",
    "init_declarator",
    "variadic_declarator",
];

/// The class a type node names, as written (`Foo`, `ns::Foo<int>`, `struct
/// foo` → `foo`), or `None` for fundamental types and `auto`.
pub(crate) fn named_type(ty: Node<'_>, source: &[u8]) -> Option<String> {
    match ty.kind() {
        "type_identifier" | "qualified_identifier" | "template_type" => Some(collapse(&text(ty, source))),
        "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier" => {
            ty.child_by_field_name("name").map(|n| collapse(&text(n, source)))
        }
        "type_descriptor" => ty.child_by_field_name("type").and_then(|t| named_type(t, source)),
        _ => None,
    }
}

/// The type an `auto` binding gets from its initializer: `new Foo(…)`,
/// `Foo{…}`, `std::make_unique<Foo>(…)` / `make_shared<Foo>(…)`.
fn initializer_type(value: Node<'_>, source: &[u8]) -> Option<String> {
    match value.kind() {
        "new_expression" => value.child_by_field_name("type").and_then(|t| named_type(t, source)),
        "compound_literal_expression" => value.child_by_field_name("type").and_then(|t| named_type(t, source)),
        "call_expression" => {
            let f = value.child_by_field_name("function")?;
            let tf = match f.kind() {
                "template_function" => f,
                "qualified_identifier" => {
                    let mut name = f.child_by_field_name("name")?;
                    while name.kind() == "qualified_identifier" {
                        name = name.child_by_field_name("name")?;
                    }
                    name
                }
                _ => return None,
            };
            if tf.kind() != "template_function" {
                return None;
            }
            let fname = text(tf.child_by_field_name("name")?, source);
            if !matches!(fname.as_ref(), "make_unique" | "make_shared") {
                return None;
            }
            let args = tf.child_by_field_name("arguments")?;
            let mut cursor = args.walk();
            let first = args.named_children(&mut cursor).find(|c| c.kind() != "comment")?;
            named_type(first, source)
        }
        _ => None,
    }
}

/// Declared type of the binding whose identifier is `ident` (see
/// [`Binding::ty`]) and its argument type ([`Binding::arg`]).
fn binding_type(ident: Node<'_>, source: &[u8]) -> (Option<String>, Option<String>) {
    let mut init: Option<Node<'_>> = None;
    let mut pointers = 0;
    let Some(mut cur) = ident.parent() else { return (None, None) };
    for _ in 0..16 {
        let kind = cur.kind();
        if DECLARATOR_WRAPPERS.contains(&kind) {
            match kind {
                "init_declarator" => init = cur.child_by_field_name("value"),
                "pointer_declarator" | "array_declarator" => pointers += 1,
                _ => {}
            }
            let Some(p) = cur.parent() else { return (None, None) };
            cur = p;
            continue;
        }
        return match kind {
            "declaration"
            | "parameter_declaration"
            | "optional_parameter_declaration"
            | "variadic_parameter_declaration"
            | "for_range_loop"
            | "condition_clause" => {
                let Some(ty) = cur.child_by_field_name("type") else { return (None, None) };
                if matches!(ty.kind(), "placeholder_type_specifier" | "auto") {
                    let t = init.and_then(|v| initializer_type(v, source));
                    let arg = t.as_deref().and_then(|t| arg_type_text(t, 0));
                    (t, arg)
                } else {
                    (named_type(ty, source), arg_type_text(&text(ty, source), pointers))
                }
            }
            _ => (None, None),
        };
    }
    (None, None)
}

enum Point {
    Def { pos: u32, name: String, binding: Binding },
}

/// Runs the locals query over `tree` and builds the scope table.
pub(crate) fn build(
    tree: &Tree,
    source: &[u8],
    language: Language,
    time_budget: Duration,
) -> Result<Locals, ExtractError> {
    let lq = locals_query(language)?;
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
    {
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches = cursor.matches_with_options(&lq.query, tree.root_node(), source, options);
        let mut seen_scopes = std::collections::HashSet::new();
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                let node = cap.node;
                match lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore) {
                    Role::Scope => {
                        if seen_scopes.insert(node.id()) {
                            scopes.push(ScopeInfo {
                                start: node.start_byte() as u32,
                                end: node.end_byte() as u32,
                                parent: None,
                                bindings: HashMap::new(),
                            });
                        }
                    }
                    Role::Def(kind) => {
                        let name = text(node, source).into_owned();
                        let (ty, arg) = if kind == DefKind::Type { (None, None) } else { binding_type(node, source) };
                        points.push(Point::Def {
                            pos: node.start_byte() as u32,
                            name,
                            binding: Binding { pos: node.start_byte() as u32, kind, ty, arg },
                        });
                    }
                    Role::Ignore => {}
                }
            }
        }
    }
    let incomplete = timed_out || cursor.did_exceed_match_limit();

    scopes.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    scopes.dedup_by(|b, a| a.start == b.start && a.end == b.end);
    points.sort_by_key(|Point::Def { pos, .. }| *pos);

    // Sweep: nest scopes and put each binding in its innermost scope.
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0;
    for point in points {
        let Point::Def { pos, name, binding } = point;
        while next < scopes.len() && scopes[next].start <= pos {
            while stack.last().is_some_and(|&t| scopes[t].end <= scopes[next].start) {
                stack.pop();
            }
            scopes[next].parent = stack.last().copied();
            stack.push(next);
            next += 1;
        }
        while stack.last().is_some_and(|&t| scopes[t].end <= pos) {
            stack.pop();
        }
        // Outside every scope: a file-, namespace- or class-level name.
        let Some(&inner) = stack.last() else { continue };
        scopes[inner].bindings.entry(name).or_default().push(binding);
    }
    while next < scopes.len() {
        while stack.last().is_some_and(|&t| scopes[t].end <= scopes[next].start) {
            stack.pop();
        }
        scopes[next].parent = stack.last().copied();
        stack.push(next);
        next += 1;
    }
    Ok(Locals { scopes, incomplete })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED_CAPTURES: &[&str] = &["local.scope", "local.definition.parameter", "local.definition.var"];

    /// Exercises every pattern of `c-locals.scm`.
    const C_GOLDEN: &str = r#"
int f(int p, struct node *n, int arr[]) {
    int a = 1;
    int b;
    int *ptr = &a;
    int buf[4];
    for (int i = 0; i < 4; i++) { buf[i] = i; }
    if (a) { a = 2; }
    while (b) { b--; }
    do { b++; } while (b < 3);
    switch (a) { case 1: { int c = 0; break; } }
    return p;
}
"#;

    /// Exercises every pattern of `c-locals.scm` + `cpp-locals.scm` with the
    /// C++ grammar.
    const CPP_GOLDEN: &str = r#"
template <typename T, typename U = int, typename... Ts>
void g(T& ref, int opt = 1, Ts... rest) {
    auto lambda = [](int q) { return q; };
    for (auto& x : ref) { x.go(); }
    try { int t = 0; } catch (const std::exception& e) { e.what(); }
    auto [k, v] = pair();
    int i = 0;
    while (i) {}
    if (i) {}
    do {} while (i);
    switch (i) { case 0: break; }
    int *p = nullptr;
    int arr[2];
    int plain;
    for (int j = 0; j < 2; j++) {}
}
template <typename T> concept C = requires(T a) { a.f(); };
"#;

    fn parse(src: &str, language: Language) -> Tree {
        let ts: rueca_extract::tree_sitter::Language =
            if language == Language::C { tree_sitter_c::LANGUAGE.into() } else { tree_sitter_cpp::LANGUAGE.into() };
        let mut parser = rueca_extract::tree_sitter::Parser::new();
        parser.set_language(&ts).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn unmatched(language: Language, golden: &str) -> Vec<String> {
        let lq = locals_query(language).unwrap();
        let tree = parse(golden, language);
        assert!(!tree.root_node().has_error(), "golden corpus must parse cleanly ({language})");
        let mut cursor = QueryCursor::new();
        let mut matched = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), golden.as_bytes());
        while let Some(m) = matches.next() {
            matched[m.pattern_index] = true;
        }
        let source = if language == Language::C { C_LOCALS_SCM.to_string() } else { cpp_query_text() };
        matched
            .iter()
            .enumerate()
            .filter(|(_, m)| !**m)
            .map(|(i, _)| {
                let b = lq.query.start_byte_for_pattern(i);
                format!("pattern {i}: {}", source[b..].lines().next().unwrap_or(""))
            })
            .collect()
    }

    #[test]
    fn locals_queries_compile_against_grammars() {
        for language in [Language::C, Language::Cpp] {
            let lq = locals_query(language).expect("locals query must compile");
            let names = lq.query.capture_names();
            for required in REQUIRED_CAPTURES {
                assert!(names.contains(required), "capture @{required} missing ({language})");
            }
            for (name, role) in names.iter().zip(&lq.roles) {
                assert_ne!(*role, Role::Ignore, "capture @{name} has no role");
            }
        }
        assert!(locals_query(Language::Cpp).unwrap().query.capture_names().contains(&"local.definition.type"));
    }

    #[test]
    fn every_locals_pattern_matches_the_golden_corpus() {
        let c = unmatched(Language::C, C_GOLDEN);
        assert!(c.is_empty(), "C patterns that no longer match:\n{}", c.join("\n"));
        let cpp = unmatched(Language::Cpp, CPP_GOLDEN);
        assert!(cpp.is_empty(), "C++ patterns that no longer match:\n{}", cpp.join("\n"));
    }

    #[test]
    fn query_headers_record_provenance() {
        for q in [C_LOCALS_SCM, CPP_LOCALS_SCM] {
            assert!(q.starts_with("; SPDX-License-Identifier: Apache-2.0"));
            assert!(q.contains("29212d826054bb2948fa9d3a99eca9bf6a050eb2"));
            let code = q.lines().filter(|l| !l.trim_start().starts_with(';'));
            assert!(code.clone().all(|l| !l.contains("#set!")));
            assert!(code.clone().any(|l| l.contains("@local.scope")));
        }
    }

    fn table(src: &str, language: Language) -> (Tree, Locals) {
        let tree = parse(src, language);
        let locals = build(&tree, src.as_bytes(), language, Duration::from_secs(5)).unwrap();
        (tree, locals)
    }

    fn at(src: &str, needle: &str) -> u32 {
        src.find(needle).unwrap_or_else(|| panic!("{needle}")) as u32
    }

    #[test]
    fn bindings_and_types() {
        let src = "struct Foo;\nint g;\nvoid f(Foo* p, const ns::Bar<int>& b, int n) {\n  auto q = new Baz(1);\n  auto u = std::make_unique<Qux>();\n  Foo local(1);\n  p->m(); b.m(); q->m(); u->m(); local.m(); g = n;\n}\n";
        let (_, l) = table(src, Language::Cpp);
        let use_pos = at(src, "p->m");
        let ty = |name: &str| l.lookup(name, use_pos).and_then(|b| b.ty.clone());
        assert_eq!(ty("p").as_deref(), Some("Foo"));
        assert_eq!(ty("b").as_deref(), Some("ns::Bar<int>"));
        assert_eq!(ty("q").as_deref(), Some("Baz"));
        assert_eq!(ty("u").as_deref(), Some("Qux"));
        assert_eq!(ty("local").as_deref(), Some("Foo"));
        assert!(l.lookup("n", use_pos).is_some_and(|b| b.ty.is_none() && b.kind == DefKind::Parameter));
        // File-level variables are not locals.
        assert!(l.lookup("g", use_pos).is_none());
    }

    #[test]
    fn scoping_and_template_parameters() {
        let src = "template <typename T>\nstruct S { void m(T t) { t.go(); } };\nvoid h() {\n  { int inner = 0; }\n  int later = 1;\n  inner; later;\n}\n";
        let (_, l) = table(src, Language::Cpp);
        assert!(l.lookup("T", at(src, "T t")).is_some_and(|b| b.kind == DefKind::Type));
        assert!(l.lookup("t", at(src, "t.go")).is_some());
        assert!(l.lookup("T", at(src, "void h")).is_none());
        assert!(l.lookup("inner", at(src, "inner; later")).is_none(), "out of its block");
        assert!(l.lookup("later", at(src, "inner; later")).is_some());
        assert!(l.lookup("later", at(src, "{ int inner")).is_none(), "used before its declaration");
    }

    #[test]
    fn c_struct_parameters() {
        let src = "int api(struct redisContext *c, redisReader *r) { return c->err + r->err; }\n";
        let (_, l) = table(src, Language::C);
        let pos = at(src, "return");
        assert_eq!(l.lookup("c", pos).and_then(|b| b.ty.clone()).as_deref(), Some("redisContext"));
        assert_eq!(l.lookup("r", pos).and_then(|b| b.ty.clone()).as_deref(), Some("redisReader"));
    }
}
