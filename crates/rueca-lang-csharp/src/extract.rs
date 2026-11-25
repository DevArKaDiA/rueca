//! Syntax-tree walk producing [`FileFacts`] for one C# file.
//!
//! A recursive visitor (depth-limited by `Budget::max_depth`) keeps the
//! namespace being walked, the enclosing definition, the stack of enclosing
//! named types (with their fields, properties and base class) and the
//! generic type parameters in scope. Local variables and their declared
//! types come from the [`Locals`] table built by the vendored
//! `queries/locals.scm`. Definitions are here; expressions, references and
//! receivers in `refs.rs`; the configuration bridge in `bridge.rs`. See the
//! crate docs for the mapping rules.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::{self, Node};
use rueca_extract::{Budget, ExtractError, count_errors, line_count, span_of};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, Locals};
use crate::types::{
    children_with_fields, class_of_type, named_children, parameter_types, strip_type_text, suffix_type, text,
};

/// Hard cap on references per file.
pub const MAX_REFS: usize = 20_000;

/// Maximum characters of a signature.
const MAX_SIGNATURE: usize = 200;
/// Maximum characters of a doc comment.
const MAX_DOC: usize = 1000;
/// Maximum characters of a raw (untyped) receiver text.
pub(crate) const MAX_RECEIVER: usize = 200;
/// Maximum names bound per file through `using static` directives.
const MAX_STATIC_NAMES: usize = 512;

/// Test attributes on methods: xUnit, NUnit, MSTest (and their common
/// extensions).
const TEST_ATTRIBUTES: &[&str] = &[
    "DataTestMethod",
    "Fact",
    "SkippableFact",
    "SkippableTheory",
    "Test",
    "TestCase",
    "TestCaseSource",
    "TestMethod",
    "Theory",
];
/// Fixture attributes (NUnit, MSTest): the method is test code.
const FIXTURE_ATTRIBUTES: &[&str] = &[
    "AssemblyCleanup",
    "AssemblyInitialize",
    "ClassCleanup",
    "ClassInitialize",
    "OneTimeSetUp",
    "OneTimeTearDown",
    "SetUp",
    "TearDown",
    "TestCleanup",
    "TestInitialize",
];
/// Test class attributes (NUnit, MSTest).
const TEST_CLASS_ATTRIBUTES: &[&str] = &["TestClass", "TestFixture"];
/// Project directory suffixes of test projects (`Foo.Tests/…`).
const TEST_PROJECT_SUFFIXES: &[&str] =
    &[".Tests", ".Test", ".UnitTests", ".IntegrationTests", ".FunctionalTests", ".Specs", ".Spec"];

pub(crate) fn extract(path: &str, original: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let mut tree = rueca_extract::parse(&language, original, budget)?;
    let mut source = std::borrow::Cow::Borrowed(original);
    // `#if` inside a declaration: one configuration parses better (preproc.rs).
    if count_errors(&tree) > 0
        && let Some(alt) = crate::preproc::first_branch(original)
    {
        let alt_tree = rueca_extract::parse(&language, &alt, budget)?;
        if count_errors(&alt_tree) < count_errors(&tree) {
            tree = alt_tree;
            source = std::borrow::Cow::Owned(alt);
        }
    }
    let source: &[u8] = &source;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time)?;

    let mut ex = Extractor::new(source, budget.max_depth, locals, is_test_path(path));
    ex.module = module_of(root, source, budget.max_depth);
    ex.prescan(root, "", 0);
    ex.record_module(root);
    ex.visit_unit(root, String::new(), 0);
    ex.bind_using_static();
    let contracts = Vec::new();

    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
        contracts,
    })
}

/// Whether a path is in a test project (`Foo.Tests/…`, `test/`, `tests/`).
pub fn is_test_path(path: &str) -> bool {
    path.split('/').rev().skip(1).any(|d| {
        matches!(d, "test" | "tests" | "Test" | "Tests") || TEST_PROJECT_SUFFIXES.iter().any(|s| d.ends_with(s))
    })
}

/// A named type declared in the file.
pub(crate) struct FileType {
    /// Local qualified name (`Parser.Options`).
    pub(crate) qn: String,
    pub(crate) name: String,
    /// Index of the enclosing file type.
    pub(crate) parent: Option<usize>,
}

/// Declared return type of a method.
#[derive(Clone, PartialEq)]
pub(crate) struct Returns {
    /// The class for receivers; `None` for value keywords and arrays.
    pub(crate) class: Option<String>,
    /// Overload-suffix form (`int`, `List`, `byte[]`), for argument types.
    pub(crate) ty: String,
}

/// A named type being visited.
pub(crate) struct TypeCtx {
    pub(crate) def: LocalDefId,
    pub(crate) name: String,
    /// Field / property / event name → declared class (`None` for value
    /// keywords and arrays).
    pub(crate) fields: HashMap<String, Option<String>>,
    /// Field / property name → declared type in overload-suffix form.
    pub(crate) field_types: HashMap<String, String>,
    /// Simple name of the base class, if one is declared.
    pub(crate) superclass: Option<String>,
    /// Members are implicitly public (interfaces).
    pub(crate) implicit_public: bool,
    /// Annotated as a test class (`[TestClass]`, `[TestFixture]`).
    pub(crate) test_class: bool,
    /// At least one member is a test.
    pub(crate) has_tests: bool,
    /// Index into `file_types` (`None` for types without one).
    pub(crate) file_type: Option<usize>,
    /// Callable members by name → their declared return type when every
    /// overload declares the same one.
    pub(crate) methods: HashMap<String, Option<Returns>>,
    /// Declared inside test code.
    pub(crate) in_test: bool,
    /// Some members may be declared in another file: the type is `partial`
    /// or has a base class.
    pub(crate) members_elsewhere: bool,
}

pub(crate) struct Extractor<'s> {
    pub(crate) source: &'s [u8],
    pub(crate) max_depth: usize,
    pub(crate) defs: Vec<Definition>,
    pub(crate) refs: Vec<Reference>,
    pub(crate) imports: Vec<Import>,
    /// Dedup keys for `Read` / `Write` / `Type` / `Throw`.
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    /// Innermost enclosing definition.
    pub(crate) enc: Option<LocalDefId>,
    pub(crate) types: Vec<TypeCtx>,
    /// Generic type parameters in scope.
    pub(crate) type_params: Vec<String>,
    /// First class constraint of each type parameter in `type_params`
    /// (`where T : IEntity` → `IEntity`), the receiver type of its values.
    pub(crate) type_bounds: Vec<Option<String>>,
    pub(crate) locals: Locals,
    /// Named types declared in this file (not local to a body).
    pub(crate) file_types: Vec<FileType>,
    /// The module: the namespace shared by every type of the file.
    pub(crate) module: String,
    /// Using aliases: alias → target (`IO` → `System.IO`).
    pub(crate) aliases: HashMap<String, String>,
    /// Namespaces imported by `using` (and `global using`).
    pub(crate) usings: HashSet<String>,
    /// Types imported by `using static`: (namespace, type, span).
    static_usings: Vec<(String, String, Span)>,
    /// Every type name declared in this file.
    pub(crate) declared_types: HashSet<String>,
    /// The file is in a test project.
    test_path: bool,
}

/// Namespace path of a `namespace` declaration's name node.
fn namespace_name(node: Node<'_>, source: &[u8]) -> String {
    node.child_by_field_name("name").map(|n| strip_type_text(text(n, source))).unwrap_or_default()
}

/// `outer.inner`, or either alone.
pub(crate) fn join_ns(outer: &str, inner: &str) -> String {
    match (outer.is_empty(), inner.is_empty()) {
        (true, _) => inner.to_string(),
        (_, true) => outer.to_string(),
        _ => format!("{outer}.{inner}"),
    }
}

pub(crate) fn is_type_decl(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "delegate_declaration"
    )
}

pub(crate) fn is_comment(kind: &str) -> bool {
    kind == "comment"
}

/// Collapses whitespace runs to one space.
pub(crate) fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// The namespace every type of the file shares: the longest common prefix
/// (by segments) of the namespaces that declare a type; the first declared
/// namespace when the file declares no type.
fn module_of(root: Node<'_>, source: &[u8], max_depth: usize) -> String {
    let mut found: Vec<String> = Vec::new();
    let mut first: Option<String> = None;
    collect_type_namespaces(root, source, "", &mut found, &mut first, max_depth);
    let Some((head, rest)) = found.split_first() else { return first.unwrap_or_default() };
    let mut common: Vec<&str> = if head.is_empty() { Vec::new() } else { head.split('.').collect() };
    for ns in rest {
        let segs: Vec<&str> = if ns.is_empty() { Vec::new() } else { ns.split('.').collect() };
        let n = common.iter().zip(&segs).take_while(|(a, b)| a == b).count();
        common.truncate(n);
    }
    common.join(".")
}

/// Namespaces of the top-level types under `node` (namespace bodies,
/// file-scoped namespaces and `#if` blocks walked).
fn collect_type_namespaces(
    node: Node<'_>,
    source: &[u8],
    ns: &str,
    found: &mut Vec<String>,
    first: &mut Option<String>,
    depth: usize,
) {
    if depth == 0 {
        return;
    }
    let mut cur = ns.to_string();
    for c in named_children(node) {
        match c.kind() {
            "namespace_declaration" => {
                let inner = join_ns(ns, &namespace_name(c, source));
                first.get_or_insert_with(|| inner.clone());
                if let Some(body) = c.child_by_field_name("body") {
                    collect_type_namespaces(body, source, &inner, found, first, depth - 1);
                }
            }
            "file_scoped_namespace_declaration" => {
                cur = join_ns(ns, &namespace_name(c, source));
                first.get_or_insert_with(|| cur.clone());
            }
            "preproc_if" | "preproc_elif" | "preproc_else" => {
                collect_type_namespaces(c, source, &cur, found, first, depth - 1);
            }
            k if is_type_decl(k) => found.push(cur.clone()),
            _ => {}
        }
    }
}

impl<'s> Extractor<'s> {
    fn new(source: &'s [u8], max_depth: usize, locals: Locals, test_path: bool) -> Self {
        Extractor {
            source,
            max_depth,
            defs: Vec::new(),
            refs: Vec::new(),
            imports: Vec::new(),
            seen_refs: HashSet::new(),
            enc: None,
            types: Vec::new(),
            type_params: Vec::new(),
            type_bounds: Vec::new(),
            locals,
            file_types: Vec::new(),
            module: String::new(),
            aliases: HashMap::new(),
            usings: HashSet::new(),
            static_usings: Vec::new(),
            declared_types: HashSet::new(),
            test_path,
        }
    }

    pub(crate) fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.source)
    }

    // -----------------------------------------------------------------------
    // Pre-scan: usings, declared types
    // -----------------------------------------------------------------------

    /// Records `using` directives and the file's types (with their local
    /// qualified names), walking namespaces and `#if` blocks.
    fn prescan(&mut self, node: Node<'_>, ns: &str, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let mut cur = ns.to_string();
        for c in named_children(node) {
            match c.kind() {
                "using_directive" => self.using(c),
                "namespace_declaration" => {
                    let inner = join_ns(ns, &namespace_name(c, self.source));
                    if let Some(body) = c.child_by_field_name("body") {
                        self.prescan(body, &inner, depth + 1);
                    }
                }
                "file_scoped_namespace_declaration" => cur = join_ns(ns, &namespace_name(c, self.source)),
                "preproc_if" | "preproc_elif" | "preproc_else" => self.prescan(c, &cur, depth + 1),
                k if is_type_decl(k) => {
                    let prefix = self.local_prefix(&cur);
                    self.prescan_type(c, None, &prefix, depth + 1);
                }
                _ => {}
            }
        }
    }

    /// The namespace segments of `ns` below the module (`Shop` for
    /// `Acme.Shop` in module `Acme`), the local-name prefix of its types.
    pub(crate) fn local_prefix(&self, ns: &str) -> String {
        if self.module.is_empty() {
            return ns.to_string();
        }
        match ns.strip_prefix(self.module.as_str()) {
            Some("") => String::new(),
            Some(rest) => rest.trim_start_matches('.').to_string(),
            None => ns.to_string(),
        }
    }

    /// Records a type declaration and its member types.
    fn prescan_type(&mut self, node: Node<'_>, parent: Option<usize>, prefix: &str, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        self.declared_types.insert(name.clone());
        let qn = match parent {
            Some(p) => format!("{}.{name}", self.file_types[p].qn),
            None => join_ns(prefix, &name),
        };
        self.file_types.push(FileType { qn, name, parent });
        let me = self.file_types.len() - 1;
        let Some(body) = node.child_by_field_name("body") else { return };
        for m in member_nodes(body) {
            if is_type_decl(m.kind()) {
                self.prescan_type(m, Some(me), prefix, depth + 1);
            }
        }
    }

    /// The file type a simple type name denotes at this point: an enclosing
    /// type, a member type of an enclosing type (innermost first), else a
    /// top-level type of the file.
    pub(crate) fn visible_type(&self, name: &str) -> Option<usize> {
        for t in self.types.iter().rev() {
            let Some(i) = t.file_type else { continue };
            if self.file_types[i].name == name {
                return Some(i);
            }
            if let Some(j) = self.file_types.iter().position(|f| f.parent == Some(i) && f.name == name) {
                return Some(j);
            }
        }
        self.file_types.iter().position(|f| f.parent.is_none() && f.name == name)
    }

    /// The module, recorded as the file's first import with no names (see
    /// the crate docs, *Imports*).
    fn record_module(&mut self, root: Node<'_>) {
        let span = named_children(root)
            .into_iter()
            .find(|c| matches!(c.kind(), "namespace_declaration" | "file_scoped_namespace_declaration"))
            .map_or_else(|| span_of(root), span_of);
        self.imports.insert(
            0,
            Import {
                module: self.module.clone(),
                relative_level: 0,
                names: Vec::new(),
                module_alias: None,
                type_only: false,
                reexport: false,
                span,
            },
        );
    }

    /// A `using` directive (crate docs, *Imports*).
    fn using(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        let keywords: Vec<&str> = node.children(&mut cursor).filter(|c| !c.is_named()).map(|c| c.kind()).collect();
        let is_global = keywords.contains(&"global");
        let is_static = keywords.contains(&"static");
        let alias = node.child_by_field_name("name").map(|n| self.text(n).to_string());
        let Some(target) = named_children(node)
            .into_iter()
            .find(|c| Some(*c) != node.child_by_field_name("name") && !is_comment(c.kind()))
        else {
            return;
        };
        let full = strip_type_text(self.text(target));
        let span = span_of(node);
        let import = match (alias, is_static) {
            (Some(a), _) => self.alias_import(&full, a),
            (None, true) => {
                let (module, class) = split_last(&full);
                self.static_usings.push((module.clone(), class.clone(), span));
                using_import(module, vec![ImportedName { name: format!("{class}.*"), alias: None }])
            }
            (None, false) => {
                self.usings.insert(full.clone());
                using_import(full, vec![ImportedName { name: "*".into(), alias: None }])
            }
        };
        self.imports.push(Import { reexport: is_global, span, ..import });
    }

    /// `using A = N.T;` (a type: names `T` as `A`) or `using A = N;` (a
    /// namespace: the module bound as `A`). Which one is unknown from the
    /// syntax: an alias whose last segment starts upper-case and whose
    /// target has a namespace is recorded as a type alias, and the resolver
    /// falls back to the namespace when no type is found (crate docs).
    fn alias_import(&mut self, full: &str, alias: String) -> Import {
        self.aliases.insert(alias.clone(), full.to_string());
        let (module, last) = split_last(full);
        if module.is_empty() {
            return Import { module_alias: Some(alias), ..using_import(full.to_string(), Vec::new()) };
        }
        using_import(module, vec![ImportedName { name: last, alias: Some(alias) }])
    }

    /// `using static N.T;` binds T's static members: every bare call or read
    /// gets a synthetic `N` import of `T.<name>`, per static using, in source
    /// order (crate docs, *Imports*). Members of enclosing and inherited
    /// types still win: implicit `this` is tried before imports.
    fn bind_using_static(&mut self) {
        if self.static_usings.is_empty() {
            return;
        }
        let names: std::collections::BTreeSet<String> = self
            .refs
            .iter()
            .filter(|r| r.receiver.is_none() && matches!(r.kind, RefKind::Call | RefKind::Read))
            .map(|r| rueca_extract::split_arg_types(&r.name).0)
            .map(str::to_string)
            .take(MAX_STATIC_NAMES)
            .collect();
        for name in names {
            for (module, class, span) in &self.static_usings {
                let names = vec![ImportedName { name: format!("{class}.{name}"), alias: None }];
                self.imports.push(Import { span: *span, ..using_import(module.clone(), names) });
            }
        }
    }

    // -----------------------------------------------------------------------
    // References (shared)
    // -----------------------------------------------------------------------

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
            RefKind::Throw => Some(3),
            _ => None,
        };
        if let Some(k) = dedup
            && !self.seen_refs.insert((k, self.enc, receiver.clone(), name.to_string()))
        {
            return;
        }
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing: self.enc, span, arity });
    }

    pub(crate) fn is_type_param(&self, name: &str) -> bool {
        self.type_params.iter().any(|t| t == name)
    }

    /// Declared class of field / property `name` of the innermost enclosing
    /// type that declares it: `Some(Some(class))`, `Some(None)` when untyped,
    /// `None` when no enclosing type declares it.
    pub(crate) fn field_class(&self, name: &str) -> Option<Option<String>> {
        self.types.iter().rev().find_map(|t| t.fields.get(name).cloned())
    }

    /// Declared type of a field / property of an enclosing type (suffix form).
    pub(crate) fn field_type(&self, name: &str) -> Option<String> {
        self.types.iter().rev().find(|t| t.fields.contains_key(name)).and_then(|t| t.field_types.get(name).cloned())
    }

    /// Declared return of method `name` of the innermost enclosing type that
    /// declares it (every overload must agree).
    pub(crate) fn method_returns_of(&self, name: &str) -> Option<Returns> {
        self.types.iter().rev().find_map(|t| t.methods.get(name)).cloned().flatten()
    }

    // -----------------------------------------------------------------------
    // Compilation unit and namespaces
    // -----------------------------------------------------------------------

    /// Members of a compilation unit, namespace body or `#if` block in
    /// namespace `ns` (a file-scoped namespace applies to what follows it).
    pub(crate) fn visit_unit(&mut self, node: Node<'_>, ns: String, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let mut cur = ns.clone();
        for c in named_children(node) {
            match c.kind() {
                "using_directive" | "extern_alias_directive" | "shebang_directive" => {}
                k if is_comment(k) => {}
                "namespace_declaration" => {
                    let inner = join_ns(&ns, &namespace_name(c, self.source));
                    if let Some(body) = c.child_by_field_name("body") {
                        self.visit_unit(body, inner, depth + 1);
                    }
                }
                "file_scoped_namespace_declaration" => cur = join_ns(&ns, &namespace_name(c, self.source)),
                "preproc_if" | "preproc_elif" | "preproc_else" => self.visit_unit(c, cur.clone(), depth + 1),
                k if is_type_decl(k) => self.visit_type_decl(c, &cur, depth + 1),
                // Top-level statements and assembly attributes run at file level.
                _ => self.visit(c, depth + 1),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Definitions
    // -----------------------------------------------------------------------

    /// Local qualified name of a definition named `name` inside the current
    /// enclosing definition, or of a top-level type in namespace `ns`.
    pub(crate) fn child_qn(&self, name: &str, ns: &str) -> String {
        match self.enc {
            Some(e) => format!("{}.{name}", self.defs[e as usize].qualified_name),
            None => join_ns(&self.local_prefix(ns), name),
        }
    }

    /// The `///` comment lines right before `node` (only whitespace
    /// between), markers and XML tags removed.
    fn doc(&self, node: Node<'_>) -> Option<String> {
        let mut lines = Vec::new();
        let mut cur = node;
        while let Some(prev) = cur.prev_sibling() {
            if prev.kind() != "comment" || !self.text(prev).starts_with("///") {
                break;
            }
            let between = &self.source[prev.end_byte()..cur.start_byte()];
            if !between.iter().all(u8::is_ascii_whitespace) {
                break;
            }
            lines.push(self.text(prev).trim_start_matches('/').trim().to_string());
            cur = prev;
        }
        lines.reverse();
        let doc = strip_xml_tags(&lines.join("\n"));
        let doc = doc.trim();
        (!doc.is_empty()).then(|| truncate_chars(doc.to_string(), MAX_DOC))
    }

    /// Declaration header from its first modifier to `end` (body start),
    /// attributes and comments removed, whitespace collapsed, ≤ 200 chars.
    /// A header that would be longer has its parameters' default values
    /// written `…` first, so the parameter list (and the arity the resolver
    /// reads from it) survives.
    fn signature(&self, node: Node<'_>, end: usize) -> String {
        let s = self.header_text(node, end, false);
        if s.chars().count() <= MAX_SIGNATURE {
            return s;
        }
        truncate_chars(self.header_text(node, end, true), MAX_SIGNATURE)
    }

    /// The header text of [`Self::signature`], default values elided or not.
    fn header_text(&self, node: Node<'_>, end: usize, elide_defaults: bool) -> String {
        let mut skips: Vec<(usize, usize, &str)> = Vec::new();
        header_skips(node, end, elide_defaults, &mut skips, 0);
        skips.sort_unstable();
        let mut out = String::new();
        let mut pos = node.start_byte();
        for (start, stop, with) in skips {
            if start < pos {
                continue;
            }
            out.push_str(&String::from_utf8_lossy(&self.source[pos..start.min(end)]));
            out.push_str(with);
            pos = stop;
        }
        if pos < end {
            out.push_str(&String::from_utf8_lossy(&self.source[pos..end]));
        }
        let s = collapse(out.trim().trim_end_matches(['{', ';', '=', '>']).trim_end_matches("=>"));
        collapse(s.trim_end_matches(['=', ' ']))
    }

    /// Modifier keywords and attributes of a declaration.
    pub(crate) fn modifiers<'t>(&self, node: Node<'t>) -> (HashSet<&'s str>, Vec<Node<'t>>) {
        let mut keywords: HashSet<&'s str> = HashSet::new();
        let mut attributes: Vec<Node<'t>> = Vec::new();
        for c in named_children(node) {
            match c.kind() {
                "modifier" => {
                    keywords.insert(self.text(c));
                }
                "attribute_list" => {
                    attributes.extend(named_children(c).into_iter().filter(|a| a.kind() == "attribute"));
                }
                _ => {}
            }
        }
        (keywords, attributes)
    }

    /// Simple names of attributes, without an `Attribute` suffix.
    fn attribute_names(&self, attributes: &[Node<'_>]) -> Vec<String> {
        attributes
            .iter()
            .filter_map(|a| a.child_by_field_name("name"))
            .map(|n| attribute_simple_name(&strip_type_text(self.text(n))).to_string())
            .collect()
    }

    fn decorator_texts(&self, attributes: &[Node<'_>]) -> Vec<String> {
        attributes.iter().map(|a| collapse(self.text(*a))).collect()
    }

    pub(crate) fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.defs.push(def);
        (self.defs.len() - 1) as LocalDefId
    }

    /// Whether the innermost type's members are implicitly public.
    fn in_implicit_public(&self) -> bool {
        self.types.last().is_some_and(|t| t.implicit_public)
            && self.enc.is_some_and(|e| Some(e) == self.types.last().map(|t| t.def))
    }

    /// Visible outside the declaring type: public, internal or protected
    /// (`internal` is visible across the assembly's namespaces); a type
    /// without modifiers is internal at top level, private when nested;
    /// interface members are public.
    fn exported(&self, keywords: &HashSet<&str>, top_level_type: bool) -> bool {
        if keywords.contains("private") && !keywords.contains("protected") {
            return false;
        }
        if keywords.contains("file") {
            return false;
        }
        keywords.contains("public")
            || keywords.contains("internal")
            || keywords.contains("protected")
            || top_level_type
            || self.in_implicit_public()
    }

    /// Whether code being visited is test code.
    pub(crate) fn in_test(&self) -> bool {
        self.test_path
            || self.enc.is_some_and(|e| self.defs[e as usize].is_test)
            || self.types.last().is_some_and(|t| t.in_test)
    }

    /// Binds a declaration's type parameters; returns the previous mark.
    fn push_type_params(&mut self, node: Node<'_>) -> usize {
        let mark = self.type_params.len();
        let tp = node
            .child_by_field_name("type_parameters")
            .or_else(|| named_children(node).into_iter().find(|c| c.kind() == "type_parameter_list"));
        if let Some(tp) = tp {
            for p in named_children(tp) {
                if let Some(n) = p.child_by_field_name("name") {
                    let name = self.text(n).to_string();
                    let bound = self.constraint_class(node, &name);
                    self.type_params.push(name);
                    self.type_bounds.push(bound);
                }
            }
        }
        mark
    }

    /// The first class constraint of type parameter `name` of a declaration
    /// (`where T : class, IEntity` → `IEntity`).
    fn constraint_class(&self, node: Node<'_>, name: &str) -> Option<String> {
        let clauses = named_children(node).into_iter().filter(|c| c.kind() == "type_parameter_constraints_clause");
        for clause in clauses {
            let parts = named_children(clause);
            if parts.first().is_none_or(|p| self.text(*p) != name) {
                continue;
            }
            return parts
                .iter()
                .filter(|k| k.kind() == "type_parameter_constraint")
                .filter_map(|k| k.child_by_field_name("type"))
                .find_map(|t| class_of_type(t, self.source).filter(|c| c != "object"));
        }
        None
    }

    /// Drops the type parameters bound after `mark`.
    pub(crate) fn pop_type_params(&mut self, mark: usize) {
        self.type_params.truncate(mark);
        self.type_bounds.truncate(mark);
    }

    /// Constraint types (`where T : IFoo, new()`).
    fn visit_constraints(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node).into_iter().filter(|c| c.kind() == "type_parameter_constraints_clause") {
            for k in named_children(c).into_iter().filter(|k| k.kind() == "type_parameter_constraint") {
                if let Some(t) = k.child_by_field_name("type") {
                    self.visit_type(t, depth + 1);
                }
            }
        }
    }

    fn type_label(&self, node: Node<'_>) -> Label {
        match node.kind() {
            "interface_declaration" => Label::Interface,
            "enum_declaration" => Label::Enum,
            "struct_declaration" => Label::Struct,
            "delegate_declaration" => Label::TypeAlias,
            "record_declaration" if self.has_keyword(node, "struct") => Label::Struct,
            _ => Label::Class,
        }
    }

    /// Whether `node` has the anonymous keyword child `kw`.
    fn has_keyword(&self, node: Node<'_>, kw: &str) -> bool {
        let mut cursor = node.walk();
        node.children(&mut cursor).any(|c| !c.is_named() && c.kind() == kw)
    }

    pub(crate) fn visit_type_decl(&mut self, node: Node<'_>, ns: &str, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = self.text(name_node).to_string();
        let label = self.type_label(node);
        let (keywords, attributes) = self.modifiers(node);
        let attr_names = self.attribute_names(&attributes);
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let (bases, implements) = self.base_lists(node, label);
        let test_class = attr_names.iter().any(|a| TEST_CLASS_ATTRIBUTES.contains(&a.as_str()));
        let in_test = self.in_test();
        let top_level = self.enc.is_none();
        let def = Definition {
            label,
            name: name.clone(),
            qualified_name: self.child_qn(&name, ns),
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.signature(node, end)),
            doc: self.doc(node),
            decorators: self.decorator_texts(&attributes),
            bases: bases.clone(),
            implements,
            exported: self.exported(&keywords, top_level),
            is_test: in_test || (test_class && label == Label::Class),
            is_declaration: false,
        };
        let id = self.push_def(def);
        let saved_enc = self.enc;
        self.enc = Some(id);
        for a in &attributes {
            self.visit_attribute(*a, true, depth + 1);
        }
        let mark = self.push_type_params(node);
        self.visit_base_list(node, label, depth + 1);
        self.visit_constraints(node, depth);
        let local_qn = self.defs[id as usize].qualified_name.clone();
        let file_type = self.file_types.iter().position(|f| f.qn == local_qn);
        let superclass = bases.first().filter(|_| label == Label::Class).map(|b| simple_name(b).to_string());
        let members_elsewhere = keywords.contains("partial") || superclass.is_some();
        self.types.push(TypeCtx {
            def: id,
            name: name.clone(),
            fields: HashMap::new(),
            field_types: HashMap::new(),
            superclass,
            implicit_public: label == Label::Interface,
            test_class,
            has_tests: false,
            file_type,
            methods: HashMap::new(),
            in_test,
            members_elsewhere,
        });
        self.visit_type_body(node, label, body, depth + 1);
        let ctx = self.types.pop().expect("pushed");
        if ctx.has_tests && label == Label::Class {
            self.defs[id as usize].is_test = true;
        }
        self.pop_type_params(mark);
        self.enc = saved_enc;
    }

    /// The parameters (records: properties; delegates: parameter types),
    /// then the members of a type.
    fn visit_type_body(&mut self, node: Node<'_>, label: Label, body: Option<Node<'_>>, depth: usize) {
        let params = named_children(node).into_iter().find(|c| c.kind() == "parameter_list");
        match (node.kind(), params) {
            ("record_declaration", Some(p)) => {
                self.collect_record_parameters(p);
                self.visit_record_parameters(p, depth);
            }
            ("delegate_declaration", _) => {
                if let Some(t) = node.child_by_field_name("type") {
                    self.visit_type(t, depth);
                }
                if let Some(p) = node.child_by_field_name("parameters") {
                    self.visit_parameters(p, depth);
                }
            }
            (_, Some(p)) => self.visit_parameters(p, depth),
            _ => {}
        }
        if let Some(b) = body {
            if label == Label::Enum {
                self.visit_enum_members(b, depth);
            } else {
                self.visit_body(b, depth);
            }
        }
    }

    /// Base and interface lists. Classes and records: the first type not
    /// named like an interface (`I` + upper case) is the base class, the
    /// others are interfaces; structs implement interfaces only; an
    /// interface's bases are interfaces it extends; an enum's is its
    /// underlying type (not a base).
    fn base_lists(&self, node: Node<'_>, label: Label) -> (Vec<String>, Vec<String>) {
        let types = self.base_types(node);
        match label {
            Label::Interface => (types, Vec::new()),
            Label::Enum | Label::TypeAlias => (Vec::new(), Vec::new()),
            Label::Struct => (Vec::new(), types),
            _ => {
                let mut bases = Vec::new();
                let mut implements = Vec::new();
                for (i, t) in types.into_iter().enumerate() {
                    if i == 0 && !looks_like_interface(&t) {
                        bases.push(t);
                    } else {
                        implements.push(t);
                    }
                }
                (bases, implements)
            }
        }
    }

    /// The type texts of a `base_list`, generic arguments removed.
    fn base_types(&self, node: Node<'_>) -> Vec<String> {
        let Some(list) = named_children(node).into_iter().find(|c| c.kind() == "base_list") else { return Vec::new() };
        named_children(list)
            .into_iter()
            .filter_map(|t| match t.kind() {
                "primary_constructor_base_type" => t.child_by_field_name("type"),
                "argument_list" => None,
                k if is_comment(k) => None,
                _ => Some(t),
            })
            .map(|t| {
                let text = strip_type_text(self.text(t));
                self.aliases.get(&text).cloned().unwrap_or(text)
            })
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Generic arguments of the bases (`: Base<Foo>`) as `Type` references,
    /// and a primary constructor's base call (`: Base(x)`).
    fn visit_base_list(&mut self, node: Node<'_>, label: Label, depth: usize) {
        let Some(list) = named_children(node).into_iter().find(|c| c.kind() == "base_list") else { return };
        let mut last_type: Option<Node<'_>> = None;
        for t in named_children(list) {
            match t.kind() {
                "argument_list" => self.base_call(last_type, t, depth),
                "primary_constructor_base_type" => {
                    let args = named_children(t).into_iter().find(|c| c.kind() == "argument_list");
                    if let (Some(ty), Some(a)) = (t.child_by_field_name("type"), args) {
                        self.visit_type_args(ty, depth);
                        self.base_call(Some(ty), a, depth);
                    }
                }
                k if is_comment(k) => {}
                _ => {
                    if label != Label::Enum {
                        self.visit_type_args(t, depth);
                    }
                    last_type = Some(t);
                }
            }
        }
    }

    /// `class C(int x) : Base(x)`: a call of the base constructor.
    fn base_call(&mut self, ty: Option<Node<'_>>, args: Node<'_>, depth: usize) {
        if let Some(t) = ty {
            let name = simple_name(&strip_type_text(self.text(t))).to_string();
            let named = self.with_args(&name, Some(args));
            self.push_ref(RefKind::Call, &named, Some("base".into()), span_of(t), Some(arity_of(args)));
        }
        self.visit_children(args, depth);
    }

    /// Record parameters are properties: recorded for receivers first.
    fn collect_record_parameters(&mut self, params: Node<'_>) {
        for p in named_children(params).into_iter().filter(|p| p.kind() == "parameter") {
            let Some(n) = p.child_by_field_name("name") else { continue };
            let ty = p.child_by_field_name("type");
            let class = ty.and_then(|t| class_of_type(t, self.source));
            let suffix = ty.map(|t| suffix_type(self.text(t)));
            let name = self.text(n).to_string();
            if let Some(t) = self.types.last_mut() {
                if let Some(s) = suffix {
                    t.field_types.insert(name.clone(), s);
                }
                t.fields.insert(name, class);
            }
        }
    }

    fn visit_record_parameters(&mut self, params: Node<'_>, depth: usize) {
        let owner_exported = self.enc.is_some_and(|e| self.defs[e as usize].exported);
        for p in named_children(params).into_iter().filter(|p| p.kind() == "parameter") {
            let Some(n) = p.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let (_, attributes) = self.modifiers(p);
            let def = Definition {
                label: Label::Field,
                name: name.clone(),
                qualified_name: self.child_qn(&name, ""),
                parent: self.enc,
                span: span_of(p),
                name_span: span_of(n),
                signature: Some(truncate_chars(collapse(self.text(p)), MAX_SIGNATURE)),
                doc: None,
                decorators: self.decorator_texts(&attributes),
                bases: Vec::new(),
                implements: Vec::new(),
                exported: owner_exported,
                is_test: false,
                is_declaration: false,
            };
            let id = self.push_def(def);
            self.with_enc(id, |ex| {
                for a in &attributes {
                    ex.visit_attribute(*a, true, depth + 1);
                }
                if let Some(t) = p.child_by_field_name("type") {
                    ex.visit_type(t, depth + 1);
                }
                ex.visit_parameter_default(p, depth);
            });
        }
    }

    /// Runs `f` with `id` as the enclosing definition.
    pub(crate) fn with_enc(&mut self, id: LocalDefId, f: impl FnOnce(&mut Self)) {
        let saved = self.enc;
        self.enc = Some(id);
        f(self);
        self.enc = saved;
    }

    /// Callable member names declared in a body, with their counts (for
    /// overload suffixes); constructors count under the type name. Explicit
    /// interface implementations are named apart and not counted.
    fn callable_counts(&self, body: Node<'_>, type_name: &str) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for m in member_nodes(body) {
            if explicit_interface(m).is_some() {
                continue;
            }
            let name = match m.kind() {
                "method_declaration" => m.child_by_field_name("name").map(|n| self.text(n).to_string()),
                "constructor_declaration" => Some(type_name.to_string()),
                "operator_declaration" | "conversion_operator_declaration" => Some(self.operator_name(m)),
                _ => None,
            };
            if let Some(n) = name {
                *counts.entry(n).or_default() += 1;
            }
        }
        counts
    }

    /// Fields, properties and events declared in a body, collected before
    /// the members so earlier methods see them.
    fn collect_fields(&mut self, body: Node<'_>) {
        let fields: Vec<FieldInfo> = member_nodes(body).into_iter().flat_map(|m| self.declared_fields(m)).collect();
        if let Some(t) = self.types.last_mut() {
            for (n, c, ty) in fields {
                if let Some(ty) = ty {
                    t.field_types.entry(n.clone()).or_insert(ty);
                }
                t.fields.entry(n).or_insert(c);
            }
        }
    }

    /// Name, declared class and suffix type of each field, property or
    /// event a member declares.
    fn declared_fields(&self, m: Node<'_>) -> Vec<FieldInfo> {
        let typed = |ty: Option<Node<'_>>| {
            (ty.and_then(|t| class_of_type(t, self.source)), ty.map(|t| suffix_type(self.text(t))))
        };
        match m.kind() {
            "field_declaration" | "event_field_declaration" => {
                let Some(decl) = named_children(m).into_iter().find(|c| c.kind() == "variable_declaration") else {
                    return Vec::new();
                };
                let (class, suffix) = typed(decl.child_by_field_name("type"));
                named_children(decl)
                    .into_iter()
                    .filter(|c| c.kind() == "variable_declarator")
                    .filter_map(|d| d.child_by_field_name("name"))
                    .map(|n| (self.text(n).to_string(), class.clone(), suffix.clone()))
                    .collect()
            }
            "property_declaration" | "event_declaration" => {
                let Some(n) = m.child_by_field_name("name") else { return Vec::new() };
                let (class, suffix) = typed(m.child_by_field_name("type"));
                vec![(self.text(n).to_string(), class, suffix)]
            }
            _ => Vec::new(),
        }
    }

    /// Declared return of each method of a body (`None` when overloads
    /// disagree, for `void`, arrays, value keywords and the method's own
    /// type parameters).
    fn method_returns(&self, body: Node<'_>) -> HashMap<String, Option<Returns>> {
        let mut out: HashMap<String, Option<Returns>> = HashMap::new();
        for m in member_nodes(body).into_iter().filter(|m| m.kind() == "method_declaration") {
            let Some(name) = m.child_by_field_name("name").map(|n| self.text(n).to_string()) else { continue };
            let own_params = self.declared_type_params(m);
            let ret = m.child_by_field_name("returns").and_then(|t| {
                let ty = suffix_type(self.text(t));
                let head = ty.split(['.', '[']).next().unwrap_or(&ty);
                if own_params.iter().any(|p| p == head) || ty == "void" {
                    return None;
                }
                let class = class_of_type(t, self.source).filter(|c| !matches!(c.as_str(), "string" | "object"));
                Some(Returns { class, ty })
            });
            match out.get(&name) {
                Some(prev) if *prev != ret => {
                    out.insert(name, None);
                }
                Some(_) => {}
                None => {
                    out.insert(name, ret);
                }
            }
        }
        out
    }

    /// Type parameter names a declaration introduces.
    fn declared_type_params(&self, node: Node<'_>) -> Vec<String> {
        let tp = node
            .child_by_field_name("type_parameters")
            .or_else(|| named_children(node).into_iter().find(|c| c.kind() == "type_parameter_list"));
        tp.map(|tp| {
            named_children(tp)
                .into_iter()
                .filter_map(|p| p.child_by_field_name("name"))
                .map(|n| self.text(n).to_string())
                .collect()
        })
        .unwrap_or_default()
    }

    /// Members of a class / struct / record / interface body.
    fn visit_body(&mut self, body: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let type_name = self.types.last().map(|t| t.name.clone()).unwrap_or_default();
        let counts = self.callable_counts(body, &type_name);
        let returns = self.method_returns(body);
        if let Some(t) = self.types.last_mut() {
            t.methods = counts.keys().map(|k| (k.clone(), returns.get(k).cloned().flatten())).collect();
        }
        self.collect_fields(body);
        for m in member_nodes(body) {
            self.visit_member(m, &counts, depth + 1);
        }
    }

    fn visit_member(&mut self, m: Node<'_>, counts: &HashMap<String, usize>, depth: usize) {
        let overloaded = |name: &str| counts.get(name).copied().unwrap_or(0) > 1;
        match m.kind() {
            "method_declaration" => {
                let name = m.child_by_field_name("name").map_or("", |n| self.text(n));
                let o = explicit_interface(m).is_none() && overloaded(name);
                self.visit_callable(m, CallableKind::Method, o, depth);
            }
            "constructor_declaration" => {
                let type_name = self.types.last().map(|t| t.name.clone()).unwrap_or_default();
                self.visit_callable(m, CallableKind::Constructor, overloaded(&type_name), depth);
            }
            "destructor_declaration" => self.visit_callable(m, CallableKind::Destructor, false, depth),
            "operator_declaration" | "conversion_operator_declaration" => {
                let o = overloaded(&self.operator_name(m));
                self.visit_callable(m, CallableKind::Operator, o, depth);
            }
            "field_declaration" | "event_field_declaration" => self.visit_field(m, depth),
            "property_declaration" | "event_declaration" | "indexer_declaration" => self.visit_property(m, depth),
            k if is_type_decl(k) => self.visit_type_decl(m, "", depth),
            k if is_comment(k) => {}
            _ => self.visit(m, depth),
        }
    }

    /// Whether a method with these attributes and name is a test.
    fn is_test_method(&self, attr_names: &[String]) -> bool {
        let Some(t) = self.types.last() else { return false };
        if attr_names.iter().any(|a| TEST_ATTRIBUTES.contains(&a.as_str())) {
            return true;
        }
        (t.test_class || t.has_tests) && attr_names.iter().any(|a| FIXTURE_ATTRIBUTES.contains(&a.as_str()))
    }

    /// Name of an operator (`operator+`, `operator true`) or a conversion
    /// operator (`implicit operator string`).
    pub(crate) fn operator_name(&self, node: Node<'_>) -> String {
        if node.kind() == "conversion_operator_declaration" {
            let kind = if self.has_keyword(node, "explicit") { "explicit" } else { "implicit" };
            let ty = node.child_by_field_name("type").map_or(String::new(), |t| suffix_type(self.text(t)));
            return format!("{kind} operator {ty}");
        }
        let op = node.child_by_field_name("operator").map_or("", |o| self.text(o));
        if op.starts_with(|c: char| c.is_ascii_alphabetic()) {
            format!("operator {op}")
        } else {
            format!("operator{op}")
        }
    }

    /// The name and name node of a callable.
    fn callable_name<'t>(&self, node: Node<'t>, kind: CallableKind) -> (String, Node<'t>) {
        let type_name = self.types.last().map(|t| t.name.clone()).unwrap_or_default();
        match kind {
            CallableKind::Method => {
                let n = node.child_by_field_name("name").unwrap_or(node);
                (self.text(n).to_string(), n)
            }
            CallableKind::Constructor => (type_name, node.child_by_field_name("name").unwrap_or(node)),
            CallableKind::Destructor => (format!("~{type_name}"), node.child_by_field_name("name").unwrap_or(node)),
            CallableKind::Operator => {
                let n = node.child_by_field_name("operator").or_else(|| node.child_by_field_name("type"));
                (self.operator_name(node), n.unwrap_or(node))
            }
        }
    }

    fn visit_callable(&mut self, node: Node<'_>, kind: CallableKind, overloaded: bool, depth: usize) {
        let (name, name_node) = self.callable_name(node, kind);
        let (keywords, attributes) = self.modifiers(node);
        let attr_names = self.attribute_names(&attributes);
        let params = node.child_by_field_name("parameters");
        let local = match explicit_interface(node) {
            Some(i) => format!("{}.{name}", simple_name(&strip_type_text(self.text(i)))),
            None => name.clone(),
        };
        let mut qn = self.child_qn(&local, "");
        if overloaded && let Some(p) = params {
            qn = format!("{qn}({})", parameter_types(p, self.source));
        }
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let is_test = (kind == CallableKind::Method && self.is_test_method(&attr_names)) || self.in_test();
        if is_test && let Some(t) = self.types.last_mut() {
            t.has_tests = true;
        }
        let exported = explicit_interface(node).is_some() || self.exported(&keywords, false);
        let def = Definition {
            label: Label::Method,
            name,
            qualified_name: qn,
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.signature(node, end)),
            doc: self.doc(node),
            decorators: self.decorator_texts(&attributes),
            bases: Vec::new(),
            implements: Vec::new(),
            exported,
            is_test,
            is_declaration: false,
        };
        let id = self.push_def(def);
        self.with_enc(id, |ex| {
            for a in &attributes {
                ex.visit_attribute(*a, true, depth + 1);
            }
            ex.visit_callable_parts(node, depth);
        });
    }

    /// Type parameters, return type, parameters, constructor initializer and
    /// body of a callable.
    pub(crate) fn visit_callable_parts(&mut self, node: Node<'_>, depth: usize) {
        let mark = self.push_type_params(node);
        for field in ["returns", "type"] {
            if let Some(t) = node.child_by_field_name(field) {
                self.visit_type(t, depth + 1);
            }
        }
        if let Some(p) = node.child_by_field_name("parameters") {
            self.visit_parameters(p, depth + 1);
        }
        self.visit_constraints(node, depth);
        for c in named_children(node) {
            if c.kind() == "constructor_initializer" {
                self.visit_constructor_initializer(c, depth + 1);
            }
        }
        if let Some(b) = node.child_by_field_name("body") {
            self.visit(b, depth + 1);
        }
        self.pop_type_params(mark);
    }

    /// `: base(…)` / `: this(…)`: a call to a constructor of the base class
    /// or of this type (constructors are named like their type).
    fn visit_constructor_initializer(&mut self, node: Node<'_>, depth: usize) {
        let args = named_children(node).into_iter().find(|c| c.kind() == "argument_list");
        let is_base = self.has_keyword(node, "base");
        let target = match self.types.last() {
            Some(t) if is_base => t.superclass.clone().map(|s| (s, "base")),
            Some(t) => Some((t.name.clone(), "this")),
            None => None,
        };
        if let Some((name, receiver)) = target {
            let named = self.with_args(&name, args);
            self.push_ref(RefKind::Call, &named, Some(receiver.into()), span_of(node), args.map(arity_of));
        }
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
    }

    /// A parameter list: attributes, types and default values.
    pub(crate) fn visit_parameters(&mut self, params: Node<'_>, depth: usize) {
        for (c, field) in children_with_fields(params) {
            match (c.kind(), field) {
                ("parameter", _) => self.visit_parameter(c, depth),
                (_, Some("type")) => self.visit_type(c, depth),
                ("attribute_list", _) => self.visit_attribute_list(c, depth),
                _ => {}
            }
        }
    }

    fn visit_parameter(&mut self, p: Node<'_>, depth: usize) {
        for c in named_children(p) {
            if c.kind() == "attribute_list" {
                self.visit_attribute_list(c, depth);
            }
        }
        if let Some(t) = p.child_by_field_name("type") {
            self.visit_type(t, depth + 1);
        }
        self.visit_parameter_default(p, depth);
    }

    /// A parameter's default value (`int n = Default`).
    fn visit_parameter_default(&mut self, p: Node<'_>, depth: usize) {
        let skip = [p.child_by_field_name("type"), p.child_by_field_name("name")];
        for c in named_children(p) {
            if !skip.contains(&Some(c)) && !matches!(c.kind(), "attribute_list" | "modifier") {
                self.visit(c, depth + 1);
            }
        }
    }

    fn visit_attribute_list(&mut self, list: Node<'_>, depth: usize) {
        for a in named_children(list).into_iter().filter(|a| a.kind() == "attribute") {
            self.visit_attribute(a, false, depth + 1);
        }
    }

    fn visit_field(&mut self, node: Node<'_>, depth: usize) {
        let (keywords, attributes) = self.modifiers(node);
        let Some(decl) = named_children(node).into_iter().find(|c| c.kind() == "variable_declaration") else { return };
        let ty = decl.child_by_field_name("type");
        let type_text = ty.map_or(String::new(), |t| collapse(self.text(t)));
        let is_const = keywords.contains("const");
        let mods = modifier_prefix(&keywords, node.kind() == "event_field_declaration");
        let exported = self.exported(&keywords, false);
        let doc = self.doc(node);
        let decorators = self.decorator_texts(&attributes);
        let mut first = true;
        for d in named_children(decl).into_iter().filter(|c| c.kind() == "variable_declarator") {
            let Some(n) = d.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let def = Definition {
                label: if is_const { Label::Constant } else { Label::Field },
                name: name.clone(),
                qualified_name: self.child_qn(&name, ""),
                parent: self.enc,
                span: span_of(node),
                name_span: span_of(n),
                signature: Some(truncate_chars(collapse(&format!("{mods}{type_text} {name}")), MAX_SIGNATURE)),
                doc: doc.clone(),
                decorators: decorators.clone(),
                bases: Vec::new(),
                implements: Vec::new(),
                exported,
                is_test: false,
                is_declaration: false,
            };
            let id = self.push_def(def);
            let header = std::mem::take(&mut first);
            self.with_enc(id, |ex| {
                if header {
                    ex.visit_field_header(&attributes, ty, depth);
                }
                ex.visit_declarator(d, depth);
            });
        }
    }

    /// The attributes (as decorators) and the type of a field declaration,
    /// attributed to its first declarator.
    fn visit_field_header(&mut self, attributes: &[Node<'_>], ty: Option<Node<'_>>, depth: usize) {
        for a in attributes {
            self.visit_attribute(*a, true, depth + 1);
        }
        if let Some(t) = ty {
            self.visit_type(t, depth + 1);
        }
    }

    /// A property, event with accessors or indexer: a `Field` (indexers are
    /// named `this[]`); its accessors' bodies and initializer belong to it.
    fn visit_property(&mut self, node: Node<'_>, depth: usize) {
        let (keywords, attributes) = self.modifiers(node);
        let name_node = node.child_by_field_name("name");
        let name = name_node.map_or_else(|| "this[]".to_string(), |n| self.text(n).to_string());
        let end = node
            .child_by_field_name("accessors")
            .or_else(|| node.child_by_field_name("value"))
            .map_or(node.end_byte(), |b| b.start_byte());
        let local = match explicit_interface(node) {
            Some(i) => format!("{}.{name}", simple_name(&strip_type_text(self.text(i)))),
            None => name.clone(),
        };
        let def = Definition {
            label: Label::Field,
            name,
            qualified_name: self.child_qn(&local, ""),
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node.unwrap_or(node)),
            signature: Some(self.signature(node, end)),
            doc: self.doc(node),
            decorators: self.decorator_texts(&attributes),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: explicit_interface(node).is_some() || self.exported(&keywords, false),
            is_test: false,
            is_declaration: false,
        };
        let id = self.push_def(def);
        self.with_enc(id, |ex| {
            for a in &attributes {
                ex.visit_attribute(*a, true, depth + 1);
            }
            if let Some(t) = node.child_by_field_name("type") {
                ex.visit_type(t, depth + 1);
            }
            if let Some(p) = node.child_by_field_name("parameters") {
                ex.visit_parameters(p, depth + 1);
            }
            ex.visit_accessors(node, depth);
        });
    }

    /// Accessor bodies and the initializer / expression body of a property.
    fn visit_accessors(&mut self, node: Node<'_>, depth: usize) {
        if let Some(list) = node.child_by_field_name("accessors") {
            for acc in named_children(list).into_iter().filter(|a| a.kind() == "accessor_declaration") {
                for c in named_children(acc) {
                    match c.kind() {
                        "attribute_list" => self.visit_attribute_list(c, depth + 1),
                        "block" | "arrow_expression_clause" => self.visit(c, depth + 1),
                        _ => {}
                    }
                }
            }
        }
        if let Some(v) = node.child_by_field_name("value") {
            self.visit(v, depth + 1);
        }
    }

    fn visit_enum_members(&mut self, body: Node<'_>, depth: usize) {
        for m in member_nodes(body).into_iter().filter(|m| m.kind() == "enum_member_declaration") {
            let Some(n) = m.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let (_, attributes) = self.modifiers(m);
            let def = Definition {
                label: Label::Constant,
                name: name.clone(),
                qualified_name: self.child_qn(&name, ""),
                parent: self.enc,
                span: span_of(m),
                name_span: span_of(n),
                signature: Some(truncate_chars(collapse(self.text(m)), MAX_SIGNATURE)),
                doc: self.doc(m),
                decorators: self.decorator_texts(&attributes),
                bases: Vec::new(),
                implements: Vec::new(),
                exported: true,
                is_test: false,
                is_declaration: false,
            };
            let id = self.push_def(def);
            self.with_enc(id, |ex| {
                for a in &attributes {
                    ex.visit_attribute(*a, true, depth + 1);
                }
                if let Some(v) = m.child_by_field_name("value") {
                    ex.visit(v, depth + 1);
                }
            });
        }
    }

    /// A local function: a `Function` child of the enclosing callable.
    pub(crate) fn visit_local_function(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = self.text(name_node).to_string();
        let (_, attributes) = self.modifiers(node);
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let def = Definition {
            label: Label::Function,
            name: name.clone(),
            qualified_name: self.child_qn(&name, ""),
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.signature(node, end)),
            doc: None,
            decorators: self.decorator_texts(&attributes),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: false,
            is_test: self.in_test(),
            is_declaration: false,
        };
        let id = self.push_def(def);
        self.with_enc(id, |ex| {
            for a in &attributes {
                ex.visit_attribute(*a, true, depth + 1);
            }
            ex.visit_callable_parts(node, depth);
        });
    }
}

/// Byte ranges of a declaration header (before `end`) left out of its
/// signature: attributes and comments (removed), and default values of
/// parameters when `elide_defaults` (written `…`).
fn header_skips(
    node: Node<'_>,
    end: usize,
    elide_defaults: bool,
    out: &mut Vec<(usize, usize, &'static str)>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for c in named_children(node).into_iter().take_while(|c| c.start_byte() < end) {
        match c.kind() {
            "attribute_list" | "comment" | "preproc_if_in_attribute_list" => {
                out.push((c.start_byte(), c.end_byte(), ""))
            }
            "parameter_list" | "bracketed_parameter_list" | "parameter" => {
                header_skips(c, end, elide_defaults, out, depth + 1);
                if elide_defaults && c.kind() == "parameter" {
                    out.extend(default_value(c).map(|v| (v.start_byte(), v.end_byte(), "…")));
                }
            }
            _ => {}
        }
    }
}

/// The default value expression of a parameter (`int n = 0` → `0`).
fn default_value(p: Node<'_>) -> Option<Node<'_>> {
    let skip = [p.child_by_field_name("type"), p.child_by_field_name("name")];
    named_children(p)
        .into_iter()
        .find(|c| !skip.contains(&Some(*c)) && !matches!(c.kind(), "attribute_list" | "modifier" | "comment"))
}

/// A field, property or event of a type being visited: name, declared
/// class, suffix type.
type FieldInfo = (String, Option<String>, Option<String>);

/// Kinds of callables with their naming rules.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallableKind {
    Method,
    Constructor,
    Destructor,
    Operator,
}

/// The members of a declaration list, `#if` blocks flattened.
pub(crate) fn member_nodes(body: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    flatten_preproc(body, &mut out, 0);
    out
}

fn flatten_preproc<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>, depth: usize) {
    for c in named_children(node) {
        match c.kind() {
            "preproc_if" | "preproc_elif" | "preproc_else" if depth < 16 => flatten_preproc(c, out, depth + 1),
            _ => out.push(c),
        }
    }
}

/// The `explicit_interface_specifier`'s type of a member (`void IFoo.Bar()`).
fn explicit_interface(node: Node<'_>) -> Option<Node<'_>> {
    named_children(node).into_iter().find(|c| c.kind() == "explicit_interface_specifier").and_then(|s| s.named_child(0))
}

/// Last segment of a dotted name.
pub(crate) fn simple_name(t: &str) -> &str {
    t.rsplit('.').next().unwrap_or(t)
}

/// An attribute's simple name without the `Attribute` suffix.
pub(crate) fn attribute_simple_name(name: &str) -> &str {
    let s = simple_name(name);
    match s.strip_suffix("Attribute") {
        Some(r) if !r.is_empty() => r,
        _ => s,
    }
}

/// Named like an interface by the .NET convention: `I` + upper case.
pub(crate) fn looks_like_interface(t: &str) -> bool {
    let s = simple_name(t).as_bytes();
    s.len() > 2 && s[0] == b'I' && s[1].is_ascii_uppercase() && s[2..].iter().any(u8::is_ascii_lowercase)
}

/// Number of arguments of an `argument_list`.
pub(crate) fn arity_of(args: Node<'_>) -> u32 {
    named_children(args).iter().filter(|c| c.kind() == "argument").count() as u32
}

/// `a.b.C` → (`a.b`, `C`); `C` → (``, `C`).
fn split_last(full: &str) -> (String, String) {
    match full.rsplit_once('.') {
        Some((m, n)) => (m.to_string(), n.to_string()),
        None => (String::new(), full.to_string()),
    }
}

fn using_import(module: String, names: Vec<ImportedName>) -> Import {
    Import {
        module,
        relative_level: 0,
        names,
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Span::default(),
    }
}

/// Modifier keywords of a field signature, in conventional order.
fn modifier_prefix(keywords: &HashSet<&str>, event: bool) -> String {
    let order = ["public", "protected", "internal", "private", "static", "readonly", "const", "volatile"];
    let mut s: String = order.into_iter().filter(|k| keywords.contains(k)).map(|k| format!("{k} ")).collect();
    if event {
        s.push_str("event ");
    }
    s
}

/// Removes XML tags (`<summary>`, `<see cref="X"/>` → `X`) from a doc comment.
fn strip_xml_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let tag = &rest[i + 1..i + j];
        if let Some(cref) = tag.split("cref=\"").nth(1).and_then(|c| c.split('"').next()) {
            out.push_str(cref.split_once(':').map_or(cref, |(_, r)| r));
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out.lines().map(str::trim).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert!(looks_like_interface("IStore") && looks_like_interface("System.IDisposable"));
        assert!(!looks_like_interface("Item") && !looks_like_interface("IO"));
        assert_eq!(attribute_simple_name("Xunit.FactAttribute"), "Fact");
        assert_eq!(attribute_simple_name("Attribute"), "Attribute");
        assert_eq!(strip_xml_tags("<summary>Adds <see cref=\"Foo\"/>.</summary>"), "Adds Foo.");
        assert!(is_test_path("tests/Acme.Tests/StoreTests.cs") && is_test_path("src/Acme.UnitTests/A.cs"));
        assert!(!is_test_path("src/Acme/Store.cs") && !is_test_path("Tests.cs"));
    }
}
