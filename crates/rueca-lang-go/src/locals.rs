//! Local-scope table driven by the vendored `queries/locals.scm`, plus the
//! file-local declared-type inference that feeds `$type:` receivers.
//!
//! The query (adapted from nvim-treesitter and bloop; see the file header and
//! `queries/README.md`) marks scopes (`@local.scope`) and bindings
//! (`@local.definition.<kind>`). This module runs it once per file with a
//! [`QueryCursor`] (match limit and deadline enforced) and applies the Go
//! rules a query cannot express:
//!
//! - A binding belongs to the innermost scope containing it; bindings
//!   directly in `source_file` are package-level names, not locals (the
//!   extractor reports their uses as references).
//! - A local is visible from the end of its declaring statement (`x := x + 1`
//!   reads the outer `x`), a parameter in its whole function, a type
//!   parameter in its whole declaration (functions, generic types, and
//!   methods through the receiver's type arguments).
//! - Parameters of function *types* (`f func(x int)`) bind nothing.
//!
//! Types are inferred from declarations in the same file only: parameter
//! and `var` types, composite literals (`T{}`, `&T{}`), `new(T)`, type
//! assertions (`x.(*T)`), type-switch cases (`case *T:` types the alias in
//! that clause), the result type of a function declared in the file, the
//! constructor convention `NewT()` / `pkg.NewT()` → `T` (heuristic), and
//! copies of typed locals. Type parameters, predeclared types other than
//! `error` and unnamed types (slices, maps, functions) are never receiver
//! types. An untyped local initialized by `q.F(…)` records `q.F` as its
//! origin (see the crate docs, *Receivers*).
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

use crate::builtins::is_predeclared_type;

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
pub(crate) const MATCH_LIMIT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefKind {
    Function,
    Type,
    Parameter,
    TypeParameter,
    Var,
    Const,
    Range,
    TypeSwitch,
    Receive,
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
        Some("function") => Role::Def(DefKind::Function),
        Some("type") => Role::Def(DefKind::Type),
        Some("parameter") => Role::Def(DefKind::Parameter),
        Some("type_parameter") => Role::Def(DefKind::TypeParameter),
        Some("var") => Role::Def(DefKind::Var),
        Some("const") => Role::Def(DefKind::Const),
        Some("range") => Role::Def(DefKind::Range),
        Some("type_switch") => Role::Def(DefKind::TypeSwitch),
        Some("receive") => Role::Def(DefKind::Receive),
        _ => Role::Ignore,
    }
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

/// Compiles `queries/locals.scm` against tree-sitter-go (once per process).
pub(crate) fn locals_query() -> Result<&'static LocalsQuery, ExtractError> {
    static QUERY: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    QUERY
        .get_or_init(|| {
            let language = tree_sitter_go::LANGUAGE.into();
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
    /// The method receiver (`r` in `func (r *T) M()`).
    pub(crate) receiver: bool,
    /// Declared or constructed type, as written (`Server`, `http.Server`).
    pub(crate) ty: Option<String>,
    /// For an untyped local initialized by `q.F(…)` with a non-local `q`
    /// (`r := require.New(t)`): `q.F`, so calls through it can be written
    /// relative to `q` (external when `q` is a third-party package).
    pub(crate) origin: Option<String>,
    /// Start byte of the identifier.
    pos: u32,
    /// For type-switch aliases: start byte of the `type_switch_statement`.
    switch: u32,
}

/// Per-file local-scope table with inferred types.
#[derive(Default)]
pub(crate) struct Locals {
    bindings: Vec<Binding>,
    by_name: HashMap<String, Vec<usize>>,
    /// True if the cursor hit [`MATCH_LIMIT`] or the deadline.
    #[allow(dead_code)]
    pub(crate) incomplete: bool,
}

/// What type inference may know about the file beyond locals.
#[derive(Default)]
pub(crate) struct FileTypes {
    /// Result type of each function declared in the file (first result).
    pub(crate) results: HashMap<String, String>,
    /// Declared type of each package-level `var` of the file.
    pub(crate) vars: HashMap<String, String>,
}

pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|c| c.kind() != "comment").collect()
}

/// Nearest ancestor of one of `kinds`.
fn ancestor<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if kinds.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The named type a type node denotes, as written (`Server`, `pkg.Server`),
/// through pointers, parentheses and generic instantiation. `None` for
/// unnamed types (slices, maps, functions, …), predeclared types and names
/// for which `is_type_param` holds.
pub(crate) fn type_text(node: Node<'_>, source: &[u8], is_type_param: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut node = node;
    // Unwrap pointers, parentheses and instantiations iteratively (input
    // may nest them thousands deep).
    loop {
        let inner = match node.kind() {
            "pointer_type" | "parenthesized_type" | "type_elem" => {
                let inner = named_children(node);
                if inner.len() == 1 { inner[0] } else { return None }
            }
            "generic_type" => node.child_by_field_name("type")?,
            "unary_expression" if node.child_by_field_name("operator").is_some_and(|o| text(o, source) == "*") => {
                node.child_by_field_name("operand")?
            }
            _ => break,
        };
        node = inner;
    }
    match node.kind() {
        "type_identifier" | "identifier" => {
            let t = text(node, source);
            // `error` is kept: it is the one predeclared type with a method,
            // and the resolver treats it as external.
            let named = !t.is_empty() && (t == "error" || !is_predeclared_type(t));
            (named && !is_type_param(t)).then(|| t.to_string())
        }
        "qualified_type" => {
            let pkg = node.child_by_field_name("package")?;
            let name = node.child_by_field_name("name")?;
            Some(format!("{}.{}", text(pkg, source), text(name, source)))
        }
        // `pkg.T` written in expression position (`new(pkg.T)`).
        "selector_expression" => {
            let op = node.child_by_field_name("operand")?;
            let field = node.child_by_field_name("field")?;
            (op.kind() == "identifier").then(|| format!("{}.{}", text(op, source), text(field, source)))
        }
        _ => None,
    }
}

/// Constructor convention: `NewServer` → `Server`.
pub(crate) fn constructed_type(func_name: &str) -> Option<&str> {
    let rest = func_name.strip_prefix("New")?;
    rest.chars().next().is_some_and(char::is_uppercase).then_some(rest)
}

impl Locals {
    /// The local binding of `name` visible at byte `pos`, innermost first.
    pub(crate) fn lookup(&self, name: &str, pos: u32) -> Option<&Binding> {
        let ids = self.by_name.get(name)?;
        let mut best: Option<&Binding> = None;
        for &i in ids {
            let b = &self.bindings[i];
            if b.start > pos || pos >= b.end || b.visible_from > pos {
                continue;
            }
            let better = match best {
                None => true,
                Some(c) => (b.start, b.visible_from) > (c.start, c.visible_from),
            };
            if better {
                best = Some(b);
            }
        }
        best
    }

    /// Whether `ident` (an identifier node) names a local binding.
    pub(crate) fn is_local(&self, ident: Node<'_>, source: &[u8]) -> bool {
        self.lookup(text(ident, source), ident.start_byte() as u32).is_some()
    }

    /// Whether `name` is a type parameter in scope at `pos`.
    pub(crate) fn is_type_param(&self, name: &str, pos: u32) -> bool {
        self.lookup(name, pos).is_some_and(|b| b.kind == DefKind::TypeParameter)
    }

    /// Type of the binding an identifier use refers to, if known. A
    /// type-switch alias takes the type of the clause it is used in, when
    /// that clause lists exactly one type.
    pub(crate) fn type_of_use(&self, b: &Binding, use_node: Node<'_>, source: &[u8]) -> Option<String> {
        if b.kind != DefKind::TypeSwitch {
            return b.ty.clone();
        }
        let mut cur = use_node.parent();
        while let Some(n) = cur {
            if n.kind() == "type_case" && n.parent().is_some_and(|p| p.start_byte() as u32 == b.switch) {
                let types: Vec<Node<'_>> = {
                    let mut c = n.walk();
                    n.children_by_field_name("type", &mut c).filter(|t| t.is_named()).collect()
                };
                if types.len() != 1 {
                    return None;
                }
                let pos = use_node.start_byte() as u32;
                return type_text(types[0], source, &|t| self.is_type_param(t, pos));
            }
            if n.start_byte() as u32 == b.switch {
                return None;
            }
            cur = n.parent();
        }
        None
    }

    /// Type of an expression, from declarations in this file (see the
    /// module docs).
    pub(crate) fn expr_type(&self, expr: Node<'_>, source: &[u8], file: &FileTypes) -> Option<String> {
        let mut expr = expr;
        while expr.kind() == "parenthesized_expression" {
            match named_children(expr).as_slice() {
                [inner] => expr = *inner,
                _ => return None,
            }
        }
        let pos = expr.start_byte() as u32;
        let tp = |t: &str| self.is_type_param(t, pos);
        match expr.kind() {
            "composite_literal" => type_text(expr.child_by_field_name("type")?, source, &tp),
            "unary_expression" => {
                let op = expr.child_by_field_name("operator")?;
                let operand = expr.child_by_field_name("operand")?;
                (text(op, source) == "&" && operand.kind() == "composite_literal")
                    .then(|| type_text(operand.child_by_field_name("type")?, source, &tp))
                    .flatten()
            }
            "type_assertion_expression" => type_text(expr.child_by_field_name("type")?, source, &tp),
            "identifier" => {
                let name = text(expr, source);
                match self.lookup(name, pos) {
                    Some(b) => self.type_of_use(b, expr, source),
                    None => file.vars.get(name).cloned(),
                }
            }
            "call_expression" => {
                let func = expr.child_by_field_name("function")?;
                match func.kind() {
                    "identifier" => {
                        let name = text(func, source);
                        if self.lookup(name, pos).is_some() {
                            return None;
                        }
                        if name == "new" {
                            let args = named_children(expr.child_by_field_name("arguments")?);
                            return if args.len() == 1 { type_text(args[0], source, &tp) } else { None };
                        }
                        if let Some(t) = file.results.get(name) {
                            return Some(t.clone());
                        }
                        constructed_type(name).filter(|t| !tp(t)).map(str::to_string)
                    }
                    "selector_expression" => {
                        let op = func.child_by_field_name("operand")?;
                        let field = func.child_by_field_name("field")?;
                        if op.kind() != "identifier" || self.lookup(text(op, source), pos).is_some() {
                            return None;
                        }
                        let ty = constructed_type(text(field, source))?;
                        Some(format!("{}.{ty}", text(op, source)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Type of the `i`-th name bound by a declaration with `left` names and
/// `right` values (`x, err := NewT()`, `a, b := A{}, B{}`).
fn value_type(
    locals: &Locals,
    values: Option<Node<'_>>,
    index: usize,
    names: usize,
    source: &[u8],
    file: &FileTypes,
) -> Option<String> {
    let values = named_children(values?);
    if values.len() == names {
        return locals.expr_type(values[index], source, file);
    }
    // One multi-valued expression: the first result is the call's type or
    // the asserted type (`v, ok := x.(T)`).
    if values.len() == 1 && index == 0 && matches!(values[0].kind(), "call_expression" | "type_assertion_expression") {
        return locals.expr_type(values[0], source, file);
    }
    None
}

/// Index of `ident` among the identifiers of its `expression_list` /
/// declaration, and their count.
fn position_in(ident: Node<'_>, container: Node<'_>, field: Option<&str>) -> (usize, usize) {
    let mut c = container.walk();
    let idents: Vec<Node<'_>> = match field {
        Some(f) => container.children_by_field_name(f, &mut c).filter(|n| n.kind() == "identifier").collect(),
        None => container.named_children(&mut c).filter(|n| n.kind() == "identifier").collect(),
    };
    let i = idents.iter().position(|n| n.id() == ident.id()).unwrap_or(0);
    (i, idents.len())
}

/// The inferred type of a binding identifier (see the module docs).
fn binding_type(locals: &Locals, ident: Node<'_>, kind: DefKind, source: &[u8], file: &FileTypes) -> Option<String> {
    let parent = ident.parent()?;
    let pos = ident.start_byte() as u32;
    let tp = |t: &str| locals.is_type_param(t, pos);
    match kind {
        DefKind::Parameter if parent.kind() == "parameter_declaration" => {
            type_text(parent.child_by_field_name("type")?, source, &tp)
        }
        DefKind::Var if parent.kind() == "var_spec" => {
            if let Some(ty) = parent.child_by_field_name("type") {
                return type_text(ty, source, &tp);
            }
            let (i, n) = position_in(ident, parent, Some("name"));
            value_type(locals, parent.child_by_field_name("value"), i, n, source, file)
        }
        DefKind::Var => {
            let decl = parent.parent().filter(|d| d.kind() == "short_var_declaration")?;
            let (i, n) = position_in(ident, parent, None);
            value_type(locals, decl.child_by_field_name("right"), i, n, source, file)
        }
        _ => None,
    }
}

/// Byte offset from which a binding is visible, and its scope range.
fn visibility(ident: Node<'_>, kind: DefKind, scope: (u32, u32)) -> (u32, u32, u32) {
    let (start, end) = scope;
    let after = |kinds: &[&str]| ancestor(ident, kinds).map_or(start, |n| n.end_byte() as u32);
    let from = match kind {
        DefKind::Var => after(&["short_var_declaration", "var_spec"]),
        DefKind::Const => after(&["const_spec"]),
        DefKind::Range => after(&["range_clause"]),
        DefKind::Receive => after(&["receive_statement"]),
        DefKind::TypeSwitch => ancestor(ident, &["type_switch_statement"])
            .and_then(|s| s.child_by_field_name("value"))
            .map_or(start, |v| v.end_byte() as u32),
        DefKind::Type => ancestor(ident, &["type_spec", "type_alias"]).map_or(start, |n| n.start_byte() as u32),
        DefKind::Parameter | DefKind::TypeParameter | DefKind::Function => start,
    };
    (start, end, from)
}

/// Runs the locals query over `tree` and builds the table. `time_budget`
/// bounds the query run.
pub(crate) fn build(tree: &Tree, source: &[u8], time_budget: Duration) -> Result<Locals, ExtractError> {
    let lq = locals_query()?;
    let mut scopes: Vec<(u32, u32, bool)> = Vec::new();
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
                let node = cap.node;
                match lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore) {
                    Role::Scope => {
                        if seen.insert(node.id()) {
                            let file = node.kind() == "source_file";
                            scopes.push((node.start_byte() as u32, node.end_byte() as u32, file));
                        }
                    }
                    Role::Def(kind) => defs.push((node, kind)),
                    Role::Ignore => {}
                }
            }
        }
    }
    let incomplete = timed_out || cursor.did_exceed_match_limit();

    // Innermost scope of a position: the containing scope with the latest
    // start (scopes nest properly).
    scopes.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let innermost = |pos: u32| -> Option<(u32, u32, bool)> {
        let upto = scopes.partition_point(|s| s.0 <= pos);
        scopes[..upto].iter().rev().find(|s| pos < s.1).copied()
    };

    let mut locals = Locals { incomplete, ..Default::default() };
    let mut seen_defs = std::collections::HashSet::new();
    for (node, kind) in defs {
        if !seen_defs.insert(node.id()) {
            continue;
        }
        let name = text(node, source);
        // Go function declarations are always package-level.
        if name.is_empty() || name == "_" || kind == DefKind::Function {
            continue;
        }
        let pos = node.start_byte() as u32;
        let mut receiver = false;
        let scope = match kind {
            DefKind::TypeParameter => {
                let decl = ancestor(
                    node,
                    &["function_declaration", "method_declaration", "type_spec", "type_alias", "method_elem"],
                );
                match decl {
                    Some(d) => (d.start_byte() as u32, d.end_byte() as u32),
                    None => continue,
                }
            }
            _ => {
                if kind == DefKind::Parameter {
                    // Parameters of function types and interface methods bind nothing.
                    let list = node.parent().and_then(|p| p.parent());
                    let owner = list.and_then(|l| l.parent());
                    if owner.is_some_and(|o| matches!(o.kind(), "function_type" | "method_elem")) {
                        continue;
                    }
                    if let (Some(l), Some(o)) = (list, owner)
                        && o.kind() == "method_declaration"
                        && o.child_by_field_name("receiver").is_some_and(|r| r.id() == l.id())
                    {
                        receiver = true;
                    }
                }
                match innermost(pos) {
                    Some((s, e, false)) => (s, e),
                    _ => continue,
                }
            }
        };
        let (start, end, visible_from) = visibility(node, kind, scope);
        let switch = if kind == DefKind::TypeSwitch {
            ancestor(node, &["type_switch_statement"]).map_or(0, |s| s.start_byte() as u32)
        } else {
            0
        };
        let id = locals.bindings.len();
        locals.bindings.push(Binding { kind, start, end, visible_from, receiver, ty: None, origin: None, pos, switch });
        locals.by_name.entry(name.to_string()).or_default().push(id);
    }
    Ok(locals)
}

/// Infers binding types in visibility order, so a copy (`y := x`) sees the
/// type of the binding it copies.
pub(crate) fn infer_types(locals: &mut Locals, root: Node<'_>, source: &[u8], file: &FileTypes) {
    let mut order: Vec<usize> = (0..locals.bindings.len()).collect();
    order.sort_by_key(|&i| (locals.bindings[i].visible_from, locals.bindings[i].pos));
    for i in order {
        let b = &locals.bindings[i];
        if !matches!(b.kind, DefKind::Parameter | DefKind::Var) {
            continue;
        }
        let Some(ident) = root.descendant_for_byte_range(b.pos as usize, b.pos as usize + 1) else { continue };
        let ident = if ident.kind() == "identifier" { ident } else { continue };
        let ty = binding_type(locals, ident, b.kind, source, file);
        let origin = if ty.is_none() { binding_origin(locals, ident, source) } else { None };
        locals.bindings[i].ty = ty;
        locals.bindings[i].origin = origin;
    }
}

/// `q.F` for a local whose value is the call `q.F(…)` (or its first result)
/// with a non-local identifier `q`.
fn binding_origin(locals: &Locals, ident: Node<'_>, source: &[u8]) -> Option<String> {
    let parent = ident.parent()?;
    let (values, i, n) = match parent.kind() {
        "var_spec" => {
            let (i, n) = position_in(ident, parent, Some("name"));
            (parent.child_by_field_name("value")?, i, n)
        }
        "expression_list" => {
            let decl = parent.parent().filter(|d| d.kind() == "short_var_declaration")?;
            let (i, n) = position_in(ident, parent, None);
            (decl.child_by_field_name("right")?, i, n)
        }
        _ => return None,
    };
    let values = named_children(values);
    let value = match values.len() {
        len if len == n => values[i],
        1 if i == 0 => values[0],
        _ => return None,
    };
    let func = value.kind().eq("call_expression").then(|| value.child_by_field_name("function")).flatten()?;
    let op = func.kind().eq("selector_expression").then(|| func.child_by_field_name("operand")).flatten()?;
    let field = func.child_by_field_name("field")?;
    if op.kind() != "identifier" || locals.lookup(text(op, source), op.start_byte() as u32).is_some() {
        return None;
    }
    Some(format!("{}.{}", text(op, source), text(field, source)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture names the Rust side relies on.
    const REQUIRED_CAPTURES: &[&str] = &[
        "local.scope",
        "local.definition.function",
        "local.definition.type",
        "local.definition.parameter",
        "local.definition.type_parameter",
        "local.definition.var",
        "local.definition.const",
        "local.definition.range",
        "local.definition.type_switch",
        "local.definition.receive",
    ];

    /// Exercises every pattern of `locals.scm` at least once.
    pub(crate) const GOLDEN: &str = r#"package p

type T struct{ a int }
type A = T

func F[X any](x X, rest ...int) (n int, err error) {
	var v, w T = T{}, T{}
	const c = 1
	y := &T{}
	for i, e := range rest {
		_ = i + e
	}
	if z := y; z != nil {
		switch q := any(z).(type) {
		case *T:
			_ = q
		default:
		}
	}
	switch c {
	case 1:
	}
	ch := make(chan int)
	select {
	case r := <-ch:
		_ = r
	}
	func(k int) {}(1)
	_, _ = v, w
	return
}

func (l *List[E]) Push(e E) {}

func (s Set[K]) Has(k K) bool { return false }
"#;

    fn parse(src: &str) -> Tree {
        let language = tree_sitter_go::LANGUAGE.into();
        let mut parser = rueca_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query().expect("locals.scm must compile against tree-sitter-go");
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

    /// `name@line → type` for every identifier use that is local and typed;
    /// `name@line → -` for untyped locals.
    fn typed(src: &str, file: &FileTypes) -> Vec<String> {
        let tree = parse(src);
        let mut locals = build(&tree, src.as_bytes(), Duration::from_secs(5)).unwrap();
        infer_types(&mut locals, tree.root_node(), src.as_bytes(), file);
        let mut out = Vec::new();
        let mut cursor = tree.walk();
        loop {
            let n = cursor.node();
            if n.kind() == "identifier"
                && let Some(b) = locals.lookup(text(n, src.as_bytes()), n.start_byte() as u32)
            {
                let line = n.start_position().row + 1;
                let ty = if b.receiver {
                    "$self".to_string()
                } else {
                    locals.type_of_use(b, n, src.as_bytes()).unwrap_or("-".into())
                };
                out.push(format!("{}@{line}={ty}", text(n, src.as_bytes())));
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

    #[test]
    fn declared_types() {
        let src = "package p\n\nfunc (s *Server) Run(c *gin.Context, n int, f func(x int)) {\n\tx := &Client{}\n\tvar y pkg.Thing\n\tz, err := NewPool()\n\tw := new(Buf)\n\tv := x\n\tq := mk()\n\tt := c.(*Other)\n\t_, _, _, _, _, _, _, _ = s, c, n, f, y, z, err, w\n\t_, _, _ = v, q, t\n}\n";
        let file = FileTypes { results: [("mk".to_string(), "Made".to_string())].into(), ..Default::default() };
        let got = typed(src, &file);
        for want in [
            "s@11=$self",
            "c@11=gin.Context",
            "n@11=-",
            "f@11=-",
            "y@11=pkg.Thing",
            "z@11=Pool",
            "err@11=-",
            "w@11=Buf",
            "v@12=Client",
            "q@12=Made",
            "t@12=Other",
        ] {
            assert!(got.iter().any(|g| g == want), "missing {want}; got {got:#?}");
        }
        // The parameter of the function type `f func(x int)` binds nothing.
        assert!(!got.iter().any(|g| g.starts_with("x@3")), "{got:#?}");
    }

    #[test]
    fn scopes_shadowing_and_visibility() {
        let src = "package p\n\nfunc f() {\n\tx := A{}\n\tif true {\n\t\tx := B{}\n\t\t_ = x\n\t}\n\t_ = x\n\ty := y\n\tswitch v := any(x).(type) {\n\tcase *C:\n\t\t_ = v\n\tcase D, E:\n\t\t_ = v\n\t}\n}\n\nfunc g[T Stringer](t T) { _ = t }\n";
        let got = typed(src, &FileTypes::default());
        for want in ["x@7=B", "x@9=A", "v@13=C", "v@15=-", "t@19=-"] {
            assert!(got.iter().any(|g| g == want), "missing {want}; got {got:#?}");
        }
        // `y := y` reads a non-local `y` (and the new `y` is not used).
        assert!(!got.iter().any(|g| g.starts_with("y@10")), "{got:#?}");
    }

    #[test]
    fn package_level_names_are_not_local() {
        let src = "package p\n\nvar g = 1\nconst k = 2\ntype T int\nfunc h() { _ = g + k; var _ T }\n";
        let got = typed(src, &FileTypes::default());
        assert!(got.is_empty(), "{got:#?}");
    }

    #[test]
    fn match_limit_is_set_and_table_is_complete_for_normal_files() {
        let tree = parse(GOLDEN);
        let locals = build(&tree, GOLDEN.as_bytes(), Duration::from_secs(5)).unwrap();
        assert!(!locals.incomplete);
    }
}
