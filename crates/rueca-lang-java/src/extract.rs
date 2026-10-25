//! Syntax-tree walk producing [`FileFacts`] for one Java file.
//!
//! A recursive visitor (depth-limited by `Budget::max_depth`) keeps the
//! enclosing definition, the stack of enclosing named types (with their
//! fields and superclass), the generic type parameters in scope and
//! whether it is inside an anonymous class body. Local variables and their
//! declared types come from the [`Locals`] table built by the vendored
//! `queries/locals.scm`. See the crate docs for the mapping rules.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::{self, Node};
use rueca_extract::{
    Budget, ExtractError, count_errors, line_count, span_of, split_arg_types, type_receiver, with_arg_types,
};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, Locals};
use crate::types::{class_of_type, is_primitive, named_children, parameter_types, strip_type_text, suffix_type, text};

/// Hard cap on references per file.
pub const MAX_REFS: usize = 20_000;

/// Maximum names bound per file through static on-demand imports.
const MAX_STATIC_NAMES: usize = 512;
/// Maximum characters of a signature.
const MAX_SIGNATURE: usize = 200;
/// Maximum characters of a doc comment.
const MAX_DOC: usize = 1000;
/// Maximum characters of a raw (untyped) receiver text.
const MAX_RECEIVER: usize = 200;

/// Test annotations on methods (JUnit 4 / 5, TestNG).
const TEST_ANNOTATIONS: &[&str] = &["ParameterizedTest", "RepeatedTest", "Test", "TestFactory", "TestTemplate"];
/// Fixture annotations (JUnit 4 / 5, TestNG): the method is test code.
const LIFECYCLE_ANNOTATIONS: &[&str] = &[
    "After",
    "AfterAll",
    "AfterClass",
    "AfterEach",
    "AfterMethod",
    "AfterSuite",
    "AfterTest",
    "Before",
    "BeforeAll",
    "BeforeClass",
    "BeforeEach",
    "BeforeMethod",
    "BeforeSuite",
    "BeforeTest",
];
/// Spring packages whose import is the file-local evidence for bridge facts.
const SPRING_VALUE: &str = "org.springframework.beans.factory.annotation";
const SPRING_ENV: &str = "org.springframework.core.env";
/// Spring `Environment`-like types whose `getProperty("a.b")` reads a key.
const SPRING_ENV_TYPES: &[&str] =
    &["ConfigurableEnvironment", "Environment", "PropertyResolver", "StandardEnvironment"];
const SPRING_ENV_METHODS: &[&str] = &["containsProperty", "getProperty", "getRequiredProperty"];
/// Config namespace of Spring keys (RFC 0003 §7.2).
const SPRING_NAMESPACE: &str = "spring";

pub(crate) fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time)?;

    let mut ex = Extractor {
        source,
        max_depth: budget.max_depth,
        defs: Vec::new(),
        refs: Vec::new(),
        imports: Vec::new(),
        seen_refs: HashSet::new(),
        enc: None,
        types: Vec::new(),
        type_params: Vec::new(),
        anon_counts: HashMap::new(),
        locals,
        file_types: Vec::new(),
        package: String::new(),
        imported: HashMap::new(),
        on_demand: HashSet::new(),
        declared_types: HashSet::new(),
        static_on_demand: Vec::new(),
        static_members: HashSet::new(),
    };
    ex.prescan(root, 0);
    ex.visit_children(root, 0);
    ex.bind_static_on_demand();

    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
    })
}

/// A named type declared in the file.
struct FileType {
    /// Local qualified name (`Parser.Options`).
    qn: String,
    name: String,
    /// Index of the enclosing file type.
    parent: Option<usize>,
}

/// Declared return type of a method.
#[derive(Clone, PartialEq)]
struct Returns {
    /// The class (`Map.Entry`, `com.x.Foo`) for receivers; `None` for
    /// primitives and arrays.
    class: Option<String>,
    /// Overload-suffix form (`int`, `Entry`, `byte[]`), for argument types.
    ty: String,
}

/// A named type being visited.
struct TypeCtx {
    def: LocalDefId,
    name: String,
    /// Field name → declared class (`None` for primitives and arrays).
    fields: HashMap<String, Option<String>>,
    /// Field name → declared type in overload-suffix form (`int`,
    /// `String[]`), for argument types.
    field_types: HashMap<String, String>,
    /// Simple name of the superclass, if declared.
    superclass: Option<String>,
    /// Members are implicitly public (interfaces, annotation types).
    implicit_public: bool,
    /// Named or annotated as a test class.
    test_class: bool,
    /// JUnit 3: extends `*TestCase`, so `test*` methods are tests.
    junit3: bool,
    /// TestNG `@Test` on the class: its public methods are tests.
    class_level_test: bool,
    /// At least one member is a test.
    has_tests: bool,
    /// Index into `file_types` (`None` for local classes).
    file_type: Option<usize>,
    /// Callable members (methods, constructors) by name → their declared
    /// return type when every overload declares the same one.
    methods: HashMap<String, Option<Returns>>,
    /// Declared inside test code (an anonymous class in a test method).
    in_test: bool,
    /// Record components' parameter types (`int,String`), for the compact
    /// constructor's overload suffix.
    record_params: Option<String>,
}

struct Extractor<'s> {
    source: &'s [u8],
    max_depth: usize,
    defs: Vec<Definition>,
    refs: Vec<Reference>,
    imports: Vec<Import>,
    /// Dedup keys for `Read` / `Write` / `Type`: (kind, enclosing, receiver, name).
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    /// Innermost enclosing definition.
    enc: Option<LocalDefId>,
    types: Vec<TypeCtx>,
    /// Generic type parameters in scope.
    type_params: Vec<String>,
    /// Anonymous classes named so far per (enclosing definition, base
    /// name), for their `Base$N` names.
    anon_counts: HashMap<(Option<LocalDefId>, String), u32>,
    locals: Locals,
    /// Named types declared in this file (top-level and member types, not
    /// local classes), for fully qualified `$type:` receivers.
    file_types: Vec<FileType>,
    /// The declared package (empty for the default package).
    package: String,
    /// Single-type imports: simple name → fully qualified name.
    imported: HashMap<String, String>,
    /// On-demand imported packages (`a.b` for `import a.b.*`).
    on_demand: HashSet<String>,
    /// Every type name declared in this file.
    declared_types: HashSet<String>,
    /// Static on-demand imports: (package, class path, span).
    static_on_demand: Vec<(String, String, Span)>,
    /// Members named by single static imports.
    static_members: HashSet<String>,
}

fn is_type_decl(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "line_comment" | "block_comment")
}

fn is_annotation(kind: &str) -> bool {
    matches!(kind, "annotation" | "marker_annotation")
}

/// `UPPER_CASE` names (constants, enum constants).
fn is_screaming(name: &str) -> bool {
    name.len() >= 2
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Collapses whitespace runs to one space.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// Content of a Java string literal without quotes (text blocks included);
/// `None` when it interpolates (string templates).
fn string_literal_value(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    if named_children(node).iter().any(|c| c.kind() == "string_interpolation") {
        return None;
    }
    let t = text(node, source);
    let t = t.strip_prefix("\"\"\"").and_then(|r| r.strip_suffix("\"\"\"")).unwrap_or(t);
    let t = t.strip_prefix('"').and_then(|r| r.strip_suffix('"')).unwrap_or(t);
    Some(t.to_string())
}

/// Spring placeholders `${key}` / `${key:default}` in a string, keys only
/// (defaults may nest further placeholders, which are scanned too).
pub(crate) fn placeholder_keys(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        rest = &rest[i + 2..];
        let end = rest.find([':', '}']).unwrap_or(rest.len());
        let key = rest[..end].trim();
        if !key.is_empty() && !key.contains("${") && !out.iter().any(|k| k == key) {
            out.push(key.to_string());
        }
    }
    out
}

/// Cleans a `/** … */` comment: delimiters and leading `*` removed.
fn clean_javadoc(raw: &str) -> Option<String> {
    let body = raw.strip_prefix("/**")?.strip_suffix("*/")?;
    let lines: Vec<&str> = body
        .lines()
        .map(|l| {
            let l = l.trim();
            l.strip_prefix('*').map_or(l, |r| r.strip_prefix(' ').unwrap_or(r)).trim_end()
        })
        .collect();
    let text = lines.join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| truncate_chars(text.to_string(), MAX_DOC))
}

/// Splits a type text that starts with a package (`a.b.C.D`: lower-case
/// segments, then a class) into the package and the class path.
fn split_package(t: &str) -> Option<(String, String)> {
    let segs: Vec<&str> = t.split('.').collect();
    if segs.len() < 2 || !segs[0].starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    let i = segs.iter().position(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))?;
    Some((segs[..i].join("."), segs[i..].join(".")))
}

/// Last segment of an annotation name (`org.junit.Test` → `Test`).
fn annotation_simple_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

impl<'s> Extractor<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.source)
    }

    // -----------------------------------------------------------------------
    // Pre-scan: package, imports, declared type names
    // -----------------------------------------------------------------------

    fn prescan(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            match c.kind() {
                "package_declaration" => self.package(c),
                "import_declaration" => self.import(c),
                k if is_type_decl(k) => self.prescan_type(c, None, depth + 1),
                _ => {}
            }
        }
        // `System` shadowed by a type declared anywhere in the file.
        self.collect_declared(node, depth);
    }

    /// Records a type declaration and its member types (not local classes).
    fn prescan_type(&mut self, node: Node<'_>, parent: Option<usize>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let qn = match parent {
            Some(p) => format!("{}.{name}", self.file_types[p].qn),
            None => name.clone(),
        };
        self.file_types.push(FileType { qn, name, parent });
        let me = self.file_types.len() - 1;
        let Some(body) = node.child_by_field_name("body") else { return };
        let mut members = named_children(body);
        if let Some(i) = members.iter().position(|c| c.kind() == "enum_body_declarations") {
            let inner = named_children(members[i]);
            members.extend(inner);
        }
        for m in members {
            if is_type_decl(m.kind()) {
                self.prescan_type(m, Some(me), depth + 1);
            }
        }
    }

    fn collect_declared(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for c in named_children(node) {
            if is_type_decl(c.kind())
                && let Some(n) = c.child_by_field_name("name")
            {
                self.declared_types.insert(self.text(n).to_string());
            }
            self.collect_declared(c, depth + 1);
        }
    }

    /// The file type a simple type name denotes at this point: an enclosing
    /// type, a member type of an enclosing type (innermost first), else a
    /// top-level type of the file.
    fn visible_type(&self, name: &str) -> Option<usize> {
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

    /// The package clause, recorded as the file's first import with no
    /// names (see the crate docs, *Imports*).
    fn package(&mut self, node: Node<'_>) {
        let Some(name) =
            named_children(node).into_iter().find(|c| matches!(c.kind(), "identifier" | "scoped_identifier"))
        else {
            return;
        };
        let module: String = self.text(name).split_whitespace().collect();
        self.package = module.clone();
        if self.imports.first().is_some_and(|i| i.names.is_empty()) {
            return;
        }
        self.imports.insert(
            0,
            Import {
                module,
                relative_level: 0,
                names: Vec::new(),
                module_alias: None,
                type_only: false,
                reexport: false,
                span: span_of(node),
            },
        );
    }

    fn import(&mut self, node: Node<'_>) {
        let children = named_children(node);
        let Some(path) = children.iter().find(|c| matches!(c.kind(), "identifier" | "scoped_identifier")) else {
            return;
        };
        let is_static = {
            let mut cursor = node.walk();
            node.children(&mut cursor).any(|c| c.kind() == "static")
        };
        let star = children.iter().any(|c| c.kind() == "asterisk");
        let full: String = self.text(*path).split_whitespace().collect();
        let segs: Vec<&str> = full.split('.').collect();
        // Package = leading lower-case segments (Java naming convention);
        // the rest is a class path (`Outer.Inner`) and, for static imports,
        // a member.
        let split = segs.iter().position(|s| s.starts_with(|c: char| c.is_ascii_uppercase()));
        let (module, name) = match (star, split) {
            (true, None) => {
                self.on_demand.insert(full.clone());
                (full.clone(), "*".to_string())
            }
            (true, Some(i)) => {
                if is_static {
                    self.static_on_demand.push((segs[..i].join("."), segs[i..].join("."), span_of(node)));
                }
                (segs[..i].join("."), format!("{}.*", segs[i..].join(".")))
            }
            (false, Some(i)) if i > 0 || segs.len() == 1 => (segs[..i].join("."), segs[i..].join(".")),
            (false, _) if segs.len() > 1 => {
                let (m, n) = full.rsplit_once('.').expect("dotted");
                (m.to_string(), n.to_string())
            }
            (false, _) => (String::new(), full.clone()),
        };
        let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
        if !star && !is_static {
            self.imported.insert(simple, full.clone());
        } else if !star {
            self.static_members.insert(simple);
        }
        self.imports.push(Import {
            module,
            relative_level: 0,
            names: vec![ImportedName { name, alias: None }],
            module_alias: None,
            type_only: false,
            reexport: false,
            span: span_of(node),
        });
    }

    /// Static on-demand imports (`import static a.b.C.*`) as member imports:
    /// every bare call or `UPPER_CASE` read whose name is not declared in
    /// this file nor imported by a single static import gets a synthetic
    /// `import static a.b.C.<name>` per on-demand class, in source order
    /// (crate docs, *Imports*). Members of enclosing and inherited classes
    /// still win: implicit `this` is tried before imports.
    fn bind_static_on_demand(&mut self) {
        if self.static_on_demand.is_empty() {
            return;
        }
        let declared: HashSet<&str> = self.defs.iter().map(|d| d.name.as_str()).collect();
        let names: std::collections::BTreeSet<String> = self
            .refs
            .iter()
            .filter(|r| r.receiver.is_none() && matches!(r.kind, RefKind::Call | RefKind::Read))
            .map(|r| split_arg_types(&r.name).0)
            .filter(|n| !declared.contains(n) && !self.static_members.contains(*n))
            .map(str::to_string)
            .take(MAX_STATIC_NAMES)
            .collect();
        for name in names {
            for (module, class, span) in &self.static_on_demand {
                self.imports.push(Import {
                    module: module.clone(),
                    relative_level: 0,
                    names: vec![ImportedName { name: format!("{class}.{name}"), alias: None }],
                    module_alias: None,
                    type_only: false,
                    reexport: false,
                    span: *span,
                });
            }
        }
    }

    /// Whether the file imports `pkg.name` (single-type or on demand).
    fn imports_from(&self, pkg: &str, name: &str) -> bool {
        self.imported.get(name).is_some_and(|f| f == &format!("{pkg}.{name}")) || self.on_demand.contains(pkg)
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    fn push_ref(&mut self, kind: RefKind, name: &str, receiver: Option<String>, span: Span, arity: Option<u32>) {
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

    fn is_type_param(&self, name: &str) -> bool {
        self.type_params.iter().any(|t| t == name)
    }

    /// Declared class of field `name` of the innermost enclosing type that
    /// declares it: `Some(Some(class))`, `Some(None)` when untyped
    /// (primitive, array), `None` when no enclosing type declares it.
    fn field_class(&self, name: &str) -> Option<Option<String>> {
        self.types.iter().rev().find_map(|t| t.fields.get(name).cloned())
    }

    /// Declared return class of method `name` of the innermost enclosing
    /// type that declares it (every overload must agree), for receivers that
    /// are calls of this file's methods (`getAsSingleElement().get()`).
    fn method_return_class(&self, name: &str) -> Option<String> {
        self.method_returns_of(name).and_then(|r| r.class)
    }

    fn method_returns_of(&self, name: &str) -> Option<Returns> {
        self.types.iter().rev().find_map(|t| t.methods.get(name)).cloned().flatten()
    }

    /// Declared type of a field of an enclosing type (overload-suffix form).
    fn field_type(&self, name: &str) -> Option<String> {
        self.types.iter().rev().find(|t| t.fields.contains_key(name)).and_then(|t| t.field_types.get(name).cloned())
    }

    /// Static type of an argument expression in overload-suffix form, when
    /// the file says it: literals, locals, parameters and fields with a
    /// declared type, `this`, `new T()`, casts, class literals, string
    /// concatenations, comparisons and calls of this file's methods.
    fn arg_type(&self, node: Node<'_>) -> Option<String> {
        let pos = node.start_byte() as u32;
        let t = self.text(node);
        let ty = match node.kind() {
            "string_literal" => "String".to_string(),
            "character_literal" => "char".to_string(),
            "true" | "false" => "boolean".to_string(),
            "decimal_integer_literal" | "hex_integer_literal" | "octal_integer_literal" | "binary_integer_literal" => {
                (if t.ends_with(['l', 'L']) { "long" } else { "int" }).to_string()
            }
            "decimal_floating_point_literal" | "hex_floating_point_literal" => {
                (if t.ends_with(['f', 'F']) { "float" } else { "double" }).to_string()
            }
            "parenthesized_expression" => return self.arg_type(node.named_child(0)?),
            "cast_expression" => suffix_type(self.text(node.child_by_field_name("type")?)),
            "object_creation_expression" => {
                if named_children(node).iter().any(|c| c.kind() == "class_body") {
                    return None;
                }
                suffix_type(self.text(node.child_by_field_name("type")?))
            }
            "class_literal" => "Class".to_string(),
            "this" => self.types.last()?.name.clone(),
            "identifier" => match self.locals.lookup(t, pos) {
                Some(b) => b.ty.clone()?,
                None => self.field_type(t)?,
            },
            "field_access" => {
                let obj = node.child_by_field_name("object")?;
                let field = node.child_by_field_name("field")?;
                if obj.kind() != "this" {
                    return None;
                }
                self.types.last()?.field_types.get(self.text(field))?.clone()
            }
            "binary_expression" => {
                let op = node.child_by_field_name("operator").map(|o| o.kind())?;
                match op {
                    "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => "boolean".to_string(),
                    "+" => {
                        let l = node.child_by_field_name("left").and_then(|l| self.arg_type(l));
                        let r = node.child_by_field_name("right").and_then(|r| self.arg_type(r));
                        if l.as_deref() == Some("String") || r.as_deref() == Some("String") {
                            "String".to_string()
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            "instanceof_expression" => "boolean".to_string(),
            "unary_expression" if t.trim_start().starts_with('!') => "boolean".to_string(),
            "method_invocation" => {
                let own = node.child_by_field_name("object").is_none_or(|o| o.kind() == "this");
                let name = self.text(node.child_by_field_name("name")?);
                if !own {
                    return None;
                }
                self.method_returns_of(name)?.ty
            }
            _ => return None,
        };
        (!ty.is_empty()).then_some(ty)
    }

    /// `name` with the static types of the arguments of `args` (see
    /// `rueca_extract::with_arg_types`).
    fn with_args(&self, name: &str, args: Option<Node<'_>>) -> String {
        let Some(args) = args else { return name.to_string() };
        let types: Vec<Option<String>> =
            named_children(args).into_iter().filter(|c| !is_comment(c.kind())).map(|a| self.arg_type(a)).collect();
        with_arg_types(name, &types)
    }

    /// A fully qualified type text (`com.x.Foo.Inner`) without its package
    /// (`Foo.Inner`), with `Foo` bound by a synthetic single-type import
    /// (crate docs, *Imports*), so that it resolves through the import like
    /// any imported class. Unchanged when the text does not start with a
    /// package, when its head is a field or a type of this file, or when
    /// `Foo` is declared in the file or imported from another package.
    fn unqualify(&mut self, t: &str, span: Span) -> String {
        let Some((pkg, path)) = split_package(t) else { return t.to_string() };
        let head = t.split('.').next().unwrap_or(t);
        if self.field_class(head).is_some() || self.visible_type(head).is_some() {
            return t.to_string();
        }
        let class = path.split('.').next().unwrap_or(&path).to_string();
        if self.declared_types.contains(&class) || self.is_type_param(&class) {
            return t.to_string();
        }
        let fq = format!("{pkg}.{class}");
        match self.imported.get(&class) {
            Some(f) if *f == fq => {}
            Some(_) => return t.to_string(),
            None => {
                self.imported.insert(class.clone(), fq);
                self.imports.push(Import {
                    module: pkg,
                    relative_level: 0,
                    names: vec![ImportedName { name: class, alias: None }],
                    module_alias: None,
                    type_only: false,
                    reexport: false,
                    span,
                });
            }
        }
        path
    }

    /// `$type:` receiver for a class text, unless it is a type parameter.
    /// The text is written as in the source (a fully qualified class is
    /// bound by a synthetic import, see [`Self::unqualify`]); the resolver
    /// looks a type name up as a type, never as a constructor.
    fn typed(&mut self, class: Option<String>, span: Span) -> Option<String> {
        let c = self.unqualify(&class?, span);
        let head = c.split('.').next().unwrap_or(&c);
        if self.is_type_param(head) {
            return None;
        }
        Some(type_receiver(&c))
    }

    /// Raw receiver text: whitespace collapsed, bounded.
    fn raw(&self, node: Node<'_>) -> String {
        truncate_chars(collapse(self.text(node)), MAX_RECEIVER)
    }

    /// Whether `node` is a dotted path of plain identifiers (`a`, `a.b.c`).
    fn identifier_path(&self, node: Node<'_>) -> Option<Vec<&'s str>> {
        match node.kind() {
            "identifier" => Some(vec![self.text(node)]),
            "field_access" => {
                let obj = node.child_by_field_name("object")?;
                let field = node.child_by_field_name("field")?;
                if field.kind() != "identifier" || named_children(node).iter().any(|c| c.kind() == "super") {
                    return None;
                }
                let mut v = self.identifier_path(obj)?;
                v.push(self.text(field));
                Some(v)
            }
            "scoped_identifier" => {
                let t: &str = self.text(node);
                Some(t.split('.').map(str::trim).collect())
            }
            _ => None,
        }
    }

    /// The receiver text of a call / field access on `object` (crate docs,
    /// *References*).
    fn receiver(&mut self, object: Node<'_>) -> String {
        let pos = object.start_byte() as u32;
        let span = span_of(object);
        match object.kind() {
            "this" => "this".to_string(),
            "super" => "super".to_string(),
            "identifier" => {
                let name = self.text(object);
                if let Some(b) = self.locals.lookup(name, pos) {
                    let class = b.class.clone();
                    return self.typed(class, span).unwrap_or_else(|| name.to_string());
                }
                if let Some(class) = self.field_class(name) {
                    return self.typed(class, span).unwrap_or_else(|| name.to_string());
                }
                if self.visible_type(name).is_some() {
                    return self.typed(Some(name.to_string()), span).unwrap_or_else(|| name.to_string());
                }
                name.to_string()
            }
            "field_access" => {
                let obj = object.child_by_field_name("object");
                let field = object.child_by_field_name("field");
                match (obj, field) {
                    // `Outer.this`
                    (Some(o), Some(f)) if f.kind() == "this" && o.kind() == "identifier" => {
                        let outer = self.text(o).to_string();
                        self.typed(Some(outer.clone()), span).unwrap_or(outer)
                    }
                    // `this.field`
                    (Some(o), Some(f)) if o.kind() == "this" && f.kind() == "identifier" => {
                        let name = self.text(f);
                        match self.types.last().and_then(|t| t.fields.get(name).cloned()) {
                            Some(class) => self.typed(class, span).unwrap_or_else(|| self.raw(object)),
                            None => self.raw(object),
                        }
                    }
                    // `a.b.C` (a qualified class) or `x.y` (a field of a local / field).
                    _ => match self.identifier_path(object) {
                        Some(path) if !self.locals.is_local(path[0], pos) && self.field_class(path[0]).is_none() => {
                            let t = path.join(".");
                            self.unqualify(&t, span)
                        }
                        _ => self.raw(object),
                    },
                }
            }
            "parenthesized_expression" => match object.named_child(0) {
                Some(inner) => self.receiver(inner),
                None => self.raw(object),
            },
            "cast_expression" => {
                let class = object.child_by_field_name("type").and_then(|t| class_of_type(t, self.source));
                self.typed(class, span).unwrap_or_else(|| self.raw(object))
            }
            "object_creation_expression" => {
                let anonymous = named_children(object).iter().any(|c| c.kind() == "class_body");
                let class = object.child_by_field_name("type").and_then(|t| class_of_type(t, self.source));
                if anonymous { self.raw(object) } else { self.typed(class, span).unwrap_or_else(|| self.raw(object)) }
            }
            "string_literal" => type_receiver("String"),
            // `m().x()` where `m` is a method of an enclosing type of this
            // file with a declared return class.
            "method_invocation" => {
                let own = object.child_by_field_name("object").is_none_or(|o| o.kind() == "this");
                let name = object.child_by_field_name("name").map(|n| self.text(n));
                match name.filter(|_| own).and_then(|n| self.method_return_class(n)) {
                    Some(class) => self.typed(Some(class), span).unwrap_or_else(|| self.raw(object)),
                    None => self.raw(object),
                }
            }
            _ => self.raw(object),
        }
    }

    /// Name and receiver of a class type node (`Foo`, `a.b.Foo`,
    /// `Outer.Inner<T>`), for `Type` / `New` / `Throw` references. A
    /// qualifier that is a type visible here is a `$type:` receiver, so the
    /// resolver finds member types of enclosing and inherited classes.
    fn type_name(&mut self, ty: Node<'_>) -> Option<(String, Option<String>)> {
        let class = match ty.kind() {
            "type_identifier" | "scoped_type_identifier" | "generic_type" => class_of_type(ty, self.source)?,
            "annotated_type" => {
                let inner = named_children(ty).into_iter().find(|c| !is_annotation(c.kind()))?;
                return self.type_name(inner);
            }
            _ => return None,
        };
        let class = self.unqualify(&class, span_of(ty));
        let head = class.split('.').next().unwrap_or(&class).to_string();
        if self.is_type_param(&head) {
            return None;
        }
        Some(match class.rsplit_once('.') {
            Some((q, n)) => {
                let receiver = if self.visible_type(&head).is_some() {
                    self.typed(Some(q.to_string()), span_of(ty)).unwrap_or_else(|| q.to_string())
                } else {
                    q.to_string()
                };
                (n.to_string(), Some(receiver))
            }
            None => (class, None),
        })
    }

    /// `Type` references for every class a type node mentions (generic
    /// arguments, array elements, wildcard bounds, annotations).
    fn visit_type(&mut self, ty: Node<'_>, depth: usize) {
        self.visit_type_as(ty, RefKind::Type, depth);
    }

    fn visit_type_as(&mut self, ty: Node<'_>, kind: RefKind, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match ty.kind() {
            "type_identifier" | "scoped_type_identifier" => {
                if let Some((name, receiver)) = self.type_name(ty) {
                    self.push_ref(kind, &name, receiver, span_of(ty), None);
                }
                // Generic arguments inside a scoped type (`Outer<A>.Inner<B>`).
                if ty.kind() == "scoped_type_identifier" {
                    for c in named_children(ty) {
                        match c.kind() {
                            "generic_type" => self.visit_type_args(c, depth + 1),
                            k if is_annotation(k) => self.visit_annotation(c, false, depth + 1),
                            _ => {}
                        }
                    }
                }
            }
            "generic_type" => {
                if let Some((name, receiver)) = self.type_name(ty) {
                    self.push_ref(kind, &name, receiver, span_of(ty), None);
                }
                self.visit_type_args(ty, depth + 1);
            }
            "array_type" => {
                if let Some(e) = ty.child_by_field_name("element") {
                    self.visit_type_as(e, kind, depth + 1);
                }
            }
            "annotated_type" => {
                for c in named_children(ty) {
                    if is_annotation(c.kind()) {
                        self.visit_annotation(c, false, depth + 1);
                    } else {
                        self.visit_type_as(c, kind, depth + 1);
                    }
                }
            }
            "type_arguments" | "wildcard" | "type_bound" | "type_list" | "catch_type" | "throws" | "class_literal"
            | "superclass" | "super_interfaces" | "extends_interfaces" | "permits" => {
                for c in named_children(ty) {
                    if is_annotation(c.kind()) {
                        self.visit_annotation(c, false, depth + 1);
                    } else {
                        self.visit_type_as(c, kind, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    /// `Type` references for the generic arguments of a type, not the type itself.
    fn visit_type_args(&mut self, ty: Node<'_>, depth: usize) {
        for c in named_children(ty) {
            match c.kind() {
                "type_arguments" => self.visit_type(c, depth + 1),
                "generic_type" | "scoped_type_identifier" => self.visit_type_args(c, depth + 1),
                _ => {}
            }
        }
    }

    /// An annotation: a `Decorator` reference when it annotates a
    /// definition (`as_decorator`), else a `Type` reference; its arguments
    /// are visited, and Spring `@Value("${a.b}")` adds `ConfigKey` reads.
    fn visit_annotation(&mut self, node: Node<'_>, as_decorator: bool, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let full: String = self.text(name_node).split_whitespace().collect();
        let full = self.unqualify(&full, span_of(name_node));
        let (receiver, name) = match full.rsplit_once('.') {
            Some((q, n)) => (Some(q.to_string()), n.to_string()),
            None => (None, full.clone()),
        };
        let kind = if as_decorator { RefKind::Decorator } else { RefKind::Type };
        self.push_ref(kind, &name, receiver.clone(), span_of(node), None);
        let Some(args) = node.child_by_field_name("arguments") else { return };
        let spring_value = name == "Value" && receiver.is_none() && self.imports_from(SPRING_VALUE, "Value");
        if spring_value {
            for a in named_children(args) {
                let value = match a.kind() {
                    "element_value_pair" => {
                        let key = a.child_by_field_name("key").map(|k| self.text(k));
                        if key != Some("value") {
                            continue;
                        }
                        a.child_by_field_name("value")
                    }
                    _ => Some(a),
                };
                if let Some(s) = value.and_then(|v| string_literal_value(v, self.source)) {
                    for key in placeholder_keys(&s) {
                        self.push_ref(RefKind::ConfigKey, &key, Some(SPRING_NAMESPACE.into()), span_of(a), None);
                    }
                }
            }
        }
        for a in named_children(args) {
            match a.kind() {
                "element_value_pair" => {
                    if let Some(v) = a.child_by_field_name("value") {
                        self.visit(v, depth + 1);
                    }
                }
                _ => self.visit(a, depth + 1),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Expressions and statements
    // -----------------------------------------------------------------------

    fn visit_children(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            self.visit(c, depth + 1);
        }
    }

    /// Visits a node. Kept small (the recursion goes through it for every
    /// level of the tree): long left-nested chains (`a + b + …`,
    /// `x.a().b()…`) are walked iteratively, and every other construct is
    /// handled out of line by [`Self::visit_construct`].
    fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            k if is_comment(k) => {}
            "binary_expression" => self.visit_binary(node, depth),
            "method_invocation" => self.visit_call(node, depth),
            _ => {
                if !self.visit_construct(node, depth) {
                    self.visit_children(node, depth);
                }
            }
        }
    }

    /// `a + b + c` nests to the left: walk the left spine iteratively.
    fn visit_binary(&mut self, node: Node<'_>, depth: usize) {
        let mut rights = Vec::new();
        let mut cur = node;
        while cur.kind() == "binary_expression" {
            if let Some(r) = cur.child_by_field_name("right") {
                rights.push(r);
            }
            match cur.child_by_field_name("left") {
                Some(l) => cur = l,
                None => break,
            }
        }
        if cur.kind() != "binary_expression" {
            self.visit(cur, depth + 1);
        }
        for r in rights.into_iter().rev() {
            self.visit(r, depth + 1);
        }
    }

    /// Constructs with their own rules; `false` when `node` is visited
    /// generically (its named children).
    #[inline(never)]
    fn visit_construct(&mut self, node: Node<'_>, depth: usize) -> bool {
        match node.kind() {
            "package_declaration" | "import_declaration" | "module_declaration" => {}
            k if is_type_decl(k) => self.visit_type_decl(node, depth),
            // Top-level methods (scripts): functions.
            "method_declaration" => self.visit_method(node, depth),
            "object_creation_expression" => self.visit_new(node, depth),
            "explicit_constructor_invocation" => self.visit_explicit_ctor(node, depth),
            "method_reference" => self.visit_method_ref(node, depth),
            "field_access" => self.visit_field_access(node, depth),
            "identifier" => self.visit_identifier(node),
            "assignment_expression" => self.visit_assignment(node, depth),
            "type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "array_type"
            | "annotated_type"
            | "type_arguments" => self.visit_type(node, depth),
            k if is_annotation(k) => self.visit_annotation(node, false, depth),
            "local_variable_declaration" | "field_declaration" | "constant_declaration" => {
                for c in named_children(node) {
                    match c.kind() {
                        "modifiers" => self.visit_modifiers(c, false, depth),
                        "variable_declarator" => self.visit_declarator(c, depth),
                        _ => self.visit_type(c, depth),
                    }
                }
            }
            "variable_declarator" => self.visit_declarator(node, depth),
            "formal_parameter"
            | "spread_parameter"
            | "receiver_parameter"
            | "catch_formal_parameter"
            | "record_pattern_component"
            | "type_pattern" => {
                for c in named_children(node) {
                    match c.kind() {
                        "modifiers" => self.visit_modifiers(c, false, depth),
                        "identifier" | "underscore_pattern" | "this" | "dimensions" => {}
                        "variable_declarator" => self.visit_declarator(c, depth),
                        _ => self.visit_type(c, depth),
                    }
                }
            }
            "modifiers" => self.visit_modifiers(node, false, depth),
            "cast_expression" => {
                for c in named_children(node) {
                    if Some(c) == node.child_by_field_name("value") {
                        self.visit(c, depth + 1);
                    } else {
                        self.visit_type(c, depth);
                    }
                }
            }
            "instanceof_expression" => {
                if let Some(l) = node.child_by_field_name("left") {
                    self.visit(l, depth + 1);
                }
                if let Some(r) = node.child_by_field_name("right") {
                    self.visit_type(r, depth);
                }
                if let Some(p) = node.child_by_field_name("pattern") {
                    self.visit(p, depth + 1);
                }
            }
            "record_pattern" => {
                for c in named_children(node) {
                    match c.kind() {
                        "identifier" => {
                            let name = self.text(c).to_string();
                            if !self.is_type_param(&name) {
                                self.push_ref(RefKind::Type, &name, None, span_of(c), None);
                            }
                        }
                        "generic_type" => self.visit_type(c, depth),
                        _ => self.visit(c, depth + 1),
                    }
                }
            }
            "class_literal" | "catch_type" | "throws" => self.visit_type(node, depth),
            "array_creation_expression" => {
                for c in named_children(node) {
                    if Some(c) == node.child_by_field_name("type") {
                        self.visit_type(c, depth);
                    } else {
                        self.visit(c, depth + 1);
                    }
                }
            }
            "throw_statement" => {
                if let Some(e) = node.named_child(0)
                    && e.kind() == "object_creation_expression"
                    && let Some(t) = e.child_by_field_name("type")
                    && let Some((name, receiver)) = self.type_name(t)
                {
                    self.push_ref(RefKind::Throw, &name, receiver, span_of(t), None);
                }
                self.visit_children(node, depth);
            }
            "enhanced_for_statement" => {
                for c in named_children(node) {
                    match c.kind() {
                        "modifiers" => self.visit_modifiers(c, false, depth),
                        "identifier" | "underscore_pattern" | "dimensions" => {}
                        _ if Some(c) == node.child_by_field_name("type") => self.visit_type(c, depth),
                        _ => self.visit(c, depth + 1),
                    }
                }
            }
            "resource" => {
                for c in named_children(node) {
                    match c.kind() {
                        "modifiers" => self.visit_modifiers(c, false, depth),
                        "dimensions" | "underscore_pattern" => {}
                        _ if Some(c) == node.child_by_field_name("name") => {}
                        _ if Some(c) == node.child_by_field_name("type") => self.visit_type(c, depth),
                        _ => self.visit(c, depth + 1),
                    }
                }
            }
            "lambda_expression" => {
                if let Some(p) = node.child_by_field_name("parameters")
                    && p.kind() == "formal_parameters"
                {
                    self.visit_children(p, depth);
                }
                if let Some(b) = node.child_by_field_name("body") {
                    self.visit(b, depth + 1);
                }
            }
            "labeled_statement" => {
                for c in named_children(node) {
                    if c.kind() != "identifier" {
                        self.visit(c, depth + 1);
                    }
                }
            }
            "break_statement"
            | "continue_statement"
            | "inferred_parameters"
            | "this"
            | "super"
            | "scoped_identifier" => {}
            "element_value_pair" => {
                if let Some(v) = node.child_by_field_name("value") {
                    self.visit(v, depth + 1);
                }
            }
            "string_literal" => {
                for c in named_children(node) {
                    if c.kind() == "string_interpolation" {
                        self.visit_children(c, depth);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// Annotations of a `modifiers` node.
    fn visit_modifiers(&mut self, node: Node<'_>, as_decorator: bool, depth: usize) {
        for c in named_children(node) {
            if is_annotation(c.kind()) {
                self.visit_annotation(c, as_decorator, depth + 1);
            }
        }
    }

    fn visit_declarator(&mut self, node: Node<'_>, depth: usize) {
        if let Some(v) = node.child_by_field_name("value") {
            self.visit(v, depth + 1);
        }
    }

    /// A bare identifier in an expression: a `Read` of a field of an
    /// enclosing type or of an `UPPER_CASE` constant, unless it is a local.
    fn visit_identifier(&mut self, node: Node<'_>) {
        let name = self.text(node);
        if self.locals.is_local(name, node.start_byte() as u32) {
            return;
        }
        if self.field_class(name).is_some() || is_screaming(name) {
            self.push_ref(RefKind::Read, name, None, span_of(node), None);
        }
    }

    fn visit_assignment(&mut self, node: Node<'_>, depth: usize) {
        if let Some(left) = node.child_by_field_name("left") {
            match left.kind() {
                "identifier" => {
                    let name = self.text(left);
                    if !self.locals.is_local(name, left.start_byte() as u32)
                        && (self.field_class(name).is_some() || is_screaming(name))
                    {
                        self.push_ref(RefKind::Write, name, None, span_of(left), None);
                    }
                }
                "field_access" => {
                    let obj = left.child_by_field_name("object");
                    let field = left.child_by_field_name("field").filter(|f| f.kind() == "identifier");
                    if let (Some(o), Some(f)) = (obj, field) {
                        let receiver = self.receiver(o);
                        let typed = receiver == "this" || receiver.starts_with("$type:");
                        let name = self.text(f);
                        if typed {
                            self.push_ref(RefKind::Write, name, Some(receiver), span_of(f), None);
                        }
                        match o.kind() {
                            "identifier" => self.visit_identifier_receiver(o),
                            "this" => {}
                            _ => self.visit(o, depth + 1),
                        }
                    }
                }
                _ => self.visit(left, depth + 1),
            }
        }
        if let Some(right) = node.child_by_field_name("right") {
            self.visit(right, depth + 1);
        }
    }

    fn arity(args: Option<Node<'_>>) -> Option<u32> {
        let args = args?;
        Some(named_children(args).iter().filter(|c| !is_comment(c.kind())).count() as u32)
    }

    /// A call chain `x.a().b().c()` nests through its receivers: visited
    /// iteratively, innermost call first (source order).
    fn visit_call(&mut self, node: Node<'_>, depth: usize) {
        let mut chain = vec![node];
        let mut cur = node;
        while let Some(o) = cur.child_by_field_name("object")
            && o.kind() == "method_invocation"
        {
            chain.push(o);
            cur = o;
        }
        for call in chain.into_iter().rev() {
            if self.refs.len() >= MAX_REFS {
                return;
            }
            self.visit_one_call(call, depth);
        }
    }

    fn visit_one_call(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return self.visit_children(node, depth);
        };
        let name = self.text(name_node).to_string();
        let args = node.child_by_field_name("arguments");
        let arity = Self::arity(args);
        let object = node.child_by_field_name("object");
        // `X.super.m()`: an interface's default method.
        let qualified_super = object.is_some_and(|o| o.kind() != "super")
            && named_children(node).iter().any(|c| c.kind() == "super" && Some(*c) != object);
        let receiver = match object {
            None => None,
            Some(o) if qualified_super && o.kind() == "identifier" => Some(type_receiver(self.text(o))),
            Some(o) => Some(self.receiver(o)),
        };
        let first_string = args.and_then(|a| named_children(a).into_iter().find(|c| !is_comment(c.kind())));
        let first_string = first_string.and_then(|a| string_literal_value(a, self.source));

        let mut bridged = false;
        // `System.getenv("X")` → env var read.
        if name == "getenv"
            && matches!(receiver.as_deref(), Some("System" | "java.lang.System"))
            && !self.declared_types.contains("System")
            && self.imported.get("System").is_none_or(|f| f == "java.lang.System")
            && let Some(var) = &first_string
        {
            self.push_ref(RefKind::EnvVar, var, None, span_of(node), None);
            bridged = true;
        }
        // Spring `env.getProperty("a.b")` on a declared `Environment`.
        if SPRING_ENV_METHODS.contains(&name.as_str())
            && let Some(ty) = receiver.as_deref().and_then(|r| r.strip_prefix("$type:"))
            && let Some(key) = &first_string
        {
            let simple = ty.rsplit('.').next().unwrap_or(ty);
            let spring = SPRING_ENV_TYPES.contains(&simple)
                && (ty.starts_with(SPRING_ENV) || self.imports_from(SPRING_ENV, simple));
            if spring && !key.contains("${") {
                self.push_ref(RefKind::ConfigKey, key, Some(SPRING_NAMESPACE.into()), span_of(node), None);
                bridged = true;
            }
        }
        if !bridged {
            let named = self.with_args(&name, args);
            self.push_ref(RefKind::Call, &named, receiver, span_of(name_node), arity);
        }

        if let Some(o) = object {
            match o.kind() {
                // A field used as a receiver is read.
                "identifier" => self.visit_identifier_receiver(o),
                // Visited by `visit_call` as part of the chain.
                "this" | "super" | "method_invocation" => {}
                _ => self.visit(o, depth + 1),
            }
        }
        if let Some(t) = node.child_by_field_name("type_arguments") {
            self.visit_type(t, depth);
        }
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
    }

    /// An identifier used as a call or field-access receiver: a `Read` of
    /// the field when it is a field of an enclosing type (class names and
    /// package heads are not reads).
    fn visit_identifier_receiver(&mut self, node: Node<'_>) {
        let name = self.text(node);
        if !self.locals.is_local(name, node.start_byte() as u32) && self.field_class(name).is_some() {
            self.push_ref(RefKind::Read, name, None, span_of(node), None);
        }
    }

    fn visit_new(&mut self, node: Node<'_>, depth: usize) {
        let args = node.child_by_field_name("arguments");
        let body = named_children(node).into_iter().find(|c| c.kind() == "class_body");
        let ty = node.child_by_field_name("type");
        if let Some(t) = ty {
            if let Some((name, receiver)) = self.type_name(t) {
                // An anonymous class body names its base: a type reference.
                let (kind, arity, name) = match body {
                    Some(_) => (RefKind::Type, None, name),
                    None => (RefKind::New, Self::arity(args), self.with_args(&name, args)),
                };
                self.push_ref(kind, &name, receiver, span_of(t), arity);
            }
            self.visit_type_args(t, depth);
        }
        for c in named_children(node) {
            if Some(c) == node.child_by_field_name("type") || Some(c) == args || Some(c) == body {
                continue;
            }
            match c.kind() {
                "type_arguments" => self.visit_type(c, depth),
                k if is_annotation(k) => self.visit_annotation(c, false, depth),
                _ => self.visit(c, depth + 1),
            }
        }
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
        if let Some(b) = body {
            let base = ty.and_then(|t| class_of_type(t, self.source)).map(|c| self.unqualify(&c, span_of(node)));
            self.visit_anonymous_class(node, base, ty.unwrap_or(node), b, depth);
        }
    }

    /// `this(…)` / `super(…)` in a constructor: a call to a constructor of
    /// this class or of its superclass (constructors are named like their
    /// class).
    fn visit_explicit_ctor(&mut self, node: Node<'_>, depth: usize) {
        let args = node.child_by_field_name("arguments");
        if let Some(c) = node.child_by_field_name("constructor")
            && let Some(t) = self.types.last()
        {
            let target = match c.kind() {
                "this" => Some((t.name.clone(), "this")),
                "super" => t.superclass.clone().map(|s| (s, "super")),
                _ => None,
            };
            if let Some((name, receiver)) = target {
                let name = self.with_args(&name, args);
                self.push_ref(RefKind::Call, &name, Some(receiver.to_string()), span_of(c), Self::arity(args));
            }
        }
        if let Some(o) = node.child_by_field_name("object") {
            self.visit(o, depth + 1);
        }
        if let Some(t) = node.child_by_field_name("type_arguments") {
            self.visit_type(t, depth);
        }
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
    }

    /// `Foo::bar`, `this::bar`, `x::bar`, `Foo::new` (best effort: a `Call`
    /// without arity, or a `New`).
    fn visit_method_ref(&mut self, node: Node<'_>, depth: usize) {
        let children = named_children(node);
        let Some(&target) = children.first() else { return };
        let t = self.text(node);
        let method = children.iter().skip(1).rev().find(|c| c.kind() == "identifier").copied();
        let is_new = method.is_none() && t.trim_end().ends_with("new");
        let is_type = matches!(
            target.kind(),
            "type_identifier" | "scoped_type_identifier" | "generic_type" | "array_type" | "annotated_type"
        );
        if is_new {
            if is_type || target.kind() == "identifier" || target.kind() == "field_access" {
                let name_recv = if is_type {
                    self.type_name(target)
                } else {
                    let p = self.text(target);
                    let p: String = p.split_whitespace().collect();
                    Some(match p.rsplit_once('.') {
                        Some((q, n)) => (n.to_string(), Some(q.to_string())),
                        None => (p, None),
                    })
                };
                if let Some((name, receiver)) = name_recv {
                    self.push_ref(RefKind::New, &name, receiver, span_of(target), None);
                }
            }
        } else if let Some(m) = method {
            let receiver = if is_type {
                class_of_type(target, self.source)
                    .filter(|c| !self.is_type_param(c.split('.').next().unwrap_or(c)))
                    .map(|c| self.unqualify(&c, span_of(target)))
            } else {
                Some(self.receiver(target))
            };
            if let Some(r) = receiver {
                self.push_ref(RefKind::Call, self.text(m), Some(r), span_of(m), None);
            }
        }
        if is_type {
            self.visit_type_args(target, depth);
        } else if target.kind() == "identifier" {
            self.visit_identifier_receiver(target);
        } else if !matches!(target.kind(), "this" | "super") {
            self.visit(target, depth + 1);
        }
        for c in children.iter().skip(1) {
            if c.kind() == "type_arguments" {
                self.visit_type(*c, depth);
            }
        }
    }

    fn visit_field_access(&mut self, node: Node<'_>, depth: usize) {
        let (Some(obj), Some(field)) = (node.child_by_field_name("object"), node.child_by_field_name("field")) else {
            return self.visit_children(node, depth);
        };
        if field.kind() != "identifier" {
            // `Outer.this`
            return;
        }
        let name = self.text(field);
        let has_super = named_children(node).iter().any(|c| c.kind() == "super" && *c != obj);
        if has_super {
            return;
        }
        match obj.kind() {
            "this" | "super" => {
                let r = self.text(obj).to_string();
                self.push_ref(RefKind::Read, name, Some(r), span_of(field), None);
            }
            "identifier" | "field_access" => {
                if let Some(path) = self.identifier_path(obj) {
                    let head = path[0];
                    let head_local = self.locals.is_local(head, obj.start_byte() as u32);
                    let head_field = !head_local && self.field_class(head).is_some();
                    if path.len() == 1 && (head_local || head_field) {
                        let receiver = self.receiver(obj);
                        if receiver.starts_with("$type:") {
                            self.push_ref(RefKind::Read, name, Some(receiver), span_of(field), None);
                        }
                        if head_field {
                            self.push_ref(RefKind::Read, head, None, span_of(obj), None);
                        }
                    } else if head_local || head_field {
                        self.visit(obj, depth + 1);
                    } else if path.last().is_some_and(|s| s.starts_with(|c: char| c.is_ascii_uppercase())) {
                        // `Type.FIELD`, `a.b.Type.FIELD`, `Outer.Inner.FIELD`.
                        let q = self.unqualify(&path.join("."), span_of(obj));
                        let receiver = if self.visible_type(head).is_some() {
                            self.typed(Some(q.clone()), span_of(obj)).unwrap_or(q)
                        } else {
                            q
                        };
                        self.push_ref(RefKind::Read, name, Some(receiver), span_of(field), None);
                    }
                } else {
                    self.visit(obj, depth + 1);
                }
            }
            _ => {
                let receiver = self.receiver(obj);
                if receiver.starts_with("$type:") {
                    self.push_ref(RefKind::Read, name, Some(receiver), span_of(field), None);
                }
                self.visit(obj, depth + 1);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Definitions
    // -----------------------------------------------------------------------

    /// Local qualified name of a definition named `name` inside the current
    /// enclosing definition.
    fn child_qn(&self, name: &str) -> String {
        match self.enc {
            Some(e) => format!("{}.{name}", self.defs[e as usize].qualified_name),
            None => name.to_string(),
        }
    }

    /// The `/** … */` comment right before `node` (only whitespace between).
    fn javadoc(&self, node: Node<'_>) -> Option<String> {
        let prev = node.prev_sibling()?;
        if prev.kind() != "block_comment" {
            return None;
        }
        let between = &self.source[prev.end_byte()..node.start_byte()];
        if !between.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        clean_javadoc(self.text(prev))
    }

    /// Declaration header from its first keyword to `end` (body start),
    /// annotations and comments removed, whitespace collapsed.
    fn signature(&self, node: Node<'_>, end: usize) -> String {
        let mut skip: Vec<(usize, usize)> = Vec::new();
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            if c.start_byte() >= end {
                break;
            }
            if c.kind() == "modifiers" {
                for m in named_children(c) {
                    if is_annotation(m.kind()) || is_comment(m.kind()) {
                        skip.push((m.start_byte(), m.end_byte()));
                    }
                }
            } else if is_annotation(c.kind()) || is_comment(c.kind()) {
                skip.push((c.start_byte(), c.end_byte()));
            }
        }
        let mut out = String::new();
        let mut pos = node.start_byte();
        for (s, e) in skip {
            if s > pos {
                out.push_str(&String::from_utf8_lossy(&self.source[pos..s.min(end)]));
            }
            pos = pos.max(e);
        }
        if pos < end {
            out.push_str(&String::from_utf8_lossy(&self.source[pos..end]));
        }
        let s = collapse(out.trim_end_matches(['{', ';', ' ', '\n', '\t', '\r']));
        truncate_chars(s, MAX_SIGNATURE)
    }

    /// Modifier keywords and annotations of a declaration.
    fn modifiers<'t>(&self, node: Node<'t>) -> (HashSet<&'t str>, Vec<Node<'t>>) {
        let mut keywords: HashSet<&'t str> = HashSet::new();
        let mut annotations: Vec<Node<'t>> = Vec::new();
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            if c.kind() == "modifiers" {
                let mut cc = c.walk();
                for m in c.children(&mut cc) {
                    if is_annotation(m.kind()) {
                        annotations.push(m);
                    } else if !m.is_named() {
                        keywords.insert(m.kind());
                    }
                }
            } else if is_annotation(c.kind()) {
                annotations.push(c);
            }
        }
        (keywords, annotations)
    }

    fn annotation_names(&self, annotations: &[Node<'_>]) -> Vec<String> {
        annotations
            .iter()
            .filter_map(|a| a.child_by_field_name("name"))
            .map(|n| annotation_simple_name(&self.text(n).split_whitespace().collect::<String>()).to_string())
            .collect()
    }

    fn decorator_texts(&self, annotations: &[Node<'_>]) -> Vec<String> {
        annotations.iter().map(|a| collapse(self.text(*a).trim_start_matches('@').trim())).collect()
    }

    fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.defs.push(def);
        (self.defs.len() - 1) as LocalDefId
    }

    /// Whether members declared in the innermost type are implicitly public.
    fn in_implicit_public(&self) -> bool {
        self.types.last().is_some_and(|t| t.implicit_public)
            && self.enc.is_some_and(|e| Some(e) == self.types.last().map(|t| t.def))
    }

    fn exported(&self, keywords: &HashSet<&str>) -> bool {
        if keywords.contains("private") {
            return false;
        }
        keywords.contains("public") || keywords.contains("protected") || self.in_implicit_public()
    }

    fn push_type_params(&mut self, node: Node<'_>, depth: usize) -> usize {
        let mark = self.type_params.len();
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            for p in named_children(tp) {
                if let Some(id) = named_children(p).into_iter().find(|c| c.kind() == "type_identifier") {
                    self.type_params.push(self.text(id).to_string());
                }
            }
            for p in named_children(tp) {
                for c in named_children(p) {
                    match c.kind() {
                        "type_bound" => self.visit_type(c, depth),
                        k if is_annotation(k) => self.visit_annotation(c, false, depth),
                        _ => {}
                    }
                }
            }
        }
        mark
    }

    fn visit_type_decl(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = self.text(name_node).to_string();
        let kind = node.kind();
        let label = match kind {
            "interface_declaration" | "annotation_type_declaration" => Label::Interface,
            "enum_declaration" => Label::Enum,
            _ => Label::Class,
        };
        let (keywords, annotations) = self.modifiers(node);
        let ann_names = self.annotation_names(&annotations);
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());

        let mut bases = Vec::new();
        let mut implements = Vec::new();
        let base_texts = |ex: &mut Self, n: Option<Node<'_>>| -> Vec<String> {
            let Some(n) = n else { return Vec::new() };
            let list = named_children(n).into_iter().find(|c| c.kind() == "type_list");
            let types = match list {
                Some(l) => named_children(l),
                None => named_children(n),
            };
            types
                .into_iter()
                .filter(|t| !is_comment(t.kind()))
                .map(|t| {
                    let raw = strip_type_text(text(t, ex.source));
                    ex.unqualify(&raw, span_of(t))
                })
                .collect()
        };
        let superclass = node.child_by_field_name("superclass");
        let interfaces = node.child_by_field_name("interfaces");
        let extends_ifaces = named_children(node).into_iter().find(|c| c.kind() == "extends_interfaces");
        match kind {
            "class_declaration" => {
                bases = base_texts(self, superclass);
                implements = base_texts(self, interfaces);
            }
            "interface_declaration" => bases = base_texts(self, extends_ifaces),
            "enum_declaration" | "record_declaration" => implements = base_texts(self, interfaces),
            _ => {}
        }
        let superclass_name = bases.first().map(|b| b.rsplit('.').next().unwrap_or(b).to_string());
        let junit3 = superclass_name.as_deref().is_some_and(|s| s.ends_with("TestCase"));
        let test_name =
            ["Test", "Tests", "IT", "TestCase", "ITCase"].iter().any(|s| name.ends_with(s) && name.len() > s.len());
        let class_level_test = ann_names.iter().any(|a| a == "Test");
        let test_class = test_name || class_level_test || ann_names.iter().any(|a| a == "Nested");
        // A local class in test code is test code.
        let in_test =
            self.enc.is_some_and(|e| self.defs[e as usize].is_test) || self.types.last().is_some_and(|t| t.in_test);

        let parent = self.enc;
        let exported = self.exported(&keywords);
        let def = Definition {
            label,
            name: name.clone(),
            qualified_name: self.child_qn(&name),
            parent,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.signature(node, end)),
            doc: self.javadoc(node),
            decorators: self.decorator_texts(&annotations),
            bases,
            implements,
            exported,
            is_test: (test_class && label == Label::Class) || in_test,
            is_declaration: false,
        };
        let id = self.push_def(def);

        let saved_enc = self.enc;
        self.enc = Some(id);
        for a in &annotations {
            self.visit_annotation(*a, true, depth + 1);
        }
        let mark = self.push_type_params(node, depth + 1);
        // Generic arguments of bases (`extends Base<Foo>`) and permits.
        for n in [superclass, interfaces, extends_ifaces].into_iter().flatten() {
            self.visit_type_args_of_list(n, depth + 1);
        }
        if let Some(p) = node.child_by_field_name("permits") {
            self.visit_type(p, depth + 1);
        }

        let local_qn = self.defs[id as usize].qualified_name.clone();
        let file_type = self.file_types.iter().position(|f| f.qn == local_qn);
        self.types.push(TypeCtx {
            file_type,
            def: id,
            name: name.clone(),
            fields: HashMap::new(),
            field_types: HashMap::new(),
            superclass: superclass_name,
            implicit_public: label == Label::Interface,
            test_class,
            junit3,
            class_level_test,
            has_tests: false,
            record_params: node.child_by_field_name("parameters").map(|p| parameter_types(p, self.source)),
            methods: HashMap::new(),
            in_test,
        });

        // Record components are fields, before the members (source order).
        let components = node.child_by_field_name("parameters").filter(|_| kind == "record_declaration");
        if let Some(params) = components {
            self.collect_record_components(params);
            self.visit_record_components(params, depth + 1);
        }
        if let Some(b) = body {
            self.visit_body(b, depth + 1);
        }

        let ctx = self.types.pop().expect("pushed");
        if ctx.has_tests && label == Label::Class {
            self.defs[id as usize].is_test = true;
        }
        self.type_params.truncate(mark);
        self.enc = saved_enc;
    }

    /// `Type` references for the generic arguments in a base list.
    fn visit_type_args_of_list(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            match c.kind() {
                "type_list" => self.visit_type_args_of_list(c, depth + 1),
                "generic_type" | "scoped_type_identifier" => self.visit_type_args(c, depth + 1),
                k if is_annotation(k) => self.visit_annotation(c, false, depth + 1),
                _ => {}
            }
        }
    }

    fn collect_record_components(&mut self, params: Node<'_>) {
        for p in named_children(params) {
            if p.kind() != "formal_parameter" {
                continue;
            }
            let Some(n) = p.child_by_field_name("name") else { continue };
            let class = p.child_by_field_name("type").and_then(|t| class_of_type(t, self.source));
            let ty = p.child_by_field_name("type").map(|t| suffix_type(self.text(t)));
            let name = self.text(n).to_string();
            if let Some(t) = self.types.last_mut() {
                if let Some(ty) = ty {
                    t.field_types.insert(name.clone(), ty);
                }
                t.fields.insert(name, class);
            }
        }
    }

    fn visit_record_components(&mut self, params: Node<'_>, depth: usize) {
        let owner_exported = self.enc.is_some_and(|e| self.defs[e as usize].exported);
        for p in named_children(params) {
            if p.kind() != "formal_parameter" {
                continue;
            }
            let Some(n) = p.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let ty = p.child_by_field_name("type");
            let (_, annotations) = self.modifiers(p);
            let def = Definition {
                label: Label::Field,
                name: name.clone(),
                qualified_name: self.child_qn(&name),
                parent: self.enc,
                span: span_of(p),
                name_span: span_of(n),
                signature: Some(truncate_chars(collapse(self.text(p)), MAX_SIGNATURE)),
                doc: None,
                decorators: self.decorator_texts(&annotations),
                bases: Vec::new(),
                implements: Vec::new(),
                exported: owner_exported,
                is_test: false,
                is_declaration: false,
            };
            let id = self.push_def(def);
            let saved = self.enc;
            self.enc = Some(id);
            for a in &annotations {
                self.visit_annotation(*a, true, depth + 1);
            }
            if let Some(t) = ty {
                self.visit_type(t, depth + 1);
            }
            self.enc = saved;
        }
    }

    /// Callable member names declared in a body, with their counts (for
    /// overload suffixes); constructors count under the class name.
    fn callable_counts(&self, body: Node<'_>, type_name: &str) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut members = named_children(body);
        if let Some(decls) = members.iter().position(|c| c.kind() == "enum_body_declarations") {
            let inner = named_children(members[decls]);
            members.extend(inner);
        }
        for m in members {
            let name = match m.kind() {
                "method_declaration" | "annotation_type_element_declaration" => {
                    m.child_by_field_name("name").map(|n| self.text(n).to_string())
                }
                "constructor_declaration" | "compact_constructor_declaration" => Some(type_name.to_string()),
                _ => None,
            };
            if let Some(n) = name {
                *counts.entry(n).or_default() += 1;
            }
        }
        counts
    }

    /// Fields declared in a body (pre-collected so earlier methods see them).
    fn collect_fields(&mut self, body: Node<'_>) {
        let mut members = named_children(body);
        if let Some(decls) = members.iter().position(|c| c.kind() == "enum_body_declarations") {
            let inner = named_children(members[decls]);
            members.extend(inner);
        }
        let mut fields = Vec::new();
        for m in members {
            match m.kind() {
                "field_declaration" | "constant_declaration" => {
                    let ty = m.child_by_field_name("type");
                    let class = ty.and_then(|t| class_of_type(t, self.source));
                    let base = ty.map(|t| suffix_type(self.text(t)));
                    for d in named_children(m).into_iter().filter(|c| c.kind() == "variable_declarator") {
                        let Some(n) = d.child_by_field_name("name") else { continue };
                        let dims = d.child_by_field_name("dimensions");
                        let c = if dims.is_some() { None } else { class.clone() };
                        let t = base.clone().map(|b| {
                            let dims: String = dims.map(|x| self.text(x)).unwrap_or("").split_whitespace().collect();
                            format!("{b}{dims}")
                        });
                        fields.push((self.text(n).to_string(), c, t));
                    }
                }
                "enum_constant" => {
                    if let Some(n) = m.child_by_field_name("name") {
                        let owner = self.types.last().map(|t| t.name.clone());
                        fields.push((self.text(n).to_string(), owner.clone(), owner));
                    }
                }
                _ => {}
            }
        }
        if let Some(t) = self.types.last_mut() {
            for (n, c, ty) in fields {
                if let Some(ty) = ty {
                    t.field_types.entry(n.clone()).or_insert(ty);
                }
                t.fields.entry(n).or_insert(c);
            }
        }
    }

    /// Members of a class / interface / enum / record / annotation body.
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
        self.visit_members(body, &counts, depth);
    }

    /// Declared return class of each method of a body (`None` when the
    /// overloads disagree, or for primitives, arrays and the method's own
    /// type parameters).
    fn method_returns(&self, body: Node<'_>) -> HashMap<String, Option<Returns>> {
        let mut out: HashMap<String, Option<Returns>> = HashMap::new();
        let mut members = named_children(body);
        if let Some(decls) = members.iter().position(|c| c.kind() == "enum_body_declarations") {
            let inner = named_children(members[decls]);
            members.extend(inner);
        }
        for m in members.into_iter().filter(|m| m.kind() == "method_declaration") {
            let Some(name) = m.child_by_field_name("name").map(|n| self.text(n).to_string()) else { continue };
            let own_params: Vec<&str> = m
                .child_by_field_name("type_parameters")
                .map(|tp| {
                    named_children(tp)
                        .into_iter()
                        .filter_map(|p| named_children(p).into_iter().find(|c| c.kind() == "type_identifier"))
                        .map(|id| self.text(id))
                        .collect()
                })
                .unwrap_or_default();
            let ret = m.child_by_field_name("type").and_then(|t| {
                let ty = suffix_type(self.text(t));
                let head = ty.split(['.', '[']).next().unwrap_or(&ty);
                if own_params.contains(&head) || ty == "void" {
                    return None;
                }
                let class = class_of_type(t, self.source).filter(|c| !is_primitive(c));
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

    /// Members of a body, in source order (the resolver keeps a field and a
    /// method with one name apart, `code` and `code()`).
    fn visit_members(&mut self, body: Node<'_>, counts: &HashMap<String, usize>, depth: usize) {
        for m in named_children(body) {
            match m.kind() {
                "method_declaration" | "annotation_type_element_declaration" => {
                    let overloaded = m
                        .child_by_field_name("name")
                        .is_some_and(|n| counts.get(self.text(n)).copied().unwrap_or(0) > 1);
                    self.visit_method_as(m, overloaded, depth + 1);
                }
                "constructor_declaration" | "compact_constructor_declaration" => {
                    let type_name = self.types.last().map(|t| t.name.clone()).unwrap_or_default();
                    let overloaded = counts.get(&type_name).copied().unwrap_or(0) > 1;
                    self.visit_ctor(m, overloaded, depth + 1);
                }
                "field_declaration" | "constant_declaration" => self.visit_field(m, depth + 1),
                "enum_constant" => self.visit_enum_constant(m, depth + 1),
                "enum_body_declarations" => self.visit_members(m, counts, depth + 1),
                k if is_type_decl(k) => self.visit_type_decl(m, depth + 1),
                // Static and instance initializers run as the class.
                "static_initializer" | "block" => self.visit_children(m, depth + 1),
                k if is_comment(k) => {}
                _ => self.visit(m, depth + 1),
            }
        }
    }

    fn visit_method(&mut self, node: Node<'_>, depth: usize) {
        self.visit_method_as(node, false, depth);
    }

    /// Whether a method with these annotations and name is a test.
    fn is_test_method(&self, ann_names: &[String], name: &str, keywords: &HashSet<&str>) -> bool {
        let Some(t) = self.types.last() else { return false };
        if ann_names.iter().any(|a| TEST_ANNOTATIONS.contains(&a.as_str())) {
            return true;
        }
        if t.test_class && ann_names.iter().any(|a| LIFECYCLE_ANNOTATIONS.contains(&a.as_str())) {
            return true;
        }
        if t.junit3 && (name.starts_with("test") || name == "setUp" || name == "tearDown") {
            return true;
        }
        t.class_level_test && keywords.contains("public") && !keywords.contains("static")
    }

    fn visit_method_as(&mut self, node: Node<'_>, overloaded: bool, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = self.text(name_node).to_string();
        let in_type = self.types.last().is_some_and(|t| Some(t.def) == self.enc);
        let (keywords, annotations) = self.modifiers(node);
        let ann_names = self.annotation_names(&annotations);
        let params = node.child_by_field_name("parameters");
        let mut qn = self.child_qn(&name);
        if overloaded && let Some(p) = params {
            qn = format!("{qn}({})", parameter_types(p, self.source));
        }
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let is_test = in_type
            && (self.is_test_method(&ann_names, &name, &keywords) || self.types.last().is_some_and(|t| t.in_test));
        if is_test && let Some(t) = self.types.last_mut() {
            t.has_tests = true;
        }
        let exported = if self.in_implicit_public() { !keywords.contains("private") } else { self.exported(&keywords) };
        let def = Definition {
            label: if in_type || self.enc.is_some() { Label::Method } else { Label::Function },
            name,
            qualified_name: qn,
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.signature(node, end)),
            doc: self.javadoc(node),
            decorators: self.decorator_texts(&annotations),
            bases: Vec::new(),
            implements: Vec::new(),
            exported,
            is_test,
            is_declaration: false,
        };
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        for a in &annotations {
            self.visit_annotation(*a, true, depth + 1);
        }
        self.visit_callable_parts(node, depth);
        self.enc = saved;
    }

    /// Type parameters, return type, parameters, `throws`, default value
    /// and body of a method or constructor.
    fn visit_callable_parts(&mut self, node: Node<'_>, depth: usize) {
        let mark = self.push_type_params(node, depth + 1);
        for c in named_children(node) {
            match c.kind() {
                // Annotations of the definition are visited as decorators.
                "modifiers" | "type_parameters" | "identifier" | "dimensions" => {}
                k if is_annotation(k) => {}
                "formal_parameters" => self.visit_children(c, depth + 1),
                "throws" => self.visit_type_as(c, RefKind::Throw, depth + 1),
                "block" | "constructor_body" => self.visit_children(c, depth + 1),
                k if is_comment(k) => {}
                _ if Some(c) == node.child_by_field_name("type") => self.visit_type(c, depth + 1),
                _ => self.visit(c, depth + 1),
            }
        }
        self.type_params.truncate(mark);
    }

    fn visit_ctor(&mut self, node: Node<'_>, overloaded: bool, depth: usize) {
        let Some(t) = self.types.last() else { return };
        let type_name = t.name.clone();
        let name_node = node.child_by_field_name("name");
        let (keywords, annotations) = self.modifiers(node);
        let mut qn = self.child_qn(&type_name);
        if overloaded {
            let params = if node.kind() == "compact_constructor_declaration" {
                // The record's components are the parameters.
                self.types.last().and_then(|t| t.record_params.clone()).unwrap_or_default()
            } else {
                node.child_by_field_name("parameters").map(|p| parameter_types(p, self.source)).unwrap_or_default()
            };
            qn = format!("{qn}({params})");
        }
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let exported = self.exported(&keywords);
        let def = Definition {
            label: Label::Method,
            name: type_name,
            qualified_name: qn,
            parent: self.enc,
            span: span_of(node),
            name_span: name_node.map_or_else(|| span_of(node), span_of),
            signature: Some(self.signature(node, end)),
            doc: self.javadoc(node),
            decorators: self.decorator_texts(&annotations),
            bases: Vec::new(),
            implements: Vec::new(),
            exported,
            is_test: false,
            is_declaration: false,
        };
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        for a in &annotations {
            self.visit_annotation(*a, true, depth + 1);
        }
        self.visit_callable_parts(node, depth);
        self.enc = saved;
    }

    fn visit_field(&mut self, node: Node<'_>, depth: usize) {
        let (keywords, annotations) = self.modifiers(node);
        let ty = node.child_by_field_name("type");
        let type_text = ty.map_or(String::new(), |t| collapse(self.text(t)));
        let constant_like = (keywords.contains("static") && keywords.contains("final"))
            || node.kind() == "constant_declaration"
            || self.in_implicit_public();
        let mods: Vec<&str> = {
            let order = ["public", "protected", "private", "static", "final", "transient", "volatile"];
            order.into_iter().filter(|k| keywords.contains(k)).collect()
        };
        let exported = self.exported(&keywords);
        let doc = self.javadoc(node);
        let decorators = self.decorator_texts(&annotations);
        let mut first: Option<LocalDefId> = None;
        let declarators: Vec<Node<'_>> =
            named_children(node).into_iter().filter(|c| c.kind() == "variable_declarator").collect();
        for d in declarators {
            let Some(n) = d.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            let label = if constant_like && is_screaming(&name) { Label::Constant } else { Label::Field };
            let dims = d.child_by_field_name("dimensions").map_or("", |x| self.text(x));
            let mut sig = mods.join(" ");
            if !sig.is_empty() {
                sig.push(' ');
            }
            sig.push_str(&format!("{type_text}{dims} {name}"));
            let def = Definition {
                label,
                name: name.clone(),
                qualified_name: self.child_qn(&name),
                parent: self.enc,
                span: span_of(node),
                name_span: span_of(n),
                signature: Some(truncate_chars(sig, MAX_SIGNATURE)),
                doc: doc.clone(),
                decorators: decorators.clone(),
                bases: Vec::new(),
                implements: Vec::new(),
                exported,
                is_test: false,
                is_declaration: false,
            };
            let id = self.push_def(def);
            let saved = self.enc;
            self.enc = Some(id);
            if first.is_none() {
                first = Some(id);
                for a in &annotations {
                    self.visit_annotation(*a, true, depth + 1);
                }
                if let Some(t) = ty {
                    self.visit_type(t, depth + 1);
                }
            }
            if let Some(v) = d.child_by_field_name("value") {
                self.visit(v, depth + 1);
            }
            self.enc = saved;
        }
    }

    fn visit_enum_constant(&mut self, node: Node<'_>, depth: usize) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.text(n).to_string();
        let (_, annotations) = self.modifiers(node);
        let def = Definition {
            label: Label::Constant,
            name: name.clone(),
            qualified_name: self.child_qn(&name),
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(n),
            signature: Some(truncate_chars(
                collapse(
                    &self.signature(node, node.child_by_field_name("body").map_or(node.end_byte(), |b| b.start_byte())),
                ),
                MAX_SIGNATURE,
            )),
            doc: self.javadoc(node),
            decorators: self.decorator_texts(&annotations),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        };
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        for a in &annotations {
            self.visit_annotation(*a, true, depth + 1);
        }
        let args = node.child_by_field_name("arguments");
        if let Some(a) = args {
            // `RED("r")` calls the enum's constructor.
            if let Some(t) = self.types.last() {
                let enum_name = self.with_args(&t.name.clone(), Some(a));
                self.push_ref(RefKind::Call, &enum_name, Some("this".into()), span_of(n), Self::arity(Some(a)));
            }
            self.visit_children(a, depth + 1);
        }
        if let Some(b) = node.child_by_field_name("body") {
            // A constant body is an anonymous subclass of the enum.
            let enum_name = self.types.last().map(|t| t.name.clone()).unwrap_or_default();
            self.visit_anonymous_class(node, Some(enum_name), n, b, depth + 1);
        }
        self.enc = saved;
    }

    /// An anonymous class (`new T(…) { … }`, an enum constant body) is a
    /// `Class` named `T$N` (the `N`-th anonymous `T` in its enclosing
    /// definition, which is its parent), with `T` as its base, so its
    /// methods are definitions, `this` / implicit `this` inside it find its
    /// own and inherited members first, and its overrides get `OVERRIDES`.
    /// `owner` is the node whose extent it covers, `name_node` its base type
    /// (the name span).
    fn visit_anonymous_class(
        &mut self,
        owner: Node<'_>,
        base: Option<String>,
        name_node: Node<'_>,
        body: Node<'_>,
        depth: usize,
    ) {
        if depth > self.max_depth {
            return;
        }
        let base_name = base.as_deref().map_or("Object", |b| b.rsplit('.').next().unwrap_or(b)).to_string();
        let n = {
            let c = self.anon_counts.entry((self.enc, base_name.clone())).or_insert(0);
            *c += 1;
            *c
        };
        let name = format!("{base_name}${n}");
        let in_test =
            self.enc.is_some_and(|e| self.defs[e as usize].is_test) || self.types.last().is_some_and(|t| t.in_test);
        let header = String::from_utf8_lossy(&self.source[owner.start_byte()..body.start_byte()]);
        let def = Definition {
            label: Label::Class,
            name: name.clone(),
            qualified_name: self.child_qn(&name),
            parent: self.enc,
            span: span_of(owner),
            name_span: span_of(name_node),
            signature: Some(truncate_chars(collapse(&header), MAX_SIGNATURE)),
            doc: None,
            decorators: Vec::new(),
            bases: base.into_iter().collect(),
            implements: Vec::new(),
            exported: false,
            is_test: in_test,
            is_declaration: false,
        };
        let id = self.push_def(def);
        let saved = self.enc;
        self.enc = Some(id);
        self.types.push(TypeCtx {
            file_type: None,
            def: id,
            name,
            fields: HashMap::new(),
            field_types: HashMap::new(),
            superclass: Some(base_name),
            implicit_public: false,
            test_class: false,
            junit3: false,
            class_level_test: false,
            has_tests: false,
            record_params: None,
            methods: HashMap::new(),
            in_test,
        });
        self.visit_body(body, depth + 1);
        self.types.pop();
        self.enc = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(placeholder_keys("${server.port}"), vec!["server.port"]);
        assert_eq!(placeholder_keys("${a.b:${c.d:x}}"), vec!["a.b", "c.d"]);
        assert_eq!(placeholder_keys("http://${host}:${port:80}/x"), vec!["host", "port"]);
        assert!(placeholder_keys("#{systemProperties['x']}").is_empty());
    }

    #[test]
    fn javadoc_cleaning() {
        assert_eq!(clean_javadoc("/** One line. */").as_deref(), Some("One line."));
        assert_eq!(
            clean_javadoc("/**\n * First.\n *\n * @param x the x\n */").as_deref(),
            Some("First.\n\n@param x the x")
        );
        assert_eq!(clean_javadoc("/* not javadoc */"), None);
        assert!(is_screaming("MAX_SIZE") && is_screaming("A1") && !is_screaming("A") && !is_screaming("Max"));
    }
}
