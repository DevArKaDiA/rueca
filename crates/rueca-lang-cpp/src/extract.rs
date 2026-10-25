//! C / C++ facts extraction (crate docs describe the output).
//!
//! Two passes over one syntax tree:
//!
//! 1. **Definitions** (`decl` and friends): namespaces, classes, functions,
//!    fields, typedefs, enums, macros, tests and includes, with containment
//!    and qualified names. Every piece of code that may contain references
//!    (bodies, initializers, parameter and base types) is queued as a job
//!    with its enclosing definition.
//! 2. **References** (`visit` and friends), once every class of the file is
//!    known, so receivers can be typed from fields declared anywhere in the
//!    class.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use rueca_extract::tree_sitter::Node;
use rueca_extract::{Budget, ExtractError, count_errors, line_count, span_of, type_receiver};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, Language, LocalDefId, RefKind, Reference, Span};

use crate::locals::{self, DefKind, Locals, named_type};
use crate::mask;
use crate::text::{collapse, declarator_ident, doc_of, parameter_types, signature, text, tidy, truncate};

/// At most this many references per file.
pub const MAX_REFS: usize = 20_000;

/// Longest receiver text kept.
const MAX_RECEIVER: usize = 200;

/// Deepest namespace / class nesting walked for definitions (real code
/// nests a handful of levels; this bounds the stack on hostile input).
const MAX_NESTING: usize = 64;

/// GoogleTest macros that define a test with a body: `TEST(Suite, Name) {}`.
const GTEST_MACROS: &[&str] = &["TEST", "TEST_F", "TEST_P", "TYPED_TEST", "TYPED_TEST_P"];

/// Catch2 / doctest macros: `TEST_CASE("title", "[tags]") {}`.
const CATCH_MACROS: &[&str] = &["TEST_CASE", "TEST_CASE_METHOD", "SCENARIO", "TEMPLATE_TEST_CASE", "TEST_CASE_FIXTURE"];

/// Namespaces outside every project: a qualified name rooted here is
/// reported with the namespace as receiver, which the resolver treats as
/// a builtin (external).
pub(crate) const BUILTIN_NAMESPACES: &[&str] = &["Catch", "__gnu_cxx", "doctest", "std", "testing"];

/// C++ cast keywords parsed as template calls.
const CASTS: &[&str] = &["const_cast", "dynamic_cast", "reinterpret_cast", "static_cast"];

/// Functions that read an environment variable by name (RFC 0003 §7.2).
const GETENV: &[&str] = &["getenv", "secure_getenv"];

/// Context of a definition-level walk.
#[derive(Clone, Default)]
struct Ctx {
    /// Enclosing named namespaces (inline namespaces included).
    ns: Vec<String>,
    /// Inside an anonymous namespace.
    anon: bool,
    /// Enclosing definition (a class, for members).
    parent: Option<LocalDefId>,
    /// Enclosing class, for members.
    class: Option<Rc<ClassCtx>>,
    /// Name of the enclosing `#ifndef` (include guard detection).
    guard: Option<String>,
    /// Namespace and class nesting level.
    level: usize,
}

struct ClassCtx {
    qn: String,
    def: LocalDefId,
}

/// What a reference walk knows about its surroundings.
#[derive(Default)]
struct RefCx {
    /// Class whose members are implicitly in scope (a method, a field
    /// initializer, an out-of-line body `Foo::bar`).
    class_qn: Option<String>,
    /// Name of the enclosing function or method.
    fn_name: Option<String>,
    is_method: bool,
    ns: Vec<String>,
}

#[derive(Clone, Copy)]
enum Mode {
    /// Any code: statements, expressions, types.
    Body,
    /// A function definition: return type, parameters, initializers, body.
    Function,
    /// One declarator of a namespace- or class-level declaration, with the
    /// declaration's type.
    Declarator,
}

struct Job<'t> {
    node: Node<'t>,
    /// The declaration's type node, for [`Mode::Declarator`].
    ty: Option<Node<'t>>,
    enc: Option<LocalDefId>,
    cx: usize,
    mode: Mode,
}

/// A definition being added.
struct NewDef<'t> {
    label: Label,
    name: String,
    qn: String,
    parent: Option<LocalDefId>,
    /// Node whose preceding comments are the doc (a wrapping template).
    outer: Node<'t>,
    node: Node<'t>,
    name_node: Option<Node<'t>>,
    signature: Option<String>,
    exported: bool,
    is_test: bool,
    is_declaration: bool,
    bases: Vec<String>,
    /// Normalized parameter types (callables), for overload suffixes.
    params: Option<String>,
}

/// The name a declarator declares.
struct DeclName<'t> {
    name: String,
    /// Qualifier segments (`Foo::bar` → `Foo`), template arguments dropped.
    qualifier: Vec<String>,
    /// Written with a leading `::`.
    global: bool,
    /// The last qualifier segment has template arguments (`Box<T>::get`).
    template_scope: bool,
    node: Node<'t>,
}

struct Extractor<'s, 't> {
    src: &'s [u8],
    locals: &'t Locals,
    max_depth: usize,
    defs: Vec<Definition>,
    params: Vec<Option<String>>,
    refs: Vec<Reference>,
    imports: Vec<Import>,
    seen: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    jobs: Vec<Job<'t>>,
    cxs: Vec<RefCx>,
    /// Type names written anywhere in the file.
    known_types: HashSet<String>,
    /// Namespace names declared, used or aliased in the file.
    known_ns: HashSet<String>,
    /// Class qualified name → field name → declared type.
    class_fields: HashMap<String, HashMap<String, Option<String>>>,
    /// Class qualified name → member names.
    class_members: HashMap<String, HashSet<String>>,
    /// Class simple name → qualified names.
    classes_by_name: HashMap<String, Vec<String>>,
    /// Namespace-level variable → declared type.
    globals: HashMap<String, Option<String>>,
    /// MISSING `;` nodes after Catch2 test heads (not real parse errors).
    benign_errors: u32,
}

fn named_kids<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|c| c.kind() != "comment").collect()
}

fn has_child_text(node: Node<'_>, kind: &str, word: &str, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| c.kind() == kind && text(c, src) == word)
}

fn join_qn(parts: &[String], name: &str) -> String {
    if parts.is_empty() { name.to_string() } else { format!("{}::{name}", parts.join("::")) }
}

/// Text without template arguments: `ns::Foo<int>::Bar` → `ns::Foo::Bar`.
fn strip_template_args(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

/// The `function_declarator` of a function declarator chain (`*f()`,
/// `&f()`, `f() [[attr]]`), unless it declares a function pointer
/// (`(*fp)(int)`), which is a variable.
fn function_declarator(mut node: Node<'_>) -> Option<Node<'_>> {
    for _ in 0..16 {
        match node.kind() {
            "function_declarator" => {
                let inner = node.child_by_field_name("declarator")?;
                return (inner.kind() != "parenthesized_declarator").then_some(node);
            }
            "pointer_declarator" | "attributed_declarator" => node = node.child_by_field_name("declarator")?,
            "reference_declarator" => node = named_kids(node).into_iter().next()?,
            _ => return None,
        }
    }
    None
}

/// Whether `word` looks like a type name rather than a namespace (used when
/// the file says nothing about it): it starts with an upper-case letter.
fn looks_like_type(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_uppercase())
}

impl<'s, 't> Extractor<'s, 't> {
    fn new(src: &'s [u8], locals: &'t Locals, budget: &Budget) -> Self {
        Extractor {
            src,
            locals,
            max_depth: budget.max_depth,
            defs: Vec::new(),
            params: Vec::new(),
            refs: Vec::new(),
            imports: Vec::new(),
            seen: HashSet::new(),
            jobs: Vec::new(),
            cxs: Vec::new(),
            known_types: HashSet::new(),
            known_ns: HashSet::new(),
            class_fields: HashMap::new(),
            class_members: HashMap::new(),
            classes_by_name: HashMap::new(),
            globals: HashMap::new(),
            benign_errors: 0,
        }
    }

    fn t(&self, node: Node<'_>) -> String {
        text(node, self.src).into_owned()
    }

    // -- prescan -------------------------------------------------------------

    /// Collects type names written anywhere and the namespaces the file
    /// declares, uses or aliases.
    fn prescan(&mut self, root: Node<'t>) {
        let mut cursor = root.walk();
        let mut depth = 0usize;
        loop {
            let n = cursor.node();
            match n.kind() {
                "type_identifier" => {
                    self.known_types.insert(self.t(n));
                }
                "namespace_definition" | "namespace_alias_definition" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        for seg in self.t(name).split("::") {
                            self.known_ns.insert(seg.trim().to_string());
                        }
                    }
                }
                "nested_namespace_specifier" => {
                    for seg in self.t(n).split("::") {
                        self.known_ns.insert(seg.trim().to_string());
                    }
                }
                "using_declaration" if has_child_text(n, "namespace", "namespace", self.src) => {
                    for c in named_kids(n) {
                        for seg in self.t(c).split("::") {
                            self.known_ns.insert(seg.trim().to_string());
                        }
                    }
                }
                _ => {}
            }
            if depth < self.max_depth && cursor.goto_first_child() {
                depth += 1;
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
                depth -= 1;
            }
        }
    }

    // -- definitions ---------------------------------------------------------

    fn push(&mut self, d: NewDef<'t>) -> LocalDefId {
        let mut span = span_of(d.outer);
        for n in [Some(d.node), d.name_node].into_iter().flatten() {
            let s = span_of(n);
            if s.start_byte < span.start_byte {
                (span.start_byte, span.start_line, span.start_col) = (s.start_byte, s.start_line, s.start_col);
            }
            if s.end_byte > span.end_byte {
                (span.end_byte, span.end_line, span.end_col) = (s.end_byte, s.end_line, s.end_col);
            }
        }
        // A preprocessor node ends after its newline: stop at the line end.
        while span.end_byte > span.start_byte && matches!(self.src[span.end_byte as usize - 1], b'\n' | b'\r') {
            span.end_byte -= 1;
            if self.src[span.end_byte as usize] == b'\n' {
                let line_start =
                    self.src[..span.end_byte as usize].iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
                span.end_line = span.end_line.saturating_sub(1);
                span.end_col = span.end_byte - line_start as u32;
            } else {
                span.end_col = span.end_col.saturating_sub(1);
            }
        }
        let name_span = d.name_node.map_or(span, span_of);
        self.defs.push(Definition {
            label: d.label,
            name: d.name,
            qualified_name: d.qn,
            parent: d.parent,
            span,
            name_span,
            signature: d.signature,
            doc: doc_of(d.outer, self.src),
            decorators: Vec::new(),
            bases: d.bases,
            implements: Vec::new(),
            exported: d.exported,
            is_test: d.is_test,
            is_declaration: d.is_declaration,
        });
        self.params.push(d.params);
        (self.defs.len() - 1) as LocalDefId
    }

    fn cx(&mut self, cx: RefCx) -> usize {
        self.cxs.push(cx);
        self.cxs.len() - 1
    }

    fn job(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, mode: Mode) {
        self.jobs.push(Job { node, ty: None, enc, cx, mode });
    }

    /// Reference context for code directly under `ctx` (class members see
    /// the class's fields).
    fn plain_cx(&mut self, ctx: &Ctx) -> usize {
        let class_qn = ctx.class.as_ref().map(|c| c.qn.clone());
        self.cx(RefCx { class_qn, fn_name: None, is_method: false, ns: ctx.ns.clone() })
    }

    /// Owner path for a definition directly under `ctx`.
    fn owner(ctx: &Ctx) -> Vec<String> {
        match &ctx.class {
            Some(c) => vec![c.qn.clone()],
            None => ctx.ns.clone(),
        }
    }

    /// Walks the declarations of a translation unit, namespace body, class
    /// body, linkage block or preprocessor branch. `access` is the current
    /// member visibility in a class body (`None` elsewhere).
    fn scope(&mut self, node: Node<'t>, ctx: &Ctx, depth: usize, mut access: Option<bool>) {
        if depth > self.max_depth {
            return;
        }
        let mut cursor = node.walk();
        let kids: Vec<(Node<'t>, bool)> = {
            let mut v = Vec::new();
            if cursor.goto_first_child() {
                loop {
                    let n = cursor.node();
                    if n.is_named() && n.kind() != "comment" {
                        v.push((n, matches!(cursor.field_name(), Some("condition" | "name"))));
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            v
        };
        let preproc = node.kind().starts_with("preproc_");
        let mut k = 0;
        while k < kids.len() {
            let (c, is_condition) = kids[k];
            // The condition of `#if` / the name of `#ifdef` is not code.
            if preproc && is_condition {
                k += 1;
                continue;
            }
            if c.kind() == "access_specifier" {
                access = Some(self.t(c).trim_end_matches(':').trim() == "public");
                k += 1;
                continue;
            }
            if let Some(next) = kids.get(k + 1).map(|(n, _)| *n)
                && self.catch_test(c, next, ctx)
            {
                k += 2;
                continue;
            }
            self.decl(c, c, ctx, depth + 1, access);
            k += 1;
        }
    }

    fn decl(&mut self, node: Node<'t>, outer: Node<'t>, ctx: &Ctx, depth: usize, access: Option<bool>) {
        if depth > self.max_depth {
            return;
        }
        let public = access.unwrap_or(true);
        match node.kind() {
            "namespace_definition" => {
                if ctx.level >= MAX_NESTING {
                    return;
                }
                let mut c = ctx.clone();
                c.level += 1;
                match node.child_by_field_name("name") {
                    None => c.anon = true,
                    Some(n) => {
                        for seg in self.t(n).split("::") {
                            let seg = seg.trim();
                            if !seg.is_empty() {
                                c.ns.push(seg.to_string());
                            }
                        }
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.scope(body, &c, depth, None);
                }
            }
            "linkage_specification" => {
                if let Some(body) = node.child_by_field_name("body") {
                    if body.kind() == "declaration_list" {
                        self.scope(body, ctx, depth, access);
                    } else {
                        self.decl(body, body, ctx, depth + 1, access);
                    }
                }
            }
            "preproc_ifdef" | "preproc_if" | "preproc_else" | "preproc_elif" | "preproc_elifdef" => {
                let mut c = ctx.clone();
                if node.kind() == "preproc_ifdef" {
                    let negated = node.child(0).is_some_and(|t| text(t, self.src).trim() == "#ifndef");
                    c.guard = negated.then(|| node.child_by_field_name("name").map(|n| self.t(n))).flatten();
                }
                self.scope(node, &c, depth, access);
            }
            "template_declaration" => {
                for c in named_kids(node) {
                    if !matches!(c.kind(), "template_parameter_list" | "requires_clause") {
                        self.decl(c, outer, ctx, depth + 1, access);
                    }
                }
            }
            "function_definition" => self.function_def(node, outer, ctx, public),
            "declaration" => self.declaration(node, outer, ctx, public, depth),
            "field_declaration" => self.field_declaration(node, outer, ctx, public, depth),
            "class_specifier" | "struct_specifier" | "union_specifier" => {
                self.class_def(node, outer, ctx, public, None, depth);
            }
            "enum_specifier" => {
                self.enum_def(node, outer, ctx, public, None);
            }
            "type_definition" => self.typedef(node, outer, ctx, public, depth),
            "alias_declaration" => self.alias(node, outer, ctx, public),
            "preproc_def" | "preproc_function_def" => self.macro_def(node, ctx),
            "declaration_list" | "ERROR" => self.scope(node, ctx, depth, access),
            "static_assert_declaration" | "expression_statement" | "compound_statement" => {
                let cx = self.plain_cx(ctx);
                self.job(node, ctx.parent, cx, Mode::Body);
            }
            _ => {}
        }
    }

    /// The name a function or variable declarator declares.
    fn decl_name(&self, node: Node<'t>) -> Option<DeclName<'t>> {
        let simple = |name: String, n: Node<'t>| DeclName {
            name,
            qualifier: Vec::new(),
            global: false,
            template_scope: false,
            node: n,
        };
        match node.kind() {
            "identifier" | "field_identifier" | "type_identifier" => Some(simple(self.t(node), node)),
            "destructor_name" => Some(simple(format!("~{}", self.t(node).trim_start_matches('~').trim()), node)),
            "operator_name" => {
                let raw = collapse(&self.t(node));
                let rest = raw.trim_start_matches("operator").trim();
                let word = rest.starts_with(|c: char| c.is_ascii_alphabetic());
                let name = if word { format!("operator {rest}") } else { format!("operator{}", rest.replace(' ', "")) };
                Some(simple(name, node))
            }
            "operator_cast" => {
                let ty = node.child_by_field_name("type").map(|t| tidy(&self.t(t))).unwrap_or_default();
                Some(simple(format!("operator {ty}"), node))
            }
            "template_function" => {
                let name = node.child_by_field_name("name")?;
                Some(simple(self.t(name), name))
            }
            "qualified_identifier" => {
                let global = self.t(node).trim_start().starts_with("::");
                let mut qualifier = Vec::new();
                let mut template_scope = false;
                let mut cur = node;
                for _ in 0..32 {
                    if let Some(scope) = cur.child_by_field_name("scope") {
                        let seg = match scope.kind() {
                            "template_type" => scope.child_by_field_name("name").map(|n| self.t(n)),
                            _ => Some(strip_template_args(&self.t(scope))),
                        };
                        template_scope = scope.kind() == "template_type";
                        if let Some(seg) = seg.filter(|s| !s.is_empty()) {
                            qualifier.push(seg);
                        }
                    }
                    let name = cur.child_by_field_name("name")?;
                    if name.kind() == "qualified_identifier" {
                        cur = name;
                        continue;
                    }
                    let leaf = self.decl_name(name)?;
                    return Some(DeclName { name: leaf.name, qualifier, global, template_scope, node: leaf.node });
                }
                None
            }
            _ => None,
        }
    }

    /// Label, qualified name, parent and export of a function-like
    /// definition, plus the class whose members its body sees.
    #[allow(clippy::type_complexity)]
    fn place(
        &mut self,
        dn: &DeclName<'t>,
        ctx: &Ctx,
        public: bool,
        is_static: bool,
    ) -> (Label, String, Option<LocalDefId>, bool, Option<String>) {
        if let Some(class) = &ctx.class
            && dn.qualifier.is_empty()
        {
            self.class_members.entry(class.qn.clone()).or_default().insert(dn.name.clone());
            return (
                Label::Method,
                join_qn(std::slice::from_ref(&class.qn), &dn.name),
                Some(class.def),
                public,
                Some(class.qn.clone()),
            );
        }
        if !dn.qualifier.is_empty() {
            let mut owner = if dn.global { Vec::new() } else { Self::owner(ctx) };
            owner.extend(dn.qualifier.iter().cloned());
            let owner_qn = owner.join("::");
            let qn = join_qn(std::slice::from_ref(&owner_qn), &dn.name);
            let last = dn.qualifier.last().map(String::as_str).unwrap_or("");
            let namespace = !dn.template_scope && self.known_ns.contains(last) && !self.known_types.contains(last);
            return if namespace {
                (Label::Function, qn, None, !is_static && !ctx.anon, None)
            } else {
                (Label::Method, qn, None, false, Some(owner_qn))
            };
        }
        (Label::Function, join_qn(&ctx.ns, &dn.name), ctx.parent, !is_static && !ctx.anon, None)
    }

    fn function_def(&mut self, node: Node<'t>, outer: Node<'t>, ctx: &Ctx, public: bool) {
        let Some(declarator) = node.child_by_field_name("declarator") else {
            let cx = self.plain_cx(ctx);
            self.job(node, ctx.parent, cx, Mode::Function);
            return;
        };
        let fd = function_declarator(declarator);
        if ctx.class.is_none()
            && node.child_by_field_name("type").is_none()
            && let Some(fd) = fd
            && let Some(n) = fd.child_by_field_name("declarator")
            && n.kind() == "identifier"
            && GTEST_MACROS.contains(&text(n, self.src).as_ref())
        {
            self.gtest(node, outer, fd, ctx);
            return;
        }
        let (name_node, params) = match fd {
            Some(fd) => (fd.child_by_field_name("declarator"), fd.child_by_field_name("parameters")),
            // `operator bool() const {}` / `Foo::operator bool() {}`.
            None if matches!(declarator.kind(), "operator_cast" | "qualified_identifier") => {
                let params = if declarator.kind() == "operator_cast" {
                    declarator.child_by_field_name("declarator").and_then(|d| d.child_by_field_name("parameters"))
                } else {
                    None
                };
                (Some(declarator), params)
            }
            None => (None, None),
        };
        let Some(dn) = name_node.and_then(|n| self.decl_name(n)) else {
            // A declarator Rueca cannot name (macro-mangled code): keep the
            // body's references under the enclosing definition.
            let cx = self.plain_cx(ctx);
            self.job(node, ctx.parent, cx, Mode::Function);
            return;
        };
        let is_static = has_child_text(node, "storage_class_specifier", "static", self.src);
        let (label, qn, parent, exported, class_qn) = self.place(&dn, ctx, public, is_static);
        let until = node.child_by_field_name("body").or_else(|| {
            let mut cursor = node.walk();
            node.children(&mut cursor).find(|c| c.kind() == "field_initializer_list")
        });
        let id = self.push(NewDef {
            label,
            name: dn.name.clone(),
            qn,
            parent,
            outer,
            node,
            name_node: Some(dn.node),
            signature: signature(node, until, self.src),
            exported,
            is_test: false,
            is_declaration: false,
            bases: Vec::new(),
            params: Some(params.map(|p| parameter_types(p, self.src)).unwrap_or_default()),
        });
        let is_method = label == Label::Method;
        let cx = self.cx(RefCx { class_qn, fn_name: Some(dn.name), is_method, ns: ctx.ns.clone() });
        self.job(node, Some(id), cx, Mode::Function);
    }

    /// `TEST(Suite, Name) { … }` and friends.
    fn gtest(&mut self, node: Node<'t>, outer: Node<'t>, fd: Node<'t>, ctx: &Ctx) {
        let args: Vec<Node<'t>> = fd.child_by_field_name("parameters").map(named_kids).unwrap_or_default();
        let words: Vec<String> = args.iter().map(|a| collapse(&self.t(*a))).collect();
        let (suite, name, name_node) = match (words.first(), words.get(1)) {
            (Some(s), Some(n)) => (s.clone(), n.clone(), args.get(1).copied()),
            (Some(s), None) => (s.clone(), s.clone(), args.first().copied()),
            _ => return,
        };
        let mut owner = ctx.ns.clone();
        owner.push(suite.clone());
        let id = self.push(NewDef {
            label: Label::Test,
            name,
            qn: join_qn(&owner, &words.get(1).cloned().unwrap_or(suite)),
            parent: None,
            outer,
            node,
            name_node,
            signature: signature(node, node.child_by_field_name("body"), self.src),
            exported: false,
            is_test: true,
            is_declaration: false,
            bases: Vec::new(),
            params: None,
        });
        // TEST_F / TEST_P bodies are members of the fixture class.
        let fixture = Some(owner.join("::"));
        let cx = self.cx(RefCx { class_qn: fixture, fn_name: None, is_method: true, ns: ctx.ns.clone() });
        if let Some(body) = node.child_by_field_name("body") {
            self.job(body, Some(id), cx, Mode::Body);
        }
    }

    /// Catch2 / doctest: `TEST_CASE("title", "[tags]")` followed by a block
    /// parses as an expression statement (missing `;`) and a compound
    /// statement. Returns whether `head` + `body` were consumed.
    fn catch_test(&mut self, head: Node<'t>, body: Node<'t>, ctx: &Ctx) -> bool {
        if head.kind() != "expression_statement" || body.kind() != "compound_statement" {
            return false;
        }
        let Some(call) = named_kids(head).into_iter().next().filter(|c| c.kind() == "call_expression") else {
            return false;
        };
        let Some(f) = call.child_by_field_name("function").filter(|f| f.kind() == "identifier") else {
            return false;
        };
        let macro_name = self.t(f);
        if !CATCH_MACROS.contains(&macro_name.as_str()) {
            return false;
        }
        let args = call.child_by_field_name("arguments").map(named_kids).unwrap_or_default();
        let Some(title_node) = args.iter().copied().find(|a| a.kind() == "string_literal") else { return false };
        let title = named_kids(title_node)
            .into_iter()
            .find(|c| c.kind() == "string_content")
            .map(|c| self.t(c))
            .unwrap_or_default();
        let fixture = match args.first() {
            Some(a)
                if a.kind() == "identifier" && (macro_name.ends_with("METHOD") || macro_name.ends_with("FIXTURE")) =>
            {
                Some(self.t(*a))
            }
            _ => None,
        };
        let qn = match &fixture {
            Some(f) => format!("{}::{title}", join_qn(&ctx.ns, f)),
            None => title.clone(),
        };
        // The MISSING `;` the grammar inserts after the head.
        let mut cursor = head.walk();
        self.benign_errors += head.children(&mut cursor).filter(|c| c.is_missing()).count() as u32;
        let id = self.push(NewDef {
            label: Label::Test,
            name: title,
            qn,
            parent: None,
            outer: head,
            node: body,
            name_node: Some(title_node),
            signature: signature(head, None, self.src),
            exported: false,
            is_test: true,
            is_declaration: false,
            bases: Vec::new(),
            params: None,
        });
        let class_qn = fixture.map(|f| join_qn(&ctx.ns, &f));
        let is_method = class_qn.is_some();
        let cx = self.cx(RefCx { class_qn, fn_name: None, is_method, ns: ctx.ns.clone() });
        self.job(body, Some(id), cx, Mode::Body);
        true
    }

    fn declaration(&mut self, node: Node<'t>, outer: Node<'t>, ctx: &Ctx, public: bool, depth: usize) {
        let ty = node.child_by_field_name("type");
        if let Some(t) = ty
            && t.child_by_field_name("body").is_some()
        {
            match t.kind() {
                "class_specifier" | "struct_specifier" | "union_specifier" => {
                    self.class_def(t, outer, ctx, public, None, depth);
                }
                "enum_specifier" => {
                    self.enum_def(t, outer, ctx, public, None);
                }
                _ => {}
            }
        }
        let is_static = has_child_text(node, "storage_class_specifier", "static", self.src);
        let is_extern = has_child_text(node, "storage_class_specifier", "extern", self.src);
        let constexpr = has_child_text(node, "type_qualifier", "constexpr", self.src);
        let is_const = constexpr || has_child_text(node, "type_qualifier", "const", self.src);
        let mut cursor = node.walk();
        let declarators: Vec<Node<'t>> = node.children_by_field_name("declarator", &mut cursor).collect();
        let sig = signature(node, None, self.src);
        for d in declarators {
            // Prototypes and in-class constructor / destructor / operator
            // declarations.
            let fd = function_declarator(d);
            let fname = match (fd, d.kind()) {
                (Some(fd), _) => fd.child_by_field_name("declarator"),
                (None, "operator_cast") => Some(d),
                _ => None,
            };
            if let Some(dn) = fname.and_then(|n| self.decl_name(n)) {
                let params = match fd {
                    Some(fd) => fd.child_by_field_name("parameters"),
                    None => d.child_by_field_name("declarator").and_then(|a| a.child_by_field_name("parameters")),
                };
                let (label, qn, parent, exported, class_qn) = self.place(&dn, ctx, public, is_static);
                let id = self.push(NewDef {
                    label,
                    name: dn.name.clone(),
                    qn,
                    parent,
                    outer,
                    node,
                    name_node: Some(dn.node),
                    signature: sig.clone(),
                    exported,
                    is_test: false,
                    is_declaration: true,
                    bases: Vec::new(),
                    params: Some(params.map(|p| parameter_types(p, self.src)).unwrap_or_default()),
                });
                let cx = self.cx(RefCx { class_qn, fn_name: Some(dn.name), is_method: false, ns: ctx.ns.clone() });
                self.jobs.push(Job { node: d, ty, enc: Some(id), cx, mode: Mode::Declarator });
                continue;
            }
            if fd.is_some() {
                continue;
            }
            // Variables.
            let Some(name_node) = declarator_ident(d) else { continue };
            let pointer = matches!(d.kind(), "pointer_declarator")
                || d.child_by_field_name("declarator").is_some_and(|x| x.kind() == "pointer_declarator");
            let constant = constexpr || (is_const && !pointer);
            let declared = ty.and_then(|t| named_type(t, self.src));
            let (label, name, qn, parent, exported) = if name_node.kind() == "qualified_identifier" {
                // `int Foo::count = 0;`: a static data member's definition.
                let Some(dn) = self.decl_name(name_node) else { continue };
                let mut owner = if dn.global { Vec::new() } else { Self::owner(ctx) };
                owner.extend(dn.qualifier.iter().cloned());
                let label = if constant { Label::Constant } else { Label::Field };
                (label, dn.name.clone(), join_qn(&owner, &dn.name), None, false)
            } else {
                let name = self.t(name_node);
                match &ctx.class {
                    Some(c) => {
                        let label = if is_static && constant { Label::Constant } else { Label::Field };
                        self.class_fields.entry(c.qn.clone()).or_default().insert(name.clone(), declared.clone());
                        self.class_members.entry(c.qn.clone()).or_default().insert(name.clone());
                        (label, name.clone(), join_qn(std::slice::from_ref(&c.qn), &name), Some(c.def), public)
                    }
                    None => {
                        self.globals.insert(name.clone(), declared.clone());
                        let label = if constant { Label::Constant } else { Label::Variable };
                        (label, name.clone(), join_qn(&ctx.ns, &name), ctx.parent, !is_static && !ctx.anon)
                    }
                }
            };
            let id = self.push(NewDef {
                label,
                name,
                qn,
                parent,
                outer,
                node,
                name_node: Some(name_node),
                signature: sig.clone(),
                exported,
                is_test: false,
                is_declaration: is_extern && d.kind() != "init_declarator",
                bases: Vec::new(),
                params: None,
            });
            let cx = self.plain_cx(ctx);
            self.jobs.push(Job { node: d, ty, enc: Some(id), cx, mode: Mode::Declarator });
        }
    }

    fn field_declaration(&mut self, node: Node<'t>, outer: Node<'t>, ctx: &Ctx, public: bool, depth: usize) {
        let Some(class) = ctx.class.clone() else {
            // A field outside a class body (error recovery): treat it as a
            // declaration.
            self.declaration(node, outer, ctx, public, depth);
            return;
        };
        let ty = node.child_by_field_name("type");
        if let Some(t) = ty
            && let Some(body) = t.child_by_field_name("body")
        {
            match t.kind() {
                "class_specifier" | "struct_specifier" | "union_specifier" => {
                    if t.child_by_field_name("name").is_some() {
                        self.class_def(t, outer, ctx, public, None, depth);
                    } else {
                        // Anonymous struct / union: its fields are members
                        // of the enclosing class.
                        let mut c = ctx.clone();
                        c.level += 1;
                        if c.level < MAX_NESTING {
                            self.scope(body, &c, depth + 1, Some(public));
                        }
                    }
                }
                "enum_specifier" => {
                    self.enum_def(t, outer, ctx, public, None);
                }
                _ => {}
            }
        }
        let is_static = has_child_text(node, "storage_class_specifier", "static", self.src);
        let is_const = has_child_text(node, "type_qualifier", "constexpr", self.src)
            || has_child_text(node, "type_qualifier", "const", self.src);
        let sig = signature(node, None, self.src);
        let declared = ty.and_then(|t| named_type(t, self.src));
        let mut cursor = node.walk();
        let declarators: Vec<Node<'t>> = node.children_by_field_name("declarator", &mut cursor).collect();
        for d in declarators {
            if let Some(fd) = function_declarator(d) {
                let Some(dn) = fd.child_by_field_name("declarator").and_then(|n| self.decl_name(n)) else { continue };
                let params = fd.child_by_field_name("parameters").map(|p| parameter_types(p, self.src));
                let (label, qn, parent, exported, class_qn) = self.place(&dn, ctx, public, is_static);
                let id = self.push(NewDef {
                    label,
                    name: dn.name.clone(),
                    qn,
                    parent,
                    outer,
                    node,
                    name_node: Some(dn.node),
                    signature: sig.clone(),
                    exported,
                    is_test: false,
                    is_declaration: true,
                    bases: Vec::new(),
                    params: Some(params.unwrap_or_default()),
                });
                let cx = self.cx(RefCx { class_qn, fn_name: Some(dn.name), is_method: false, ns: ctx.ns.clone() });
                self.jobs.push(Job { node: d, ty, enc: Some(id), cx, mode: Mode::Declarator });
                continue;
            }
            let Some(name_node) = declarator_ident(d) else { continue };
            let name = self.t(name_node);
            let label = if is_static && is_const { Label::Constant } else { Label::Field };
            self.class_fields.entry(class.qn.clone()).or_default().insert(name.clone(), declared.clone());
            self.class_members.entry(class.qn.clone()).or_default().insert(name.clone());
            let id = self.push(NewDef {
                label,
                name: name.clone(),
                qn: join_qn(std::slice::from_ref(&class.qn), &name),
                parent: Some(class.def),
                outer,
                node,
                name_node: Some(name_node),
                signature: sig.clone(),
                exported: public,
                is_test: false,
                is_declaration: false,
                bases: Vec::new(),
                params: None,
            });
            let cx = self.plain_cx(ctx);
            self.jobs.push(Job { node: d, ty, enc: Some(id), cx, mode: Mode::Declarator });
            // `int x = 1;` in a class: the default value is a sibling field.
            if let Some(v) = node.child_by_field_name("default_value") {
                self.job(v, Some(id), cx, Mode::Body);
            }
        }
    }

    /// Text recorded in `bases` for a base or aliased type: the last
    /// segment (`ns::Base<T>` → `Base`, resolved like a type reference),
    /// or the whole name when it is rooted in a builtin namespace
    /// (`std::exception`, which the resolver cannot find, marking the type
    /// as open).
    fn base_text(&self, node: Node<'_>) -> Option<String> {
        let full = strip_template_args(&self.t(node));
        let full = full.trim_start_matches("::");
        let root = full.split("::").next().unwrap_or("");
        if full.is_empty() || matches!(node.kind(), "primitive_type" | "sized_type_specifier") {
            return None;
        }
        if BUILTIN_NAMESPACES.contains(&root) {
            return Some(full.to_string());
        }
        full.rsplit("::").next().map(str::to_string)
    }

    /// `class` / `struct` / `union` with a body. `alias` names an anonymous
    /// one (`typedef struct { … } name_t;`).
    fn class_def(
        &mut self,
        node: Node<'t>,
        outer: Node<'t>,
        ctx: &Ctx,
        public: bool,
        alias: Option<(String, Node<'t>)>,
        depth: usize,
    ) -> Option<LocalDefId> {
        let body = node.child_by_field_name("body")?;
        let (name, quals, name_node) = match node.child_by_field_name("name") {
            Some(n) => match n.kind() {
                "qualified_identifier" => {
                    let dn = self.decl_name(n)?;
                    (dn.name, dn.qualifier, dn.node)
                }
                "template_type" => {
                    let inner = n.child_by_field_name("name")?;
                    (self.t(inner), Vec::new(), inner)
                }
                _ => (self.t(n), Vec::new(), n),
            },
            None => {
                let (a, n) = alias?;
                (a, Vec::new(), n)
            }
        };
        let label = if node.kind() == "class_specifier" { Label::Class } else { Label::Struct };
        let mut owner = Self::owner(ctx);
        owner.extend(quals);
        let qn = join_qn(&owner, &name);
        let mut bases = Vec::new();
        let mut cursor = node.walk();
        let clause = node.children(&mut cursor).find(|c| c.kind() == "base_class_clause");
        if let Some(clause) = clause {
            for b in named_kids(clause) {
                if matches!(b.kind(), "access_specifier" | "virtual") {
                    continue;
                }
                if let Some(t) = self.base_text(b) {
                    bases.push(t);
                }
            }
        }
        let exported = if ctx.class.is_some() { public } else { !ctx.anon };
        if let Some(c) = &ctx.class {
            self.class_members.entry(c.qn.clone()).or_default().insert(name.clone());
        }
        let id = self.push(NewDef {
            label,
            name: name.clone(),
            qn: qn.clone(),
            parent: ctx.class.as_ref().map(|c| c.def),
            outer,
            node,
            name_node: Some(name_node),
            signature: if node.child_by_field_name("name").is_some() {
                signature(node, Some(body), self.src)
            } else {
                Some(format!("{} {}", node.kind().trim_end_matches("_specifier"), self.t(name_node)))
            },
            exported,
            is_test: false,
            is_declaration: false,
            bases,
            params: None,
        });
        self.class_members.entry(qn.clone()).or_default();
        self.classes_by_name.entry(name).or_default().push(qn.clone());
        let inner = Ctx {
            ns: ctx.ns.clone(),
            anon: ctx.anon,
            parent: Some(id),
            class: Some(Rc::new(ClassCtx { qn: qn.clone(), def: id })),
            guard: ctx.guard.clone(),
            level: ctx.level + 1,
        };
        if let Some(clause) = clause {
            let cx = self.plain_cx(ctx);
            self.job(clause, Some(id), cx, Mode::Body);
        }
        let default_public = node.kind() != "class_specifier";
        if inner.level < MAX_NESTING {
            self.scope(body, &inner, depth + 1, Some(default_public));
        }
        Some(id)
    }

    fn enum_def(
        &mut self,
        node: Node<'t>,
        outer: Node<'t>,
        ctx: &Ctx,
        public: bool,
        alias: Option<(String, Node<'t>)>,
    ) -> Option<LocalDefId> {
        let body = node.child_by_field_name("body")?;
        let named = node.child_by_field_name("name").map(|n| (self.t(n), n)).or(alias);
        let owner = Self::owner(ctx);
        let exported = if ctx.class.is_some() { public } else { !ctx.anon };
        let (enum_id, member_owner, member_parent) = match named {
            Some((name, name_node)) => {
                let name = strip_template_args(&name).rsplit("::").next().unwrap_or("").to_string();
                let qn = join_qn(&owner, &name);
                if let Some(c) = &ctx.class {
                    self.class_members.entry(c.qn.clone()).or_default().insert(name.clone());
                }
                let id = self.push(NewDef {
                    label: Label::Enum,
                    name: name.clone(),
                    qn: qn.clone(),
                    parent: ctx.class.as_ref().map(|c| c.def),
                    outer,
                    node,
                    name_node: Some(name_node),
                    signature: signature(node, Some(body), self.src),
                    exported,
                    is_test: false,
                    is_declaration: false,
                    bases: Vec::new(),
                    params: None,
                });
                self.classes_by_name.entry(name).or_default().push(qn.clone());
                (Some(id), vec![qn], Some(id))
            }
            None => (None, owner, ctx.class.as_ref().map(|c| c.def)),
        };
        let member_qn = member_owner.first().cloned();
        for e in named_kids(body) {
            if e.kind() != "enumerator" {
                continue;
            }
            let Some(n) = e.child_by_field_name("name") else { continue };
            let name = self.t(n);
            if let Some(q) = &member_qn {
                self.class_members.entry(q.clone()).or_default().insert(name.clone());
            }
            if enum_id.is_none()
                && let Some(c) = &ctx.class
            {
                self.class_members.entry(c.qn.clone()).or_default().insert(name.clone());
            }
            let id = self.push(NewDef {
                label: Label::Constant,
                name: name.clone(),
                qn: join_qn(&member_owner, &name),
                parent: member_parent,
                outer: e,
                node: e,
                name_node: Some(n),
                signature: signature(e, None, self.src).map(|s| s.trim_end_matches(',').to_string()),
                exported,
                is_test: false,
                is_declaration: false,
                bases: Vec::new(),
                params: None,
            });
            if let Some(v) = e.child_by_field_name("value") {
                let cx = self.plain_cx(ctx);
                self.job(v, Some(id), cx, Mode::Body);
            }
        }
        enum_id
    }

    /// `typedef T name;`, including `typedef struct tag { … } name_t;`.
    fn typedef(&mut self, node: Node<'t>, outer: Node<'t>, ctx: &Ctx, public: bool, depth: usize) {
        let Some(ty) = node.child_by_field_name("type") else { return };
        let mut cursor = node.walk();
        let names: Vec<(String, Node<'t>)> = node
            .children_by_field_name("declarator", &mut cursor)
            .filter_map(|d| declarator_ident(d).filter(|n| n.kind() == "type_identifier"))
            .map(|n| (self.t(n), n))
            .collect();
        let with_body = ty.child_by_field_name("body").is_some();
        let tag = ty.child_by_field_name("name").map(|n| self.t(n));
        let mut target: Option<String> = None;
        let mut skip_first = false;
        if with_body
            && matches!(ty.kind(), "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier")
        {
            let alias = if tag.is_none() { names.first().cloned() } else { None };
            skip_first = alias.is_some();
            let defined = if ty.kind() == "enum_specifier" {
                self.enum_def(ty, outer, ctx, public, alias.clone())
            } else {
                self.class_def(ty, outer, ctx, public, alias.clone(), depth)
            };
            if defined.is_some() {
                target = tag.clone().or(alias.map(|(a, _)| a));
            }
        } else {
            target = match ty.kind() {
                "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier" => tag.clone(),
                _ => self.base_text(ty),
            };
        }
        let sig = signature(node, None, self.src);
        let owner = Self::owner(ctx);
        let exported = if ctx.class.is_some() { public } else { !ctx.anon };
        let mut first_alias = None;
        for (i, (name, n)) in names.into_iter().enumerate() {
            if (i == 0 && skip_first) || target.as_deref() == Some(name.as_str()) {
                continue;
            }
            if let Some(c) = &ctx.class {
                self.class_members.entry(c.qn.clone()).or_default().insert(name.clone());
            }
            let id = self.push(NewDef {
                label: Label::TypeAlias,
                name: name.clone(),
                qn: join_qn(&owner, &name),
                parent: ctx.class.as_ref().map(|c| c.def),
                outer,
                node,
                name_node: Some(n),
                signature: sig.clone(),
                exported,
                is_test: false,
                is_declaration: false,
                bases: target.iter().cloned().collect(),
                params: None,
            });
            first_alias.get_or_insert(id);
        }
        if !with_body {
            let cx = self.plain_cx(ctx);
            self.job(ty, first_alias.or(ctx.parent), cx, Mode::Body);
        }
    }

    /// `using name = T;`.
    fn alias(&mut self, node: Node<'t>, outer: Node<'t>, ctx: &Ctx, public: bool) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.t(n);
        let ty = node.child_by_field_name("type");
        let target = ty
            .and_then(|t| t.child_by_field_name("type").or(Some(t)))
            .and_then(|t| self.base_text(t))
            .filter(|t| !self.locals.lookup(t, node.start_byte() as u32).is_some_and(|b| b.kind == DefKind::Type));
        if let Some(c) = &ctx.class {
            self.class_members.entry(c.qn.clone()).or_default().insert(name.clone());
        }
        let id = self.push(NewDef {
            label: Label::TypeAlias,
            name: name.clone(),
            qn: join_qn(&Self::owner(ctx), &name),
            parent: ctx.class.as_ref().map(|c| c.def),
            outer,
            node,
            name_node: Some(n),
            signature: signature(node, None, self.src),
            exported: if ctx.class.is_some() { public } else { !ctx.anon },
            is_test: false,
            is_declaration: false,
            bases: target.filter(|t| *t != name).into_iter().collect(),
            params: None,
        });
        if let Some(t) = ty {
            let cx = self.plain_cx(ctx);
            self.job(t, Some(id), cx, Mode::Body);
        }
    }

    /// `#define NAME value` → `Constant`; `#define NAME(args) body` →
    /// `Function`. Macros ignore namespaces and classes: global, top level.
    fn macro_def(&mut self, node: Node<'t>, ctx: &Ctx) {
        let Some(n) = node.child_by_field_name("name") else { return };
        let name = self.t(n);
        let function_like = node.kind() == "preproc_function_def";
        if !function_like && node.child_by_field_name("value").is_none() {
            // Include guards and feature flags without a value.
            let guard_like = ["_H", "_H_", "_HPP", "_HPP_", "_HH", "_INCLUDED", "_INCLUDED_", "_H__"]
                .iter()
                .any(|s| name.ends_with(s));
            if guard_like || ctx.guard.as_deref() == Some(name.as_str()) {
                return;
            }
        }
        let line = self.t(node);
        let sig =
            truncate(collapse(line.lines().next().unwrap_or("").trim_end_matches('\\')), crate::text::MAX_SIGNATURE);
        self.push(NewDef {
            label: if function_like { Label::Function } else { Label::Constant },
            name: name.clone(),
            qn: name,
            parent: None,
            outer: node,
            node,
            name_node: Some(n),
            signature: Some(sig),
            exported: true,
            is_test: false,
            is_declaration: false,
            bases: Vec::new(),
            params: None,
        });
    }

    /// Includes, read lexically (they survive parse errors around them).
    fn includes(&mut self) {
        for (module, a, b) in mask::includes(self.src) {
            let line_of = |at: usize| self.src[..at].iter().filter(|c| **c == b'\n').count() as u32;
            let col_of =
                |at: usize| (at - self.src[..at].iter().rposition(|c| *c == b'\n').map_or(0, |i| i + 1)) as u32;
            let end = if b > a && self.src[b - 1] == b'\r' { b - 1 } else { b };
            let span = Span {
                start_byte: a as u32,
                end_byte: end as u32,
                start_line: line_of(a),
                start_col: col_of(a),
                end_line: line_of(end),
                end_col: col_of(end),
            };
            self.imports.push(Import {
                module,
                relative_level: 0,
                names: vec![ImportedName { name: "*".into(), alias: None }],
                module_alias: None,
                type_only: false,
                reexport: false,
                span,
            });
        }
    }

    // -- references ----------------------------------------------------------

    fn run_jobs(&mut self) {
        let jobs = std::mem::take(&mut self.jobs);
        for j in jobs {
            if self.refs.len() >= MAX_REFS {
                break;
            }
            match j.mode {
                Mode::Body => self.visit(j.node, j.enc, j.cx, 0),
                Mode::Function => self.visit_function(j.node, j.enc, j.cx),
                Mode::Declarator => {
                    if let Some(t) = j.ty {
                        self.visit(t, j.enc, j.cx, 0);
                    }
                    self.visit_declarator(j.node, j.ty, j.enc, j.cx, 0);
                }
            }
        }
    }

    fn emit(
        &mut self,
        kind: RefKind,
        name: &str,
        receiver: Option<String>,
        at: Node<'_>,
        enc: Option<LocalDefId>,
        arity: Option<u32>,
    ) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        if matches!(kind, RefKind::Read | RefKind::Type | RefKind::Write) {
            let key = (kind as u8, enc, receiver.clone(), name.to_string());
            if !self.seen.insert(key) {
                return;
            }
        }
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing: enc, span: span_of(at), arity });
    }

    fn is_local(&self, node: Node<'_>) -> bool {
        self.locals.lookup(&text(node, self.src), node.start_byte() as u32).is_some()
    }

    fn arity(&self, args: Option<Node<'_>>) -> Option<u32> {
        args.map(|a| named_kids(a).len() as u32)
    }

    fn visit_function(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize) {
        if let Some(t) = node.child_by_field_name("type") {
            self.visit(t, enc, cx, 1);
        }
        if let Some(d) = node.child_by_field_name("declarator") {
            self.visit_declarator(d, None, enc, cx, 1);
        }
        let mut cursor = node.walk();
        let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for c in kids {
            if c.kind() == "field_initializer_list" {
                self.visit(c, enc, cx, 1);
            }
        }
        if let Some(b) = node.child_by_field_name("body") {
            self.visit(b, enc, cx, 1);
        }
    }

    /// The parts of a declarator that hold code: parameter types and
    /// defaults, trailing return types, array sizes, initializers.
    #[inline(never)]
    fn visit_declarator(
        &mut self,
        d: Node<'t>,
        ty: Option<Node<'t>>,
        enc: Option<LocalDefId>,
        cx: usize,
        depth: usize,
    ) {
        if depth > self.max_depth {
            return;
        }
        match d.kind() {
            "init_declarator" => {
                if let Some(inner) = d.child_by_field_name("declarator") {
                    self.visit_declarator(inner, ty, enc, cx, depth + 1);
                }
                if let Some(v) = d.child_by_field_name("value") {
                    // `Foo f(1);` / `Foo f{1};`: a constructor call.
                    if matches!(v.kind(), "argument_list" | "initializer_list")
                        && let Some(t) = ty
                    {
                        self.constructor_call(t, v, enc, cx);
                    }
                    self.visit(v, enc, cx, depth + 1);
                }
            }
            "function_declarator" => {
                if let Some(p) = d.child_by_field_name("parameters") {
                    self.visit(p, enc, cx, depth + 1);
                }
                for c in named_kids(d) {
                    if c.kind() == "trailing_return_type" {
                        self.visit(c, enc, cx, depth + 1);
                    }
                }
                if let Some(inner) = d.child_by_field_name("declarator")
                    && inner.kind() == "parenthesized_declarator"
                {
                    self.visit_declarator(inner, ty, enc, cx, depth + 1);
                }
            }
            "array_declarator" => {
                if let Some(s) = d.child_by_field_name("size") {
                    self.visit(s, enc, cx, depth + 1);
                }
                if let Some(inner) = d.child_by_field_name("declarator") {
                    self.visit_declarator(inner, ty, enc, cx, depth + 1);
                }
            }
            "pointer_declarator" | "attributed_declarator" => {
                if let Some(inner) = d.child_by_field_name("declarator") {
                    self.visit_declarator(inner, ty, enc, cx, depth + 1);
                }
            }
            "reference_declarator" | "parenthesized_declarator" => {
                for c in named_kids(d) {
                    self.visit_declarator(c, ty, enc, cx, depth + 1);
                }
            }
            "operator_cast" => {
                if let Some(t) = d.child_by_field_name("type") {
                    self.visit(t, enc, cx, depth + 1);
                }
                if let Some(inner) = d.child_by_field_name("declarator") {
                    self.visit_declarator(inner, ty, enc, cx, depth + 1);
                }
            }
            "abstract_function_declarator" => {
                if let Some(p) = d.child_by_field_name("parameters") {
                    self.visit(p, enc, cx, depth + 1);
                }
            }
            "bitfield_clause" => self.visit(d, enc, cx, depth + 1),
            _ => {}
        }
    }

    /// `Foo f(1)` / `Foo f{1}` / `Foo{1}`: a `Call` of the class (the
    /// resolver adds the constructor named like it).
    #[inline(never)]
    fn constructor_call(&mut self, ty: Node<'t>, args: Node<'t>, enc: Option<LocalDefId>, cx: usize) {
        let arity = self.arity(Some(args));
        match ty.kind() {
            "type_identifier" => {
                if !self.is_local(ty) {
                    let name = self.t(ty);
                    self.emit(RefKind::Call, &name, None, ty, enc, arity);
                }
            }
            "template_type" => {
                if let Some(n) = ty.child_by_field_name("name") {
                    self.constructor_call(n, args, enc, cx);
                }
            }
            "qualified_identifier" => {
                if let Some((quals, global, template_scope, leaf)) = self.split_qualified(ty) {
                    let leaf =
                        if leaf.kind() == "template_type" { leaf.child_by_field_name("name") } else { Some(leaf) };
                    if let Some(leaf) = leaf {
                        let name = self.t(leaf);
                        let recv = self.qualified_receiver(&quals, global, template_scope, &name, cx);
                        self.emit(RefKind::Call, &name, recv, leaf, enc, arity);
                    }
                }
            }
            _ => {}
        }
    }

    /// Qualifier segments, leading `::`, whether the last scope is a
    /// template, and the leaf node of a `qualified_identifier`.
    fn split_qualified(&self, node: Node<'t>) -> Option<(Vec<String>, bool, bool, Node<'t>)> {
        let global = self.t(node).trim_start().starts_with("::");
        let mut quals = Vec::new();
        let mut template_scope = false;
        let mut cur = node;
        for _ in 0..32 {
            if let Some(scope) = cur.child_by_field_name("scope") {
                template_scope = scope.kind() == "template_type";
                let seg = match scope.kind() {
                    "template_type" => scope.child_by_field_name("name").map(|n| self.t(n)).unwrap_or_default(),
                    _ => strip_template_args(&self.t(scope)),
                };
                if !seg.is_empty() {
                    quals.push(seg);
                }
            }
            let name = cur.child_by_field_name("name")?;
            if name.kind() == "qualified_identifier" {
                cur = name;
                continue;
            }
            return Some((quals, global, template_scope, name));
        }
        None
    }

    /// Prefixes a relative qualified type with the enclosing namespaces,
    /// for an exact qualified-name match in the resolver: inside
    /// `namespace fmt { namespace v11 {`, `detail::buffer` →
    /// `fmt::v11::detail::buffer`, `v11::detail::buffer` → the same.
    fn qualify_type(&self, quals: &[String], global: bool, cx: usize) -> String {
        let written = quals.join("::");
        if global || quals.len() < 2 {
            return written;
        }
        let ns = &self.cxs[cx].ns;
        let cut = ns.iter().position(|s| *s == quals[0]).unwrap_or(ns.len());
        if cut == 0 {
            return written;
        }
        format!("{}::{written}", ns[..cut].join("::"))
    }

    /// Receiver for `Q::name` (see the crate docs, *Qualified calls*).
    fn qualified_receiver(
        &self,
        quals: &[String],
        global: bool,
        template_scope: bool,
        name: &str,
        cx: usize,
    ) -> Option<String> {
        let root = quals.first()?;
        if BUILTIN_NAMESPACES.contains(&root.as_str()) {
            return Some(root.clone());
        }
        let last = quals.last()?.as_str();
        let is_type = template_scope
            || self.known_types.contains(last)
            || (!self.known_ns.contains(last) && looks_like_type(last));
        if is_type {
            return Some(type_receiver(&self.qualify_type(quals, global, cx)));
        }
        // A namespace function: no receiver (namespaces are not nodes), so
        // it resolves through includes and same-file names. Inside a method
        // with a member of that name, the implicit-self rung would pick the
        // member; keep the qualifier so it does not.
        let c = &self.cxs[cx];
        let shadowed = c.is_method
            && (c.fn_name.as_deref() == Some(name)
                || c.class_qn.as_ref().and_then(|q| self.class_members.get(q)).is_some_and(|m| m.contains(name)));
        shadowed.then(|| quals.join("::"))
    }

    /// Declared type of a variable named `name` at `node`: a local, a field
    /// of the class in scope, or a namespace-level variable of this file.
    fn var_type(&self, node: Node<'_>, cx: usize) -> Option<String> {
        let name = text(node, self.src);
        if let Some(b) = self.locals.lookup(&name, node.start_byte() as u32) {
            return b.ty.clone();
        }
        if let Some(q) = &self.cxs[cx].class_qn
            && let Some(t) = self.class_fields.get(q).and_then(|f| f.get(name.as_ref()))
        {
            return t.clone();
        }
        self.globals.get(name.as_ref()).cloned().flatten()
    }

    /// Fields of the class a type text names (exact qualified name, else
    /// the only class of the file with that simple name).
    fn fields_of(&self, ty: &str) -> Option<&HashMap<String, Option<String>>> {
        let t = strip_template_args(ty);
        let t = t.trim_start_matches("::");
        if let Some(f) = self.class_fields.get(t) {
            return Some(f);
        }
        let simple = t.rsplit("::").next()?;
        match self.classes_by_name.get(simple).map(Vec::as_slice) {
            Some([q]) => self.class_fields.get(q),
            _ => None,
        }
    }

    /// Declared type of a receiver expression, when the file says it.
    fn expr_type(&self, node: Node<'_>, cx: usize, depth: usize) -> Option<String> {
        if depth > 4 {
            return None;
        }
        match node.kind() {
            "this" => self.cxs[cx].class_qn.clone(),
            "identifier" => self.var_type(node, cx),
            "field_expression" => {
                let inner = node.child_by_field_name("argument")?;
                let field = node.child_by_field_name("field")?;
                let owner = self.expr_type(inner, cx, depth + 1)?;
                self.fields_of(&owner)?.get(text(field, self.src).as_ref()).cloned().flatten()
            }
            "pointer_expression" | "parenthesized_expression" => {
                let inner = named_kids(node).into_iter().next_back()?;
                self.expr_type(inner, cx, depth + 1)
            }
            _ => None,
        }
    }

    /// Receiver text of `obj.m()` / `p->m()`: `this`, `$type:T` when the
    /// declared type is known, else the source text.
    #[inline(never)]
    fn receiver(&self, arg: Node<'_>, cx: usize) -> String {
        let inner = match arg.kind() {
            "parenthesized_expression" | "pointer_expression" => named_kids(arg).into_iter().next_back(),
            _ => Some(arg),
        };
        if inner.is_some_and(|n| n.kind() == "this") {
            return "this".into();
        }
        if let Some(t) = self.expr_type(arg, cx, 0) {
            // A template's own type parameter is not a project type.
            let usable = !self
                .locals
                .lookup(&strip_template_args(&t), arg.start_byte() as u32)
                .is_some_and(|b| b.kind == DefKind::Type);
            if usable {
                let quals: Vec<String> = strip_template_args(&t).split("::").map(str::to_string).collect();
                let global = t.starts_with("::");
                let quals: Vec<String> = quals.into_iter().filter(|s| !s.is_empty()).collect();
                return type_receiver(&self.qualify_type(&quals, global, cx));
            }
        }
        truncate(collapse(&text(arg, self.src)), MAX_RECEIVER)
    }

    /// Reference walk. A thin dispatcher: every construct with its own logic
    /// is a separate, non-inlined method, so the frames on the recursion
    /// path stay small and the depth budget fits a 2 MiB stack.
    fn visit(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            "call_expression" => self.call(node, enc, cx, depth),
            "identifier" => self.ident_read(node, enc),
            "qualified_identifier" => self.qualified_use(node, RefKind::Read, None, enc, cx, depth),
            "type_identifier" | "template_type" => self.type_use(node, RefKind::Type, None, enc, cx, depth),
            "field_expression" => self.field_read(node, enc, cx, depth),
            "new_expression" => self.new_expr(node, enc, cx, depth),
            "throw_statement" | "throw_expression" => self.throw(node, enc, cx, depth),
            "compound_literal_expression" => self.compound_literal(node, enc, cx, depth),
            "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier" => {
                self.type_specifier(node, enc, cx, depth)
            }
            "declaration" | "field_declaration" => self.local_declaration(node, enc, cx, depth),
            "assignment_expression" | "update_expression" => self.assignment(node, enc, cx, depth),
            "parameter_declaration"
            | "optional_parameter_declaration"
            | "variadic_parameter_declaration"
            | "lambda_expression"
            | "enumerator" => self.fields(node, enc, cx, depth),
            "field_initializer" | "labeled_statement" | "preproc_if" | "preproc_ifdef" | "preproc_elif"
            | "preproc_else" | "preproc_elifdef" => self.children_except_names(node, enc, cx, depth),
            "comment"
            | "string_literal"
            | "raw_string_literal"
            | "char_literal"
            | "number_literal"
            | "concatenated_string"
            | "system_lib_string"
            | "primitive_type"
            | "sized_type_specifier"
            | "auto"
            | "placeholder_type_specifier"
            | "field_identifier"
            | "namespace_identifier"
            | "statement_identifier"
            | "attribute_declaration"
            | "attribute_specifier"
            | "preproc_call"
            | "preproc_def"
            | "preproc_function_def"
            | "preproc_include"
            | "using_declaration"
            | "namespace_alias_definition"
            | "goto_statement"
            | "field_designator"
            | "operator_name"
            | "destructor_name"
            | "this"
            | "true"
            | "false"
            | "null"
            | "nullptr"
            | "template_parameter_list"
            | "access_specifier" => {}
            _ => self.children(node, enc, cx, depth),
        }
    }

    /// Visits every named child.
    #[inline(never)]
    fn children(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let c = cursor.node();
            if c.is_named() {
                self.visit(c, enc, cx, depth + 1);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Visits the named children that are code: not a label, a field name
    /// of an initializer, or the condition / name of a preprocessor branch.
    #[inline(never)]
    fn children_except_names(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let c = cursor.node();
            let skip = matches!(cursor.field_name(), Some("condition" | "name"))
                || matches!(c.kind(), "statement_identifier" | "field_identifier");
            if c.is_named() && !skip {
                self.visit(c, enc, cx, depth + 1);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// The fields of a parameter, lambda or enumerator that hold code.
    #[inline(never)]
    fn fields(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        for field in ["type", "default_value", "value", "body"] {
            if let Some(c) = node.child_by_field_name(field) {
                self.visit(c, enc, cx, depth + 1);
            }
        }
        if let Some(d) = node.child_by_field_name("declarator") {
            self.visit_declarator(d, None, enc, cx, depth + 1);
        }
    }

    #[inline(never)]
    fn ident_read(&mut self, node: Node<'t>, enc: Option<LocalDefId>) {
        if !self.is_local(node) {
            let name = self.t(node);
            self.emit(RefKind::Read, &name, None, node, enc, None);
        }
    }

    /// `this->x` / `obj.x` with a typed receiver: a read of member `x`.
    #[inline(never)]
    fn field_read(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let (Some(arg), Some(field)) = (node.child_by_field_name("argument"), node.child_by_field_name("field")) else {
            return;
        };
        let recv = self.receiver(arg, cx);
        if recv == "this" || recv.starts_with(rueca_extract::TYPE_RECEIVER_PREFIX) {
            let name = self.member_name(field);
            self.emit(RefKind::Read, &name, Some(recv), field, enc, None);
        }
        self.visit(arg, enc, cx, depth + 1);
    }

    #[inline(never)]
    fn new_expr(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let ty = node.child_by_field_name("type");
        if let Some(t) = ty {
            let arity = self.arity(node.child_by_field_name("arguments")).or(Some(0));
            self.type_use(t, RefKind::New, arity, enc, cx, depth);
        }
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let c = cursor.node();
            if c.is_named() && Some(c) != ty {
                self.visit(c, enc, cx, depth + 1);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    #[inline(never)]
    fn throw(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        for c in named_kids(node) {
            if c.kind() == "call_expression"
                && let Some(f) = c.child_by_field_name("function")
            {
                self.callee(f, RefKind::Throw, None, enc, cx);
            }
            if c.kind() == "compound_literal_expression"
                && let Some(t) = c.child_by_field_name("type")
            {
                self.type_use(t, RefKind::Throw, None, enc, cx, depth);
            }
            self.visit(c, enc, cx, depth + 1);
        }
    }

    /// `T{…}`: a construction of `T`.
    #[inline(never)]
    fn compound_literal(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        if let Some(t) = node.child_by_field_name("type") {
            let arity = self.arity(node.child_by_field_name("value"));
            self.type_use(t, RefKind::Call, arity, enc, cx, depth);
        }
        if let Some(v) = node.child_by_field_name("value") {
            self.visit(v, enc, cx, depth + 1);
        }
    }

    /// `struct foo *p` (a type use) or a local class body.
    #[inline(never)]
    fn type_specifier(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        if let Some(n) = node.child_by_field_name("name") {
            self.type_use(n, RefKind::Type, None, enc, cx, depth);
        }
        for c in named_kids(node) {
            if matches!(c.kind(), "base_class_clause" | "field_declaration_list" | "enumerator_list") {
                self.visit(c, enc, cx, depth + 1);
            }
        }
    }

    /// A declaration inside code: its type, initializers and constructor call.
    #[inline(never)]
    fn local_declaration(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let ty = node.child_by_field_name("type");
        if let Some(t) = ty {
            self.visit(t, enc, cx, depth + 1);
        }
        let mut cursor = node.walk();
        let ds: Vec<Node<'t>> = node.children_by_field_name("declarator", &mut cursor).collect();
        for d in ds {
            self.visit_declarator(d, ty, enc, cx, depth + 1);
        }
        if let Some(v) = node.child_by_field_name("default_value") {
            self.visit(v, enc, cx, depth + 1);
        }
    }

    #[inline(never)]
    fn assignment(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let target = node.child_by_field_name("left").or_else(|| node.child_by_field_name("argument"));
        if let Some(t) = target {
            self.write(t, enc, cx, depth);
        }
        if let Some(r) = node.child_by_field_name("right") {
            self.visit(r, enc, cx, depth + 1);
        }
    }

    /// Name of a member in a field expression: `m`, `template get<T>`, `~T`.
    fn member_name(&self, field: Node<'_>) -> String {
        match field.kind() {
            "template_method" => field.child_by_field_name("name").map(|n| self.t(n)).unwrap_or_default(),
            "qualified_identifier" => {
                let mut cur = field;
                while let Some(n) = cur.child_by_field_name("name") {
                    cur = n;
                }
                self.t(cur)
            }
            _ => self.t(field),
        }
    }

    #[inline(never)]
    fn call(&mut self, node: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        let args = node.child_by_field_name("arguments");
        if let Some(f) = node.child_by_field_name("function") {
            self.callee(f, RefKind::Call, self.arity(args), enc, cx);
            self.env_read(f, args, enc);
            self.callee_parts(f, enc, cx, depth);
        }
        if let Some(a) = args {
            self.visit(a, enc, cx, depth + 1);
        }
    }

    /// `getenv("NAME")` / `std::getenv("NAME")`: an `EnvVar` reference.
    #[inline(never)]
    fn env_read(&mut self, f: Node<'t>, args: Option<Node<'t>>, enc: Option<LocalDefId>) {
        let fname = match f.kind() {
            "identifier" => Some(self.t(f)),
            "qualified_identifier" => self.split_qualified(f).filter(|(q, ..)| q == &["std"]).map(|(.., l)| self.t(l)),
            _ => None,
        };
        if fname.is_some_and(|n| GETENV.contains(&n.as_str()))
            && let Some(first) = args.and_then(|a| named_kids(a).into_iter().next())
            && first.kind() == "string_literal"
        {
            let kids = named_kids(first);
            if let [content] = kids.as_slice()
                && content.kind() == "string_content"
            {
                let name = self.t(*content);
                self.emit(RefKind::EnvVar, &name, None, first, enc, None);
            }
        }
    }

    /// Sub-expressions of a callee: the receiver, template arguments.
    #[inline(never)]
    fn callee_parts(&mut self, f: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        match f.kind() {
            "field_expression" => {
                if let Some(a) = f.child_by_field_name("argument") {
                    self.visit(a, enc, cx, depth + 1);
                }
                if let Some(field) = f.child_by_field_name("field")
                    && let Some(targs) = field.child_by_field_name("arguments")
                {
                    self.visit(targs, enc, cx, depth + 1);
                }
            }
            "template_function" => {
                if let Some(targs) = f.child_by_field_name("arguments") {
                    self.visit(targs, enc, cx, depth + 1);
                }
            }
            "qualified_identifier" => {
                if let Some((_, _, _, leaf)) = self.split_qualified(f)
                    && let Some(targs) = leaf.child_by_field_name("arguments")
                {
                    self.visit(targs, enc, cx, depth + 1);
                }
            }
            "identifier" => {}
            _ => self.visit(f, enc, cx, depth + 1),
        }
    }

    /// Emits the reference for a called expression (`Call` or `Throw`).
    #[inline(never)]
    fn callee(&mut self, f: Node<'t>, kind: RefKind, arity: Option<u32>, enc: Option<LocalDefId>, cx: usize) {
        match f.kind() {
            "identifier" => {
                if !self.is_local(f) {
                    let name = self.t(f);
                    self.emit(kind, &name, None, f, enc, arity);
                }
            }
            "template_function" => {
                let Some(n) = f.child_by_field_name("name") else { return };
                let name = self.t(n);
                if !CASTS.contains(&name.as_str()) && !self.is_local(n) {
                    self.emit(kind, &name, None, n, enc, arity);
                }
            }
            "field_expression" => {
                let (Some(arg), Some(field)) = (f.child_by_field_name("argument"), f.child_by_field_name("field"))
                else {
                    return;
                };
                let name = self.member_name(field);
                let recv = self.receiver(arg, cx);
                self.emit(kind, &name, Some(recv), field, enc, arity);
            }
            "qualified_identifier" => {
                let Some((quals, global, template_scope, leaf)) = self.split_qualified(f) else { return };
                let name_node = match leaf.kind() {
                    "template_function" => leaf.child_by_field_name("name"),
                    "identifier" | "destructor_name" | "operator_name" => Some(leaf),
                    _ => None,
                };
                let Some(n) = name_node else { return };
                let name = self.t(n);
                if CASTS.contains(&name.as_str()) {
                    return;
                }
                let recv = self.qualified_receiver(&quals, global, template_scope, &name, cx);
                self.emit(kind, &name, recv, n, enc, arity);
            }
            _ => {}
        }
    }

    /// `Q::name` in an expression (`Color::Red`, `Foo::kMax`, `ns::g`).
    #[inline(never)]
    fn qualified_use(
        &mut self,
        node: Node<'t>,
        kind: RefKind,
        arity: Option<u32>,
        enc: Option<LocalDefId>,
        cx: usize,
        depth: usize,
    ) {
        let Some((quals, global, template_scope, leaf)) = self.split_qualified(node) else { return };
        match leaf.kind() {
            "identifier" => {
                let name = self.t(leaf);
                let recv = self.qualified_receiver(&quals, global, template_scope, &name, cx);
                self.emit(kind, &name, recv, leaf, enc, arity);
            }
            // A qualified type in a type position.
            "type_identifier" | "template_type" => self.type_use(node, RefKind::Type, None, enc, cx, depth),
            "template_function" => {
                if let Some(targs) = leaf.child_by_field_name("arguments") {
                    self.visit(targs, enc, cx, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// A type written in source (`Foo`, `ns::Foo`, `Foo<Bar>`, `std::x`):
    /// `kind` is `Type`, or `New` / `Call` / `Throw` for `new T`, `T{…}`,
    /// `throw T{…}`.
    #[inline(never)]
    fn type_use(
        &mut self,
        node: Node<'t>,
        kind: RefKind,
        arity: Option<u32>,
        enc: Option<LocalDefId>,
        cx: usize,
        depth: usize,
    ) {
        match node.kind() {
            "type_identifier" => {
                // Template type parameters and local types are not project types.
                if self.locals.lookup(&self.t(node), node.start_byte() as u32).is_some() {
                    return;
                }
                let name = self.t(node);
                self.emit(kind, &name, None, node, enc, arity);
            }
            "template_type" => {
                if let Some(n) = node.child_by_field_name("name") {
                    self.type_use(n, kind, arity, enc, cx, depth);
                }
                if let Some(a) = node.child_by_field_name("arguments") {
                    self.visit(a, enc, cx, depth + 1);
                }
            }
            "qualified_identifier" => {
                let Some((quals, global, template_scope, leaf)) = self.split_qualified(node) else { return };
                let (name_node, targs) = match leaf.kind() {
                    "template_type" => (leaf.child_by_field_name("name"), leaf.child_by_field_name("arguments")),
                    "type_identifier" | "identifier" => (Some(leaf), None),
                    _ => (None, None),
                };
                if let Some(n) = name_node {
                    let name = self.t(n);
                    let root = quals.first().map(String::as_str).unwrap_or("");
                    let recv = if BUILTIN_NAMESPACES.contains(&root) {
                        Some(root.to_string())
                    } else if kind == RefKind::Type {
                        // Namespaces are not nodes: a qualified type is
                        // found by its name (includes, same file, unique).
                        None
                    } else {
                        self.qualified_receiver(&quals, global, template_scope, &name, cx)
                            .filter(|r| r.starts_with(rueca_extract::TYPE_RECEIVER_PREFIX))
                    };
                    self.emit(kind, &name, recv, n, enc, arity);
                }
                if let Some(a) = targs {
                    self.visit(a, enc, cx, depth + 1);
                }
            }
            _ => self.visit(node, enc, cx, depth + 1),
        }
    }

    /// The target of an assignment or increment.
    #[inline(never)]
    fn write(&mut self, target: Node<'t>, enc: Option<LocalDefId>, cx: usize, depth: usize) {
        match target.kind() {
            "identifier" => {
                if !self.is_local(target) {
                    let name = self.t(target);
                    self.emit(RefKind::Write, &name, None, target, enc, None);
                }
            }
            "field_expression" => {
                if let (Some(arg), Some(field)) =
                    (target.child_by_field_name("argument"), target.child_by_field_name("field"))
                {
                    let recv = self.receiver(arg, cx);
                    if recv == "this" || recv.starts_with(rueca_extract::TYPE_RECEIVER_PREFIX) {
                        let name = self.member_name(field);
                        self.emit(RefKind::Write, &name, Some(recv), field, enc, None);
                    }
                    self.visit(arg, enc, cx, depth + 1);
                }
            }
            "qualified_identifier" => self.qualified_use(target, RefKind::Write, None, enc, cx, depth),
            _ => self.visit(target, enc, cx, depth + 1),
        }
    }

    // -- finish --------------------------------------------------------------

    /// Overload suffixes: callables sharing a qualified name with different
    /// parameter lists in this file get `(<parameter types>)` appended.
    fn overload_suffixes(&mut self) {
        let mut groups: HashMap<&str, BTreeSet<&str>> = HashMap::new();
        for (d, p) in self.defs.iter().zip(&self.params) {
            if let Some(p) = p {
                groups.entry(d.qualified_name.as_str()).or_default().insert(p.as_str());
            }
        }
        let overloaded: HashSet<String> =
            groups.into_iter().filter(|(_, ps)| ps.len() > 1).map(|(qn, _)| qn.to_string()).collect();
        for (d, p) in self.defs.iter_mut().zip(&self.params) {
            if let Some(p) = p
                && overloaded.contains(&d.qualified_name)
            {
                d.qualified_name = format!("{}({p})", d.qualified_name);
            }
        }
    }
}

/// Extracts the facts of one C (`Language::C`) or C++ file.
pub(crate) fn extract(source: &[u8], language: Language, budget: &Budget) -> Result<FileFacts, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    let masked = mask::mask(source);
    let parsed: &[u8] = masked.as_deref().unwrap_or(source);
    let grammar: rueca_extract::tree_sitter::Language = match language {
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        other => return Err(ExtractError::Unsupported(other)),
    };
    let tree = rueca_extract::parse(&grammar, parsed, budget)?;
    let locals = locals::build(&tree, parsed, language, budget.max_parse_time)?;
    let mut x = Extractor::new(source, &locals, budget);
    let root = tree.root_node();
    x.prescan(root);
    x.includes();
    x.scope(root, &Ctx::default(), 0, None);
    x.run_jobs();
    x.overload_suffixes();
    let parse_errors = count_errors(&tree).saturating_sub(x.benign_errors);
    Ok(FileFacts {
        definitions: x.defs,
        references: x.refs,
        imports: x.imports,
        parse_errors,
        line_count: line_count(source),
    })
}
