//! Definitions: namespaces, methods, accessors, aliases, constants and test
//! blocks (crate docs, *Definitions*). References are visited in
//! `refs.rs`, receivers typed in `receivers.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_extract::{Budget, ExtractError, first_line, span_of};
use rueca_model::{Definition, FileFacts, Import, Label, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, Locals};
use crate::types::{self, MethodTypes, SELF_TYPE};

/// At most this many references per file.
pub const MAX_REFS: usize = 20_000;
/// Longest signature kept, in characters.
const MAX_SIGNATURE: usize = 200;
/// Longest doc comment kept, in characters.
const MAX_DOC: usize = 1000;
/// Longest test title kept, in characters.
const MAX_TEST_NAME: usize = 120;

/// Method visibility in a class body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vis {
    Public,
    Private,
    Protected,
}

impl Vis {
    fn prefix(self) -> &'static str {
        match self {
            Vis::Public => "",
            Vis::Private => "private ",
            Vis::Protected => "protected ",
        }
    }
}

/// Where a node is: the enclosing definition, namespace and `self`.
#[derive(Debug, Clone)]
pub(crate) struct Ctx {
    /// Innermost enclosing definition (the caller of references).
    pub(crate) parent: Option<LocalDefId>,
    /// The namespace (class or module) the code is in, as a constant path.
    pub(crate) ns: Option<String>,
    /// `self` is the namespace itself (`class << self`, `def self.x`).
    pub(crate) singleton: bool,
    /// Visibility of the next `def` of a class body.
    pub(crate) vis: Vis,
    /// `module_function` without arguments is in effect.
    pub(crate) module_function: bool,
    /// The enclosing method, for `super`.
    pub(crate) method: Option<LocalDefId>,
    /// The class is a test class (`Minitest::Test`, `*TestCase`).
    pub(crate) test_class: bool,
}

impl Ctx {
    fn top() -> Ctx {
        Ctx {
            parent: None,
            ns: None,
            singleton: false,
            vis: Vis::Public,
            module_function: false,
            method: None,
            test_class: false,
        }
    }
}

/// Extraction state of one file.
pub(crate) struct Extractor<'s> {
    pub(crate) src: &'s [u8],
    pub(crate) path_is_test: bool,
    pub(crate) locals: Locals,
    pub(crate) defs: Vec<Definition>,
    pub(crate) refs: Vec<Reference>,
    pub(crate) imports: Vec<Import>,
    pub(crate) budget_depth: usize,
    /// `Read` / `Type` / `Write` references already emitted.
    pub(crate) seen: HashSet<(Option<LocalDefId>, RefKind, Option<String>, String)>,
    /// Receiver encodings of local bindings (by binding index), memoized.
    pub(crate) binding_memo: RefCell<HashMap<usize, Option<String>>>,
    /// Declared types of parameters, by identifier start byte.
    pub(crate) param_types: HashMap<u32, String>,
    /// Types of instance variables per (namespace, singleton, name).
    pub(crate) ivars: HashMap<(String, bool, String), Option<String>>,
    /// Namespaces by `class` / `module` node id, for receivers typed in
    /// the pre-pass.
    pub(crate) in_progress: RefCell<HashSet<usize>>,
}

pub(crate) fn text<'s>(node: Node<'_>, src: &'s [u8]) -> &'s str {
    locals::text(node, src)
}

/// A constant path as written: `A`, `A::B`, `::A::B`; `None` for anything
/// else (`self::X`, a call).
pub(crate) fn const_path(node: Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "constant" => Some(text(node, src).to_string()),
        "scope_resolution" => {
            let name = text(node.child_by_field_name("name")?, src);
            match node.child_by_field_name("scope") {
                None => Some(format!("::{name}")),
                Some(s) => Some(format!("{}::{name}", const_path(s, src)?)),
            }
        }
        _ => None,
    }
}

/// `ns::path`, or `path` alone at top level or when it is absolute.
pub(crate) fn join_ns(ns: Option<&str>, path: &str) -> String {
    if let Some(abs) = path.strip_prefix("::") {
        return abs.to_string();
    }
    match ns {
        Some(n) => format!("{n}::{path}"),
        None => path.to_string(),
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

/// A one-line text of `src[from..to]`, whitespace collapsed.
pub(crate) fn one_line(src: &[u8], from: usize, to: usize) -> String {
    let s = String::from_utf8_lossy(&src[from..to.max(from)]);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl<'s> Extractor<'s> {
    pub(crate) fn new(src: &'s [u8], path_is_test: bool, locals: Locals) -> Self {
        Extractor {
            src,
            path_is_test,
            locals,
            defs: Vec::new(),
            refs: Vec::new(),
            imports: Vec::new(),
            budget_depth: 512,
            seen: HashSet::new(),
            binding_memo: RefCell::new(HashMap::new()),
            param_types: HashMap::new(),
            ivars: HashMap::new(),
            in_progress: RefCell::new(HashSet::new()),
        }
    }

    pub(crate) fn t(&self, node: Node<'_>) -> &'s str {
        text(node, self.src)
    }

    pub(crate) fn add_def(&mut self, d: Definition) -> LocalDefId {
        self.defs.push(d);
        (self.defs.len() - 1) as LocalDefId
    }

    pub(crate) fn push_ref(
        &mut self,
        kind: RefKind,
        name: &str,
        receiver: Option<String>,
        enclosing: Option<LocalDefId>,
        span: Span,
    ) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        if matches!(kind, RefKind::Read | RefKind::Type | RefKind::Write)
            && !self.seen.insert((enclosing, kind, receiver.clone(), name.to_string()))
        {
            return;
        }
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing, span, arity: None });
    }

    /// A new definition with the common fields filled.
    pub(crate) fn def(
        &self,
        label: Label,
        name: &str,
        qn: String,
        parent: Option<LocalDefId>,
        node: Node<'_>,
        name_node: Node<'_>,
    ) -> Definition {
        Definition {
            label,
            name: name.to_string(),
            qualified_name: qn,
            parent,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: None,
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: self.path_is_test,
            is_declaration: false,
        }
    }

    // -----------------------------------------------------------------
    // Walk
    // -----------------------------------------------------------------

    pub(crate) fn run(mut self, root: Node<'_>, parse_errors: u32, line_count: u32) -> FileFacts {
        self.prepass_ivars(root);
        self.statements(root, &Ctx::top(), 0);
        FileFacts {
            definitions: self.defs,
            references: self.refs,
            imports: self.imports,
            parse_errors,
            line_count,
            contracts: Vec::new(),
        }
    }

    /// Visits the statements of a body in order, applying `private` /
    /// `protected` / `public` / `module_function` without arguments to the
    /// definitions that follow them.
    pub(crate) fn statements(&mut self, body: Node<'_>, ctx: &Ctx, depth: usize) {
        let mut ctx = ctx.clone();
        let mut cursor = body.walk();
        let children: Vec<Node<'_>> = body.named_children(&mut cursor).collect();
        for child in children {
            if let Some(change) = self.visibility_switch(child) {
                match change {
                    Switch::Vis(v) => {
                        ctx.vis = v;
                        ctx.module_function = false;
                    }
                    Switch::ModuleFunction => ctx.module_function = true,
                }
                continue;
            }
            self.statement(child, &ctx, depth + 1);
        }
    }

    /// A bare `private` / `protected` / `public` / `module_function`.
    fn visibility_switch(&self, node: Node<'_>) -> Option<Switch> {
        let name = match node.kind() {
            "identifier" => self.t(node),
            "call"
                if node.child_by_field_name("receiver").is_none()
                    && node.child_by_field_name("arguments").is_none() =>
            {
                self.t(node.child_by_field_name("method")?)
            }
            _ => return None,
        };
        match name {
            "private" => Some(Switch::Vis(Vis::Private)),
            "protected" => Some(Switch::Vis(Vis::Protected)),
            "public" => Some(Switch::Vis(Vis::Public)),
            "module_function" => Some(Switch::ModuleFunction),
            _ => None,
        }
    }

    /// One statement (or any node in statement position).
    pub(crate) fn statement(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        if depth > self.budget_depth {
            return;
        }
        match node.kind() {
            "class" => self.class(node, ctx, depth),
            "module" => self.module(node, ctx, depth),
            "singleton_class" => self.singleton_class(node, ctx, depth),
            "method" | "singleton_method" => {
                self.method(node, ctx, depth, None);
            }
            "alias" => self.alias(node, ctx),
            "assignment" if self.constant_assignment(node, ctx, depth) => {}
            "call" if self.special_call(node, ctx, depth) => {}
            "comment" => {}
            _ => self.expr(node, ctx, depth),
        }
    }

    // -----------------------------------------------------------------
    // Namespaces
    // -----------------------------------------------------------------

    fn class(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return self.expr(node, ctx, depth) };
        let Some(path) = const_path(name_node, self.src) else { return self.expr(node, ctx, depth) };
        let qn = join_ns(ctx.ns.as_deref(), &path);
        let mut d = self.def(Label::Class, last_segment(&qn), qn.clone(), ctx.parent, node, name_node);
        d.signature = Some(truncate(&self.header(node, "body"), MAX_SIGNATURE));
        d.doc = self.doc_comment(node);
        let mut test_class = false;
        if let Some(sup) = node.child_by_field_name("superclass") {
            if let Some(base) = self.superclass_text(sup) {
                test_class = is_test_base(&base);
                d.bases.push(base);
            }
            // References inside a computed superclass (`Struct.new(:a)`).
            if let Some(e) = sup.named_child(0)
                && const_path(e, self.src).is_none()
            {
                self.expr(e, ctx, depth + 1);
            }
        }
        d.is_test = self.path_is_test || test_class;
        let id = self.add_def(d);
        self.namespace_body(node, id, qn, test_class, depth);
    }

    fn module(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let Some(path) = const_path(name_node, self.src) else { return self.expr(node, ctx, depth) };
        let qn = join_ns(ctx.ns.as_deref(), &path);
        let mut d = self.def(Label::Module, last_segment(&qn), qn.clone(), ctx.parent, node, name_node);
        d.signature = Some(truncate(&self.header(node, "body"), MAX_SIGNATURE));
        d.doc = self.doc_comment(node);
        let id = self.add_def(d);
        self.namespace_body(node, id, qn, false, depth);
    }

    fn namespace_body(&mut self, node: Node<'_>, id: LocalDefId, qn: String, test: bool, depth: usize) {
        let inner = Ctx {
            parent: Some(id),
            ns: Some(qn),
            singleton: false,
            vis: Vis::Public,
            module_function: false,
            method: None,
            test_class: test,
        };
        if let Some(body) = node.child_by_field_name("body") {
            self.statements(body, &inner, depth + 1);
        }
    }

    /// The superclass as a constant path; for `Struct.new(…)` or
    /// `Migration[7.0]` the constant the expression starts from.
    fn superclass_text(&self, sup: Node<'_>) -> Option<String> {
        let e = sup.named_child(0)?;
        if let Some(p) = const_path(e, self.src) {
            return Some(p);
        }
        let head = match e.kind() {
            "call" => e.child_by_field_name("receiver")?,
            "element_reference" => e.child_by_field_name("object")?,
            _ => return None,
        };
        const_path(head, self.src)
    }

    /// `class << self … end`: the body's `def`s are singleton methods.
    fn singleton_class(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        let is_self = node.child_by_field_name("value").is_some_and(|v| v.kind() == "self");
        let inner =
            Ctx { singleton: is_self || ctx.singleton, vis: Vis::Public, module_function: false, ..ctx.clone() };
        if let Some(body) = node.child_by_field_name("body") {
            self.statements(body, &inner, depth + 1);
        }
    }

    // -----------------------------------------------------------------
    // Methods
    // -----------------------------------------------------------------

    /// `def m` / `def self.m`; `vis` overrides the context's visibility
    /// (`private def m`).
    pub(crate) fn method(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize, vis: Option<Vis>) -> Option<LocalDefId> {
        let name_node = node.child_by_field_name("name")?;
        let name = self.t(name_node).to_string();
        let mut singleton = ctx.singleton;
        let mut ns = ctx.ns.clone();
        if node.kind() == "singleton_method" {
            let obj = node.child_by_field_name("object")?;
            if obj.kind() == "self" {
                singleton = true;
            } else if let Some(p) = const_path(obj, self.src) {
                // `def Foo.bar`: a singleton method of `Foo`.
                singleton = true;
                ns = Some(join_ns(ctx.ns.as_deref(), &p));
            } else {
                self.walk_children(node, ctx, depth);
                return None;
            }
        }
        let module_function = ctx.module_function && !singleton;
        let vis = vis.unwrap_or(ctx.vis);
        let (label, qn) = match &ns {
            Some(n) if singleton || module_function => (Label::Method, format!("{n}.{name}")),
            Some(n) => (Label::Method, format!("{n}#{name}")),
            None => (Label::Function, name.clone()),
        };
        let mtypes = self.method_types(node);
        let mut d = self.def(label, &name, qn, ctx.parent, node, name_node);
        d.signature = Some(self.method_signature(node, vis, module_function, mtypes.as_ref(), ns.as_deref()));
        d.doc = self.doc_comment(statement_node(node));
        d.exported = vis == Vis::Public || module_function;
        d.is_test = self.path_is_test || (ctx.test_class && name.starts_with("test_"));
        let id = self.add_def(d);
        if let Some(mt) = &mtypes {
            self.type_parameters(node, mt, ns.as_deref());
        }
        let inner = Ctx {
            parent: Some(id),
            ns,
            singleton,
            vis: Vis::Public,
            module_function: false,
            method: Some(id),
            test_class: false,
        };
        if let Some(params) = node.child_by_field_name("parameters") {
            self.parameter_defaults(params, &inner, depth);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.statements(body, &inner, depth + 1);
        }
        Some(id)
    }

    /// `def m(a, b) #: -> R`: the header on one line, with the visibility
    /// and `module_function` written before it and the declared return
    /// type after `#: ->` (read by the resolver for `$ret:` receivers).
    fn method_signature(
        &self,
        node: Node<'_>,
        vis: Vis,
        module_function: bool,
        mt: Option<&MethodTypes>,
        ns: Option<&str>,
    ) -> String {
        let head = self.header(node, "body");
        let prefix = if module_function { "module_function " } else { vis.prefix() };
        let ret = mt.and_then(|m| m.returns.as_deref()).map(|r| absolute_type(r, ns));
        let sig = match ret {
            Some(r) => format!("{prefix}{head} #: -> {r}"),
            None => format!("{prefix}{head}"),
        };
        truncate(&sig, MAX_SIGNATURE)
    }

    /// The text of `node` up to its `field` child (or its first line).
    pub(crate) fn header(&self, node: Node<'_>, field: &str) -> String {
        let body = node.child_by_field_name(field).map_or(node.end_byte(), |b| b.start_byte());
        // Up to the last part before the body that is not a comment.
        let mut cursor = node.walk();
        let end = node
            .children(&mut cursor)
            .filter(|c| c.end_byte() <= body && !matches!(c.kind(), "comment" | "end"))
            .map(|c| c.end_byte())
            .max()
            .unwrap_or(body);
        let line = one_line(self.src, node.start_byte(), end);
        let line = line.trim_end_matches(';').trim_end().to_string();
        if line.is_empty() { first_line(self.t(node), MAX_SIGNATURE) } else { line }
    }

    /// The comment lines right above `node` (RBS type comments and Sorbet
    /// annotations excluded).
    pub(crate) fn doc_comment(&self, node: Node<'_>) -> Option<String> {
        let mut lines = Vec::new();
        let mut line = node.start_position().row;
        let mut cur = prev_sibling_through_body(node);
        while let Some(c) = cur {
            if c.kind() != "comment" || c.end_position().row + 1 < line {
                break;
            }
            let t = self.t(c).trim_start_matches('#').trim();
            if !self.t(c).starts_with("#:") && !t.starts_with('@') && !t.starts_with("typed:") {
                lines.push(t.to_string());
            }
            line = c.start_position().row;
            cur = prev_sibling_through_body(c);
        }
        lines.reverse();
        let doc = lines.join("\n").trim().to_string();
        (!doc.is_empty()).then(|| truncate(&doc, MAX_DOC))
    }

    /// Declared types of a method: the Sorbet `sig` or the RBS comment
    /// right above it.
    pub(crate) fn method_types(&self, node: Node<'_>) -> Option<MethodTypes> {
        let mut cur = prev_sibling_through_body(statement_node(node));
        let mut rbs: Option<MethodTypes> = None;
        while let Some(c) = cur {
            match c.kind() {
                "comment" => {
                    let t = self.t(c);
                    if let Some(rest) = t.strip_prefix("#:")
                        && rbs.is_none()
                    {
                        rbs = types::parse_rbs_method(rest);
                    }
                }
                "call" if self.is_sig(c) => return Some(types::parse_sig(self.t(c))),
                _ => break,
            }
            cur = prev_sibling_through_body(c);
        }
        rbs
    }

    fn is_sig(&self, call: Node<'_>) -> bool {
        call.child_by_field_name("receiver").is_none()
            && call.child_by_field_name("method").is_some_and(|m| self.t(m) == "sig")
            && call.child_by_field_name("block").is_some()
    }

    /// Records the declared types of `node`'s parameters (by name, else by
    /// position) for receivers.
    pub(crate) fn type_parameters(&mut self, node: Node<'_>, mt: &MethodTypes, ns: Option<&str>) {
        let Some(params) = node.child_by_field_name("parameters") else { return };
        let mut positional = 0usize;
        let mut cursor = params.walk();
        for p in params.named_children(&mut cursor) {
            let (ident, is_positional) = match p.kind() {
                "identifier" => (Some(p), true),
                "optional_parameter" => (p.child_by_field_name("name"), true),
                "keyword_parameter" => (p.child_by_field_name("name"), false),
                _ => (None, false),
            };
            let Some(ident) = ident else { continue };
            let name = self.t(ident);
            let ty = mt
                .params
                .get(name)
                .cloned()
                .or_else(|| if is_positional { mt.positional.get(positional).cloned().flatten() } else { None });
            if is_positional {
                positional += 1;
            }
            if let Some(ty) = ty {
                self.param_types.insert(ident.start_byte() as u32, absolute_type(&ty, ns));
            }
        }
    }

    /// References in parameter default values (`def m(a = build)`).
    fn parameter_defaults(&mut self, params: Node<'_>, ctx: &Ctx, depth: usize) {
        let mut cursor = params.walk();
        let children: Vec<Node<'_>> = params.named_children(&mut cursor).collect();
        for p in children {
            if let Some(v) = p.child_by_field_name("value") {
                self.expr(v, ctx, depth + 1);
            }
        }
    }

    // -----------------------------------------------------------------
    // Aliases, accessors, constants
    // -----------------------------------------------------------------

    /// `alias new old`: a method `new` that calls `old`.
    fn alias(&mut self, node: Node<'_>, ctx: &Ctx) {
        let (Some(new), Some(old)) = (node.child_by_field_name("name"), node.child_by_field_name("alias")) else {
            return;
        };
        let (new_name, old_name) = (self.t(new).to_string(), self.t(old).to_string());
        self.alias_def(node, new, &new_name, &old_name, ctx, format!("alias {new_name} {old_name}"));
    }

    pub(crate) fn alias_def(
        &mut self,
        node: Node<'_>,
        name_node: Node<'_>,
        new_name: &str,
        old_name: &str,
        ctx: &Ctx,
        signature: String,
    ) {
        let Some(ns) = ctx.ns.clone() else { return };
        let sep = if ctx.singleton { "." } else { "#" };
        let new_name = new_name.trim_start_matches(':');
        let old_name = old_name.trim_start_matches(':');
        let mut d = self.def(Label::Method, new_name, format!("{ns}{sep}{new_name}"), ctx.parent, node, name_node);
        d.signature = Some(truncate(&signature, MAX_SIGNATURE));
        d.exported = ctx.vis == Vis::Public;
        let id = self.add_def(d);
        self.push_ref(RefKind::Call, old_name, None, Some(id), span_of(node));
    }

    /// `attr_reader :a, :b` (and `attr_writer`, `attr_accessor`, `attr`):
    /// one `Field` per name.
    pub(crate) fn accessors(&mut self, call: Node<'_>, kind: &str, ctx: &Ctx) {
        let Some(ns) = ctx.ns.clone() else { return };
        let Some(args) = call.child_by_field_name("arguments") else { return };
        let sep = if ctx.singleton { "." } else { "#" };
        let mut cursor = args.walk();
        let names: Vec<Node<'_>> = args.named_children(&mut cursor).collect();
        for a in names {
            let Some(name) = symbol_name(a, self.src) else { continue };
            let ty = self.ivars.get(&(ns.clone(), ctx.singleton, format!("@{name}"))).cloned().flatten();
            let mut d = self.def(Label::Field, &name, format!("{ns}{sep}{name}"), ctx.parent, call, a);
            let sig = match ty.as_deref().and_then(|t| t.strip_prefix("$type:")) {
                Some(t) if kind != "attr_writer" => format!("{kind} :{name} #: -> {t}"),
                _ => format!("{kind} :{name}"),
            };
            d.signature = Some(sig);
            d.exported = ctx.vis == Vis::Public;
            self.add_def(d);
        }
    }

    /// `X = …` in a class body or at top level: a constant, or a class
    /// when the value builds one (`Struct.new`, `Data.define`,
    /// `Class.new(Base)`, `Module.new`). `false` when the target is not a
    /// constant.
    fn constant_assignment(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) -> bool {
        let Some(left) = node.child_by_field_name("left") else { return false };
        let Some(path) = const_path(left, self.src) else { return false };
        if ctx.method.is_some() {
            return false;
        }
        let qn = join_ns(ctx.ns.as_deref(), &path);
        let right = node.child_by_field_name("right");
        if let Some((label, base)) = right.and_then(|r| self.class_builder(r)) {
            let r = right.unwrap_or(node);
            let mut d = self.def(label, last_segment(&qn), qn.clone(), ctx.parent, node, left);
            d.signature = Some(truncate(
                &one_line(self.src, node.start_byte(), r.start_byte() + 64.min(r.end_byte() - r.start_byte())),
                MAX_SIGNATURE,
            ));
            d.doc = self.doc_comment(node);
            d.bases.extend(base);
            let id = self.add_def(d);
            self.struct_members(r, ctx, id, &qn);
            let inner = Ctx {
                parent: Some(id),
                ns: Some(qn),
                singleton: false,
                vis: Vis::Public,
                module_function: false,
                method: None,
                test_class: false,
            };
            // Arguments are code of the outer scope; the block is the body.
            if let Some(args) = r.child_by_field_name("arguments") {
                self.expr(args, ctx, depth + 1);
            }
            if let Some(body) = r.child_by_field_name("block").and_then(|b| b.child_by_field_name("body")) {
                self.statements(body, &inner, depth + 1);
            }
            return true;
        }
        let name = last_segment(&qn).to_string();
        let mut d = self.def(Label::Constant, &name, qn, ctx.parent, node, left);
        d.signature = Some(truncate(&first_line(self.t(node), MAX_SIGNATURE), MAX_SIGNATURE));
        d.doc = self.doc_comment(node);
        let id = self.add_def(d);
        if let Some(r) = right {
            let inner = Ctx { parent: Some(id), ..ctx.clone() };
            self.expr(r, &inner, depth + 1);
        }
        true
    }

    /// `Struct.new(…)` / `Data.define(…)` / `Class.new(Base)` /
    /// `Module.new`: the label and base of the class it builds.
    fn class_builder(&self, value: Node<'_>) -> Option<(Label, Option<String>)> {
        if value.kind() != "call" {
            return None;
        }
        let recv = const_path(value.child_by_field_name("receiver")?, self.src)?;
        let method = self.t(value.child_by_field_name("method")?);
        let recv = recv.trim_start_matches("::");
        match (recv, method) {
            ("Struct", "new") => Some((Label::Class, Some("Struct".into()))),
            ("Data", "define") => Some((Label::Class, Some("Data".into()))),
            ("Module", "new") => Some((Label::Module, None)),
            ("Class", "new") => {
                let base = value
                    .child_by_field_name("arguments")
                    .and_then(|a| a.named_child(0))
                    .and_then(|a| const_path(a, self.src));
                Some((Label::Class, base))
            }
            _ => None,
        }
    }

    /// The members of `Struct.new(:a, :b)` / `Data.define(:a)` as fields.
    fn struct_members(&mut self, value: Node<'_>, ctx: &Ctx, id: LocalDefId, qn: &str) {
        let Some(args) = value.child_by_field_name("arguments") else { return };
        let mut cursor = args.walk();
        let names: Vec<Node<'_>> = args.named_children(&mut cursor).collect();
        let _ = ctx;
        for a in names {
            let Some(name) = symbol_name(a, self.src) else { continue };
            let mut d = self.def(Label::Field, &name, format!("{qn}#{name}"), Some(id), a, a);
            d.signature = Some(format!(":{name}"));
            self.add_def(d);
        }
    }

    // -----------------------------------------------------------------
    // Calls that define or import
    // -----------------------------------------------------------------

    /// Calls with a meaning beyond a call: imports, mixins, accessors,
    /// aliases, `define_method`, visibility with arguments, test blocks.
    /// `true` when handled.
    fn special_call(&mut self, call: Node<'_>, ctx: &Ctx, depth: usize) -> bool {
        let Some(method) = call.child_by_field_name("method") else { return false };
        let name = self.t(method);
        let receiver = call.child_by_field_name("receiver");
        if receiver.is_none() && self.import_call(call, name) {
            return true;
        }
        if let Some(r) = receiver {
            // `RSpec.describe X do … end`
            if self.t(r) == "RSpec" && crate::tests::is_group(name) && self.test_block(call, ctx, depth) {
                return true;
            }
            return false;
        }
        if ctx.method.is_none() && ctx.ns.is_some() && self.class_body_call(call, name, ctx, depth) {
            return true;
        }
        if crate::tests::is_test_call(name)
            && (self.path_is_test || ctx.test_class)
            && self.test_block(call, ctx, depth)
        {
            return true;
        }
        false
    }

    /// `include` / `extend` / `prepend`, accessors, `alias_method`,
    /// `define_method`, `private :a` / `private def a` in a class body.
    fn class_body_call(&mut self, call: Node<'_>, name: &str, ctx: &Ctx, depth: usize) -> bool {
        match name {
            "include" | "extend" | "prepend" => {
                self.mixins(call, name, ctx);
                false // the call is still visited for its references
            }
            "attr_reader" | "attr_writer" | "attr_accessor" | "attr" => {
                self.accessors(call, name, ctx);
                true
            }
            "alias_method" => {
                let args = call.child_by_field_name("arguments");
                let names: Vec<Node<'_>> = args
                    .map(|a| {
                        let mut c = a.walk();
                        a.named_children(&mut c).collect()
                    })
                    .unwrap_or_default();
                if let [new, old, ..] = names.as_slice()
                    && let (Some(n), Some(o)) = (symbol_name(*new, self.src), symbol_name(*old, self.src))
                {
                    let sig = format!("alias_method :{n}, :{o}");
                    self.alias_def(call, *new, &n, &o, ctx, sig);
                    return true;
                }
                false
            }
            "define_method" => self.define_method(call, ctx, depth),
            "private" | "protected" | "public" | "module_function" | "private_class_method" => {
                self.visibility_call(call, name, ctx, depth)
            }
            _ => false,
        }
    }

    /// `include M, N` → bases `include:M`, `include:N` on the namespace.
    fn mixins(&mut self, call: Node<'_>, name: &str, ctx: &Ctx) {
        let (Some(args), Some(owner)) = (call.child_by_field_name("arguments"), ctx.parent) else { return };
        let mut cursor = args.walk();
        let targets: Vec<String> = args
            .named_children(&mut cursor)
            .filter_map(|a| if a.kind() == "self" { Some("self".to_string()) } else { const_path(a, self.src) })
            .collect();
        let kind = if ctx.singleton && name == "include" { "extend" } else { name };
        let d = &mut self.defs[owner as usize];
        if matches!(d.label, Label::Class | Label::Module) {
            d.bases.extend(targets.into_iter().map(|t| format!("{kind}:{t}")));
        }
    }

    /// `define_method(:name) { … }` / `do … end`: a method whose body is
    /// the block.
    fn define_method(&mut self, call: Node<'_>, ctx: &Ctx, depth: usize) -> bool {
        let Some(first) = call.child_by_field_name("arguments").and_then(|a| a.named_child(0)) else { return false };
        let Some(name) = symbol_name(first, self.src) else { return false };
        let Some(block) = call.child_by_field_name("block") else { return false };
        let Some(ns) = ctx.ns.clone() else { return false };
        let sep = if ctx.singleton { "." } else { "#" };
        let mut d = self.def(Label::Method, &name, format!("{ns}{sep}{name}"), ctx.parent, call, first);
        d.signature = Some(truncate(&self.header(call, "block"), MAX_SIGNATURE));
        d.exported = ctx.vis == Vis::Public;
        let id = self.add_def(d);
        let inner = Ctx { parent: Some(id), method: Some(id), ..ctx.clone() };
        self.walk_children(block, &inner, depth);
        true
    }

    /// `private :a, :b` (retroactive), `private def a` / `private
    /// attr_reader :a` (one definition), `module_function :a`.
    fn visibility_call(&mut self, call: Node<'_>, name: &str, ctx: &Ctx, depth: usize) -> bool {
        let Some(args) = call.child_by_field_name("arguments") else { return false };
        let vis = match name {
            "private" | "private_class_method" => Vis::Private,
            "protected" => Vis::Protected,
            _ => Vis::Public,
        };
        let mut cursor = args.walk();
        let items: Vec<Node<'_>> = args.named_children(&mut cursor).collect();
        for a in items {
            match a.kind() {
                "method" | "singleton_method" => {
                    let inner = Ctx { module_function: name == "module_function", ..ctx.clone() };
                    self.method(a, &inner, depth, Some(vis));
                }
                "call" | "alias" => {
                    let inner = Ctx { vis, ..ctx.clone() };
                    self.statement(a, &inner, depth + 1);
                }
                _ => {
                    if let Some(sym) = symbol_name(a, self.src) {
                        self.mark_visibility(ctx, &sym, name);
                    }
                }
            }
        }
        true
    }

    /// Applies `private :sym` / `module_function :sym` to the method `sym`
    /// already defined in this namespace.
    fn mark_visibility(&mut self, ctx: &Ctx, sym: &str, how: &str) {
        let Some(ns) = &ctx.ns else { return };
        let inst = format!("{ns}#{sym}");
        let sing = format!("{ns}.{sym}");
        let Some(d) = self.defs.iter_mut().rev().find(|d| d.qualified_name == inst || d.qualified_name == sing) else {
            return;
        };
        match how {
            "module_function" => {
                d.qualified_name = sing;
                if let Some(s) = &mut d.signature
                    && !s.starts_with("module_function ")
                {
                    *s = format!(
                        "module_function {}",
                        s.trim_start_matches("private ").trim_start_matches("protected ")
                    );
                }
                d.exported = true;
            }
            "public" => d.exported = true,
            _ => {
                d.exported = false;
                if let Some(s) = &mut d.signature
                    && !s.starts_with(how)
                {
                    let how = if how == "private_class_method" { "private" } else { how };
                    *s = format!("{how} {s}");
                }
            }
        }
    }

    /// `require "x"`, `require_relative "x"`, `load "x.rb"`, `autoload :X,
    /// "x"`: an import of the file. `true` when it is one.
    fn import_call(&mut self, call: Node<'_>, name: &str) -> bool {
        if !matches!(name, "require" | "require_relative" | "load" | "autoload" | "require_dependency") {
            return false;
        }
        let Some(args) = call.child_by_field_name("arguments") else { return false };
        let arg = if name == "autoload" { args.named_child(1) } else { args.named_child(0) };
        let Some(arg) = arg else { return false };
        let Some((module, relative)) = self.import_path(arg, name == "require_relative") else { return false };
        self.imports.push(Import {
            module,
            relative_level: u32::from(relative),
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span: span_of(call),
        });
        true
    }

    /// The path a `require` names: a literal string, or
    /// `File.expand_path("x", __dir__)` / `File.join(__dir__, "x")`
    /// (relative to the file's directory).
    fn import_path(&self, arg: Node<'_>, relative: bool) -> Option<(String, bool)> {
        if let Some(s) = literal_string(arg, self.src) {
            return Some((s, relative));
        }
        if arg.kind() != "call" || arg.child_by_field_name("receiver").is_none_or(|r| self.t(r) != "File") {
            return None;
        }
        let method = self.t(arg.child_by_field_name("method")?);
        let args = arg.child_by_field_name("arguments")?;
        let mut cursor = args.walk();
        let parts: Vec<Node<'_>> = args.named_children(&mut cursor).collect();
        match (method, parts.as_slice()) {
            ("expand_path", [p, base]) => {
                let p = literal_string(*p, self.src)?;
                match self.t(*base) {
                    "__dir__" => Some((p, true)),
                    // Relative to the file itself: one `..` is its directory.
                    "__FILE__" => Some((p.strip_prefix("../").unwrap_or(&p).to_string(), true)),
                    _ => None,
                }
            }
            ("join", [base, rest @ ..]) if self.t(*base) == "__dir__" && !rest.is_empty() => {
                let segs: Option<Vec<String>> = rest.iter().map(|r| literal_string(*r, self.src)).collect();
                Some((segs?.join("/"), true))
            }
            _ => None,
        }
    }

    /// RSpec / ActiveSupport test blocks: a `Test` definition whose body is
    /// the block. `false` when the call has no block.
    fn test_block(&mut self, call: Node<'_>, ctx: &Ctx, depth: usize) -> bool {
        let Some(block) = call.child_by_field_name("block") else { return false };
        let method = call.child_by_field_name("method").map(|m| self.t(m)).unwrap_or("");
        let first = call.child_by_field_name("arguments").and_then(|a| a.named_child(0));
        let title = first
            .map(|a| {
                literal_string(a, self.src)
                    .or_else(|| const_path(a, self.src))
                    .unwrap_or_else(|| one_line(self.src, a.start_byte(), a.end_byte()))
            })
            .unwrap_or_else(|| method.to_string());
        let title = truncate(&title, MAX_TEST_NAME);
        let parent_test = ctx.parent.filter(|&p| self.defs[p as usize].label == Label::Test);
        let qn = match parent_test {
            Some(p) => format!("{} > {title}", self.defs[p as usize].qualified_name),
            None => title.clone(),
        };
        let mut d = self.def(Label::Test, &title, qn, ctx.parent, call, first.unwrap_or(call));
        d.signature = Some(truncate(&self.header(call, "block"), MAX_SIGNATURE));
        d.exported = false;
        d.is_test = true;
        let id = self.add_def(d);
        if let Some(args) = call.child_by_field_name("arguments") {
            self.expr(args, ctx, depth + 1);
        }
        let inner = Ctx { parent: Some(id), method: None, ..ctx.clone() };
        if let Some(body) = block.child_by_field_name("body") {
            self.statements(body, &inner, depth + 1);
        }
        true
    }
}

enum Switch {
    Vis(Vis),
    ModuleFunction,
}

/// The node that stands for a `def` in its body: the `private def x` call
/// around it, else the `def` itself.
pub(crate) fn statement_node(node: Node<'_>) -> Node<'_> {
    match node.parent() {
        Some(p) if p.kind() == "argument_list" => p.parent().filter(|c| c.kind() == "call").unwrap_or(node),
        _ => node,
    }
}

/// The named sibling before `node`; for the first statement of a body,
/// the comments tree-sitter attaches before the body (`class C` ⏎ `#: …` ⏎
/// `def m`).
pub(crate) fn prev_sibling_through_body(node: Node<'_>) -> Option<Node<'_>> {
    if let Some(p) = node.prev_named_sibling() {
        return Some(p);
    }
    let parent = node.parent()?;
    if parent.kind() != "body_statement" {
        return None;
    }
    parent.prev_named_sibling().filter(|c| c.kind() == "comment")
}

/// The name of a symbol or string literal argument (`:foo`, `"foo"`).
pub(crate) fn symbol_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_symbol" => Some(text(node, src).trim_start_matches(':').to_string()),
        "string" | "delimited_symbol" => literal_string(node, src),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

/// The content of a string literal without interpolation.
pub(crate) fn literal_string(node: Node<'_>, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "delimited_symbol") {
        return None;
    }
    let mut cursor = node.walk();
    let mut out = String::new();
    for c in node.named_children(&mut cursor) {
        match c.kind() {
            "string_content" => out.push_str(text(c, src)),
            "escape_sequence" => {}
            _ => return None,
        }
    }
    Some(out)
}

/// A superclass that makes a test class.
fn is_test_base(base: &str) -> bool {
    let last = last_segment(base);
    last.ends_with("Test") || last.ends_with("TestCase") || last == "Spec"
}

/// `T` as an absolute constant path when it is the enclosing class.
pub(crate) fn absolute_type(ty: &str, ns: Option<&str>) -> String {
    if ty == SELF_TYPE {
        return ns.map_or_else(|| "Object".to_string(), |n| format!("::{n}"));
    }
    ty.to_string()
}

/// Parses and extracts one file.
pub(crate) fn extract_file(path: &str, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: rueca_extract::tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let mut errors = rueca_extract::count_errors(&tree);
    // Syntax the grammar misses: a same-length rewrite (`repair.rs`).
    let repaired = if errors > 0 { crate::repair::repair(source) } else { None };
    let mut chosen: (&[u8], _) = (source, tree);
    if let Some(fixed) = repaired.as_deref() {
        let t2 = rueca_extract::parse(&language, fixed, budget)?;
        let e2 = rueca_extract::count_errors(&t2);
        if e2 < errors {
            errors = e2;
            chosen = (fixed, t2);
        }
    }
    let (src, tree) = chosen;
    let locals = locals::build(&tree, src, budget.max_parse_time)?;
    let mut x = Extractor::new(src, crate::tests::is_test_file(path), locals);
    x.budget_depth = budget.max_depth;
    Ok(x.run(tree.root_node(), errors, rueca_extract::line_count(source)))
}
