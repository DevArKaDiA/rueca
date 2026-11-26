//! Definitions, imports and the walk over a Swift file (expressions are in
//! `refs.rs`). See the crate docs for the facts produced.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_extract::{Budget, ExtractError, count_errors, line_count, span_of};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, Locals};
use crate::types::{
    child_of_kind, class_of_type, declared_type, keyword_child, named_children, selector, strip_type_text, text,
};

/// Maximum references per file.
pub const MAX_REFS: usize = 200_000;
const MAX_SIGNATURE: usize = 200;
const MAX_DOC: usize = 1000;

/// Base classes that make a class an XCTest / Quick test case.
const TEST_BASES: &[&str] = &["AsyncSpec", "QuickSpec", "XCTestCase"];

/// Where definitions are being declared: the file, a type body, an
/// extension body or a function body.
#[derive(Debug, Clone)]
pub(crate) struct Container {
    /// Parent definition of new definitions (`None` at file level and in
    /// extensions: extension members are top-level `T.m`).
    pub(crate) parent: Option<LocalDefId>,
    /// Qualified-name prefix of new definitions (`""` at file level).
    pub(crate) prefix: String,
    /// The type whose members are declared here (a type or the type an
    /// extension extends), for `self` and property receivers.
    pub(crate) type_name: Option<String>,
}

pub(crate) struct Extractor<'s> {
    pub(crate) source: &'s [u8],
    /// The file's syntax tree (initializers are typed on demand).
    pub(crate) root: Option<Node<'s>>,
    pub(crate) max_depth: usize,
    pub(crate) defs: Vec<Definition>,
    pub(crate) refs: Vec<Reference>,
    pub(crate) imports: Vec<Import>,
    /// Dedup keys for `Type` references: (enclosing, receiver, name).
    seen_types: HashSet<(Option<LocalDefId>, Option<String>, String)>,
    /// Innermost enclosing definition.
    pub(crate) enc: Option<LocalDefId>,
    pub(crate) containers: Vec<Container>,
    pub(crate) locals: Locals,
    /// Generic type parameters in scope.
    pub(crate) type_params: Vec<String>,
    /// The first constraint of each parameter in `type_params` (`T: Shape`
    /// → `Shape`): the type a value of that parameter is used as.
    pub(crate) type_bounds: Vec<Option<String>>,
    /// Properties of the file's types (and of the file itself, key `""`):
    /// type name → property → type held.
    pub(crate) props: HashMap<String, HashMap<String, Option<String>>>,
    /// Functions of the file by (type name or `""`, name) → declared return
    /// type when every overload agrees.
    pub(crate) returns: HashMap<(String, String), Option<String>>,
    /// Inside a test definition (local functions inherit it).
    in_test: bool,
    /// Parameter types of each callable (`String,[Node]`), for overloads
    /// that share a selector.
    param_types: HashMap<LocalDefId, String>,
    /// Declared return type of the enclosing callables and computed
    /// properties (`Self` as the enclosing type), innermost last.
    pub(crate) return_types: Vec<Option<String>>,
    /// Nesting of receivers typed through initializers.
    pub(crate) recv_depth: std::cell::Cell<usize>,
}

pub(crate) fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language = locals::language();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time)?;
    let mut ex = Extractor::new(source, budget.max_depth, locals);
    ex.root = Some(root);
    ex.prescan(root, "", 0);
    ex.visit_children(root, 0);
    ex.disambiguate_selectors();
    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
        contracts: Vec::new(),
    })
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max { s } else { s.chars().take(max).collect() }
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "comment" | "multiline_comment")
}

/// Doc comment text: `///` lines or a `/** … */` block, delimiters removed.
fn clean_doc(lines: &[&str]) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in lines {
        let t = raw.trim();
        if let Some(r) = t.strip_prefix("///") {
            out.push(r.trim().to_string());
        } else {
            let inner = t.strip_prefix("/**").and_then(|r| r.strip_suffix("*/"))?;
            out.extend(inner.lines().map(|l| l.trim().trim_start_matches('*').trim().to_string()));
        }
    }
    let s = out.join("\n").trim().to_string();
    (!s.is_empty()).then(|| truncate_chars(s, MAX_DOC))
}

/// The base name of a callable (its selector suffix removed).
fn base_name(qn: &str) -> &str {
    match qn.find('(') {
        Some(i) => &qn[..i],
        None => qn,
    }
}

impl<'s> Extractor<'s> {
    fn new(source: &'s [u8], max_depth: usize, locals: Locals) -> Self {
        Extractor {
            source,
            root: None,
            max_depth,
            defs: Vec::new(),
            refs: Vec::new(),
            imports: Vec::new(),
            seen_types: HashSet::new(),
            enc: None,
            containers: vec![Container { parent: None, prefix: String::new(), type_name: None }],
            locals,
            type_params: Vec::new(),
            type_bounds: Vec::new(),
            props: HashMap::new(),
            returns: HashMap::new(),
            in_test: false,
            param_types: HashMap::new(),
            return_types: Vec::new(),
            recv_depth: std::cell::Cell::new(0),
        }
    }

    pub(crate) fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.source)
    }

    pub(crate) fn container(&self) -> &Container {
        self.containers.last().expect("file container")
    }

    // -- prescan ---------------------------------------------------------------

    /// Properties and function return types of the file's types, extensions
    /// and file level (for receivers declared later in the file or in
    /// another type body of the file).
    fn prescan(&mut self, node: Node<'_>, owner: &str, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for c in named_children(node) {
            match c.kind() {
                "class_declaration" | "protocol_declaration" => self.prescan_type(c, owner, depth),
                "property_declaration" | "protocol_property_declaration" => self.prescan_property(c, owner),
                "function_declaration" | "protocol_function_declaration" => self.prescan_function(c, owner),
                _ => {}
            }
        }
    }

    fn prescan_type(&mut self, node: Node<'_>, owner: &str, depth: usize) {
        let Some(name) = self.type_decl_name(node) else { return };
        let is_ext = self.decl_kind(node) == "extension";
        let qn = if is_ext || owner.is_empty() { name } else { format!("{owner}.{name}") };
        if let Some(body) = node.child_by_field_name("body") {
            self.prescan(body, &qn, depth + 1);
        }
    }

    fn prescan_property(&mut self, node: Node<'_>, owner: &str) {
        for (id, class) in self.property_bindings(node) {
            let name = self.text(id).trim_matches('`').to_string();
            self.props.entry(owner.to_string()).or_default().insert(name, class);
        }
    }

    fn prescan_function(&mut self, node: Node<'_>, owner: &str) {
        let Some(name) = node.child_by_field_name("name").filter(|n| n.kind() == "simple_identifier") else {
            return;
        };
        let ret = node.child_by_field_name("return_type").and_then(|t| class_of_type(t, self.source));
        let key = (owner.to_string(), self.text(name).trim_matches('`').to_string());
        match self.returns.get(&key) {
            Some(prev) if *prev != ret => {
                self.returns.insert(key, None);
            }
            Some(_) => {}
            None => {
                self.returns.insert(key, ret);
            }
        }
    }

    /// The identifiers a property declaration binds with the type each
    /// holds (annotation, else a constructed / literal initializer).
    pub(crate) fn property_bindings<'t>(&self, node: Node<'t>) -> Vec<(Node<'t>, Option<String>)> {
        let kids = named_children(node);
        let mut out = Vec::new();
        for (i, c) in kids.iter().enumerate() {
            if c.kind() != "pattern" {
                continue;
            }
            let Some(id) = c.child_by_field_name("bound_identifier") else { continue };
            let mut class = None;
            for n in &kids[i + 1..] {
                match n.kind() {
                    "pattern" => break,
                    "type_annotation" => class = declared_type(*n).and_then(|t| class_of_type(t, self.source)),
                    k if class.is_none()
                        && !is_comment(k)
                        && k != "computed_property"
                        && k != "willset_didset_block" =>
                    {
                        class = locals::initializer(*n, self.source).0;
                    }
                    _ => {}
                }
            }
            out.push((id, class));
        }
        out
    }

    // -- walk --------------------------------------------------------------------

    pub(crate) fn visit_children(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for c in named_children(node) {
            self.visit(c, depth + 1);
        }
    }

    pub(crate) fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            "import_declaration" => self.import(node),
            "class_declaration" => self.type_decl(node, depth),
            "protocol_declaration" => self.protocol_decl(node, depth),
            "function_declaration" | "protocol_function_declaration" => self.function(node, depth),
            "init_declaration" => self.special_callable(node, "init", depth),
            "deinit_declaration" => self.special_callable(node, "deinit", depth),
            "subscript_declaration" => self.special_callable(node, "subscript", depth),
            "property_declaration" | "protocol_property_declaration" => self.property(node, depth),
            "typealias_declaration" | "associatedtype_declaration" => self.typealias(node, depth),
            "enum_entry" => self.enum_entry(node, depth),
            _ => self.visit_expression(node, depth),
        }
    }

    // -- imports -------------------------------------------------------------------

    /// `import Foo` / `@testable import Foo` → module `Foo`, names `*`;
    /// `import struct Foo.Bar` → module `Foo`, name `Bar`.
    fn import(&mut self, node: Node<'_>) {
        let Some(id) = child_of_kind(node, "identifier") else { return };
        let path: String = self.text(id).split_whitespace().collect();
        let kinded = ["struct", "class", "enum", "protocol", "typealias", "func", "var", "let", "actor"]
            .iter()
            .any(|k| keyword_child(node, k).is_some());
        let (module, name) = match path.split_once('.') {
            Some((m, rest)) if kinded => (m.to_string(), rest.to_string()),
            Some((m, _)) => (m.to_string(), "*".to_string()),
            None => (path.clone(), "*".to_string()),
        };
        self.imports.push(Import {
            module,
            names: vec![ImportedName { name, alias: None }],
            module_alias: None,
            relative_level: 0,
            type_only: false,
            reexport: false,
            span: span_of(node),
        });
    }

    // -- shared helpers ------------------------------------------------------------

    /// The qualified name of a new definition `name` in the current container.
    fn child_qn(&self, name: &str) -> String {
        let prefix = &self.container().prefix;
        if prefix.is_empty() { name.to_string() } else { format!("{prefix}.{name}") }
    }

    /// The doc comment right before `node` (only whitespace between).
    fn doc(&self, node: Node<'_>) -> Option<String> {
        let mut lines: Vec<&str> = Vec::new();
        let mut cur = node;
        let mut end = node.start_byte();
        while let Some(prev) = cur.prev_named_sibling() {
            if !is_comment(prev.kind()) || !self.source[prev.end_byte()..end].iter().all(u8::is_ascii_whitespace) {
                break;
            }
            lines.push(self.text(prev));
            end = prev.start_byte();
            cur = prev;
        }
        lines.reverse();
        clean_doc(&lines)
    }

    /// Attribute nodes of a declaration (`@Test("x")`, `@MainActor`).
    fn attributes<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        for c in named_children(node) {
            match c.kind() {
                "attribute" => out.push(c),
                "modifiers" => out.extend(named_children(c).into_iter().filter(|a| a.kind() == "attribute")),
                _ => {}
            }
        }
        out
    }

    /// Attribute texts without `@`, and their bare names (`Test`).
    fn decorators(&self, node: Node<'_>) -> (Vec<String>, Vec<String>) {
        let attrs = self.attributes(node);
        let texts = attrs.iter().map(|a| collapse(self.text(*a).trim_start_matches('@'))).collect();
        let names = attrs
            .iter()
            .filter_map(|a| child_of_kind(*a, "user_type"))
            .map(|t| strip_type_text(self.text(t)))
            .collect();
        (texts, names)
    }

    /// Whether the declaration is `private` / `fileprivate`.
    fn is_private(&self, node: Node<'_>) -> bool {
        child_of_kind(node, "modifiers").is_some_and(|m| {
            named_children(m).into_iter().any(|c| {
                c.kind() == "visibility_modifier"
                    && matches!(self.text(c).split('(').next(), Some("private" | "fileprivate"))
            })
        })
    }

    /// Declaration header up to `end` (body start), attributes and comments
    /// removed, whitespace collapsed.
    fn signature(&self, node: Node<'_>, end: usize) -> String {
        let mut skip: Vec<(usize, usize)> =
            self.attributes(node).iter().map(|a| (a.start_byte(), a.end_byte())).collect();
        for c in named_children(node).into_iter().filter(|c| is_comment(c.kind())) {
            skip.push((c.start_byte(), c.end_byte()));
        }
        skip.sort_unstable();
        let mut out = String::new();
        let mut pos = node.start_byte();
        for (s, e) in skip {
            if s > pos && s < end {
                out.push_str(&String::from_utf8_lossy(&self.source[pos..s]));
            }
            pos = pos.max(e);
        }
        if pos < end {
            out.push_str(&String::from_utf8_lossy(&self.source[pos..end]));
        }
        truncate_chars(collapse(out.trim_end_matches(['{', ' ', '\n', '\t', '\r'])), MAX_SIGNATURE)
    }

    /// Pushes a definition; returns its id.
    pub(crate) fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.defs.push(def);
        (self.defs.len() - 1) as LocalDefId
    }

    /// A definition of `node` named `name` (name node `name_node`) in the
    /// current container.
    fn new_def(
        &self,
        node: Node<'_>,
        label: Label,
        name: &str,
        qn: String,
        name_node: Node<'_>,
        body: Option<Node<'_>>,
    ) -> Definition {
        let (decorators, _) = self.decorators(node);
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        Definition {
            label,
            name: name.to_string(),
            qualified_name: qn,
            parent: self.container().parent,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.signature(node, end)),
            doc: self.doc(node),
            decorators,
            bases: Vec::new(),
            implements: Vec::new(),
            exported: !self.is_private(node),
            is_test: self.in_test,
            is_declaration: false,
        }
    }

    // -- references ----------------------------------------------------------------

    pub(crate) fn push_ref(
        &mut self,
        kind: RefKind,
        name: &str,
        receiver: Option<String>,
        span: Span,
        arity: Option<u32>,
    ) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        if kind == RefKind::Type && !self.seen_types.insert((self.enc, receiver.clone(), name.to_string())) {
            return;
        }
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing: self.enc, span, arity });
    }

    // -- types ---------------------------------------------------------------------

    fn decl_kind(&self, node: Node<'_>) -> &'s str {
        node.child_by_field_name("declaration_kind").map_or("class", |k| self.text(k))
    }

    /// The name a type declaration declares, or the type an extension
    /// extends (`Point`, `Outer.Inner`, `Array`), generic arguments removed.
    fn type_decl_name(&self, node: Node<'_>) -> Option<String> {
        let n = node.child_by_field_name("name")?;
        let s = strip_type_text(self.text(n));
        (!s.is_empty()).then_some(s)
    }

    /// Supertype texts of a declaration (`: Base, Proto`), generic
    /// arguments removed.
    fn supertypes<'t>(&self, node: Node<'t>) -> Vec<(String, Node<'t>)> {
        let mut out = Vec::new();
        for spec in named_children(node).into_iter().filter(|c| c.kind() == "inheritance_specifier") {
            if let Some(t) = named_children(spec).into_iter().next() {
                let s = strip_type_text(text(t, self.source));
                if !s.is_empty() {
                    out.push((s, t));
                }
            }
        }
        out
    }

    fn is_test_type(&self, name: &str, decorators: &[String], supers: &[String], body: Option<Node<'_>>) -> bool {
        if self.in_test || decorators.iter().any(|d| d == "Suite") {
            return true;
        }
        if supers.iter().any(|s| TEST_BASES.contains(&s.rsplit('.').next().unwrap_or(s))) {
            return true;
        }
        name.ends_with("Tests") && body.is_some_and(|b| self.has_test_function(b))
    }

    /// Whether a type body declares a `@Test` function.
    fn has_test_function(&self, body: Node<'_>) -> bool {
        named_children(body)
            .into_iter()
            .any(|c| c.kind() == "function_declaration" && self.decorators(c).1.iter().any(|d| d == "Test"))
    }

    /// `class` / `struct` / `enum` / `actor` / `extension` declarations.
    fn type_decl(&mut self, node: Node<'_>, depth: usize) {
        let Some(name) = self.type_decl_name(node) else { return self.visit_children(node, depth) };
        let kind = self.decl_kind(node);
        let supers = self.supertypes(node);
        let super_names: Vec<String> = supers.iter().map(|(s, _)| s.clone()).collect();
        for (_, t) in &supers {
            self.type_ref(*t);
        }
        if kind == "extension" {
            return self.extension(node, name, depth);
        }
        let label = match kind {
            "struct" => Label::Struct,
            "enum" => Label::Enum,
            _ => Label::Class,
        };
        let body = node.child_by_field_name("body");
        let (decorators, deco_names) = self.decorators(node);
        let test = self.is_test_type(&name, &deco_names, &super_names, body);
        let qn = self.child_qn(&name);
        let name_node = node.child_by_field_name("name").unwrap_or(node);
        let mut def = self.new_def(node, label, &name, qn.clone(), name_node, body);
        def.decorators = decorators;
        def.is_test = test;
        if label == Label::Class {
            def.bases = super_names;
        } else {
            def.implements = super_names;
        }
        let id = self.push_def(def);
        self.enter_type(node, id, qn, test, depth);
    }

    /// Visits a type's generic parameters and body with the type as container.
    fn enter_type(&mut self, node: Node<'_>, id: LocalDefId, qn: String, test: bool, depth: usize) {
        let n_params = self.push_generic_params(node);
        let saved = (self.enc, self.in_test);
        self.enc = Some(id);
        self.in_test = test;
        self.containers.push(Container { parent: Some(id), prefix: qn.clone(), type_name: Some(qn) });
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_children(body, depth);
        }
        self.containers.pop();
        (self.enc, self.in_test) = saved;
        self.pop_generic_params(n_params);
    }

    /// `extension T { … }`: members are top-level definitions `T.m`.
    fn extension(&mut self, node: Node<'_>, target: String, depth: usize) {
        let (_, deco_names) = self.decorators(node);
        let body = node.child_by_field_name("body");
        let supers: Vec<String> = self.supertypes(node).into_iter().map(|(s, _)| s).collect();
        let test = self.is_test_type(&target, &deco_names, &supers, body);
        let saved = (self.enc, self.in_test);
        self.in_test = test;
        self.containers.push(Container { parent: None, prefix: target.clone(), type_name: Some(target) });
        if let Some(body) = body {
            self.visit_children(body, depth);
        }
        self.containers.pop();
        (self.enc, self.in_test) = saved;
    }

    /// Pushes the generic parameters of a declaration (`<T: P, U>`) with
    /// their first constraint; returns how many.
    fn push_generic_params(&mut self, node: Node<'_>) -> usize {
        let Some(tp) = child_of_kind(node, "type_parameters") else { return 0 };
        let mut n = 0;
        for c in named_children(tp).into_iter().filter(|c| c.kind() == "type_parameter") {
            let Some(id) = child_of_kind(c, "type_identifier") else { continue };
            let bound = named_children(c)
                .into_iter()
                .find(|b| b.kind() == "user_type")
                .and_then(|b| class_of_type(b, self.source));
            self.type_params.push(self.text(id).to_string());
            self.type_bounds.push(bound);
            n += 1;
        }
        n
    }

    fn pop_generic_params(&mut self, n: usize) {
        self.type_params.truncate(self.type_params.len() - n);
        self.type_bounds.truncate(self.type_bounds.len() - n);
    }

    /// `protocol P: Q { … }`: an `Interface`; requirements are its members.
    fn protocol_decl(&mut self, node: Node<'_>, depth: usize) {
        let Some(name) = self.type_decl_name(node) else { return };
        let supers = self.supertypes(node);
        for (_, t) in &supers {
            self.type_ref(*t);
        }
        let body = node.child_by_field_name("body");
        let qn = self.child_qn(&name);
        let name_node = node.child_by_field_name("name").unwrap_or(node);
        let mut def = self.new_def(node, Label::Interface, &name, qn.clone(), name_node, body);
        def.bases = supers.into_iter().map(|(s, _)| s).collect();
        let id = self.push_def(def);
        let test = self.in_test;
        self.enter_type(node, id, qn, test, depth);
    }

    // -- callables -------------------------------------------------------------------

    /// The label of a function in the current container: `Method` in a type
    /// or extension, `Function` at file level or inside a function.
    fn function_label(&self) -> Label {
        if self.container().type_name.is_some() { Label::Method } else { Label::Function }
    }

    fn function(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return self.visit_children(node, depth) };
        let name = self.text(name_node).trim_matches('`').to_string();
        let (_, deco_names) = self.decorators(node);
        let label = self.function_label();
        let test = self.in_test && label == Label::Method || deco_names.iter().any(|d| d == "Test");
        let saved = self.in_test;
        self.in_test = self.in_test || test;
        self.callable(node, label, &name, name_node, depth);
        self.in_test = saved;
    }

    /// `init`, `deinit` and `subscript`: methods named by their keyword.
    fn special_callable(&mut self, node: Node<'_>, name: &str, depth: usize) {
        let name_node = keyword_child(node, name).unwrap_or(node);
        self.callable(node, Label::Method, name, name_node, depth);
    }

    /// A function, method, `init`, `deinit` or `subscript`: the definition
    /// (named with its selector, `draw(at:_:)`), parameter types, return
    /// type, then the body with the callable as container.
    fn callable(&mut self, node: Node<'_>, label: Label, name: &str, name_node: Node<'_>, depth: usize) {
        // Subscript and operator parameters are unlabeled unless they name one.
        let unlabeled = name == "subscript" || !name.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        let sel = if name == "deinit" { String::new() } else { selector(node, self.source, unlabeled) };
        let qn = self.child_qn(&format!("{name}{sel}"));
        let body = child_of_kind(node, "function_body").or_else(|| child_of_kind(node, "computed_property"));
        let def = self.new_def(node, label, name, qn.clone(), name_node, body);
        let id = self.push_def(def);
        self.param_types.insert(id, self.parameter_types(node));
        let n_params = self.push_generic_params(node);
        let saved = self.enc;
        self.enc = Some(id);
        self.signature_types(node);
        let ret = node.child_by_field_name("return_type").and_then(|t| class_of_type(t, self.source));
        self.return_types.push(self.resolve_self_type(ret));
        let prefix = base_name(&qn).to_string();
        self.containers.push(Container { parent: Some(id), prefix, type_name: None });
        for c in named_children(node) {
            if !matches!(c.kind(), "parameter" | "type_parameters" | "modifiers" | "attribute") {
                self.visit(c, depth + 1);
            }
        }
        self.containers.pop();
        self.enc = saved;
        self.return_types.pop();
        self.pop_generic_params(n_params);
    }

    /// `Self` as the type of the enclosing container.
    fn resolve_self_type(&self, ty: Option<String>) -> Option<String> {
        match ty.as_deref() {
            Some("Self") => self.containers.iter().rev().find_map(|c| c.type_name.clone()),
            _ => ty,
        }
    }

    /// The parameter types of a callable as written, whitespace removed,
    /// joined by `,` (`String,[Node<Context>]`).
    fn parameter_types(&self, node: Node<'_>) -> String {
        named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "parameter")
            .map(|p| declared_type(p).map(|t| self.text(t).split_whitespace().collect::<String>()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Callables of one scope that share a selector (overloads by type or
    /// by generic constraint, `element(_:nodes:)` twice) get their
    /// parameter types after a `|` in the selector
    /// (`element(_:nodes:|String,[Node])`), so their qualified names
    /// differ; a group whose types also agree keeps its names.
    fn disambiguate_selectors(&mut self) {
        let mut groups: HashMap<(Label, String), Vec<usize>> = HashMap::new();
        for (i, d) in self.defs.iter().enumerate() {
            if d.label.is_callable() && d.qualified_name.ends_with(')') {
                groups.entry((d.label, d.qualified_name.clone())).or_default().push(i);
            }
        }
        for members in groups.into_values().filter(|m| m.len() > 1) {
            let types: Vec<String> = members
                .iter()
                .map(|&i| self.param_types.get(&(i as LocalDefId)).cloned().unwrap_or_default())
                .collect();
            if types.iter().all(|t| *t == types[0]) {
                continue;
            }
            for (&i, t) in members.iter().zip(types) {
                let qn = &mut self.defs[i].qualified_name;
                qn.pop();
                qn.push('|');
                qn.push_str(&t);
                qn.push(')');
            }
        }
    }

    /// `Type` references of a callable's parameter and return types, and the
    /// expressions of default values.
    fn signature_types(&mut self, node: Node<'_>) {
        for p in named_children(node).into_iter().filter(|c| c.kind() == "parameter") {
            if let Some(t) = declared_type(p) {
                self.type_ref(t);
            }
        }
        if let Some(t) = node.child_by_field_name("return_type") {
            self.type_ref(t);
        }
    }

    // -- properties ------------------------------------------------------------------

    /// A property of a type or of the file: one definition per bound name;
    /// a local `let` / `var`: only its expressions. Accessor bodies and
    /// initializers belong to the property.
    fn property(&mut self, node: Node<'_>, depth: usize) {
        let member = node
            .parent()
            .is_some_and(|p| matches!(p.kind(), "class_body" | "source_file" | "enum_class_body" | "protocol_body"));
        if !member {
            self.local_property(node, depth);
            return;
        }
        let in_type = self.container().type_name.is_some();
        let is_let = self.text(node).contains("let ")
            && child_of_kind(node, "value_binding_pattern").is_some_and(|v| self.text(v) == "let");
        let label = match (in_type, is_let) {
            (true, _) => Label::Field,
            (false, true) => Label::Constant,
            (false, false) => Label::Variable,
        };
        let bindings = self.property_bindings(node);
        let mut first = None;
        for (id, _) in &bindings {
            let name = self.text(*id).trim_matches('`').to_string();
            let def = self.new_def(node, label, &name, self.child_qn(&name), *id, None);
            let d = self.push_def(def);
            first.get_or_insert(d);
        }
        let saved = self.enc;
        let ty = declared_type(node).and_then(|t| class_of_type(t, self.source));
        self.return_types.push(self.resolve_self_type(ty));
        self.enc = first.or(saved);
        if let Some(t) = declared_type(node) {
            self.type_ref(t);
        }
        for c in named_children(node) {
            if !matches!(c.kind(), "pattern" | "type_annotation" | "modifiers" | "attribute" | "value_binding_pattern")
            {
                self.visit(c, depth + 1);
            }
        }
        self.return_types.pop();
        self.enc = saved;
    }

    /// A local `let` / `var`: its type annotation and value expressions.
    fn local_property(&mut self, node: Node<'_>, depth: usize) {
        if let Some(t) = declared_type(node) {
            self.type_ref(t);
        }
        for c in named_children(node) {
            if !matches!(c.kind(), "pattern" | "type_annotation" | "modifiers" | "attribute" | "value_binding_pattern")
            {
                self.visit(c, depth + 1);
            }
        }
    }

    /// `typealias` / `associatedtype`: a `TypeAlias`.
    fn typealias(&mut self, node: Node<'_>, depth: usize) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = strip_type_text(self.text(n));
        if name.is_empty() {
            return;
        }
        let def = self.new_def(node, Label::TypeAlias, &name, self.child_qn(&name), n, None);
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        for c in named_children(node).into_iter().filter(|c| *c != n) {
            if crate::types::is_type_node(c.kind()) {
                self.type_ref(c);
            } else {
                self.visit(c, depth + 1);
            }
        }
        self.enc = saved;
    }

    /// `case a, b(Int)`: one `Constant` per case, child of its enum.
    fn enum_entry(&mut self, node: Node<'_>, depth: usize) {
        for n in named_children(node).into_iter().filter(|c| c.kind() == "simple_identifier") {
            let name = self.text(n).trim_matches('`').to_string();
            let def = self.new_def(node, Label::Constant, &name, self.child_qn(&name), n, None);
            let mut def = def;
            def.span = span_of(n);
            def.signature = Some(format!("case {name}"));
            self.push_def(def);
        }
        for c in named_children(node).into_iter().filter(|c| c.kind() != "simple_identifier") {
            self.visit_children(c, depth + 1);
        }
    }
}
