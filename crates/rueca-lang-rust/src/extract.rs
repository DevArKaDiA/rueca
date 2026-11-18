//! Syntax-tree walk producing [`FileFacts`] for one Rust file: items
//! (definitions) here, expressions and types in `refs.rs`, receivers and
//! type inference in `receivers.rs`, macro arguments in `macros.rs`, `use`
//! and `mod` declarations in `uses.rs`.
//!
//! A recursive visitor (depth-limited by `Budget::max_depth`) keeps the
//! enclosing definition so every reference is attributed to it. Whether an
//! identifier is a local binding comes from the [`Locals`] table built by
//! the vendored `queries/locals.scm` query (see `locals.rs`). See the crate
//! docs for the mapping rules.

use std::collections::{BTreeMap, HashSet};

use rueca_extract::tree_sitter::{self, Node};
use rueca_extract::{Budget, ExtractError, count_errors, line_count, span_of};
use rueca_model::{Definition, FileFacts, Import, Label, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, Binding, DefKind, Locals};
use crate::receivers::FileInfo;
use crate::types::ty_of;

/// Hard cap on references per file.
pub const MAX_REFS: usize = 20_000;

/// Longest signature kept, in chars.
const MAX_ONE_LINE: usize = 200;

/// Longest doc comment kept, in chars.
const MAX_DOC: usize = 1000;

/// The text a visitor reads: the file itself, or a macro argument re-parsed
/// on its own (`delta` maps its byte offsets back to the file's).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cx<'a> {
    pub(crate) src: &'a [u8],
    /// Offset of this text's byte 0 in the file (may be negative: the
    /// wrapper a macro argument is parsed in starts before it).
    pub(crate) delta: i64,
    /// Whether this is a re-parsed macro argument (no definitions are made
    /// from it).
    pub(crate) snippet: bool,
}

impl<'a> Cx<'a> {
    pub(crate) fn main(src: &'a [u8]) -> Cx<'a> {
        Cx { src, delta: 0, snippet: false }
    }

    /// UTF-8 text of a node (empty when not valid UTF-8).
    pub(crate) fn text(self, node: Node<'_>) -> &'a str {
        std::str::from_utf8(&self.src[node.start_byte()..node.end_byte()]).unwrap_or("")
    }

    /// Byte offset of `node`'s start in the file.
    pub(crate) fn pos(self, node: Node<'_>) -> u32 {
        (node.start_byte() as i64 + self.delta).max(0) as u32
    }

    /// Byte offset of `node`'s end in the file.
    pub(crate) fn end(self, node: Node<'_>) -> u32 {
        (node.end_byte() as i64 + self.delta).max(0) as u32
    }
}

/// Named children of a node, comments left out.
pub(crate) fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|c| !is_comment(c.kind())).collect()
}

pub(crate) fn is_comment(kind: &str) -> bool {
    matches!(kind, "line_comment" | "block_comment")
}

/// Line starts of a file, to place byte offsets of re-parsed macro
/// arguments.
pub(crate) struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(src: &[u8]) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(src.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i as u32 + 1));
        LineIndex { starts }
    }

    fn point(&self, byte: u32) -> (u32, u32) {
        let line = self.starts.partition_point(|&s| s <= byte).saturating_sub(1);
        (line as u32, byte - self.starts[line])
    }

    pub(crate) fn span(&self, start: u32, end: u32) -> Span {
        let (sl, sc) = self.point(start);
        let (el, ec) = self.point(end.max(start));
        Span { start_byte: start, end_byte: end.max(start), start_line: sl, start_col: sc, end_line: el, end_col: ec }
    }
}

/// Whitespace-collapsed text, truncated to `max` chars (with `…`).
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, max)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// Where items being walked are declared.
#[derive(Debug, Clone, Default)]
pub(crate) struct Container {
    /// Prefix of their local qualified names (`m.`, `outer.`, empty).
    pub(crate) prefix: String,
    /// Their parent definition.
    pub(crate) parent: Option<LocalDefId>,
    /// Inline modules enclosing them, outermost first.
    pub(crate) mods: Vec<String>,
    /// Test code (`#[cfg(test)]` module, test function).
    pub(crate) test: bool,
}

impl Container {
    fn child(&self, qn: &str, parent: LocalDefId, test: bool) -> Container {
        Container { prefix: format!("{qn}."), parent: Some(parent), mods: self.mods.clone(), test: self.test || test }
    }
}

/// The `impl` block whose items are being walked.
struct ImplCtx {
    /// Self type path as written, `::`-joined (`fmt::Formatter`, `Foo`).
    self_path: String,
    /// Last segment of the self type (`Foo`).
    head: String,
    /// Implemented trait as written (`fmt::Display`), if any.
    trait_path: Option<String>,
}

/// Attributes of an item, as written inside `#[…]`.
struct Attrs(Vec<String>);

impl Attrs {
    /// `#[test]`, `#[tokio::test]`, `#[bench]`, `#[rstest]`, `#[test_case(…)]`.
    fn is_test(&self) -> bool {
        self.0.iter().any(|a| {
            let path = a.split(['(', ' ', '=']).next().unwrap_or("");
            let last = path.rsplit("::").next().unwrap_or(path);
            matches!(last, "test" | "bench" | "rstest" | "test_case" | "quickcheck" | "proptest")
        })
    }

    /// `#[cfg(test)]` (also inside `all(…)` / `any(…)`).
    fn is_cfg_test(&self) -> bool {
        self.0.iter().any(|a| {
            a.strip_prefix("cfg").is_some_and(|rest| {
                rest.trim_start().starts_with('(')
                    && rest.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == "test")
                    && !rest.contains("not(test)")
            })
        })
    }

    /// `#[path = "…"]`.
    fn path(&self) -> Option<String> {
        self.0.iter().find_map(|a| {
            let rest = a.strip_prefix("path")?.trim_start().strip_prefix('=')?.trim();
            Some(rest.trim_matches('"').to_string())
        })
    }
}

/// The attributes and doc comment directly above an item.
fn attrs_and_doc(node: Node<'_>, src: &[u8]) -> (Attrs, Option<String>) {
    let mut attrs = Vec::new();
    let mut docs: Vec<String> = Vec::new();
    let mut cur = node.prev_sibling();
    while let Some(c) = cur {
        match c.kind() {
            "attribute_item" => {
                if let Some(a) = named_children(c).first() {
                    attrs.push(locals::text(*a, src).to_string());
                }
            }
            "line_comment" | "block_comment" => {
                if let Some(d) = doc_text(c, src) {
                    docs.push(d);
                }
            }
            _ => break,
        }
        cur = c.prev_sibling();
    }
    docs.reverse();
    let doc = docs.join("\n").trim().to_string();
    (Attrs(attrs), (!doc.is_empty()).then(|| truncate(&doc, MAX_DOC)))
}

/// The text of an outer doc comment (`///`, `/** */`).
fn doc_text(comment: Node<'_>, src: &[u8]) -> Option<String> {
    comment.child_by_field_name("outer")?;
    let doc = comment.child_by_field_name("doc")?;
    let t = locals::text(doc, src);
    let lines: Vec<&str> =
        t.lines().map(|l| l.trim().trim_start_matches('*').strip_prefix(' ').unwrap_or(l.trim())).collect();
    Some(lines.join("\n").trim_end().to_string())
}

/// Whether an item node has a `pub` visibility.
fn is_pub(node: Node<'_>) -> bool {
    let mut c = node.walk();
    node.children(&mut c).any(|ch| ch.kind() == "visibility_modifier")
}

/// The extraction state of one file.
pub(crate) struct Extractor<'s> {
    pub(crate) src: &'s [u8],
    pub(crate) lines: LineIndex,
    /// The file is test code (`tests/`, `benches/`).
    test_file: bool,
    pub(crate) max_depth: usize,
    pub(crate) budget: Budget,
    pub(crate) locals: Locals,
    pub(crate) file: FileInfo,
    pub(crate) defs: Vec<Definition>,
    pub(crate) refs: Vec<Reference>,
    pub(crate) imports: Vec<Import>,
    /// Synthetic imports of path anchors used in code (`crate`, `super`, a
    /// crate name), by binding name.
    pub(crate) anchors: BTreeMap<String, Import>,
    /// Dedup keys for `Read` / `Write` / `Type`: (kind, enclosing, receiver, name).
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    pub(crate) enclosing: Option<LocalDefId>,
    /// Where items met while visiting are declared.
    pub(crate) container: Container,
    /// Emit type references to standard types too (field types, read by
    /// the resolver's `$member:` receivers).
    pub(crate) std_types: bool,
    /// Macro re-parse nesting and the bytes re-parsed so far.
    pub(crate) macro_depth: usize,
    pub(crate) macro_bytes: usize,
    /// Local scopes of the macro arguments being visited (innermost last),
    /// with the offset of each re-parsed text in the file.
    pub(crate) snippet_locals: Vec<(Locals, i64)>,
    /// Trait impls of the file: (local qualified name of the self type,
    /// trait as a base text), added to the type's `implements` when the
    /// file declares it (an impl without items has no other trace).
    trait_impls: Vec<(String, String)>,
}

pub(crate) fn extract(path: &str, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    // Item-shaped macro bodies opened in place (same length, see `expand.rs`).
    let expanded = crate::expand::expand(source, tree, budget);
    let source: &[u8] = expanded.text.as_deref().unwrap_or(source);
    let tree = expanded.tree;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time)?;
    let mut ex = Extractor {
        src: source,
        lines: LineIndex::new(source),
        test_file: is_test_path(path),
        max_depth: budget.max_depth,
        budget: budget.clone(),
        locals,
        file: FileInfo::default(),
        defs: Vec::new(),
        refs: Vec::new(),
        imports: Vec::new(),
        anchors: BTreeMap::new(),
        seen_refs: HashSet::new(),
        enclosing: None,
        container: Container::default(),
        std_types: false,
        macro_depth: 0,
        macro_bytes: expanded.bytes,
        snippet_locals: Vec::new(),
        trait_impls: Vec::new(),
    };
    ex.prepass(root);
    ex.infer_types(root);
    let cx = Cx::main(source);
    ex.container = Container { test: ex.test_file, ..Default::default() };
    for child in named_children(root) {
        let c = ex.container.clone();
        ex.item(cx, child, &c);
    }
    ex.attach_trait_impls();
    let anchors = std::mem::take(&mut ex.anchors);
    ex.imports.extend(anchors.into_values());
    let contracts = crate::contracts::hints(root, source, &ex.defs, budget.max_depth);
    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
        contracts,
    })
}

/// Integration tests, benches and examples are test code by place:
/// `tests/`, `benches/` (any depth).
fn is_test_path(path: &str) -> bool {
    path.split('/').rev().skip(1).any(|d| matches!(d, "tests" | "benches"))
}

impl<'s> Extractor<'s> {
    // -- facts ---------------------------------------------------------------

    /// The local binding of `name` visible at `node`: in the macro
    /// arguments being visited (innermost first), then in the file.
    pub(crate) fn binding(&self, cx: Cx<'_>, node: Node<'_>, name: &str) -> Option<&Binding> {
        let orig = i64::from(cx.pos(node));
        for (locals, delta) in self.snippet_locals.iter().rev() {
            let pos = orig - delta;
            if let Some(b) = u32::try_from(pos).ok().and_then(|p| locals.lookup(name, p)) {
                return Some(b);
            }
        }
        self.locals.lookup(name, cx.pos(node))
    }

    /// Whether `name` is a generic parameter at `node`.
    pub(crate) fn is_type_param(&self, cx: Cx<'_>, node: Node<'_>, name: &str) -> bool {
        self.binding(cx, node, name).is_some_and(|b| b.kind == DefKind::TypeParameter)
    }

    pub(crate) fn span(&self, cx: Cx<'_>, node: Node<'_>) -> Span {
        if cx.snippet { self.lines.span(cx.pos(node), cx.end(node)) } else { span_of(node) }
    }

    /// A definition at `node` named by `name_node`, declared in `c`, not
    /// yet pushed.
    fn new_def(c: &Container, label: Label, name: &str, qn: String, node: Node<'_>, name_node: Node<'_>) -> Definition {
        Definition {
            label,
            name: name.to_string(),
            qualified_name: qn,
            parent: c.parent,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: None,
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: is_pub(node),
            is_test: c.test,
            is_declaration: false,
        }
    }

    fn push_def(&mut self, d: Definition) -> LocalDefId {
        self.defs.push(d);
        (self.defs.len() - 1) as LocalDefId
    }

    pub(crate) fn push_ref(&mut self, kind: RefKind, name: &str, receiver: Option<String>, span: Span) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        let enclosing = self.enclosing;
        let dedup = match kind {
            RefKind::Read => Some(0u8),
            RefKind::Write => Some(1u8),
            RefKind::Type => Some(2u8),
            _ => None,
        };
        if let Some(k) = dedup
            && !self.seen_refs.insert((k, enclosing, receiver.clone(), name.to_string()))
        {
            return;
        }
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing, span, arity: None });
    }

    /// Runs `f` with `d` as the enclosing definition and `c` as the
    /// container of nested items.
    pub(crate) fn within<R>(&mut self, d: Option<LocalDefId>, c: Container, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved_enc = std::mem::replace(&mut self.enclosing, d);
        let saved_c = std::mem::replace(&mut self.container, c);
        let r = f(self);
        self.enclosing = saved_enc;
        self.container = saved_c;
        r
    }

    /// A base type path as the resolver reads it: normalized at its site
    /// (`super::Foo` → the inline module chain or `super$1.Foo`), `.`-joined.
    fn base_text(&mut self, cx: Cx<'_>, path: &[String], node: Node<'_>) -> String {
        self.normalize_path(cx, path, node).join(".")
    }

    /// Signature of an item: its text up to the body, one line.
    fn signature_until(&self, node: Node<'_>, body: Option<Node<'_>>) -> String {
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let t = std::str::from_utf8(&self.src[node.start_byte()..end]).unwrap_or("");
        one_line(t.trim_end().trim_end_matches(['{', ';']), MAX_ONE_LINE)
    }

    // -- items ---------------------------------------------------------------

    /// An item (or anything else met where items are) declared in `c`.
    pub(crate) fn item(&mut self, cx: Cx<'_>, node: Node<'_>, c: &Container) {
        if cx.snippet {
            // No definitions from re-parsed macro arguments: their parts
            // are visited for references.
            return self.visit_children(cx, node, 1);
        }
        match node.kind() {
            "function_item" | "function_signature_item" => self.function(node, c),
            "struct_item" | "union_item" => self.structure(node, c),
            "enum_item" => self.enumeration(node, c),
            "trait_item" => self.trait_item(node, c),
            "impl_item" => self.impl_block(node, c),
            "type_item" => self.type_alias(node, c),
            "const_item" | "static_item" => self.value_item(node, c),
            "macro_definition" => self.macro_definition(node, c),
            "mod_item" => self.inline_module(node, c),
            "foreign_mod_item" => {
                for child in node.child_by_field_name("body").map(named_children).unwrap_or_default() {
                    self.item(cx, child, c);
                }
            }
            "use_declaration"
            | "extern_crate_declaration"
            | "attribute_item"
            | "inner_attribute_item"
            | "line_comment"
            | "block_comment"
            | "empty_statement" => {}
            _ => self.within(c.parent, c.clone(), |ex| ex.visit(cx, node, 1)),
        }
    }

    fn function(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (attrs, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let mut d = Self::new_def(c, Label::Function, name, qn.clone(), node, name_node);
        d.is_test = c.test || attrs.is_test();
        d.signature = Some(self.signature_until(node, node.child_by_field_name("body")));
        d.doc = doc;
        let test = d.is_test;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        self.callable_parts(node, id, c.child(&qn, id, test));
    }

    /// Generics, parameters, return type and body of a function, with `id`
    /// enclosing and `inner` the container of items in the body.
    fn callable_parts(&mut self, node: Node<'_>, id: LocalDefId, inner: Container) {
        let cx = Cx::main(self.src);
        self.within(Some(id), inner, |ex| {
            for field in ["type_parameters", "parameters", "return_type"] {
                if let Some(p) = node.child_by_field_name(field) {
                    ex.visit_type(cx, p, 1);
                }
            }
            if let Some(w) = named_children(node).into_iter().find(|n| n.kind() == "where_clause") {
                ex.visit_type(cx, w, 1);
            }
            if let Some(body) = node.child_by_field_name("body") {
                ex.visit(cx, body, 1);
            }
        });
    }

    fn structure(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (_, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let body = node.child_by_field_name("body");
        let mut d = Self::new_def(c, Label::Struct, name, qn.clone(), node, name_node);
        // A tuple struct's signature keeps its fields: `struct Price(u64)`.
        let sig_end = body.filter(|b| b.kind() == "field_declaration_list");
        d.signature = Some(self.signature_until(node, sig_end));
        d.doc = doc;
        let exported = d.exported;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        let cx = Cx::main(self.src);
        self.within(Some(id), c.child(&qn, id, false), |ex| {
            ex.generics(cx, node);
            match body {
                Some(b) if b.kind() == "field_declaration_list" => ex.fields(b, &qn, id, exported),
                Some(b) => ex.visit_type(cx, b, 1),
                None => {}
            }
        });
    }

    /// Type parameters and `where` clause of an item.
    fn generics(&mut self, cx: Cx<'_>, node: Node<'_>) {
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            self.visit_type(cx, tp, 1);
        }
        if let Some(w) = named_children(node).into_iter().find(|n| n.kind() == "where_clause") {
            self.visit_type(cx, w, 1);
        }
    }

    /// Named fields of a struct, union or struct-like variant: `Field`
    /// children whose type references are enclosed by the field.
    fn fields(&mut self, list: Node<'_>, owner_qn: &str, owner: LocalDefId, owner_exported: bool) {
        let cx = Cx::main(self.src);
        for field in named_children(list).into_iter().filter(|f| f.kind() == "field_declaration") {
            let (Some(n), Some(ty)) = (field.child_by_field_name("name"), field.child_by_field_name("type")) else {
                continue;
            };
            let fname = locals::text(n, self.src);
            let (_, doc) = attrs_and_doc(field, self.src);
            let mut d = Self::new_def(&self.container, Label::Field, fname, format!("{owner_qn}.{fname}"), field, n);
            d.parent = Some(owner);
            d.exported = owner_exported && is_pub(field);
            d.signature = Some(one_line(locals::text(field, self.src), MAX_ONE_LINE));
            d.doc = doc;
            let fd = self.push_def(d);
            let c = self.container.clone();
            self.within(Some(fd), c, |ex| {
                ex.std_types = true;
                ex.visit_type(cx, ty, 1);
                ex.std_types = false;
            });
        }
    }

    fn enumeration(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (_, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let body = node.child_by_field_name("body");
        let mut d = Self::new_def(c, Label::Enum, name, qn.clone(), node, name_node);
        d.signature = Some(self.signature_until(node, body));
        d.doc = doc;
        let exported = d.exported;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        let cx = Cx::main(self.src);
        self.within(Some(id), c.child(&qn, id, false), |ex| {
            ex.generics(cx, node);
            for v in body.map(named_children).unwrap_or_default().into_iter().filter(|v| v.kind() == "enum_variant") {
                ex.variant(v, &qn, id, exported);
            }
        });
    }

    /// An enum variant: a `Constant` child of the enum.
    fn variant(&mut self, v: Node<'_>, owner_qn: &str, owner: LocalDefId, exported: bool) {
        let Some(n) = v.child_by_field_name("name") else { return };
        let vname = locals::text(n, self.src);
        let (_, doc) = attrs_and_doc(v, self.src);
        let mut d = Self::new_def(&self.container, Label::Constant, vname, format!("{owner_qn}.{vname}"), v, n);
        d.parent = Some(owner);
        d.exported = exported;
        d.signature = Some(one_line(locals::text(v, self.src), MAX_ONE_LINE));
        d.doc = doc;
        let vd = self.push_def(d);
        let cx = Cx::main(self.src);
        let c = self.container.clone();
        self.within(Some(vd), c, |ex| {
            match v.child_by_field_name("body") {
                Some(b) if b.kind() == "field_declaration_list" => {
                    for f in named_children(b) {
                        if let Some(t) = f.child_by_field_name("type") {
                            ex.visit_type(cx, t, 1);
                        }
                    }
                }
                Some(b) => ex.visit_type(cx, b, 1),
                None => {}
            }
            if let Some(value) = v.child_by_field_name("value") {
                ex.visit(cx, value, 1);
            }
        });
    }

    fn trait_item(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (_, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let body = node.child_by_field_name("body");
        let cx = Cx::main(self.src);
        let mut d = Self::new_def(c, Label::Interface, name, qn.clone(), node, name_node);
        d.signature = Some(self.signature_until(node, body));
        d.doc = doc;
        if let Some(bounds) = node.child_by_field_name("bounds") {
            let tys: Vec<_> = named_children(bounds).into_iter().filter_map(|b| ty_of(cx, b).map(|t| (t, b))).collect();
            d.bases =
                self.within(c.parent, c.clone(), |ex| tys.iter().map(|(t, b)| ex.base_text(cx, &t.path, *b)).collect());
        }
        let exported = d.exported;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        let inner = c.child(&qn, id, false);
        self.within(Some(id), inner.clone(), |ex| {
            ex.generics(cx, node);
            if let Some(b) = node.child_by_field_name("bounds") {
                ex.visit_type(cx, b, 1);
            }
        });
        for member in body.map(named_children).unwrap_or_default() {
            self.trait_member(member, &inner, exported);
        }
    }

    /// A method, associated type or constant declared by a trait.
    fn trait_member(&mut self, node: Node<'_>, c: &Container, exported: bool) {
        let label = match node.kind() {
            "function_item" | "function_signature_item" => Label::Method,
            "associated_type" => Label::TypeAlias,
            "const_item" => Label::Constant,
            _ => {
                let cx = Cx::main(self.src);
                return self.within(c.parent, c.clone(), |ex| ex.visit(cx, node, 1));
            }
        };
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (attrs, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let mut d = Self::new_def(c, label, name, qn.clone(), node, name_node);
        d.exported = exported;
        d.is_test = c.test || attrs.is_test();
        d.signature = Some(self.signature_until(node, node.child_by_field_name("body")));
        d.doc = doc;
        let test = d.is_test;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        self.member_parts(node, id, c.child(&qn, id, test));
    }

    /// Signature, body or value of an associated item, with `id` enclosing.
    fn member_parts(&mut self, node: Node<'_>, id: LocalDefId, inner: Container) {
        if matches!(node.kind(), "function_item" | "function_signature_item") {
            return self.callable_parts(node, id, inner);
        }
        let cx = Cx::main(self.src);
        self.within(Some(id), inner, |ex| {
            for field in ["type", "bounds", "value", "default_type"] {
                if let Some(p) = node.child_by_field_name(field) {
                    if field == "value" { ex.visit(cx, p, 1) } else { ex.visit_type(cx, p, 1) }
                }
            }
        });
    }

    /// `impl [Trait for] Type { … }`: its items are `Type.item`, top level
    /// (or children of the enclosing inline module), with the self type as
    /// their base and the trait as their implemented interface.
    fn impl_block(&mut self, node: Node<'_>, c: &Container) {
        let cx = Cx::main(self.src);
        let self_ty = node.child_by_field_name("type").and_then(|t| ty_of(cx, t));
        let negative = {
            let mut cur = node.walk();
            node.children(&mut cur).any(|ch| ch.kind() == "!")
        };
        let trait_ty = node.child_by_field_name("trait").filter(|_| !negative).and_then(|t| ty_of(cx, t));
        self.within(c.parent, c.clone(), |ex| {
            ex.generics(cx, node);
            for field in ["trait", "type"] {
                if let Some(t) = node.child_by_field_name(field) {
                    ex.visit_type(cx, t, 1);
                }
            }
        });
        let body = node.child_by_field_name("body").map(named_children).unwrap_or_default();
        let Some(self_ty) = self_ty.filter(|t| !t.head().is_empty() && t.head() != "[]") else {
            // No named self type (`impl Tr for [T; N]`): items are visited
            // for references only.
            return self.within(c.parent, c.clone(), |ex| body.iter().for_each(|m| ex.visit(cx, *m, 1)));
        };
        let type_node = node.child_by_field_name("type").unwrap_or(node);
        let head = self_ty.head().to_string();
        // `impl<T: Bound> Tr for T`: a blanket impl, marked `<T>`.
        let blanket = self_ty.path.len() == 1 && generic_params(node, self.src).contains(&head);
        let self_path = if blanket {
            // `for T` applies to every type; `for &mut T` / `for Box<T>`
            // forward to `T`'s own impl.
            let bare = type_node.kind() == "type_identifier";
            if bare { format!("<{head}>") } else { format!("<&{head}>") }
        } else {
            self.within(c.parent, c.clone(), |ex| ex.base_text(cx, &self_ty.path, type_node))
        };
        let trait_node = node.child_by_field_name("trait").unwrap_or(node);
        let trait_path = trait_ty.map(|t| self.within(c.parent, c.clone(), |ex| ex.base_text(cx, &t.path, trait_node)));
        if let (Some(t), false) = (&trait_path, blanket) {
            self.trait_impls.push((format!("{}{head}", c.prefix), t.clone()));
        }
        let imp = ImplCtx { self_path, head, trait_path };
        for member in body {
            self.impl_member(member, c, &imp);
        }
    }

    /// Adds the traits a type of the file implements to its `implements`.
    fn attach_trait_impls(&mut self) {
        let impls = std::mem::take(&mut self.trait_impls);
        for (qn, t) in impls {
            let ty = self.defs.iter_mut().find(|d| d.qualified_name == qn && d.label.is_type());
            if let Some(d) = ty.filter(|d| !d.implements.contains(&t)) {
                d.implements.push(t);
            }
        }
    }

    /// A method or constant of an `impl` block (associated types are not
    /// definitions; their types are references).
    fn impl_member(&mut self, node: Node<'_>, c: &Container, imp: &ImplCtx) {
        let cx = Cx::main(self.src);
        let label = match node.kind() {
            "function_item" | "function_signature_item" => Label::Method,
            "const_item" => Label::Constant,
            "type_item" | "associated_type" => {
                // `type Output = T;`: not a definition, `T` is a reference.
                return self.within(c.parent, c.clone(), |ex| {
                    if let Some(t) = node.child_by_field_name("type") {
                        ex.visit_type(cx, t, 1);
                    }
                });
            }
            _ => return self.within(c.parent, c.clone(), |ex| ex.visit(cx, node, 1)),
        };
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (attrs, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{}.{name}", c.prefix, imp.head);
        let mut d = Self::new_def(c, label, name, qn.clone(), node, name_node);
        d.exported = d.exported || imp.trait_path.is_some();
        d.is_test = c.test || attrs.is_test();
        d.signature = Some(self.signature_until(node, node.child_by_field_name("body")));
        d.doc = doc;
        d.bases = vec![imp.self_path.clone()];
        d.implements = imp.trait_path.iter().cloned().collect();
        let test = d.is_test;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        self.member_parts(node, id, c.child(&qn, id, test));
    }

    fn type_alias(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (_, doc) = attrs_and_doc(node, self.src);
        let cx = Cx::main(self.src);
        let qn = format!("{}{name}", c.prefix);
        let target = node.child_by_field_name("type");
        let mut d = Self::new_def(c, Label::TypeAlias, name, qn.clone(), node, name_node);
        d.signature = Some(one_line(locals::text(node, self.src), MAX_ONE_LINE));
        d.doc = doc;
        // `type A = B<…>`: A is B, so B's members are found through A.
        if let Some((t, tn)) = target.and_then(|tn| ty_of(cx, tn).map(|t| (t, tn))).filter(|(t, _)| t.head() != "[]") {
            let base = self.within(c.parent, c.clone(), |ex| ex.base_text(cx, &t.path, tn));
            d.bases.push(base);
        }
        let id = self.push_def(d);
        self.decorate(node, id, c);
        self.within(Some(id), c.child(&qn, id, false), |ex| {
            ex.generics(cx, node);
            if let Some(t) = target {
                ex.visit_type(cx, t, 1);
            }
        });
    }

    /// `const` / `static` items.
    fn value_item(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        if name == "_" {
            // `const _: () = …;`: no definition, its parts are references.
            let cx = Cx::main(self.src);
            return self.within(c.parent, c.clone(), |ex| ex.visit_children(cx, node, 1));
        }
        let (_, doc) = attrs_and_doc(node, self.src);
        let label = if node.kind() == "static_item" { Label::Variable } else { Label::Constant };
        let qn = format!("{}{name}", c.prefix);
        let mut d = Self::new_def(c, label, name, qn.clone(), node, name_node);
        d.signature = Some(self.signature_until(node, node.child_by_field_name("value")));
        d.doc = doc;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        self.member_parts(node, id, c.child(&qn, id, false));
    }

    /// `macro_rules! name { … }`: a callable `Function`, so invocations are
    /// calls (its rules' bodies are not code, see `macros.rs`).
    fn macro_definition(&mut self, node: Node<'_>, c: &Container) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = locals::text(name_node, self.src);
        let (_, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let mut d = Self::new_def(c, Label::Function, name, qn.clone(), node, name_node);
        d.signature = Some(format!("macro_rules! {name}"));
        d.doc = doc;
        d.exported = true;
        self.push_def(d);
    }

    /// `mod name { … }`: a `Module` whose items are `name.item`.
    fn inline_module(&mut self, node: Node<'_>, c: &Container) {
        let (Some(name_node), Some(body)) = (node.child_by_field_name("name"), node.child_by_field_name("body")) else {
            return;
        };
        let name = locals::text(name_node, self.src);
        let (attrs, doc) = attrs_and_doc(node, self.src);
        let qn = format!("{}{name}", c.prefix);
        let test = c.test || attrs.is_cfg_test();
        let mut d = Self::new_def(c, Label::Module, name, qn.clone(), node, name_node);
        d.signature = Some(self.signature_until(node, Some(body)));
        d.doc = doc;
        d.is_test = test;
        let id = self.push_def(d);
        self.decorate(node, id, c);
        let mut inner = c.child(&qn, id, test);
        inner.mods.push(name.to_string());
        let cx = Cx::main(self.src);
        for child in named_children(body) {
            self.item(cx, child, &inner);
        }
    }
}

/// Names of an item's generic parameters.
fn generic_params(node: Node<'_>, src: &[u8]) -> Vec<String> {
    let Some(tp) = node.child_by_field_name("type_parameters") else { return Vec::new() };
    named_children(tp)
        .into_iter()
        .filter_map(|p| p.child_by_field_name("name").map(|n| locals::text(n, src).to_string()))
        .collect()
}

/// `#[path = "…"]` of a `mod` item (`uses.rs`).
pub(crate) fn mod_path_attr(node: Node<'_>, src: &[u8]) -> Option<String> {
    attrs_and_doc(node, src).0.path()
}
