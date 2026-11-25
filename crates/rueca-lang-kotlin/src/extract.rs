//! Definitions, imports and the walk over a Kotlin file (expressions are
//! in `refs.rs`). See the crate docs for the facts produced.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_extract::{Budget, ExtractError, count_errors, line_count, span_of};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, Locals};
use crate::types::{
    child_of_kind, class_of_type, declared_type, has_keyword, is_type_node, named_children, suffix_type, text,
};

/// Maximum references per file.
pub const MAX_REFS: usize = 200_000;
const MAX_SIGNATURE: usize = 200;
const MAX_DOC: usize = 1000;

/// Annotations that make a function a test (JUnit 4 / 5, kotlin.test, TestNG).
const TEST_ANNOTATIONS: &[&str] = &["ParameterizedTest", "RepeatedTest", "Test", "TestFactory", "TestTemplate"];
/// Fixture annotations: tests when declared in a test class.
const LIFECYCLE_ANNOTATIONS: &[&str] = &[
    "After",
    "AfterAll",
    "AfterClass",
    "AfterEach",
    "AfterTest",
    "Before",
    "BeforeAll",
    "BeforeClass",
    "BeforeEach",
    "BeforeTest",
];
/// Kotest spec styles: a class extending one is a test class.
const KOTEST_SPECS: &[&str] = &[
    "AnnotationSpec",
    "BehaviorSpec",
    "DescribeSpec",
    "ExpectSpec",
    "FeatureSpec",
    "FreeSpec",
    "FunSpec",
    "ShouldSpec",
    "StringSpec",
    "WordSpec",
];

pub(crate) fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language = locals::language();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time)?;
    let mut ex = Extractor::new(source, budget.max_depth, locals, root);
    ex.prescan(root);
    ex.visit_children(root, 0);
    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
        contracts: Vec::new(),
    })
}

/// A class, interface, object or companion being visited.
pub(crate) struct ClassCtx {
    pub(crate) def: LocalDefId,
    pub(crate) name: String,
    /// Property name → class held (`None` when unknown).
    pub(crate) props: HashMap<String, Option<String>>,
    /// Untyped property → byte range of its initializer.
    pub(crate) prop_inits: HashMap<String, (usize, usize)>,
    /// Function name → declared return class when every overload agrees.
    pub(crate) methods: HashMap<String, Option<String>>,
    /// Named, annotated or derived as a test class.
    test_class: bool,
    /// At least one member is a test.
    has_tests: bool,
}

pub(crate) struct Extractor<'s> {
    pub(crate) source: &'s [u8],
    /// The file's syntax tree (initializers are typed on demand).
    pub(crate) root: Node<'s>,
    /// Smart casts in scope: (name, class, start byte, end byte).
    pub(crate) casts: Vec<(String, String, usize, usize)>,
    /// Nesting of receivers typed through initializers.
    pub(crate) recv_depth: usize,
    /// Lambdas of scope functions with a typed receiver: (start byte, end
    /// byte, class, whether it is `it` rather than `this`).
    pub(crate) lambda_recv: Vec<(usize, usize, String, bool)>,
    /// Untyped top-level property → byte range of its initializer.
    pub(crate) file_prop_inits: HashMap<String, (usize, usize)>,
    pub(crate) max_depth: usize,
    pub(crate) defs: Vec<Definition>,
    pub(crate) refs: Vec<Reference>,
    pub(crate) imports: Vec<Import>,
    /// Dedup keys for `Read` / `Write` / `Type`: (kind, enclosing, receiver, name).
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    /// Innermost enclosing definition.
    pub(crate) enc: Option<LocalDefId>,
    pub(crate) classes: Vec<ClassCtx>,
    /// Generic type parameters in scope.
    pub(crate) type_params: Vec<String>,
    /// First bound of each type parameter in `type_params` (`T : Named` →
    /// `Named`), the class a value of that type holds.
    pub(crate) type_bounds: Vec<Option<String>>,
    /// Anonymous objects named so far per (enclosing definition, base).
    anon_counts: HashMap<(Option<LocalDefId>, String), u32>,
    pub(crate) locals: Locals,
    /// Simple name → fully qualified name of single-name imports.
    pub(crate) imported: HashMap<String, String>,
    /// Top-level functions of the file → declared return class.
    pub(crate) file_functions: HashMap<String, Option<String>>,
    /// Top-level functions per local name without suffix (`f`,
    /// `Recv.f` for extensions), for overload suffixes.
    top_counts: HashMap<String, usize>,
    /// Top-level properties of the file → class held.
    pub(crate) file_props: HashMap<String, Option<String>>,
    /// Every type name declared in this file.
    pub(crate) declared_types: HashSet<String>,
    /// Inside a test definition (local functions and objects inherit it).
    in_test: bool,
}

/// Whether a declaration kind is a class-like declaration.
fn is_class_like(kind: &str) -> bool {
    matches!(kind, "class_declaration" | "object_declaration" | "companion_object")
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "line_comment" | "multiline_comment")
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max { s } else { s.chars().take(max).collect() }
}

/// `/** … */` text without delimiters and leading `*`.
fn clean_kdoc(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix("/**")?.strip_suffix("*/")?;
    let lines: Vec<&str> = inner.lines().map(|l| l.trim().trim_start_matches('*').trim()).collect();
    let s = lines.join("\n").trim().to_string();
    (!s.is_empty()).then(|| truncate_chars(s, MAX_DOC))
}

/// The last segment of a dotted name.
pub(crate) fn last_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

/// Splits an import path into (package, imported name): the package is
/// every segment before the first upper-case one (Kotlin naming), the name
/// the rest (`a.b.C.Inner` → (`a.b`, `C.Inner`)); a path without an
/// upper-case segment imports its last segment (`a.b.max`).
pub(crate) fn split_import(path: &str) -> (String, String) {
    let segs: Vec<&str> = path.split('.').collect();
    let first_upper = segs.iter().position(|s| s.chars().next().is_some_and(char::is_uppercase));
    let at = match first_upper {
        Some(i) => i.min(segs.len() - 1),
        None => segs.len() - 1,
    };
    (segs[..at].join("."), segs[at..].join("."))
}

impl<'s> Extractor<'s> {
    fn new(source: &'s [u8], max_depth: usize, locals: Locals, root: Node<'s>) -> Self {
        Extractor {
            source,
            root,
            casts: Vec::new(),
            recv_depth: 0,
            lambda_recv: Vec::new(),
            file_prop_inits: HashMap::new(),
            max_depth,
            defs: Vec::new(),
            refs: Vec::new(),
            imports: Vec::new(),
            seen_refs: HashSet::new(),
            enc: None,
            classes: Vec::new(),
            type_params: Vec::new(),
            type_bounds: Vec::new(),
            anon_counts: HashMap::new(),
            locals,
            imported: HashMap::new(),
            file_functions: HashMap::new(),
            file_props: HashMap::new(),
            top_counts: HashMap::new(),
            declared_types: HashSet::new(),
            in_test: false,
        }
    }

    pub(crate) fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.source)
    }

    // -- prescan ---------------------------------------------------------------

    /// Package, imports, declared type names and the file's top-level
    /// functions and properties (for receivers declared later in the file).
    fn prescan(&mut self, root: Node<'_>) {
        for c in named_children(root) {
            match c.kind() {
                "package_header" => self.package(c),
                "import_list" => named_children(c).into_iter().for_each(|i| self.import(i)),
                "import_header" => self.import(c),
                "function_declaration" => self.prescan_function(c),
                "property_declaration" => self.prescan_property(c),
                _ => {}
            }
        }
        self.collect_type_names(root, 0);
    }

    fn prescan_function(&mut self, node: Node<'_>) {
        if let Some(name) = child_of_kind(node, "simple_identifier") {
            let ret = self.return_class(node);
            let name = self.text(name);
            merge_return(&mut self.file_functions, name, ret);
            *self.top_counts.entry(self.extension_qn(node, name)).or_default() += 1;
        }
    }

    /// The local name of a top-level function: `Recv.f` for an extension
    /// of `Recv` (the receiver type as written, generic arguments and
    /// nullability removed), else `f`.
    fn extension_qn(&self, node: Node<'_>, name: &str) -> String {
        let recv = child_of_kind(node, "receiver_type")
            .and_then(|r| named_children(r).into_iter().find(|c| is_type_node(c.kind())))
            .and_then(|t| class_of_type(t, self.source));
        match recv {
            Some(r) => format!("{r}.{name}"),
            None => name.to_string(),
        }
    }

    fn prescan_property(&mut self, node: Node<'_>) {
        if let Some((name, class)) = self.property_name_and_class(node) {
            if let Some(r) = self.untyped_init(node) {
                self.file_prop_inits.insert(name.clone(), r);
            }
            self.file_props.insert(name, class);
        }
    }

    /// Byte range of the initializer of a property without a declared type.
    fn untyped_init(&self, node: Node<'_>) -> Option<(usize, usize)> {
        let var = child_of_kind(node, "variable_declaration")?;
        if declared_type(var).is_some() {
            return None;
        }
        self.property_initializer(node, var).map(|v| (v.start_byte(), v.end_byte()))
    }

    fn collect_type_names(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for c in named_children(node) {
            if is_class_like(c.kind())
                && let Some(n) = child_of_kind(c, "type_identifier")
            {
                self.declared_types.insert(self.text(n).to_string());
            }
            if is_class_like(c.kind()) || matches!(c.kind(), "class_body" | "enum_class_body") {
                self.collect_type_names(c, depth + 1);
            }
        }
    }

    fn package(&mut self, node: Node<'_>) {
        let Some(id) = child_of_kind(node, "identifier") else { return };
        let module = self.text(id).split_whitespace().collect::<String>();
        self.imports.push(Import {
            module,
            names: Vec::new(),
            module_alias: None,
            relative_level: 0,
            type_only: false,
            reexport: false,
            span: span_of(node),
        });
    }

    fn import(&mut self, node: Node<'_>) {
        if node.kind() != "import_header" {
            return;
        }
        let Some(id) = child_of_kind(node, "identifier") else { return };
        let path = self.text(id).split_whitespace().collect::<String>();
        let span = span_of(node);
        let (module, name) = if child_of_kind(node, "wildcard_import").is_some() {
            (path.clone(), "*".to_string())
        } else {
            split_import(&path)
        };
        let alias = child_of_kind(node, "import_alias")
            .and_then(|a| child_of_kind(a, "type_identifier"))
            .map(|a| self.text(a).to_string());
        if name != "*" {
            let bound = alias.clone().unwrap_or_else(|| last_segment(&name).to_string());
            self.imported.insert(bound, path.clone());
        }
        self.imports.push(Import {
            module,
            names: vec![ImportedName { name, alias }],
            module_alias: None,
            relative_level: 0,
            type_only: false,
            reexport: false,
            span,
        });
    }

    // -- references --------------------------------------------------------------

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
        let dedup = match kind {
            RefKind::Read => Some(0u8),
            RefKind::Write => Some(1),
            RefKind::Type => Some(2),
            _ => None,
        };
        if let Some(k) = dedup
            && !self.seen_refs.insert((k, self.enc, receiver.clone(), name.to_string()))
        {
            return;
        }
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing: self.enc, span, arity });
    }

    // -- shared helpers ------------------------------------------------------------

    fn child_qn(&self, name: &str) -> String {
        match self.enc {
            Some(e) => format!("{}.{name}", self.defs[e as usize].qualified_name),
            None => name.to_string(),
        }
    }

    /// The KDoc right before `node` (only whitespace between).
    fn kdoc(&self, node: Node<'_>) -> Option<String> {
        let prev = node.prev_sibling()?;
        if prev.kind() != "multiline_comment" {
            return None;
        }
        let between = &self.source[prev.end_byte()..node.start_byte()];
        if !between.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        clean_kdoc(self.text(prev))
    }

    /// Declaration header up to `end` (body start), annotations and
    /// comments removed, whitespace collapsed.
    fn signature(&self, node: Node<'_>, end: usize) -> String {
        let mut skip: Vec<(usize, usize)> = Vec::new();
        if let Some(m) = child_of_kind(node, "modifiers") {
            for a in named_children(m).into_iter().filter(|a| a.kind() == "annotation") {
                skip.push((a.start_byte(), a.end_byte()));
            }
        }
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
        let s = collapse(out.trim_end_matches(['{', '=', ' ', '\n', '\t', '\r']));
        truncate_chars(s, MAX_SIGNATURE)
    }

    /// Modifier keywords (`private`, `data`, `companion`, …) and annotations
    /// of a declaration.
    fn modifiers<'t>(&self, node: Node<'t>) -> (HashSet<&'s str>, Vec<Node<'t>>) {
        let mut keywords = HashSet::new();
        let mut annotations = Vec::new();
        let Some(m) = child_of_kind(node, "modifiers") else { return (keywords, annotations) };
        for c in named_children(m) {
            if c.kind() == "annotation" {
                annotations.push(c);
            } else {
                keywords.insert(self.text(c));
            }
        }
        (keywords, annotations)
    }

    /// The annotation's simple name (`Test` for `@org.junit.Test`, `Value`
    /// for `@Value("…")`).
    pub(crate) fn annotation_name(&self, a: Node<'_>) -> Option<String> {
        let ty = named_children(a).into_iter().find_map(|c| match c.kind() {
            "user_type" => Some(c),
            "constructor_invocation" => child_of_kind(c, "user_type"),
            _ => None,
        })?;
        Some(last_segment(&crate::types::strip_type_text(self.text(ty))).to_string())
    }

    fn decorator_texts(&self, annotations: &[Node<'_>]) -> Vec<String> {
        annotations.iter().map(|a| collapse(self.text(*a).trim_start_matches('@').trim())).collect()
    }

    fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.defs.push(def);
        (self.defs.len() - 1) as LocalDefId
    }

    /// A definition with the common fields filled in.
    #[allow(clippy::too_many_arguments)]
    fn new_def(&self, label: Label, name: &str, qn: String, node: Node<'_>, name_node: Node<'_>) -> Definition {
        Definition {
            label,
            name: name.to_string(),
            qualified_name: qn,
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: None,
            doc: self.kdoc(node),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: self.in_test,
            is_declaration: false,
        }
    }

    // -- walk ------------------------------------------------------------------------

    pub(crate) fn visit_children(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            self.visit(c, depth + 1);
        }
    }

    /// Declarations; everything else is an expression (`refs.rs`).
    pub(crate) fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            k if is_comment(k) => {}
            "package_header" | "import_list" | "import_header" | "shebang_line" => {}
            "file_annotation" => self.visit_children(node, depth),
            k if is_class_like(k) => self.visit_class(node, depth),
            "function_declaration" => self.visit_function(node, depth),
            "property_declaration" => self.visit_property(node, depth),
            "type_alias" => self.visit_type_alias(node, depth),
            "secondary_constructor" => self.visit_secondary_ctor(node, depth),
            "anonymous_initializer" => self.visit_children(node, depth),
            "object_literal" => self.visit_object_literal(node, None, depth),
            _ => self.visit_expr(node, depth),
        }
    }

    /// Annotations of a declaration as `Decorator` references (attributed
    /// to the decorated definition, the current `enc`).
    fn visit_annotations(&mut self, annotations: &[Node<'_>], depth: usize) {
        for a in annotations {
            self.visit_annotation(*a, true, depth);
        }
    }

    /// An annotation: a `Decorator` (declarations) or `Type` reference to
    /// its class, then its arguments.
    pub(crate) fn visit_annotation(&mut self, a: Node<'_>, as_decorator: bool, depth: usize) {
        for c in named_children(a) {
            match c.kind() {
                "user_type" => self.push_annotation_ref(c, as_decorator),
                "constructor_invocation" => {
                    if let Some(t) = child_of_kind(c, "user_type") {
                        self.push_annotation_ref(t, as_decorator);
                    }
                    if let Some(args) = child_of_kind(c, "value_arguments") {
                        self.visit_children(args, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn push_annotation_ref(&mut self, ty: Node<'_>, as_decorator: bool) {
        let path = crate::types::strip_type_text(self.text(ty));
        let (receiver, name) = match path.rsplit_once('.') {
            Some((r, n)) => (Some(r.to_string()), n.to_string()),
            None => (None, path.clone()),
        };
        let kind = if as_decorator { RefKind::Decorator } else { RefKind::Type };
        self.push_ref(kind, &name, receiver, span_of(ty), None);
    }

    // -- type parameters -----------------------------------------------------------

    fn push_type_params(&mut self, node: Node<'_>, depth: usize) -> usize {
        let mark = self.type_params.len();
        let Some(tp) = child_of_kind(node, "type_parameters") else { return mark };
        for p in named_children(tp) {
            if let Some(id) = child_of_kind(p, "type_identifier") {
                let bound = named_children(p)
                    .into_iter()
                    .find(|c| is_type_node(c.kind()))
                    .and_then(|t| class_of_type(t, self.source));
                self.type_params.push(self.text(id).to_string());
                self.type_bounds.push(bound);
            }
        }
        for p in named_children(tp) {
            for c in named_children(p).into_iter().filter(|c| is_type_node(c.kind())) {
                self.visit_type(c, depth);
            }
        }
        if let Some(tc) = child_of_kind(node, "type_constraints") {
            self.visit_type_constraints(tc, depth);
        }
        mark
    }

    fn visit_type_constraints(&mut self, tc: Node<'_>, depth: usize) {
        for c in named_children(tc) {
            for t in named_children(c).into_iter().filter(|t| is_type_node(t.kind())) {
                self.visit_type(t, depth);
            }
        }
    }

    // -- classes -------------------------------------------------------------------

    fn class_label(&self, node: Node<'_>, keywords: &HashSet<&str>) -> Label {
        if node.kind() != "class_declaration" {
            return Label::Class;
        }
        if has_keyword(node, "interface") || keywords.contains("annotation") {
            Label::Interface
        } else if has_keyword(node, "enum") || keywords.contains("enum") {
            Label::Enum
        } else {
            Label::Class
        }
    }

    /// Supertypes: (bases, implements, superclass constructor calls).
    /// A supertype called like a constructor (`Base(…)`) is the superclass;
    /// the others are interfaces (all of them for an interface).
    fn supertypes<'t>(&self, node: Node<'t>, label: Label) -> (Vec<String>, Vec<String>, Vec<Node<'t>>) {
        let (mut bases, mut implements, mut calls) = (Vec::new(), Vec::new(), Vec::new());
        for d in named_children(node).into_iter().filter(|c| c.kind() == "delegation_specifier") {
            let Some(inner) = d.named_child(0) else { continue };
            let (ty, is_call) = match inner.kind() {
                "constructor_invocation" => (child_of_kind(inner, "user_type"), true),
                "explicit_delegation" => (declared_type(inner), false),
                k if is_type_node(k) => (Some(inner), false),
                _ => (None, false),
            };
            let Some(name) = ty.and_then(|t| class_of_type(t, self.source)) else { continue };
            if is_call {
                calls.push(inner);
            }
            if is_call || label == Label::Interface {
                bases.push(name);
            } else {
                implements.push(name);
            }
        }
        (bases, implements, calls)
    }

    fn is_test_class(&self, name: &str, ann: &[String], bases: &[String]) -> bool {
        ["Test", "Tests", "Spec", "IT", "TestCase"].iter().any(|s| name.ends_with(s) && name.len() > s.len())
            || ann.iter().any(|a| a == "Nested" || a == "RunWith" || a == "ExtendWith")
            || bases.iter().any(|b| KOTEST_SPECS.contains(&last_segment(b)))
    }

    fn visit_class(&mut self, node: Node<'_>, depth: usize) {
        let name_node = child_of_kind(node, "type_identifier");
        let name = match (name_node, node.kind()) {
            (Some(n), _) => self.text(n).to_string(),
            (None, "companion_object") => "Companion".to_string(),
            _ => return,
        };
        let (keywords, annotations) = self.modifiers(node);
        let label = self.class_label(node, &keywords);
        let (bases, implements, super_calls) = self.supertypes(node, label);
        let ann_names: Vec<String> = annotations.iter().filter_map(|a| self.annotation_name(*a)).collect();
        let test_class = self.in_test || self.is_test_class(&name, &ann_names, &bases);
        let body = named_children(node).into_iter().find(|c| matches!(c.kind(), "class_body" | "enum_class_body"));
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let mut def = self.new_def(label, &name, self.child_qn(&name), node, name_node.unwrap_or(node));
        def.signature = Some(self.signature(node, end));
        def.decorators = self.decorator_texts(&annotations);
        def.bases = bases;
        def.implements = implements;
        def.exported = !keywords.contains("private");
        def.is_test = test_class;
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.visit_annotations(&annotations, depth);
        let mark = self.push_type_params(node, depth);
        self.classes.push(ClassCtx {
            def: id,
            name: name.clone(),
            props: HashMap::new(),
            prop_inits: HashMap::new(),
            methods: HashMap::new(),
            test_class,
            has_tests: false,
        });
        self.prescan_members(node, body);
        self.visit_class_parts(node, body, &super_calls, depth);
        let ctx = self.classes.pop().expect("class context");
        if ctx.has_tests {
            self.defs[id as usize].is_test = true;
        }
        self.type_params.truncate(mark);
        self.type_bounds.truncate(mark);
        self.enc = saved;
    }

    /// Records the class's properties and methods before its members are
    /// visited (a member may use one declared further down).
    fn prescan_members(&mut self, node: Node<'_>, body: Option<Node<'_>>) {
        if let Some(pc) = child_of_kind(node, "primary_constructor") {
            for p in named_children(pc).into_iter().filter(|p| p.kind() == "class_parameter") {
                self.prescan_class_parameter(p);
            }
        }
        for m in body.map(named_children).unwrap_or_default() {
            match m.kind() {
                "property_declaration" => self.prescan_member_property(m),
                "function_declaration" => self.prescan_method(m),
                _ => {}
            }
        }
    }

    /// A `val` / `var` primary-constructor parameter: a property.
    fn prescan_class_parameter(&mut self, p: Node<'_>) {
        if child_of_kind(p, "binding_pattern_kind").is_none() {
            return;
        }
        let Some(n) = child_of_kind(p, "simple_identifier") else { return };
        let class = declared_type(p).and_then(|t| class_of_type(t, self.source));
        let name = self.text(n).to_string();
        self.cur_class().props.insert(name, class);
    }

    fn prescan_member_property(&mut self, m: Node<'_>) {
        let Some((n, c)) = self.property_name_and_class(m) else { return };
        if let Some(r) = self.untyped_init(m) {
            self.cur_class().prop_inits.insert(n.clone(), r);
        }
        self.cur_class().props.insert(n, c);
    }

    fn prescan_method(&mut self, m: Node<'_>) {
        let Some(n) = child_of_kind(m, "simple_identifier") else { return };
        let ret = self.return_class(m);
        let name = self.text(n);
        merge_return(&mut self.cur_class().methods, name, ret);
    }

    pub(crate) fn cur_class(&mut self) -> &mut ClassCtx {
        self.classes.last_mut().expect("inside a class")
    }

    fn visit_class_parts(&mut self, node: Node<'_>, body: Option<Node<'_>>, super_calls: &[Node<'_>], depth: usize) {
        let counts = self.callable_counts(node, body);
        for d in named_children(node).into_iter().filter(|c| c.kind() == "delegation_specifier") {
            self.visit_delegation_types(d, depth);
        }
        let ctor = self.visit_primary_ctor(node, &counts, depth);
        let saved = self.enc;
        if let Some(c) = ctor {
            self.enc = Some(c);
        }
        for call in super_calls {
            self.visit_super_call(*call, depth);
        }
        for d in named_children(node).into_iter().filter(|c| c.kind() == "delegation_specifier") {
            if let Some(e) = child_of_kind(d, "explicit_delegation") {
                named_children(e).into_iter().filter(|c| !is_type_node(c.kind())).for_each(|c| self.visit(c, depth));
            }
        }
        self.enc = saved;
        if let Some(b) = body {
            self.visit_body(b, &counts, depth);
        }
    }

    /// Supertype references (`Type`; the resolver turns a class's own base
    /// into `INHERITS` / `IMPLEMENTS`) with their generic arguments.
    fn visit_delegation_types(&mut self, d: Node<'_>, depth: usize) {
        let Some(inner) = d.named_child(0) else { return };
        let ty = match inner.kind() {
            "constructor_invocation" => child_of_kind(inner, "user_type"),
            "explicit_delegation" => declared_type(inner),
            k if is_type_node(k) => Some(inner),
            _ => None,
        };
        if let Some(t) = ty {
            self.visit_type(t, depth);
        }
    }

    /// `Base(args)` in a supertype list: a call of the superclass
    /// constructor.
    fn visit_super_call(&mut self, inv: Node<'_>, depth: usize) {
        let Some(ty) = child_of_kind(inv, "user_type") else { return };
        let Some(class) = class_of_type(ty, self.source) else { return };
        let args = child_of_kind(inv, "value_arguments");
        let arity = args.map(|a| named_children(a).iter().filter(|c| c.kind() == "value_argument").count() as u32);
        let (receiver, name) = match class.rsplit_once('.') {
            Some((r, n)) => (Some(r.to_string()), n.to_string()),
            None => (None, class.clone()),
        };
        let named = self.with_args(&name, args);
        self.push_ref(RefKind::Call, &named, receiver, span_of(ty), arity.or(Some(0)));
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
    }

    /// Callables per name declared by a class (functions; constructors
    /// under the class name), for overload suffixes.
    fn callable_counts(&self, node: Node<'_>, body: Option<Node<'_>>) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let class_name = child_of_kind(node, "type_identifier").map(|n| self.text(n).to_string()).unwrap_or_default();
        if child_of_kind(node, "primary_constructor").is_some() {
            *counts.entry(class_name.clone()).or_default() += 1;
        }
        for m in body.map(named_children).unwrap_or_default() {
            match m.kind() {
                "function_declaration" => {
                    if let Some(n) = child_of_kind(m, "simple_identifier") {
                        *counts.entry(self.text(n).to_string()).or_default() += 1;
                    }
                }
                "secondary_constructor" => *counts.entry(class_name.clone()).or_default() += 1,
                _ => {}
            }
        }
        counts
    }

    fn visit_body(&mut self, body: Node<'_>, counts: &HashMap<String, usize>, depth: usize) {
        for m in named_children(body) {
            match m.kind() {
                "function_declaration" => {
                    let overloaded = self.name_count(m, counts) > 1;
                    self.visit_function_as(m, overloaded, depth + 1);
                }
                "secondary_constructor" => {
                    let class = self.classes.last().map(|c| c.name.clone()).unwrap_or_default();
                    let overloaded = counts.get(&class).copied().unwrap_or(0) > 1;
                    self.visit_ctor(m, overloaded, depth + 1);
                }
                // `init { … }` belongs to the class, like a Java initializer.
                "anonymous_initializer" => self.visit_children(m, depth + 1),
                "enum_entry" => self.visit_enum_entry(m, depth + 1),
                _ => self.visit(m, depth + 1),
            }
        }
    }

    fn name_count(&self, node: Node<'_>, counts: &HashMap<String, usize>) -> usize {
        child_of_kind(node, "simple_identifier").and_then(|n| counts.get(self.text(n)).copied()).unwrap_or(0)
    }

    /// The primary constructor (`class C(…)`, `class C @Inject
    /// constructor(…)`): a method named like the class; its `val` / `var`
    /// parameters are properties.
    fn visit_primary_ctor(
        &mut self,
        node: Node<'_>,
        counts: &HashMap<String, usize>,
        depth: usize,
    ) -> Option<LocalDefId> {
        let Some(pc) = child_of_kind(node, "primary_constructor") else {
            return self.implicit_primary_ctor(node);
        };
        let class = self.classes.last()?.name.clone();
        let params: Vec<Node<'_>> = named_children(pc).into_iter().filter(|p| p.kind() == "class_parameter").collect();
        let overloaded = counts.get(&class).copied().unwrap_or(0) > 1;
        let suffix = overloaded.then(|| self.param_suffix(&params));
        let qn = format!("{}{}", self.child_qn(&class), suffix.unwrap_or_default());
        let (keywords, annotations) = self.modifiers(pc);
        let name_node = child_of_kind(node, "type_identifier").unwrap_or(pc);
        let mut def = self.new_def(Label::Method, &class, qn, pc, name_node);
        // The class header, like the implicit constructor's.
        def.span = span_between(node, pc);
        def.signature = Some(format!("constructor{}", collapse(&self.params_text(pc))));
        def.decorators = self.decorator_texts(&annotations);
        def.exported = !keywords.contains("private");
        def.doc = None;
        let id = self.push_def(def);
        for p in &params {
            self.visit_class_parameter(*p, id, depth);
        }
        Some(id)
    }

    /// The implicit primary constructor of a class without parameters
    /// (`class Foo { … }`): a method named like the class whose extent is
    /// the class header; none for interfaces, objects and classes that
    /// declare only secondary constructors.
    fn implicit_primary_ctor(&mut self, node: Node<'_>) -> Option<LocalDefId> {
        if node.kind() != "class_declaration" || has_keyword(node, "interface") {
            return None;
        }
        let body = named_children(node).into_iter().find(|c| matches!(c.kind(), "class_body" | "enum_class_body"));
        if body.is_some_and(|b| child_of_kind(b, "secondary_constructor").is_some()) {
            return None;
        }
        let name_node = child_of_kind(node, "type_identifier")?;
        let class = self.classes.last()?.name.clone();
        let header_end = named_children(node)
            .into_iter()
            .take_while(|c| !matches!(c.kind(), "class_body" | "enum_class_body"))
            .last()
            .unwrap_or(name_node);
        let mut def = self.new_def(Label::Method, &class, self.child_qn(&class), node, name_node);
        def.span = span_between(node, header_end);
        def.signature = Some("constructor()".to_string());
        def.exported = !self.modifiers(node).0.contains("private");
        def.doc = None;
        Some(self.push_def(def))
    }

    /// `(…)` of a parameter list, as written.
    fn params_text(&self, node: Node<'_>) -> String {
        let s = self.text(node);
        match s.find('(') {
            Some(i) => s[i..].to_string(),
            None => String::new(),
        }
    }

    /// A primary-constructor parameter: a property (child of the class)
    /// when declared `val` / `var`; its type and default value belong to
    /// the constructor.
    fn visit_class_parameter(&mut self, p: Node<'_>, ctor: LocalDefId, depth: usize) {
        let saved = self.enc;
        let field = self.class_parameter_field(p, ctor);
        self.enc = Some(field.unwrap_or(ctor));
        for t in named_children(p).into_iter().filter(|c| is_type_node(c.kind())) {
            self.visit_type(t, depth);
        }
        self.enc = Some(ctor);
        for c in named_children(p) {
            if !is_type_node(c.kind())
                && !matches!(c.kind(), "simple_identifier" | "binding_pattern_kind" | "modifiers")
            {
                self.visit(c, depth);
            }
        }
        self.enc = saved;
    }

    /// The property a `val` / `var` primary-constructor parameter declares
    /// (a child of the class), with its annotations.
    fn class_parameter_field(&mut self, p: Node<'_>, ctor: LocalDefId) -> Option<LocalDefId> {
        let (keywords, annotations) = self.modifiers(p);
        if child_of_kind(p, "binding_pattern_kind").is_none() {
            let saved = self.enc;
            self.enc = Some(ctor);
            for a in &annotations {
                self.visit_annotation(*a, false, 0);
            }
            self.visit_bridge_annotations(&annotations);
            self.enc = saved;
            return None;
        }
        let n = child_of_kind(p, "simple_identifier")?;
        let name = self.text(n).to_string();
        let mut def = self.new_def(Label::Field, &name, self.child_qn(&name), p, n);
        def.signature = Some(self.signature(p, p.end_byte()));
        def.decorators = self.decorator_texts(&annotations);
        def.exported = !keywords.contains("private");
        def.doc = None;
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.visit_annotations(&annotations, 0);
        self.visit_bridge_annotations(&annotations);
        self.enc = saved;
        Some(id)
    }

    /// Overload suffix of a parameter list: `(Long,Int)`, varargs `T...`.
    fn param_suffix(&self, params: &[Node<'_>]) -> String {
        let types: Vec<String> = params
            .iter()
            .map(|p| {
                let t = declared_type(*p).map_or_else(|| "?".to_string(), |t| suffix_type(t, self.source));
                let vararg = self.modifiers(*p).0.contains("vararg") || self.prev_is_vararg(*p);
                if vararg { format!("{t}...") } else { t }
            })
            .collect();
        format!("({})", types.join(","))
    }

    /// `vararg` written as a sibling `parameter_modifiers` node before the
    /// parameter (function parameters).
    fn prev_is_vararg(&self, p: Node<'_>) -> bool {
        p.prev_named_sibling()
            .filter(|s| s.kind() == "parameter_modifiers")
            .is_some_and(|s| self.text(s).split_whitespace().any(|w| w == "vararg"))
    }

    fn visit_secondary_ctor(&mut self, node: Node<'_>, depth: usize) {
        self.visit_ctor(node, false, depth);
    }

    /// `constructor(…) : this(…) { … }`: a method named like the class.
    fn visit_ctor(&mut self, node: Node<'_>, overloaded: bool, depth: usize) {
        let Some(class) = self.classes.last().map(|c| c.name.clone()) else { return };
        let params = self.function_params(node);
        let suffix = overloaded.then(|| self.param_suffix(&params));
        let qn = format!("{}{}", self.child_qn(&class), suffix.unwrap_or_default());
        let (keywords, annotations) = self.modifiers(node);
        let body = child_of_kind(node, "statements");
        let end =
            child_of_kind(node, "constructor_delegation_call").or(body).map_or(node.end_byte(), |b| b.start_byte());
        let mut def = self.new_def(Label::Method, &class, qn, node, node);
        def.signature = Some(self.signature(node, end).trim_end_matches(':').trim().to_string());
        def.decorators = self.decorator_texts(&annotations);
        def.exported = !keywords.contains("private");
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.visit_annotations(&annotations, depth);
        self.visit_params(node, depth);
        if let Some(d) = child_of_kind(node, "constructor_delegation_call") {
            self.visit_delegation_call(d, &class, depth);
        }
        for c in named_children(node).into_iter().filter(|c| c.kind() == "statements") {
            self.visit_children(c, depth);
        }
        self.enc = saved;
    }

    /// `: this(…)` / `: super(…)`: a call of this class's / the
    /// superclass's constructor.
    fn visit_delegation_call(&mut self, d: Node<'_>, class: &str, depth: usize) {
        let args = child_of_kind(d, "value_arguments");
        let arity = args.map(|a| named_children(a).iter().filter(|c| c.kind() == "value_argument").count() as u32);
        let receiver = if has_keyword(d, "super") { "super" } else { "this" };
        let name = if receiver == "this" {
            class.to_string()
        } else {
            // A class with only secondary constructors writes its
            // superclass without arguments: the first supertype then.
            let class_def = &self.defs[self.classes.last().map_or(0, |c| c.def) as usize];
            let sup = class_def.bases.first().or(class_def.implements.first()).cloned();
            sup.map(|s| last_segment(&s).to_string()).unwrap_or_default()
        };
        let named = self.with_args(&name, args);
        self.push_ref(RefKind::Call, &named, Some(receiver.to_string()), span_of(d), arity.or(Some(0)));
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
    }

    /// Enum entry: a constant of the enum; its arguments call the enum's
    /// constructor; a body's members are its children.
    fn visit_enum_entry(&mut self, node: Node<'_>, depth: usize) {
        let Some(n) = child_of_kind(node, "simple_identifier") else { return };
        let name = self.text(n).to_string();
        let (_, annotations) = self.modifiers(node);
        let mut def = self.new_def(Label::Constant, &name, self.child_qn(&name), node, n);
        def.decorators = self.decorator_texts(&annotations);
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.visit_annotations(&annotations, depth);
        if let Some(args) = child_of_kind(node, "value_arguments") {
            let enum_name = self.classes.last().map(|c| c.name.clone()).unwrap_or_default();
            let arity = named_children(args).iter().filter(|c| c.kind() == "value_argument").count() as u32;
            let named = self.with_args(&enum_name, Some(args));
            self.push_ref(RefKind::Call, &named, None, span_of(n), Some(arity));
            self.visit_children(args, depth);
        }
        if let Some(body) = child_of_kind(node, "class_body") {
            let counts = HashMap::new();
            self.visit_body(body, &counts, depth);
        }
        self.enc = saved;
    }

    /// An anonymous object (`object : T { … }`): a class named `T$N` (the
    /// `N`-th anonymous `T` of its enclosing definition), child of that
    /// definition. `members` is the body when the grammar parsed it as a
    /// trailing lambda.
    pub(crate) fn visit_object_literal(&mut self, node: Node<'_>, members: Option<Node<'_>>, depth: usize) {
        let (bases, implements, super_calls) = self.supertypes(node, Label::Class);
        let base = bases.first().or(implements.first()).map(|b| last_segment(b).to_string());
        let base = base.unwrap_or_else(|| "Object".to_string());
        let n = self.anon_counts.entry((self.enc, base.clone())).or_default();
        *n += 1;
        let name = format!("{base}${n}");
        let body = child_of_kind(node, "class_body").or(members);
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let mut def = self.new_def(Label::Class, &name, self.child_qn(&name), node, node);
        def.signature = Some(self.signature(node, end));
        def.bases = bases;
        def.implements = implements;
        def.exported = false;
        def.doc = None;
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.classes.push(ClassCtx {
            def: id,
            name,
            props: HashMap::new(),
            prop_inits: HashMap::new(),
            methods: HashMap::new(),
            test_class: self.in_test,
            has_tests: false,
        });
        self.prescan_members(node, body);
        self.visit_class_parts(node, None, &super_calls, depth);
        if let Some(b) = body {
            let counts = self.callable_counts(node, Some(b));
            self.visit_body(b, &counts, depth);
        }
        self.classes.pop();
        self.enc = saved;
    }

    // -- functions -------------------------------------------------------------------

    fn function_params<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        child_of_kind(node, "function_value_parameters")
            .map(|ps| named_children(ps).into_iter().filter(|p| p.kind() == "parameter").collect())
            .unwrap_or_default()
    }

    /// The declared return type node of a function (the type after its
    /// parameter list).
    fn return_type<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        let params = child_of_kind(node, "function_value_parameters")?;
        named_children(node).into_iter().find(|c| c.start_byte() >= params.end_byte() && is_type_node(c.kind()))
    }

    /// The class a function returns: its declared return type, else the
    /// class an expression body constructs (`fun f() = Foo()`).
    pub(crate) fn return_class(&self, node: Node<'_>) -> Option<String> {
        if let Some(t) = self.return_type(node) {
            return class_of_type(t, self.source);
        }
        let body = child_of_kind(node, "function_body")?;
        if self.text(body).trim_start().starts_with('{') {
            return None;
        }
        let expr = body.named_child(0)?;
        locals::initializer(expr, self.source).0
    }

    fn visit_function(&mut self, node: Node<'_>, depth: usize) {
        let top = self.enc.is_none();
        let overloaded = top
            && child_of_kind(node, "simple_identifier")
                .is_some_and(|n| self.top_counts.get(&self.extension_qn(node, self.text(n))).copied().unwrap_or(0) > 1);
        self.visit_function_as(node, overloaded, depth);
    }

    fn function_label(&self) -> Label {
        match (self.enc, self.classes.last()) {
            (Some(e), Some(c)) if c.def == e => Label::Method,
            _ => Label::Function,
        }
    }

    fn is_test_function(&self, ann: &[String]) -> bool {
        let test_class = self.classes.last().is_some_and(|c| c.test_class);
        ann.iter().any(|a| TEST_ANNOTATIONS.contains(&a.as_str()))
            || (test_class && ann.iter().any(|a| LIFECYCLE_ANNOTATIONS.contains(&a.as_str())))
    }

    fn visit_function_as(&mut self, node: Node<'_>, overloaded: bool, depth: usize) {
        let Some(name_node) = child_of_kind(node, "simple_identifier") else {
            return self.visit_children(node, depth);
        };
        let name = self.text(name_node).to_string();
        let (keywords, annotations) = self.modifiers(node);
        let ann_names: Vec<String> = annotations.iter().filter_map(|a| self.annotation_name(*a)).collect();
        let params = self.function_params(node);
        let suffix = overloaded.then(|| self.param_suffix(&params));
        let base = if self.enc.is_none() { self.extension_qn(node, &name) } else { self.child_qn(&name) };
        let qn = format!("{base}{}", suffix.unwrap_or_default());
        let body = child_of_kind(node, "function_body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let is_test = self.in_test && self.function_label() != Label::Method || self.is_test_function(&ann_names);
        let mut def = self.new_def(self.function_label(), &name, qn, node, name_node);
        def.signature = Some(self.signature(node, end));
        def.decorators = self.decorator_texts(&annotations);
        def.exported = !keywords.contains("private");
        def.is_test = is_test;
        def.is_declaration = body.is_none();
        if is_test && let Some(c) = self.classes.last_mut() {
            c.has_tests = true;
        }
        let id = self.push_def(def);
        let saved = (self.enc, self.in_test);
        self.enc = Some(id);
        self.in_test = is_test || self.in_test;
        self.visit_annotations(&annotations, depth);
        let mark = self.push_type_params(node, depth);
        self.visit_function_parts(node, depth);
        self.type_params.truncate(mark);
        self.type_bounds.truncate(mark);
        (self.enc, self.in_test) = saved;
    }

    fn visit_function_parts(&mut self, node: Node<'_>, depth: usize) {
        let receiver = child_of_kind(node, "receiver_type");
        if let Some(r) = receiver {
            self.visit_children(r, depth);
        }
        self.visit_params(node, depth);
        if let Some(t) = self.return_type(node) {
            self.visit_type(t, depth);
        }
        let Some(b) = child_of_kind(node, "function_body") else { return };
        // An extension function's body has its receiver as `this`.
        let mark = self.lambda_recv.len();
        let class = receiver
            .and_then(|r| named_children(r).into_iter().find(|c| is_type_node(c.kind())))
            .and_then(|t| class_of_type(t, self.source))
            .and_then(|c| self.bound_or_class(c));
        if let Some(c) = class {
            self.lambda_recv.push((b.start_byte(), b.end_byte(), c, false));
        }
        self.visit_children(b, depth);
        self.lambda_recv.truncate(mark);
    }

    /// A class as a receiver type: a type parameter in scope stands for its
    /// bound (`T : Named` → `Named`; none without one).
    pub(crate) fn bound_or_class(&self, class: String) -> Option<String> {
        match self.type_params.iter().rposition(|t| *t == class) {
            Some(i) => self.type_bounds[i].clone(),
            None => Some(class),
        }
    }

    /// Parameter types, annotations and default values.
    fn visit_params(&mut self, node: Node<'_>, depth: usize) {
        let Some(ps) = child_of_kind(node, "function_value_parameters") else { return };
        for c in named_children(ps) {
            match c.kind() {
                "parameter" => {
                    for t in named_children(c).into_iter().filter(|t| is_type_node(t.kind())) {
                        self.visit_type(t, depth);
                    }
                }
                "parameter_modifiers" => {
                    let anns: Vec<Node<'_>> =
                        named_children(c).into_iter().filter(|a| a.kind() == "annotation").collect();
                    for a in &anns {
                        self.visit_annotation(*a, false, depth);
                    }
                    self.visit_bridge_annotations(&anns);
                }
                _ => self.visit(c, depth),
            }
        }
    }

    // -- properties ------------------------------------------------------------------

    /// Name and class held of a property (declared type, else what the
    /// initializer constructs).
    pub(crate) fn property_name_and_class(&self, node: Node<'_>) -> Option<(String, Option<String>)> {
        let var = child_of_kind(node, "variable_declaration")?;
        let n = child_of_kind(var, "simple_identifier")?;
        let class = match declared_type(var) {
            Some(t) => class_of_type(t, self.source),
            None => self.property_initializer(node, var).and_then(|v| locals::initializer(v, self.source).0),
        };
        Some((self.text(n).to_string(), class))
    }

    fn property_initializer<'t>(&self, node: Node<'t>, var: Node<'t>) -> Option<Node<'t>> {
        named_children(node).into_iter().find(|c| {
            c.start_byte() > var.end_byte()
                && !matches!(c.kind(), "property_delegate" | "getter" | "setter" | "type_constraints" | "modifiers")
                && !is_comment(c.kind())
        })
    }

    fn property_label(&self, keywords: &HashSet<&str>, name: &str) -> Label {
        let in_class = matches!((self.enc, self.classes.last()), (Some(e), Some(c)) if c.def == e);
        let screaming = name.chars().all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
            && name.chars().any(char::is_uppercase);
        if keywords.contains("const") || (screaming && has_val(keywords)) {
            Label::Constant
        } else if in_class {
            Label::Field
        } else {
            Label::Variable
        }
    }

    /// A property of a class, object or file (local `val` / `var` in a body
    /// are not definitions: their initializers belong to the enclosing one).
    fn visit_property(&mut self, node: Node<'_>, depth: usize) {
        let member =
            node.parent().is_some_and(|p| matches!(p.kind(), "class_body" | "source_file" | "enum_class_body"));
        if !member {
            return self.visit_local_property(node, depth);
        }
        let Some(var) = child_of_kind(node, "variable_declaration") else {
            return self.visit_local_property(node, depth);
        };
        let Some(n) = child_of_kind(var, "simple_identifier") else { return };
        let name = self.text(n).to_string();
        let (mut keywords, annotations) = self.modifiers(node);
        if let Some(k) = child_of_kind(node, "binding_pattern_kind") {
            keywords.insert(self.text(k));
        }
        let end = named_children(node)
            .into_iter()
            .find(|c| matches!(c.kind(), "getter" | "setter" | "property_delegate"))
            .map_or(node.end_byte(), |c| c.start_byte());
        let mut def = self.new_def(self.property_label(&keywords, &name), &name, self.child_qn(&name), node, n);
        def.signature = Some(self.signature(node, end));
        def.decorators = self.decorator_texts(&annotations);
        def.exported = !keywords.contains("private");
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.visit_annotations(&annotations, depth);
        self.visit_bridge_annotations(&annotations);
        let mark = self.push_type_params(node, depth);
        self.visit_property_parts(node, depth);
        self.type_params.truncate(mark);
        self.type_bounds.truncate(mark);
        self.enc = saved;
    }

    fn visit_property_parts(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            match c.kind() {
                "modifiers" | "binding_pattern_kind" | "type_parameters" | "type_constraints" => {}
                "receiver_type" => self.visit_children(c, depth),
                "variable_declaration" | "multi_variable_declaration" => self.visit_declared_types(c, depth),
                _ => self.visit(c, depth),
            }
        }
    }

    /// Types written in a (possibly destructuring) variable declaration.
    pub(crate) fn visit_declared_types(&mut self, var: Node<'_>, depth: usize) {
        for c in named_children(var) {
            match c.kind() {
                "variable_declaration" => self.visit_declared_types(c, depth),
                k if is_type_node(k) => self.visit_type(c, depth),
                _ => {}
            }
        }
    }

    /// A local `val` / `var`: its type and initializer belong to the
    /// enclosing definition.
    fn visit_local_property(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            match c.kind() {
                "modifiers" => {
                    let anns: Vec<Node<'_>> =
                        named_children(c).into_iter().filter(|a| a.kind() == "annotation").collect();
                    for a in anns {
                        self.visit_annotation(a, false, depth);
                    }
                }
                "binding_pattern_kind" => {}
                "variable_declaration" | "multi_variable_declaration" => self.visit_declared_types(c, depth),
                _ => self.visit(c, depth),
            }
        }
    }

    fn visit_type_alias(&mut self, node: Node<'_>, depth: usize) {
        let Some(n) = child_of_kind(node, "type_identifier") else { return };
        let name = self.text(n).to_string();
        let (keywords, annotations) = self.modifiers(node);
        let mut def = self.new_def(Label::TypeAlias, &name, self.child_qn(&name), node, n);
        def.signature = Some(self.signature(node, node.end_byte()));
        def.decorators = self.decorator_texts(&annotations);
        def.exported = !keywords.contains("private");
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        let mark = self.push_type_params(node, depth);
        for t in named_children(node).into_iter().filter(|t| is_type_node(t.kind())) {
            self.visit_type(t, depth);
        }
        self.type_params.truncate(mark);
        self.type_bounds.truncate(mark);
        self.enc = saved;
    }
}

/// The span from the start of `a` to the end of `b`.
fn span_between(a: Node<'_>, b: Node<'_>) -> Span {
    let (s, e) = (span_of(a), span_of(b));
    Span {
        start_byte: s.start_byte,
        end_byte: e.end_byte,
        start_line: s.start_line,
        start_col: s.start_col,
        end_line: e.end_line,
        end_col: e.end_col,
    }
}

fn has_val(keywords: &HashSet<&str>) -> bool {
    keywords.contains("val") || keywords.contains("const")
}

/// Records a function's return class; overloads that disagree forget it.
fn merge_return(map: &mut HashMap<String, Option<String>>, name: &str, ret: Option<String>) {
    match map.get(name) {
        Some(old) if *old != ret => {
            map.insert(name.to_string(), None);
        }
        Some(_) => {}
        None => {
            map.insert(name.to_string(), ret);
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn import_paths() {
        assert_eq!(split_import("a.b.C"), ("a.b".to_string(), "C".to_string()));
        assert_eq!(split_import("a.b.C.Inner"), ("a.b".to_string(), "C.Inner".to_string()));
        assert_eq!(split_import("kotlin.math.max"), ("kotlin.math".to_string(), "max".to_string()));
        assert_eq!(split_import("Top"), (String::new(), "Top".to_string()));
    }

    #[test]
    fn kdoc() {
        assert_eq!(clean_kdoc("/**\n * Hello.\n */").as_deref(), Some("Hello."));
        assert_eq!(clean_kdoc("/* no */"), None);
    }
}
