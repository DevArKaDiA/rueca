//! Syntax-tree walker that turns one ECMAScript file into [`FileFacts`].
//!
//! The mapping rules are documented at the crate root. The walker is a single
//! recursive pass over the tree (bounded by `Budget::max_depth`) that keeps
//! three stacks: the innermost enclosing definition (`Ctx::parent`), the
//! function scopes used to decide whether a value read is local (their
//! bindings come from the vendored `locals.scm` queries, see
//! [`crate::locals`]), and the type parameters in scope (so `T` in
//! `function f<T>(x: T)` is not a type reference).

use std::collections::{HashMap, HashSet};

use rueca_extract::{span_of, text_of, type_receiver};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, RefKind, Reference, Span};
use tree_sitter::Node;

use crate::builtins::is_builtin;
use crate::locals::Locals;
use crate::types::{OWNERS, TypeTable};

/// Maximum references kept per file.
pub const MAX_REFS: usize = 20_000;
const MAX_SIGNATURE: usize = 200;
const MAX_DOC: usize = 1000;
const MAX_RECEIVER: usize = 200;
const MAX_TEST_NAME: usize = 200;
/// Longest source slice ever whitespace-collapsed for a signature/receiver.
const MAX_SCAN: usize = 2048;

const FUNCTION_VALUES: &[&str] = &["arrow_function", "function_expression", "function", "generator_function"];
const DESCRIBE_FNS: &[&str] = &["context", "describe", "fdescribe", "suite", "xdescribe"];
const TEST_FNS: &[&str] = &["fit", "it", "specify", "test", "xit", "xtest"];
/// Higher-order wrappers whose function argument is the real definition
/// (`const Button = forwardRef((props, ref) => ...)`).
const FUNCTION_WRAPPERS: &[&str] = &["forwardRef", "memo", "observer"];

#[derive(Clone, Copy)]
struct Ctx {
    /// Innermost enclosing definition; also the `enclosing` of references.
    parent: Option<u32>,
    /// Module level or directly inside a namespace body: variables become
    /// definitions and CommonJS export patterns are recognized.
    top: bool,
    /// Inside `declare module "x" { ... }`: every declaration is exported.
    ambient: bool,
}

/// Fields of a definition being created; the qualified name is derived from
/// the parent when it is added.
struct NewDef {
    label: Label,
    name: String,
    span: Span,
    name_span: Span,
    signature: Option<String>,
    doc: Option<String>,
    decorators: Vec<String>,
    bases: Vec<String>,
    implements: Vec<String>,
    exported: bool,
    is_test: bool,
}

impl NewDef {
    fn new(label: Label, name: String, span: Span, name_span: Span) -> NewDef {
        NewDef {
            label,
            name,
            span,
            name_span,
            signature: None,
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: false,
            is_test: false,
        }
    }
}

pub(crate) struct Extractor<'s> {
    src: &'s [u8],
    /// `./<own file name>`: module of self re-export imports.
    self_specifier: String,
    max_depth: usize,
    defs: Vec<Definition>,
    refs: Vec<Reference>,
    imports: Vec<Import>,
    read_seen: HashSet<(Option<u32>, Option<String>, String)>,
    /// Local bindings per owner, from the locals query.
    locals: &'s Locals,
    /// Declared and constructed types of bindings and class fields
    /// (`types.rs`), for `$type:` receivers.
    types: TypeTable,
    /// Owners in scope: `scopes[0]` is the root node (module-level block
    /// bindings: loop variables, catch parameters); the rest are the
    /// function-like nodes being walked.
    scopes: Vec<usize>,
    type_params: Vec<String>,
    /// Top-level names exported indirectly (`export { a }`, `export default a`,
    /// `module.exports = a`).
    export_names: HashSet<String>,
    /// Top-level definition index by name (last wins).
    top_level: HashMap<String, u32>,
    /// Env / config APIs for the code ↔ config bridge (`bridge.rs`).
    env: crate::bridge::EnvApi,
    /// `(parent, name)` of accessors already emitted, so a get/set pair yields one Method.
    accessors: HashSet<(Option<u32>, String)>,
}

/// Owner key that never has bindings (signature-only members).
const NO_SCOPE: usize = usize::MAX;

/// The class whose instance `this` is at `n`: the nearest class around it,
/// unless a non-arrow function (which rebinds `this`) comes first.
fn enclosing_class(n: Node<'_>) -> Option<usize> {
    let mut cur = n.parent();
    while let Some(p) = cur {
        match p.kind() {
            "class" | "class_declaration" | "abstract_class_declaration" => return Some(p.id()),
            k if OWNERS.contains(&k) && !matches!(k, "arrow_function" | "method_definition") => return None,
            _ => {}
        }
        cur = p.parent();
    }
    None
}

/// What [`Extractor::walk`] does after [`Extractor::visit`].
enum Next<'t> {
    Done,
    Children,
    Child(Node<'t>),
}

/// `path` is the file's repository-relative path; only its file name is used,
/// as the specifier of self re-exports (`./<file name>`).
pub(crate) fn extract_tree(root: Node<'_>, src: &[u8], path: &str, max_depth: usize, locals: &Locals) -> FileFacts {
    let mut ex = Extractor {
        src,
        self_specifier: format!("./{}", path.rsplit('/').next().unwrap_or(path)),
        max_depth,
        defs: Vec::new(),
        refs: Vec::new(),
        imports: Vec::new(),
        read_seen: HashSet::new(),
        locals,
        types: TypeTable::build(root, src, max_depth),
        scopes: vec![root.id()],
        type_params: Vec::new(),
        export_names: HashSet::new(),
        top_level: HashMap::new(),
        accessors: HashSet::new(),
        env: crate::bridge::EnvApi::scan(root, src),
    };
    let ctx = Ctx { parent: None, top: true, ambient: false };
    if root.kind() == "ERROR" {
        ex.error_region(root, ctx, 0, false);
    } else {
        ex.walk_children(root, ctx, 0);
    }
    if !ex.export_names.is_empty() {
        // Indirect exports are only known at the end; members (methods,
        // fields, enum constants) inherit the flag like direct exports do.
        // Parents always precede their children in `defs`.
        let mut flipped = vec![false; ex.defs.len()];
        for i in 0..ex.defs.len() {
            let d = &ex.defs[i];
            let flip = match d.parent {
                None => ex.export_names.contains(&d.name),
                Some(p) => flipped[p as usize] && matches!(d.label, Label::Method | Label::Field | Label::Constant),
            };
            if flip && !d.exported {
                ex.defs[i].exported = true;
                flipped[i] = true;
            }
        }
    }
    FileFacts { definitions: ex.defs, references: ex.refs, imports: ex.imports, parse_errors: 0, line_count: 0 }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn field<'t>(n: Node<'t>, name: &str) -> Option<Node<'t>> {
    n.child_by_field_name(name)
}

fn named_children<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

fn children<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut c = n.walk();
    n.children(&mut c).collect()
}

fn has_token(n: Node<'_>, token: &str) -> bool {
    let mut c = n.walk();
    n.children(&mut c).any(|ch| !ch.is_named() && ch.kind() == token)
}

fn span_from(start: Node<'_>, end: Node<'_>) -> Span {
    let s = span_of(start);
    let e = span_of(end);
    Span {
        start_byte: s.start_byte,
        start_line: s.start_line,
        start_col: s.start_col,
        end_byte: e.end_byte,
        end_line: e.end_line,
        end_col: e.end_col,
    }
}

fn is_function_value(n: Node<'_>) -> bool {
    FUNCTION_VALUES.contains(&n.kind())
}

/// Strips wrappers that do not change what a value is: parentheses, `as`,
/// `satisfies`, `!`, `<T>x`.
fn unwrap_value(mut n: Node<'_>) -> Node<'_> {
    for _ in 0..32 {
        let inner = match n.kind() {
            "parenthesized_expression" | "as_expression" | "satisfies_expression" | "non_null_expression" => {
                n.named_child(0)
            }
            "type_assertion" => n.named_child(n.named_child_count().saturating_sub(1) as u32),
            _ => None,
        };
        match inner {
            Some(i) => n = i,
            None => break,
        }
    }
    n
}

/// Collapses runs of whitespace to one space and trims.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(256));
    let mut pending_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    } else {
        s
    }
}

fn is_upper_const(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Cleans a `/** ... */` block: drops the delimiters and leading `*`.
fn clean_jsdoc(raw: &str) -> Option<String> {
    let body = raw.strip_prefix("/**")?;
    let body = body.strip_suffix("*/").unwrap_or(body);
    let lines: Vec<&str> = body
        .lines()
        .map(|l| {
            let l = l.trim();
            let l = l.strip_prefix('*').unwrap_or(l);
            l.strip_prefix(' ').unwrap_or(l).trim_end()
        })
        .collect();
    let text = lines.join("\n");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut t = text.to_string();
    if t.chars().count() > MAX_DOC {
        t = t.chars().take(MAX_DOC).collect();
    }
    Some(t)
}

// ---------------------------------------------------------------------------
// Extractor
// ---------------------------------------------------------------------------

impl<'s> Extractor<'s> {
    fn text(&self, n: Node<'_>) -> String {
        text_of(n, self.src).into_owned()
    }

    /// Whitespace-collapsed text of `[start, end)` (scanning at most
    /// `MAX_SCAN` bytes) minus the `exclude` ranges, with trailing body
    /// punctuation removed, truncated to `max` chars.
    fn header(&self, start: usize, end: usize, exclude: &[(usize, usize)], max: usize) -> Option<String> {
        let end = end.min(self.src.len()).min(start.saturating_add(MAX_SCAN));
        if start >= end {
            return None;
        }
        let mut raw = String::new();
        let mut pos = start;
        for &(a, b) in exclude {
            if a >= end || b <= pos {
                continue;
            }
            if a > pos {
                raw.push_str(&String::from_utf8_lossy(&self.src[pos..a]));
                raw.push(' ');
            }
            pos = b.max(pos);
        }
        if pos < end {
            raw.push_str(&String::from_utf8_lossy(&self.src[pos..end]));
        }
        let mut s = collapse_ws(&raw);
        loop {
            let t = s.trim_end();
            let t = t
                .strip_suffix("=>")
                .or_else(|| t.strip_suffix('{'))
                .or_else(|| t.strip_suffix('='))
                .or_else(|| t.strip_suffix(';'))
                .or_else(|| t.strip_suffix(','));
            match t {
                Some(t) => s = t.trim_end().to_string(),
                None => break,
            }
        }
        // Parameter decorators removed above can leave `( id` / `(, x`.
        let s = s.replace("( ", "(").replace(" )", ")").replace(" ,", ",").replace(",)", ")");
        if s.is_empty() { None } else { Some(truncate_chars(s, max)) }
    }

    fn collapsed(&self, n: Node<'_>, max: usize) -> String {
        let start = n.start_byte();
        let end = n.end_byte().min(start + MAX_SCAN);
        let s = collapse_ws(&String::from_utf8_lossy(&self.src[start..end]));
        truncate_chars(s, max)
    }

    /// The JSDoc comment immediately before `anchor`, if any.
    fn doc_before(&self, anchor: Node<'_>) -> Option<String> {
        let prev = anchor.prev_sibling()?;
        if prev.kind() != "comment" {
            return None;
        }
        let gap = &self.src[prev.end_byte()..anchor.start_byte()];
        if !gap.iter().all(|b| b.is_ascii_whitespace()) {
            return None;
        }
        clean_jsdoc(&text_of(prev, self.src))
    }

    /// Decorator text without `@`, whitespace-collapsed.
    fn decorator_text(&self, d: Node<'_>) -> String {
        let t = self.collapsed(d, MAX_SIGNATURE);
        t.trim_start_matches('@').trim().to_string()
    }

    fn qualify(&self, parent: Option<u32>, name: &str, is_test: bool) -> String {
        match parent {
            None => name.to_string(),
            Some(p) => {
                let pd = &self.defs[p as usize];
                if is_test && pd.label == Label::Test {
                    format!("{} > {}", pd.qualified_name, name)
                } else {
                    format!("{}.{}", pd.qualified_name, name)
                }
            }
        }
    }

    fn add_def(&mut self, nd: NewDef, parent: Option<u32>) -> u32 {
        let qn_name = if nd.is_test { nd.name.replace('.', "_") } else { nd.name.clone() };
        let qualified_name = self.qualify(parent, &qn_name, nd.is_test);
        let id = self.defs.len() as u32;
        if parent.is_none() {
            self.top_level.insert(nd.name.clone(), id);
        }
        self.defs.push(Definition {
            label: nd.label,
            name: nd.name,
            qualified_name,
            parent,
            span: nd.span,
            name_span: nd.name_span,
            signature: nd.signature,
            doc: nd.doc,
            decorators: nd.decorators,
            bases: nd.bases,
            implements: nd.implements,
            exported: nd.exported,
            is_test: nd.is_test,
            is_declaration: false,
        });
        id
    }

    fn push_ref(&mut self, kind: RefKind, name: String, receiver: Option<String>, enclosing: Option<u32>, span: Span) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        self.refs.push(Reference { kind, name, receiver, enclosing, span, arity: None });
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| self.locals.contains(*s, name))
    }

    /// Leftmost identifier of a receiver chain (`a` in `a.b().c[0].d`).
    fn root_ident(&self, mut n: Node<'_>) -> Option<String> {
        for _ in 0..256 {
            n = match n.kind() {
                "identifier" | "this" | "super" => return Some(self.text(n)),
                "member_expression" | "subscript_expression" => field(n, "object")?,
                "call_expression" => field(n, "function")?,
                "non_null_expression" | "parenthesized_expression" => n.named_child(0)?,
                _ => return None,
            };
        }
        None
    }

    /// `(name, receiver)` of a callee / constructor / decorator / JSX tag expression.
    fn callee_parts(&self, n: Node<'_>) -> Option<(String, Option<String>)> {
        match n.kind() {
            "identifier" | "property_identifier" | "type_identifier" => Some((self.text(n), None)),
            "member_expression" | "nested_identifier" => {
                let obj = field(n, "object")?;
                let prop = field(n, "property")?;
                Some((self.text(prop), Some(self.collapsed(obj, MAX_RECEIVER))))
            }
            "nested_type_identifier" => {
                let obj = field(n, "module")?;
                let name = field(n, "name")?;
                Some((self.text(name), Some(self.collapsed(obj, MAX_RECEIVER))))
            }
            "generic_type" => self.callee_parts(field(n, "name")?),
            "parenthesized_expression" | "non_null_expression" => self.callee_parts(n.named_child(0)?),
            _ => None,
        }
    }

    /// Whether a callee's root is a builtin global (and not shadowed locally).
    fn builtin_root(&self, callee: Node<'_>) -> bool {
        match self.root_ident(callee) {
            Some(r) => r != "this" && r != "super" && is_builtin(&r) && !self.is_local(&r),
            None => false,
        }
    }

    // -- scopes ---------------------------------------------------------------

    /// Pushes the type parameters declared by `n`; returns the previous stack length.
    fn push_type_params(&mut self, n: Node<'_>) -> usize {
        let mark = self.type_params.len();
        if let Some(tp) = field(n, "type_parameters") {
            for p in named_children(tp) {
                if p.kind() == "type_parameter"
                    && let Some(name) = field(p, "name")
                {
                    let t = self.text(name);
                    self.type_params.push(t);
                }
            }
        }
        mark
    }

    // -- walking --------------------------------------------------------------

    /// Walks every child except `name` fields (declaration names are handled
    /// by the declaration handlers and must not become references).
    fn walk_children(&mut self, n: Node<'_>, ctx: Ctx, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        let mut cur = n.walk();
        if !cur.goto_first_child() {
            return;
        }
        loop {
            let child = cur.node();
            if cur.field_name() != Some("name") && child.is_named() {
                self.walk(child, ctx, depth + 1);
            }
            if !cur.goto_next_sibling() {
                break;
            }
        }
    }

    /// Recursion hub. Kept small: everything that only emits facts for the
    /// current node lives in [`Extractor::visit`], so deep trees cost little
    /// stack per level (also in debug builds, where frames are not reused).
    fn walk(&mut self, n: Node<'_>, ctx: Ctx, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        if matches!(n.kind(), "member_expression" | "subscript_expression" | "call_expression") {
            self.bridge(n, ctx);
        }
        match n.kind() {
            "export_statement" => self.export_statement(n, ctx, depth),
            "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "internal_module"
            | "module"
            | "lexical_declaration"
            | "variable_declaration"
            | "ambient_declaration" => self.declaration(n, n, &[], ctx, depth, false),
            "expression_statement" => {
                if ctx.top && self.statement_assignment(n, ctx, depth) {
                    return;
                }
                self.walk_children(n, ctx, depth)
            }
            "function_expression" | "function" | "generator_function" if field(n, "name").is_some() => {
                self.named_function_expression(n, ctx, depth)
            }
            "arrow_function" | "function_expression" | "function" | "generator_function" | "method_definition" => {
                self.function_body(n, ctx.parent, ctx, depth)
            }
            "call_expression" => self.call(n, ctx, depth),
            "decorator" => self.decorator_ref(n, ctx, depth),
            "ERROR" => self.error_region(n, ctx, depth, false),
            // Generic signatures and function types declare their own type
            // parameters (`<P extends string>(path: P) => R`).
            "call_signature" | "construct_signature" | "function_type" | "constructor_type" => {
                let mark = self.push_type_params(n);
                self.walk_children(n, ctx, depth);
                self.type_params.truncate(mark);
            }
            "class" => self.class(n, n, &[], None, ctx, depth, false),
            _ => match self.visit(n, ctx, depth) {
                Next::Done => {}
                Next::Children => self.walk_children(n, ctx, depth),
                Next::Child(c) => self.walk(c, ctx, depth + 1),
            },
        }
    }

    /// Env var / config key reads at `n` (RFC 0003 §7.2, `bridge.rs`).
    #[inline(never)]
    fn bridge(&mut self, n: Node<'_>, ctx: Ctx) {
        for (kind, name, receiver) in self.env.facts(n, self.src, |r| self.is_local(r)) {
            self.push_ref(kind, name, receiver, ctx.parent, span_of(n));
        }
    }

    /// A parse-error region. tree-sitter-typescript 0.23 cannot parse a
    /// generic call signature that starts a new line after another member
    /// (`interface H<E> { (a: E): R \n <P>(p: P): R }`): the rest of the
    /// declaration is split into fragments (`type_parameters`,
    /// `formal_parameters`, nested `ERROR`s) that follow it as siblings, and
    /// the error can climb up to the whole file. Statements inside the region
    /// are walked normally; each fragment is attributed to the declaration
    /// before it, with that declaration's type parameters and those of earlier
    /// `type_parameters` fragments in scope, so `E`, `P`, … in the fragments
    /// are not reported as type references.
    #[inline(never)]
    /// `nested`: a fragment of an enclosing region, whose type parameters stay
    /// in scope for the fragments that follow it.
    fn error_region(&mut self, n: Node<'_>, ctx: Ctx, depth: usize, nested: bool) {
        if depth >= self.max_depth {
            return;
        }
        let base = self.type_params.len();
        let mut owner = ctx;
        if !nested {
            // The declaration the region continues, right before it.
            let mut prev = n.prev_named_sibling();
            while let Some(p) = prev.filter(|p| p.kind() == "comment") {
                prev = p.prev_named_sibling();
            }
            if let Some(p) = prev {
                owner = self.fragment_owner(p, ctx);
            }
        }
        for c in named_children(n) {
            let k = c.kind();
            if k == "comment" {
                continue;
            }
            let statement = k.ends_with("_statement")
                || k.ends_with("_declaration")
                || matches!(k, "internal_module" | "module" | "ambient_declaration");
            if !statement {
                match k {
                    "ERROR" => self.error_region(c, owner, depth + 1, true),
                    "type_parameters" => {
                        for p in named_children(c) {
                            if p.kind() == "type_parameter"
                                && let Some(name) = field(p, "name")
                            {
                                let t = self.text(name);
                                self.type_params.push(t);
                            }
                        }
                        self.walk(c, owner, depth + 1);
                    }
                    _ => self.walk(c, owner, depth + 1),
                }
                continue;
            }
            self.type_params.truncate(base);
            self.walk(c, ctx, depth + 1);
            owner = self.fragment_owner(c, ctx);
        }
        if !nested {
            self.type_params.truncate(base);
        }
    }

    /// A named function expression in any position not already handled as a
    /// definition (call arguments, `return`, operands): `defineGetter(req,
    /// "query", function query() {})`, `app.use(function handler() {})`,
    /// `return async function timing(c, next) {}`. It becomes a `Function`
    /// child of the enclosing definition; its doc is the JSDoc before the
    /// statement holding it.
    #[inline(never)]
    fn named_function_expression(&mut self, n: Node<'_>, ctx: Ctx, depth: usize) {
        let Some(name_node) = field(n, "name") else { return self.function_body(n, ctx.parent, ctx, depth) };
        let mut nd = NewDef::new(Label::Function, self.text(name_node), span_of(n), span_of(name_node));
        nd.signature = self.fn_signature(n);
        let mut anchor = n;
        for _ in 0..4 {
            match anchor.parent() {
                Some(p) if !matches!(p.kind(), "statement_block" | "program" | "class_body") => anchor = p,
                _ => break,
            }
            if anchor.kind().ends_with("_statement") {
                break;
            }
        }
        nd.doc = self.doc_before(n).or_else(|| self.doc_before(anchor));
        let id = self.add_def(nd, ctx.parent);
        self.function_body(n, Some(id), ctx, depth);
    }

    /// For a statement followed by error fragments: pushes the type parameters
    /// of the declaration it holds and returns the context fragments are
    /// attributed to (that declaration when it is top-level, else `ctx`).
    fn fragment_owner(&mut self, stmt: Node<'_>, ctx: Ctx) -> Ctx {
        let decl = if stmt.kind() == "export_statement" { field(stmt, "declaration") } else { Some(stmt) };
        let Some(d) = decl.filter(|d| {
            matches!(
                d.kind(),
                "interface_declaration" | "type_alias_declaration" | "class_declaration" | "abstract_class_declaration"
            )
        }) else {
            return ctx;
        };
        self.push_type_params(d);
        if ctx.parent.is_none()
            && let Some(name) = field(d, "name")
            && let Some(&id) = self.top_level.get(&self.text(name))
        {
            return Ctx { parent: Some(id), top: false, ambient: false };
        }
        ctx
    }

    /// Module-level `a = b` statements (CommonJS exports, prototype methods).
    #[inline(never)]
    fn statement_assignment(&mut self, n: Node<'_>, ctx: Ctx, depth: usize) -> bool {
        match n.named_child(0) {
            Some(e) if e.kind() == "assignment_expression" => self.top_assignment(e, ctx, depth),
            _ => false,
        }
    }

    /// Facts of one node that do not recurse into [`Extractor::walk`];
    /// tells the caller where to continue.
    #[inline(never)]
    fn visit<'t>(&mut self, n: Node<'t>, ctx: Ctx, depth: usize) -> Next<'t> {
        match n.kind() {
            "comment"
            | "identifier"
            | "property_identifier"
            | "string"
            | "number"
            | "regex"
            | "jsx_closing_element" => Next::Done,
            "import_statement" => {
                self.import_statement(n);
                Next::Done
            }
            "new_expression" => {
                if let Some(c) = field(n, "constructor")
                    && !self.builtin_root(c)
                    && let Some((name, recv)) = self.callee_parts(c)
                {
                    self.push_ref(RefKind::New, name, recv, ctx.parent, span_of(n));
                }
                Next::Children
            }
            "throw_statement" => {
                if let Some(e) = n.named_child(0)
                    && e.kind() == "new_expression"
                    && let Some(c) = field(e, "constructor")
                    && !self.builtin_root(c)
                    && let Some((name, recv)) = self.callee_parts(c)
                {
                    self.push_ref(RefKind::Throw, name, recv, ctx.parent, span_of(e));
                }
                Next::Children
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(tag) = field(n, "name") {
                    let parts = match tag.kind() {
                        "identifier" => {
                            let t = self.text(tag);
                            if t.starts_with(|c: char| c.is_ascii_uppercase()) { Some((t, None)) } else { None }
                        }
                        "member_expression" | "nested_identifier" => self.callee_parts(tag),
                        _ => None,
                    };
                    if let Some((name, recv)) = parts {
                        self.push_ref(RefKind::Call, name, recv, ctx.parent, span_of(n));
                    }
                }
                Next::Children
            }
            "type_identifier" | "nested_type_identifier" => {
                self.type_ref(n, ctx);
                Next::Done
            }
            "generic_type" => {
                if let Some(name) = field(n, "name") {
                    self.type_ref(name, ctx);
                }
                field(n, "type_arguments").map_or(Next::Done, Next::Child)
            }
            "mapped_type_clause" => {
                if let Some(name) = field(n, "name") {
                    let t = self.text(name);
                    self.type_params.push(t);
                }
                Next::Children
            }
            "infer_type" => {
                if let Some(name) = n.named_child(0) {
                    let t = self.text(name);
                    self.type_params.push(t);
                }
                Next::Done
            }
            "arguments" | "array" => {
                for ch in named_children(n) {
                    self.read(ch, ctx, depth);
                }
                Next::Children
            }
            "return_statement" | "spread_element" | "jsx_expression" => {
                if let Some(e) = n.named_child(0) {
                    self.read(e, ctx, depth);
                }
                Next::Children
            }
            "assignment_expression" | "augmented_assignment_expression" => {
                if let Some(l) = field(n, "left")
                    && l.kind() == "member_expression"
                    && field(l, "object").is_some_and(|o| o.kind() == "this")
                    && let Some(p) = field(l, "property")
                {
                    let name = self.text(p);
                    self.push_ref(RefKind::Write, name, Some("this".into()), ctx.parent, span_of(l));
                }
                if let Some(r) = field(n, "right") {
                    self.read(r, ctx, depth);
                }
                Next::Children
            }
            "pair" => {
                if let Some(v) = field(n, "value") {
                    self.read(v, ctx, depth);
                }
                Next::Children
            }
            "shorthand_property_identifier" => {
                // `{ id }` reads `id`.
                let name = self.text(n);
                if !self.is_local(&name) && !is_builtin(&name) {
                    self.read_name(name, None, ctx, span_of(n));
                }
                Next::Done
            }
            // `type_parameter`: the name is a binding, not a reference (the
            // `name` field is skipped by `walk_children`).
            _ => Next::Children,
        }
    }

    // -- declarations ---------------------------------------------------------

    /// Dispatches a declaration. `outer` is the node whose extent is the
    /// definition span (the `export` statement when exported); `decorators`
    /// are decorators attached to the enclosing `export` statement.
    fn declaration(
        &mut self,
        n: Node<'_>,
        outer: Node<'_>,
        decorators: &[Node<'_>],
        ctx: Ctx,
        depth: usize,
        exported: bool,
    ) {
        let exported = exported || ctx.ambient;
        match n.kind() {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.decl_function(n, outer, ctx, depth, exported)
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.class(n, outer, decorators, None, ctx, depth, exported)
            }
            "interface_declaration" => self.decl_interface(n, outer, ctx, depth, exported),
            "type_alias_declaration" => self.decl_type_alias(n, outer, ctx, depth, exported),
            "enum_declaration" => self.decl_enum(n, outer, ctx, depth, exported),
            "internal_module" | "module" => self.decl_module(n, outer, ctx, depth, exported),
            "ambient_declaration" => {
                // `declare ...`: the wrapped declaration keeps `outer` as its span.
                for ch in named_children(n) {
                    if ch.kind() == "statement_block" {
                        // `declare global { ... }`
                        self.walk_children(ch, ctx, depth + 1);
                    } else {
                        self.declaration(ch, outer, decorators, ctx, depth + 1, exported);
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => self.variables(n, outer, ctx, depth, exported),
            _ => self.walk(n, ctx, depth + 1),
        }
    }

    #[inline(never)]
    fn decl_function(&mut self, n: Node<'_>, outer: Node<'_>, ctx: Ctx, depth: usize, exported: bool) {
        if n.kind() == "function_signature" && self.overload_followed(n, outer) {
            self.walk_children(n, ctx, depth);
            return;
        }
        let Some(name_node) = field(n, "name") else { return self.walk_children(n, ctx, depth) };
        let mut nd = NewDef::new(Label::Function, self.text(name_node), span_of(outer), span_of(name_node));
        nd.signature = self.fn_signature(n);
        nd.doc = self.doc_before(outer);
        nd.exported = exported;
        let id = self.add_def(nd, ctx.parent);
        self.function_body(n, Some(id), ctx, depth);
    }

    #[inline(never)]
    fn decl_interface(&mut self, n: Node<'_>, outer: Node<'_>, ctx: Ctx, depth: usize, exported: bool) {
        let Some(name_node) = field(n, "name") else { return };
        let mut nd = NewDef::new(Label::Interface, self.text(name_node), span_of(outer), span_of(name_node));
        nd.signature = self.until_body(n);
        nd.doc = self.doc_before(outer);
        nd.exported = exported;
        for ch in named_children(n) {
            if ch.kind() == "extends_type_clause" {
                for t in named_children(ch) {
                    nd.bases.push(self.type_base_text(t));
                }
            }
        }
        let id = self.add_def(nd, ctx.parent);
        let mark = self.push_type_params(n);
        let inner = Ctx { parent: Some(id), top: false, ambient: false };
        for ch in named_children(n) {
            match ch.kind() {
                "interface_body" | "object_type" => self.type_members(ch, id, depth + 1),
                _ if ch.id() == name_node.id() => {}
                _ => self.walk(ch, inner, depth + 1),
            }
        }
        self.type_params.truncate(mark);
    }

    #[inline(never)]
    fn decl_type_alias(&mut self, n: Node<'_>, outer: Node<'_>, ctx: Ctx, depth: usize, exported: bool) {
        let Some(name_node) = field(n, "name") else { return };
        let mut nd = NewDef::new(Label::TypeAlias, self.text(name_node), span_of(outer), span_of(name_node));
        // Object types are bodies (`type A = { ... }` → `type A`); other
        // values are part of the signature (`type Id = string`).
        let end = field(n, "value").filter(|v| v.kind() == "object_type").map_or(n.end_byte(), |v| v.start_byte());
        nd.signature = self.header(n.start_byte(), end, &[], MAX_SIGNATURE);
        nd.doc = self.doc_before(outer);
        nd.exported = exported;
        let id = self.add_def(nd, ctx.parent);
        let mark = self.push_type_params(n);
        let inner = Ctx { parent: Some(id), top: false, ambient: false };
        if let Some(tp) = field(n, "type_parameters") {
            self.walk(tp, inner, depth + 1);
        }
        if let Some(v) = field(n, "value") {
            if v.kind() == "object_type" {
                self.type_members(v, id, depth + 1);
            } else {
                self.walk(v, inner, depth + 1);
            }
        }
        self.type_params.truncate(mark);
    }

    #[inline(never)]
    fn decl_enum(&mut self, n: Node<'_>, outer: Node<'_>, ctx: Ctx, depth: usize, exported: bool) {
        let Some(name_node) = field(n, "name") else { return };
        let mut nd = NewDef::new(Label::Enum, self.text(name_node), span_of(outer), span_of(name_node));
        nd.signature = self.until_body(n);
        nd.doc = self.doc_before(outer);
        nd.exported = exported;
        let id = self.add_def(nd, ctx.parent);
        let inner = Ctx { parent: Some(id), top: false, ambient: false };
        if let Some(body) = field(n, "body") {
            for m in named_children(body) {
                let (name_node, value) = match m.kind() {
                    "enum_assignment" => (field(m, "name"), field(m, "value")),
                    "property_identifier" | "string" | "computed_property_name" => (Some(m), None),
                    _ => (None, None),
                };
                let Some(name_node) = name_node else { continue };
                let mut name = self.text(name_node);
                if name_node.kind() == "string" {
                    name = name.trim_matches(|c| c == '"' || c == '\'').to_string();
                }
                let mut md = NewDef::new(Label::Constant, name, span_of(m), span_of(name_node));
                md.signature = self.header(m.start_byte(), m.end_byte(), &[], MAX_SIGNATURE);
                md.doc = self.doc_before(m);
                md.exported = exported;
                self.add_def(md, Some(id));
                if let Some(v) = value {
                    self.walk(v, inner, depth + 1);
                }
            }
        }
    }

    #[inline(never)]
    fn decl_module(&mut self, n: Node<'_>, outer: Node<'_>, ctx: Ctx, depth: usize, exported: bool) {
        let Some(name_node) = field(n, "name") else { return self.walk_children(n, ctx, depth) };
        let name = if name_node.kind() == "string" {
            let t = self.text(name_node);
            t.get(1..t.len().saturating_sub(1)).unwrap_or("").to_string()
        } else {
            self.text(name_node)
        };
        let ambient = ctx.ambient || name_node.kind() == "string";
        let mut nd = NewDef::new(Label::Module, name, span_of(outer), span_of(name_node));
        nd.signature = self.until_body(n);
        nd.doc = self.doc_before(outer);
        nd.exported = exported;
        let id = self.add_def(nd, ctx.parent);
        if let Some(body) = field(n, "body") {
            let inner = Ctx { parent: Some(id), top: true, ambient };
            self.walk_children(body, inner, depth + 1);
        }
    }

    /// True if a TS overload signature is followed by more overloads and then
    /// the implementation with the same name (only the implementation is kept).
    fn overload_followed(&self, n: Node<'_>, anchor: Node<'_>) -> bool {
        let Some(name) = field(n, "name").map(|x| self.text(x)) else { return false };
        let mut cur = anchor.next_named_sibling();
        let mut steps = 0;
        while let Some(s) = cur {
            steps += 1;
            if steps > 64 {
                return false;
            }
            let decl = if s.kind() == "export_statement" { field(s, "declaration") } else { Some(s) };
            match decl {
                Some(d) if matches!(d.kind(), "function_declaration" | "method_definition") => {
                    return field(d, "name").is_some_and(|x| self.text(x) == name);
                }
                Some(d) if matches!(d.kind(), "function_signature" | "method_signature") => {}
                _ if s.kind() == "decorator" || s.kind() == "comment" => {}
                _ => return false,
            }
            cur = s.next_named_sibling();
        }
        false
    }

    /// Signature of a function-like node: from its first non-decorator child
    /// to its body, parameter decorators removed.
    fn fn_signature(&self, n: Node<'_>) -> Option<String> {
        let start = children(n)
            .into_iter()
            .find(|c| c.kind() != "decorator" && c.kind() != "comment")
            .map_or(n.start_byte(), |c| c.start_byte());
        let end = field(n, "body").map_or(n.end_byte(), |b| {
            if n.kind() == "arrow_function" {
                children(n).into_iter().find(|c| c.kind() == "=>").map_or(b.start_byte(), |a| a.start_byte())
            } else {
                b.start_byte()
            }
        });
        let mut excl = Vec::new();
        if let Some(params) = field(n, "parameters") {
            for p in named_children(params) {
                for d in named_children(p) {
                    if d.kind() == "decorator" {
                        excl.push((d.start_byte(), d.end_byte()));
                    }
                }
            }
        }
        self.header(start, end, &excl, MAX_SIGNATURE)
    }

    fn until_body(&self, n: Node<'_>) -> Option<String> {
        let start = children(n)
            .into_iter()
            .find(|c| c.kind() != "decorator" && c.kind() != "comment")
            .map_or(n.start_byte(), |c| c.start_byte());
        let end = field(n, "body").map_or(n.end_byte(), |b| b.start_byte());
        self.header(start, end, &[], MAX_SIGNATURE)
    }

    /// Text of a base / implemented type without type arguments.
    fn type_base_text(&self, t: Node<'_>) -> String {
        let t = if t.kind() == "generic_type" { field(t, "name").unwrap_or(t) } else { t };
        self.collapsed(t, MAX_RECEIVER)
    }

    /// Walks a function-like node with its own scope and type parameters.
    /// `def` becomes the enclosing definition of everything inside.
    fn function_body(&mut self, n: Node<'_>, def: Option<u32>, _ctx: Ctx, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        self.scopes.push(n.id());
        let mark = self.push_type_params(n);
        let inner = Ctx { parent: def, top: false, ambient: false };
        if n.kind() == "arrow_function"
            && let Some(b) = field(n, "body")
            && b.kind() != "statement_block"
        {
            self.read(b, inner, depth);
        }
        self.walk_children(n, inner, depth);
        self.type_params.truncate(mark);
        self.scopes.pop();
    }

    /// Interface / object-type members: property signatures → `Field`,
    /// method signatures → `Method`.
    fn type_members(&mut self, body: Node<'_>, owner: u32, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        let owner_ctx = Ctx { parent: Some(owner), top: false, ambient: false };
        for m in named_children(body) {
            let label = match m.kind() {
                "property_signature" => Label::Field,
                "method_signature" => Label::Method,
                "comment" => continue,
                _ => {
                    self.walk(m, owner_ctx, depth + 1);
                    continue;
                }
            };
            let Some(name_node) = field(m, "name") else { continue };
            let name = self.member_name(name_node);
            if label == Label::Method
                && self
                    .defs
                    .iter()
                    .rev()
                    .take(64)
                    .any(|d| d.parent == Some(owner) && d.name == name && d.label == Label::Method)
            {
                // Overloaded method signature: keep the first.
                self.walk_children(m, Ctx { parent: Some(owner), ..owner_ctx }, depth + 1);
                continue;
            }
            let mut nd = NewDef::new(label, name, span_of(m), span_of(name_node));
            nd.signature = self.header(m.start_byte(), m.end_byte(), &[], MAX_SIGNATURE);
            nd.doc = self.doc_before(m);
            nd.exported = self.defs[owner as usize].exported;
            let id = self.add_def(nd, Some(owner));
            if label == Label::Method {
                self.scopes.push(NO_SCOPE);
                let mark = self.push_type_params(m);
                self.walk_children(m, Ctx { parent: Some(id), top: false, ambient: false }, depth + 1);
                self.type_params.truncate(mark);
                self.scopes.pop();
            } else {
                self.walk_children(m, Ctx { parent: Some(id), top: false, ambient: false }, depth + 1);
            }
        }
    }

    /// Name of a class / object / interface member key.
    fn member_name(&self, n: Node<'_>) -> String {
        match n.kind() {
            "string" => {
                let t = self.text(n);
                t.get(1..t.len().saturating_sub(1)).unwrap_or("").to_string()
            }
            _ => self.collapsed(n, MAX_RECEIVER),
        }
    }

    // -- classes ----------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn class(
        &mut self,
        n: Node<'_>,
        outer: Node<'_>,
        export_decorators: &[Node<'_>],
        name_override: Option<(String, Span)>,
        ctx: Ctx,
        depth: usize,
        exported: bool,
    ) {
        if depth >= self.max_depth {
            return;
        }
        let Some((id, extends_nodes)) = self.class_def(n, outer, export_decorators, name_override, ctx, exported)
        else {
            // Anonymous class expression: no definition, just walk it.
            return self.class_members(n, None, ctx, depth);
        };
        let inner = Ctx { parent: Some(id), top: false, ambient: false };
        for d in export_decorators {
            self.walk(*d, inner, depth + 1);
        }
        for d in children(n) {
            if d.kind() == "decorator" {
                self.walk(d, inner, depth + 1);
            }
        }
        let mark = self.push_type_params(n);
        if let Some(tp) = field(n, "type_parameters") {
            self.walk(tp, inner, depth + 1);
        }
        for e in &extends_nodes {
            if let Some((name, recv)) = self.callee_parts(*e) {
                self.push_ref(RefKind::Type, name, recv, Some(id), span_of(*e));
            } else {
                self.read(*e, inner, depth + 1);
                self.walk(*e, inner, depth + 1);
            }
        }
        for ch in named_children(n) {
            if ch.kind() == "class_heritage" {
                for h in named_children(ch) {
                    match h.kind() {
                        "extends_clause" => {
                            if let Some(ta) = field(h, "type_arguments") {
                                self.walk(ta, inner, depth + 2);
                            }
                        }
                        "implements_clause" => self.walk(h, inner, depth + 2),
                        _ => {}
                    }
                }
            }
        }
        self.class_members(n, Some(id), inner, depth);
        self.type_params.truncate(mark);
    }

    /// Creates the `Class` definition; returns its id and the `extends`
    /// expressions, or `None` for an anonymous class expression.
    #[inline(never)]
    fn class_def<'t>(
        &mut self,
        n: Node<'t>,
        outer: Node<'_>,
        export_decorators: &[Node<'_>],
        name_override: Option<(String, Span)>,
        ctx: Ctx,
        exported: bool,
    ) -> Option<(u32, Vec<Node<'t>>)> {
        let own_name = field(n, "name");
        let (name, name_span) = match (name_override, own_name) {
            (Some(o), _) => o,
            (None, Some(nn)) => (self.text(nn), span_of(nn)),
            (None, None) => {
                if n.kind() == "class" && outer.id() == n.id() {
                    return None;
                }
                let kw = children(n).into_iter().find(|c| c.kind() == "class").unwrap_or(n);
                ("default".to_string(), span_of(kw))
            }
        };
        let own_decorators: Vec<Node<'_>> = children(n).into_iter().filter(|c| c.kind() == "decorator").collect();
        let mut nd = NewDef::new(Label::Class, name, span_of(outer), name_span);
        nd.signature = self.until_body(n);
        nd.doc = self.doc_before(outer);
        nd.exported = exported;
        for d in export_decorators.iter().chain(own_decorators.iter()) {
            nd.decorators.push(self.decorator_text(*d));
        }
        let mut extends_nodes = Vec::new();
        let mut implements_nodes = Vec::new();
        for ch in named_children(n) {
            if ch.kind() == "class_heritage" {
                for h in named_children(ch) {
                    match h.kind() {
                        "extends_clause" => {
                            let mut c = h.walk();
                            for (i, v) in h.children(&mut c).enumerate() {
                                if h.field_name_for_child(i as u32) == Some("value") {
                                    extends_nodes.push(v);
                                }
                            }
                        }
                        "implements_clause" => implements_nodes.extend(named_children(h)),
                        "comment" => {}
                        // JavaScript: `class_heritage` holds the expression directly.
                        _ => extends_nodes.push(h),
                    }
                }
            }
        }
        for e in &extends_nodes {
            nd.bases.push(self.collapsed(*e, MAX_RECEIVER));
        }
        for t in &implements_nodes {
            nd.implements.push(self.type_base_text(*t));
        }
        Some((self.add_def(nd, ctx.parent), extends_nodes))
    }

    /// Class body members. `class_id` is `None` for anonymous class expressions.
    fn class_members(&mut self, n: Node<'_>, class_id: Option<u32>, ctx: Ctx, depth: usize) {
        let Some(body) = field(n, "body") else { return };
        let mut pending: Vec<Node<'_>> = Vec::new();
        for m in named_children(body) {
            match m.kind() {
                "decorator" => pending.push(m),
                "comment" => {}
                "method_definition" | "method_signature" | "abstract_method_signature" => {
                    let decs = std::mem::take(&mut pending);
                    match class_id {
                        Some(cid) => self.method(m, &decs, cid, depth + 1),
                        None => {
                            for d in &decs {
                                self.walk(*d, ctx, depth + 1);
                            }
                            self.function_body(m, ctx.parent, ctx, depth + 1);
                        }
                    }
                }
                "public_field_definition" | "field_definition" => {
                    let decs = std::mem::take(&mut pending);
                    match class_id {
                        Some(cid) => self.class_field(m, &decs, cid, depth + 1),
                        None => self.walk_children(m, ctx, depth + 1),
                    }
                }
                _ => {
                    pending.clear();
                    self.walk(m, ctx, depth + 1);
                }
            }
        }
    }

    fn method(&mut self, m: Node<'_>, sibling_decorators: &[Node<'_>], class_id: u32, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        let class_ctx = Ctx { parent: Some(class_id), top: false, ambient: false };
        let Some(name_node) = field(m, "name") else { return self.walk_children(m, class_ctx, depth) };
        let name = self.member_name(name_node);
        let is_signature = m.kind() != "method_definition";
        if is_signature && self.overload_followed(m, m) {
            return;
        }
        if (has_token(m, "get") || has_token(m, "set")) && !self.accessors.insert((Some(class_id), name.clone())) {
            // Second half of a get/set pair: attribute its body to the first.
            let prev = self
                .defs
                .iter()
                .rposition(|d| d.parent == Some(class_id) && d.name == name && d.label == Label::Method)
                .map(|i| i as u32);
            return self.function_body(m, prev.or(Some(class_id)), class_ctx, depth);
        }
        let own_decorators: Vec<Node<'_>> = children(m).into_iter().filter(|c| c.kind() == "decorator").collect();
        let first = sibling_decorators.first().copied().unwrap_or(m);
        let mut nd = NewDef::new(Label::Method, name.clone(), span_from(first, m), span_of(name_node));
        nd.signature = self.fn_signature(m);
        nd.doc = self.doc_before(first);
        nd.exported = self.defs[class_id as usize].exported;
        for d in sibling_decorators.iter().chain(own_decorators.iter()) {
            nd.decorators.push(self.decorator_text(*d));
        }
        let id = self.add_def(nd, Some(class_id));
        let inner = Ctx { parent: Some(id), top: false, ambient: false };
        for d in sibling_decorators {
            self.walk(*d, inner, depth + 1);
        }
        if name == "constructor"
            && let Some(params) = field(m, "parameters")
        {
            self.parameter_properties(params, class_id);
        }
        self.function_body(m, Some(id), class_ctx, depth);
    }

    /// TS parameter properties (`constructor(private readonly repo: Repo)`)
    /// become `Field`s of the class.
    fn parameter_properties(&mut self, params: Node<'_>, class_id: u32) {
        for p in named_children(params) {
            if !matches!(p.kind(), "required_parameter" | "optional_parameter") {
                continue;
            }
            let is_property = children(p).into_iter().any(|c| {
                c.kind() == "accessibility_modifier" || c.kind() == "readonly" || c.kind() == "override_modifier"
            });
            if !is_property {
                continue;
            }
            let Some(pat) = field(p, "pattern") else { continue };
            if pat.kind() != "identifier" {
                continue;
            }
            let excl: Vec<(usize, usize)> = named_children(p)
                .into_iter()
                .filter(|c| c.kind() == "decorator")
                .map(|d| (d.start_byte(), d.end_byte()))
                .collect();
            let decorators: Vec<String> = named_children(p)
                .into_iter()
                .filter(|c| c.kind() == "decorator")
                .map(|d| self.decorator_text(d))
                .collect();
            let value_start = field(p, "value").map_or(p.end_byte(), |v| v.start_byte());
            let mut nd = NewDef::new(Label::Field, self.text(pat), span_of(p), span_of(pat));
            nd.signature = self.header(p.start_byte(), value_start, &excl, MAX_SIGNATURE);
            nd.decorators = decorators;
            nd.exported = self.defs[class_id as usize].exported;
            self.add_def(nd, Some(class_id));
        }
    }

    fn class_field(&mut self, m: Node<'_>, sibling_decorators: &[Node<'_>], class_id: u32, depth: usize) {
        let class_ctx = Ctx { parent: Some(class_id), top: false, ambient: false };
        let name_node = field(m, "name").or_else(|| field(m, "property"));
        let Some(name_node) = name_node else { return self.walk_children(m, class_ctx, depth) };
        let value = field(m, "value");
        let fn_value = value.map(unwrap_value).filter(|v| is_function_value(*v));
        let label = if fn_value.is_some() { Label::Method } else { Label::Field };
        let own_decorators: Vec<Node<'_>> = children(m).into_iter().filter(|c| c.kind() == "decorator").collect();
        let first = sibling_decorators.first().copied().unwrap_or(m);
        let mut nd = NewDef::new(label, self.member_name(name_node), span_from(first, m), span_of(name_node));
        let sig_start = children(m)
            .into_iter()
            .find(|c| c.kind() != "decorator" && c.kind() != "comment")
            .map_or(m.start_byte(), |c| c.start_byte());
        nd.signature = match fn_value {
            Some(f) => {
                let head = self.header(sig_start, value.map_or(m.end_byte(), |v| v.start_byte()), &[], MAX_SIGNATURE);
                let tail = self.fn_signature(f);
                match (head, tail) {
                    (Some(h), Some(t)) => Some(truncate_chars(format!("{h} = {t}"), MAX_SIGNATURE)),
                    (h, t) => h.or(t),
                }
            }
            None => self.header(sig_start, value.map_or(m.end_byte(), |v| v.start_byte()), &[], MAX_SIGNATURE),
        };
        nd.doc = self.doc_before(first);
        nd.exported = self.defs[class_id as usize].exported;
        for d in sibling_decorators.iter().chain(own_decorators.iter()) {
            nd.decorators.push(self.decorator_text(*d));
        }
        let id = self.add_def(nd, Some(class_id));
        let inner = Ctx { parent: Some(id), top: false, ambient: false };
        for d in sibling_decorators.iter().chain(own_decorators.iter()) {
            self.walk(*d, inner, depth + 1);
        }
        if let Some(t) = field(m, "type") {
            self.walk(t, inner, depth + 1);
        }
        match (fn_value, value) {
            (Some(f), _) => self.function_body(f, Some(id), inner, depth + 1),
            (None, Some(v)) => {
                self.read(v, inner, depth + 1);
                self.walk(v, inner, depth + 1);
            }
            _ => {}
        }
    }

    // -- variables --------------------------------------------------------------

    fn variables(&mut self, n: Node<'_>, outer: Node<'_>, ctx: Ctx, depth: usize, exported: bool) {
        let kind = children(n).first().map(|c| self.text(*c)).unwrap_or_default();
        let declarators: Vec<Node<'_>> =
            named_children(n).into_iter().filter(|c| c.kind() == "variable_declarator").collect();
        let single = declarators.len() == 1;
        for d in declarators {
            let d_outer = if single { outer } else { d };
            self.declarator(d, d_outer, &kind, ctx, depth + 1, exported);
        }
    }

    fn declarator(&mut self, d: Node<'_>, outer: Node<'_>, kind: &str, ctx: Ctx, depth: usize, exported: bool) {
        if depth >= self.max_depth {
            return;
        }
        let Some(name_node) = field(d, "name") else { return };
        let value = field(d, "value");
        let inner_value = value.map(unwrap_value);

        // CommonJS / dynamic imports bound to a name.
        if let Some(v) = inner_value
            && self.bound_import(name_node, v, d)
        {
            return;
        }

        let ident = name_node.kind() == "identifier";
        if ident && let Some(v) = inner_value {
            if is_function_value(v) {
                let mut nd = NewDef::new(Label::Function, self.text(name_node), span_of(outer), span_of(name_node));
                nd.signature = self.var_fn_signature(kind, d, v);
                nd.doc = self.doc_before(outer);
                nd.exported = exported;
                let id = self.add_def(nd, ctx.parent);
                if let Some(t) = field(d, "type") {
                    self.walk(t, Ctx { parent: Some(id), ..ctx }, depth + 1);
                }
                return self.function_body(v, Some(id), ctx, depth);
            }
            if v.kind() == "class" {
                let name = (self.text(name_node), span_of(name_node));
                return self.class(v, outer, &[], Some(name), ctx, depth, exported);
            }
            if v.kind() == "call_expression" && self.is_function_wrapper(v) {
                let mut nd = NewDef::new(Label::Function, self.text(name_node), span_of(outer), span_of(name_node));
                nd.signature = self.header(d.start_byte(), d.end_byte(), &[], 80).map(|s| format!("{kind} {s}"));
                nd.doc = self.doc_before(outer);
                nd.exported = exported;
                let id = self.add_def(nd, ctx.parent);
                return self.walk(v, Ctx { parent: Some(id), ..ctx }, depth + 1);
            }
        }

        if !ctx.top {
            if let Some(t) = field(d, "type") {
                self.walk(t, ctx, depth + 1);
            }
            self.walk_children(name_node, ctx, depth);
            if let Some(v) = value {
                self.read(v, ctx, depth + 1);
                self.walk(v, ctx, depth + 1);
            }
            return;
        }

        // Module-level variables become definitions.
        let exported = exported || value.is_some_and(|v| self.assigns_module_exports(v));
        let mut names: Vec<Node<'_>> = Vec::new();
        if ident {
            names.push(name_node);
        } else {
            self.pattern_name_nodes(name_node, &mut names);
        }
        let mut first_id = None;
        for nn in names {
            let name = self.text(nn);
            let label = if kind == "const" && is_upper_const(&name) { Label::Constant } else { Label::Variable };
            let mut nd = NewDef::new(label, name, span_of(outer), span_of(nn));
            nd.signature = self.var_signature(kind, d, value);
            nd.doc = self.doc_before(outer);
            nd.exported = exported;
            let id = self.add_def(nd, ctx.parent);
            first_id.get_or_insert(id);
        }
        let vctx = if ident {
            Ctx { parent: first_id.or(ctx.parent), top: false, ambient: false }
        } else {
            Ctx { top: false, ..ctx }
        };
        if let Some(t) = field(d, "type") {
            self.walk(t, vctx, depth + 1);
        }
        let Some(v) = value else { return };
        let iv = unwrap_value(v);
        if ident
            && iv.kind() == "object"
            && let Some(owner) = first_id
        {
            self.object_members(iv, owner, vctx, depth + 1);
            // Wrappers like `as const` / `satisfies T` still carry type refs.
            if iv.id() != v.id() {
                for ch in named_children(v) {
                    if ch.id() != iv.id() {
                        self.walk(ch, vctx, depth + 1);
                    }
                }
            }
        } else {
            self.read(v, vctx, depth + 1);
            self.walk(v, vctx, depth + 1);
        }
    }

    fn pattern_name_nodes<'t>(&self, n: Node<'t>, out: &mut Vec<Node<'t>>) {
        let mut stack = vec![n];
        let mut budget = 10_000usize;
        while let Some(n) = stack.pop() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                break;
            }
            match n.kind() {
                "identifier" | "shorthand_property_identifier_pattern" => out.push(n),
                "pair_pattern" => stack.extend(field(n, "value")),
                "assignment_pattern" | "object_assignment_pattern" => stack.extend(field(n, "left")),
                "object_pattern" | "array_pattern" | "rest_pattern" => {
                    let mut ch = named_children(n);
                    ch.reverse();
                    stack.extend(ch);
                }
                _ => {}
            }
        }
    }

    fn var_fn_signature(&self, kind: &str, d: Node<'_>, f: Node<'_>) -> Option<String> {
        let tail = self.fn_signature(f)?;
        let head = self.header(d.start_byte(), f.start_byte(), &[], MAX_SIGNATURE)?;
        Some(truncate_chars(format!("{kind} {head} = {tail}"), MAX_SIGNATURE))
    }

    fn var_signature(&self, kind: &str, d: Node<'_>, value: Option<Node<'_>>) -> Option<String> {
        let full = self.header(d.start_byte(), d.end_byte(), &[], usize::MAX);
        let s = match (full, value) {
            (Some(f), _) if f.chars().count() <= 80 => f,
            (_, Some(v)) => self.header(d.start_byte(), v.start_byte(), &[], MAX_SIGNATURE)?,
            (f, None) => f?,
        };
        Some(truncate_chars(format!("{kind} {s}"), MAX_SIGNATURE))
    }

    fn is_function_wrapper(&self, call: Node<'_>) -> bool {
        let Some(f) = field(call, "function") else { return false };
        let name = match f.kind() {
            "identifier" => self.text(f),
            "member_expression" => field(f, "property").map(|p| self.text(p)).unwrap_or_default(),
            _ => return false,
        };
        FUNCTION_WRAPPERS.contains(&name.as_str())
            && field(call, "arguments")
                .is_some_and(|a| named_children(a).into_iter().any(|x| is_function_value(unwrap_value(x))))
    }

    /// Methods of an object literal assigned to a module-level variable.
    fn object_members(&mut self, obj: Node<'_>, owner: u32, ctx: Ctx, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        let exported = self.defs[owner as usize].exported;
        for m in named_children(obj) {
            match m.kind() {
                "method_definition" => {
                    let Some(name_node) = field(m, "name") else { continue };
                    let name = self.member_name(name_node);
                    if (has_token(m, "get") || has_token(m, "set"))
                        && !self.accessors.insert((Some(owner), name.clone()))
                    {
                        self.function_body(m, Some(owner), ctx, depth);
                        continue;
                    }
                    let mut nd = NewDef::new(Label::Method, name, span_of(m), span_of(name_node));
                    nd.signature = self.fn_signature(m);
                    nd.doc = self.doc_before(m);
                    nd.exported = exported;
                    let id = self.add_def(nd, Some(owner));
                    self.function_body(m, Some(id), ctx, depth);
                }
                "pair" => {
                    let (Some(k), Some(v)) = (field(m, "key"), field(m, "value")) else { continue };
                    let fv = unwrap_value(v);
                    if is_function_value(fv) {
                        let mut nd = NewDef::new(Label::Method, self.member_name(k), span_of(m), span_of(k));
                        let head = self.header(m.start_byte(), v.start_byte(), &[], MAX_SIGNATURE);
                        nd.signature = match (head, self.fn_signature(fv)) {
                            (Some(h), Some(t)) => Some(truncate_chars(format!("{h} {t}"), MAX_SIGNATURE)),
                            (h, t) => h.or(t),
                        };
                        nd.doc = self.doc_before(m);
                        nd.exported = exported;
                        let id = self.add_def(nd, Some(owner));
                        self.function_body(fv, Some(id), ctx, depth);
                    } else {
                        self.walk(m, ctx, depth + 1);
                    }
                }
                _ => self.walk(m, ctx, depth + 1),
            }
        }
    }

    /// `module.exports = ...` / `exports = module.exports = ...` anywhere in an
    /// assignment chain.
    fn assigns_module_exports(&self, v: Node<'_>) -> bool {
        let mut cur = v;
        for _ in 0..16 {
            if cur.kind() != "assignment_expression" {
                return false;
            }
            if let Some(l) = field(cur, "left") {
                let t = self.text(l);
                if t == "module.exports" || t == "exports" {
                    return true;
                }
            }
            match field(cur, "right") {
                Some(r) => cur = r,
                None => return false,
            }
        }
        false
    }

    // -- CommonJS & prototype assignments ----------------------------------------

    /// Module-level `module.exports = ...`, `exports.x = ...`,
    /// `X.prototype.y = function`, `X.y = function`. Returns false when the
    /// statement is not one of these (the caller walks it normally).
    fn top_assignment(&mut self, a: Node<'_>, ctx: Ctx, depth: usize) -> bool {
        // Flatten `a = b = c = value`.
        let mut lefts = Vec::new();
        let mut cur = a;
        let mut value = None;
        for _ in 0..16 {
            let (Some(l), Some(r)) = (field(cur, "left"), field(cur, "right")) else { return false };
            lefts.push(l);
            if r.kind() == "assignment_expression" {
                cur = r;
            } else {
                value = Some(r);
                break;
            }
        }
        let Some(value) = value else { return false };
        let v = unwrap_value(value);
        let lefts_text: Vec<String> = lefts.iter().map(|l| self.text(*l)).collect();
        let to_module = lefts_text.iter().any(|t| t == "module.exports" || t == "exports");

        if to_module {
            match v.kind() {
                "identifier" => {
                    let t = self.text(v);
                    self.export_names.insert(t.clone());
                    if lefts_text.iter().any(|t| t == "module.exports") {
                        // `import x from "./this"` binds `module.exports` (ESM interop).
                        self.self_reexport(t, "default".into(), span_of(a));
                    }
                    return false;
                }
                "object" => {
                    for m in named_children(v) {
                        match m.kind() {
                            "shorthand_property_identifier" => {
                                let t = self.text(m);
                                self.export_names.insert(t);
                            }
                            "pair" => {
                                if let Some(pv) = field(m, "value")
                                    && pv.kind() == "identifier"
                                {
                                    let t = self.text(pv);
                                    self.export_names.insert(t);
                                }
                            }
                            _ => {}
                        }
                    }
                    return false;
                }
                k if is_function_value(v) || k == "class" => {
                    let name_node = field(v, "name");
                    let (name, name_span) = match name_node {
                        Some(nn) => (self.text(nn), span_of(nn)),
                        None => ("default".to_string(), span_of(lefts[0])),
                    };
                    if k == "class" {
                        self.class(v, a, &[], Some((name, name_span)), ctx, depth, true);
                    } else {
                        let mut nd = NewDef::new(Label::Function, name, span_of(a), name_span);
                        nd.signature = self.fn_signature(v);
                        nd.doc = a.parent().and_then(|p| self.doc_before(p));
                        nd.exported = true;
                        let id = self.add_def(nd, ctx.parent);
                        self.function_body(v, Some(id), ctx, depth);
                    }
                    return true;
                }
                _ => return false,
            }
        }

        // `exports.x = ...`, `module.exports.x = ...`, `X.prototype.y = ...`, `X.y = ...`
        if lefts.len() != 1 {
            return false;
        }
        let l = lefts[0];
        if l.kind() != "member_expression" {
            return false;
        }
        let (Some(obj), Some(prop)) = (field(l, "object"), field(l, "property")) else { return false };
        if prop.kind() != "property_identifier" {
            return false;
        }
        let obj_text = self.text(obj);
        let name = self.text(prop);
        let doc = a.parent().and_then(|p| self.doc_before(p));
        let stmt_span = a.parent().map_or(span_of(a), span_of);
        if obj_text == "exports" || obj_text == "module.exports" {
            if is_function_value(v) {
                let mut nd = NewDef::new(Label::Function, name, stmt_span, span_of(prop));
                nd.signature = self.fn_signature(v).map(|t| format!("{} = {}", self.text(l), t));
                nd.doc = doc;
                nd.exported = true;
                let id = self.add_def(nd, ctx.parent);
                self.function_body(v, Some(id), ctx, depth);
                return true;
            }
            if v.kind() == "identifier" {
                let t = self.text(v);
                self.export_names.insert(t);
            }
            return false;
        }
        if !is_function_value(v) {
            return false;
        }
        // `X.prototype.y` → owner X; `X.y` → owner X.
        let owner_text = match obj.kind() {
            "identifier" => obj_text.clone(),
            "member_expression"
                if field(obj, "property").is_some_and(|p| self.text(p) == "prototype")
                    && field(obj, "object").is_some_and(|o| o.kind() == "identifier") =>
            {
                field(obj, "object").map(|o| self.text(o)).unwrap_or_default()
            }
            _ => return false,
        };
        let owner = self.top_level.get(&owner_text).copied().filter(|_| ctx.parent.is_none());
        let (label, qualified_parent, display_name) = match owner {
            Some(o) => (Label::Method, Some(o), name),
            None => (Label::Function, ctx.parent, name),
        };
        let mut nd = NewDef::new(label, display_name, stmt_span, span_of(prop));
        nd.signature = self.fn_signature(v).map(|t| format!("{} = {}", self.collapsed(l, MAX_SIGNATURE), t));
        nd.doc = doc;
        nd.exported = owner.is_some_and(|o| self.defs[o as usize].exported || self.export_names.contains(&owner_text));
        let id = self.add_def(nd, qualified_parent);
        if owner.is_none() {
            // Keep the receiver visible in the qualified name: `X.y`.
            let qn = self.qualify(ctx.parent, &format!("{owner_text}.{}", self.defs[id as usize].name), false);
            self.defs[id as usize].qualified_name = qn;
        }
        self.function_body(v, Some(id), ctx, depth);
        true
    }

    // -- imports & exports ------------------------------------------------------

    fn string_value(&self, n: Node<'_>) -> Option<String> {
        match n.kind() {
            "string" => {
                let t = self.text(n);
                Some(t.get(1..t.len().saturating_sub(1)).unwrap_or("").to_string())
            }
            "template_string" => {
                if named_children(n).iter().any(|c| c.kind() == "template_substitution") {
                    return None;
                }
                let t = self.text(n);
                Some(t.get(1..t.len().saturating_sub(1)).unwrap_or("").to_string())
            }
            _ => None,
        }
    }

    fn push_import(
        &mut self,
        module: String,
        names: Vec<ImportedName>,
        module_alias: Option<String>,
        type_only: bool,
        reexport: bool,
        span: Span,
    ) {
        self.imports.push(Import { module, relative_level: 0, names, module_alias, type_only, reexport, span });
    }

    /// `export default local` / `export { local as exported }` without
    /// `from`: a re-export of the file's own `local` under `exported`, so the
    /// resolver binds `import exported from "./this"` exactly instead of
    /// guessing the default export. Plain `export { a }` needs no fact (the
    /// name is unchanged; `exported` flags carry it).
    fn self_reexport(&mut self, local: String, exported: String, span: Span) {
        let module = self.self_specifier.clone();
        let names = vec![ImportedName { name: local, alias: Some(exported) }];
        self.push_import(module, names, None, false, true, span);
    }

    fn import_statement(&mut self, n: Node<'_>) {
        let span = span_of(n);
        let stmt_type = has_token(n, "type") || has_token(n, "typeof");
        let mut module = field(n, "source").and_then(|s| self.string_value(s));
        let mut value_names = Vec::new();
        let mut type_names = Vec::new();
        let mut alias = None;
        for ch in named_children(n) {
            match ch.kind() {
                "import_clause" => {
                    for c in named_children(ch) {
                        match c.kind() {
                            "identifier" => {
                                value_names.push(ImportedName { name: "default".into(), alias: Some(self.text(c)) })
                            }
                            "namespace_import" => alias = c.named_child(0).map(|i| self.text(i)),
                            "named_imports" => {
                                for spec in named_children(c) {
                                    if spec.kind() != "import_specifier" {
                                        continue;
                                    }
                                    let Some(nm) = field(spec, "name") else { continue };
                                    let name = self.member_name(nm);
                                    let al = field(spec, "alias").map(|a| self.text(a)).filter(|a| *a != name);
                                    let item = ImportedName { name, alias: al };
                                    if has_token(spec, "type") || has_token(spec, "typeof") {
                                        type_names.push(item);
                                    } else {
                                        value_names.push(item);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "import_require_clause" => {
                    module = field(ch, "source").and_then(|s| self.string_value(s));
                    alias = ch.named_child(0).filter(|c| c.kind() == "identifier").map(|i| self.text(i));
                }
                _ => {}
            }
        }
        let Some(module) = module else { return };
        if stmt_type {
            value_names.append(&mut type_names);
            return self.push_import(module, value_names, alias, true, false, span);
        }
        let all_types = value_names.is_empty() && alias.is_none() && !type_names.is_empty();
        if all_types {
            return self.push_import(module, type_names, None, true, false, span);
        }
        self.push_import(module.clone(), value_names, alias, false, false, span);
        if !type_names.is_empty() {
            self.push_import(module, type_names, None, true, false, span);
        }
    }

    fn export_statement(&mut self, n: Node<'_>, ctx: Ctx, depth: usize) {
        let decorators: Vec<Node<'_>> = children(n).into_iter().filter(|c| c.kind() == "decorator").collect();
        if let Some(source) = field(n, "source") {
            let Some(module) = self.string_value(source) else { return };
            let span = span_of(n);
            let stmt_type = has_token(n, "type");
            let mut value_names = Vec::new();
            let mut type_names = Vec::new();
            let mut alias = None;
            let mut star = has_token(n, "*");
            for ch in named_children(n) {
                match ch.kind() {
                    "export_clause" => {
                        for spec in named_children(ch) {
                            if spec.kind() != "export_specifier" {
                                continue;
                            }
                            let Some(nm) = field(spec, "name") else { continue };
                            let name = self.member_name(nm);
                            let al = field(spec, "alias").map(|a| self.member_name(a)).filter(|a| *a != name);
                            let item = ImportedName { name, alias: al };
                            if has_token(spec, "type") {
                                type_names.push(item);
                            } else {
                                value_names.push(item);
                            }
                        }
                    }
                    "namespace_export" => {
                        star = false;
                        alias = named_children(ch).into_iter().last().map(|i| self.member_name(i));
                    }
                    _ => {}
                }
            }
            if star {
                value_names.push(ImportedName { name: "*".into(), alias: None });
            }
            if stmt_type {
                value_names.append(&mut type_names);
                return self.push_import(module, value_names, alias, true, true, span);
            }
            if value_names.is_empty() && alias.is_none() && !type_names.is_empty() {
                return self.push_import(module, type_names, None, true, true, span);
            }
            self.push_import(module.clone(), value_names, alias, false, true, span);
            if !type_names.is_empty() {
                self.push_import(module, type_names, None, true, true, span);
            }
            return;
        }
        if let Some(decl) = field(n, "declaration") {
            return self.declaration(decl, n, &decorators, ctx, depth + 1, true);
        }
        if let Some(value) = field(n, "value") {
            let v = unwrap_value(value);
            if is_function_value(v) {
                let name_node = field(v, "name");
                let (name, name_span) = match name_node {
                    Some(nn) => (self.text(nn), span_of(nn)),
                    None => {
                        let kw = children(n).into_iter().find(|c| c.kind() == "default").unwrap_or(v);
                        ("default".to_string(), span_of(kw))
                    }
                };
                let mut nd = NewDef::new(Label::Function, name, span_of(n), name_span);
                nd.signature = self.fn_signature(v);
                nd.doc = self.doc_before(n);
                nd.exported = true;
                let id = self.add_def(nd, ctx.parent);
                return self.function_body(v, Some(id), ctx, depth + 1);
            }
            if v.kind() == "class" {
                return self.class(v, n, &decorators, None, ctx, depth + 1, true);
            }
            if v.kind() == "identifier" {
                let t = self.text(v);
                self.export_names.insert(t.clone());
                self.self_reexport(t, "default".into(), span_of(n));
                return;
            }
            self.read(value, ctx, depth + 1);
            return self.walk(value, ctx, depth + 1);
        }
        let mut handled = false;
        for ch in named_children(n) {
            match ch.kind() {
                "export_clause" => {
                    handled = true;
                    for spec in named_children(ch) {
                        if spec.kind() == "export_specifier"
                            && let Some(nm) = field(spec, "name")
                        {
                            let t = self.text(nm);
                            self.export_names.insert(t.clone());
                            if let Some(alias) = field(spec, "alias").map(|a| self.member_name(a))
                                && alias != t
                            {
                                self.self_reexport(t, alias, span_of(n));
                            }
                        }
                    }
                }
                // `export = Foo`
                "identifier" => {
                    handled = true;
                    let t = self.text(ch);
                    self.export_names.insert(t);
                }
                _ => {}
            }
        }
        if !handled {
            self.walk_children(n, ctx, depth);
        }
    }

    /// `x = require("m")`, `{ a } = require("m")`, `x = require("m").y`,
    /// `x = await import("m")`. Returns true if `value` was such an import.
    fn bound_import(&mut self, pattern: Node<'_>, value: Node<'_>, decl: Node<'_>) -> bool {
        let mut v = value;
        if v.kind() == "await_expression"
            && let Some(inner) = v.named_child(0)
        {
            v = unwrap_value(inner);
        }
        let mut member: Option<String> = None;
        if v.kind() == "member_expression"
            && let (Some(o), Some(p)) = (field(v, "object"), field(v, "property"))
            && o.kind() == "call_expression"
        {
            member = Some(self.text(p));
            v = o;
        }
        let Some(module) = self.import_call_module(v) else { return false };
        let span = span_of(decl);
        match (pattern.kind(), member) {
            ("identifier", Some(m)) => {
                let local = self.text(pattern);
                let alias = if local == m { None } else { Some(local) };
                self.push_import(module, vec![ImportedName { name: m, alias }], None, false, false, span);
            }
            ("identifier", None) => {
                let local = self.text(pattern);
                self.push_import(module, Vec::new(), Some(local), false, false, span);
            }
            ("object_pattern", None) => {
                let mut names = Vec::new();
                for p in named_children(pattern) {
                    match p.kind() {
                        "shorthand_property_identifier_pattern" => {
                            names.push(ImportedName { name: self.text(p), alias: None })
                        }
                        "pair_pattern" => {
                            if let (Some(k), Some(val)) = (field(p, "key"), field(p, "value")) {
                                let name = self.member_name(k);
                                let target = if val.kind() == "assignment_pattern" {
                                    field(val, "left").unwrap_or(val)
                                } else {
                                    val
                                };
                                let local = self.text(target);
                                let alias = if local == name { None } else { Some(local) };
                                names.push(ImportedName { name, alias });
                            }
                        }
                        "object_assignment_pattern" => {
                            if let Some(l) = field(p, "left") {
                                names.push(ImportedName { name: self.text(l), alias: None });
                            }
                        }
                        _ => {}
                    }
                }
                self.push_import(module, names, None, false, false, span);
            }
            _ => self.push_import(module, Vec::new(), None, false, false, span),
        }
        true
    }

    /// Module specifier of `require("m")` / `import("m")`.
    fn import_call_module(&self, call: Node<'_>) -> Option<String> {
        if call.kind() != "call_expression" {
            return None;
        }
        let f = field(call, "function")?;
        let is_import = match f.kind() {
            "import" => true,
            "identifier" => self.text(f) == "require" && !self.is_local("require"),
            _ => false,
        };
        if !is_import {
            return None;
        }
        let args = field(call, "arguments")?;
        let first = args.named_child(0)?;
        self.string_value(first)
    }

    // -- references ---------------------------------------------------------------

    fn call(&mut self, n: Node<'_>, ctx: Ctx, depth: usize) {
        if let Some(module) = self.import_call_module(n) {
            self.push_import(module, Vec::new(), None, false, false, span_of(n));
            return;
        }
        let Some(callee) = field(n, "function") else { return self.walk_children(n, ctx, depth) };
        if let Some(test_name) = self.test_call(n, callee) {
            let mut nd = NewDef::new(Label::Test, test_name, span_of(n), span_of(callee));
            nd.is_test = true;
            let first_arg = field(n, "arguments").and_then(|a| a.named_child(0));
            nd.signature = first_arg.map(|a| {
                truncate_chars(
                    format!("{}({})", self.collapsed(callee, 80), self.collapsed(a, MAX_TEST_NAME)),
                    MAX_SIGNATURE,
                )
            });
            let parent = ctx.parent;
            let id = self.add_def(nd, parent);
            return self.walk_children(n, Ctx { parent: Some(id), top: false, ambient: false }, depth);
        }
        if !matches!(callee.kind(), "super" | "import")
            && !self.builtin_root(callee)
            && let Some((name, recv)) = self.callee_parts(callee)
        {
            let typed = match callee.kind() {
                "member_expression" => field(callee, "object").and_then(|o| self.receiver_type(o)),
                _ => None,
            };
            let recv = typed.map(|t| type_receiver(&t)).or(recv);
            self.push_ref(RefKind::Call, name, recv, ctx.parent, span_of(n));
        }
        self.walk_children(n, ctx, depth)
    }

    /// The type of a call's receiver expression when the file says it
    /// (`types.rs`): a local, parameter or module-level binding with a
    /// declared or constructed type, `this.field` of the enclosing class,
    /// or `new T()` / `v as T` itself.
    fn receiver_type(&self, obj: Node<'_>) -> Option<String> {
        match obj.kind() {
            "identifier" => {
                let name = self.text(obj);
                match self.scopes.iter().rev().find(|&&s| s != NO_SCOPE && self.locals.contains(s, &name)) {
                    Some(&owner) => self.types.var(owner, &name).map(str::to_string),
                    // Not a local: a module-level binding.
                    None => self.types.var(self.scopes[0], &name).map(str::to_string),
                }
            }
            "member_expression" => {
                let o = field(obj, "object")?;
                let p = field(obj, "property")?;
                if o.kind() != "this" {
                    return None;
                }
                let class = enclosing_class(obj)?;
                self.types.field(class, &self.text(p)).map(str::to_string)
            }
            "parenthesized_expression" | "non_null_expression" => {
                let inner = obj.named_child(0)?;
                match inner.kind() {
                    "new_expression" | "as_expression" | "type_assertion" => self.constructed_type(inner),
                    _ => self.receiver_type(inner),
                }
            }
            "new_expression" => self.constructed_type(obj),
            _ => None,
        }
    }

    /// `new T(…)` → `T`; `v as T` / `<T>v` → `T` (simple and dotted names).
    fn constructed_type(&self, v: Node<'_>) -> Option<String> {
        let t = match v.kind() {
            "new_expression" => field(v, "constructor")?,
            "as_expression" => *named_children(v).last()?,
            "type_assertion" => named_children(v).into_iter().find(|c| c.kind() == "type_arguments")?.named_child(0)?,
            _ => return None,
        };
        let t = if t.kind() == "generic_type" { field(t, "name")? } else { t };
        let text: String = self.text(t).split_whitespace().collect();
        let simple = text.split('.').all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$'));
        (simple && !self.type_params.contains(&text) && !is_builtin(&text)).then_some(text)
    }

    /// `describe("x", fn)` / `it("y", fn)` / `it.each(t)("z", fn)` / `test.skip(...)`.
    fn test_call(&self, call: Node<'_>, callee: Node<'_>) -> Option<String> {
        let mut root = callee;
        let mut props: Vec<String> = Vec::new();
        for _ in 0..8 {
            match root.kind() {
                "call_expression" => root = field(root, "function")?,
                "member_expression" => {
                    if let Some(p) = field(root, "property") {
                        props.push(self.text(p));
                    }
                    root = field(root, "object")?;
                }
                _ => break,
            }
        }
        if root.kind() != "identifier" {
            return None;
        }
        let r = self.text(root);
        if !DESCRIBE_FNS.contains(&r.as_str()) && !TEST_FNS.contains(&r.as_str()) {
            return None;
        }
        if self.is_local(&r) {
            return None;
        }
        let args = field(call, "arguments")?;
        let list = named_children(args);
        let name = self.string_value(*list.first()?)?;
        let has_fn = list.iter().skip(1).any(|a| is_function_value(unwrap_value(*a)));
        let todo = props.iter().any(|p| p == "todo" || p == "skip");
        if !has_fn && !todo {
            return None;
        }
        let name = truncate_chars(collapse_ws(&name), MAX_TEST_NAME);
        if name.is_empty() {
            return None;
        }
        Some(name)
    }

    fn decorator_ref(&mut self, d: Node<'_>, ctx: Ctx, depth: usize) {
        let Some(expr) = d.named_child(0) else { return };
        let target = if expr.kind() == "call_expression" { field(expr, "function").unwrap_or(expr) } else { expr };
        if let Some((name, recv)) = self.callee_parts(target) {
            self.push_ref(RefKind::Decorator, name, recv, ctx.parent, span_of(d));
        }
        if expr.kind() == "call_expression" {
            if let Some(args) = field(expr, "arguments") {
                self.walk(args, ctx, depth + 1);
            }
            if let Some(ta) = field(expr, "type_arguments") {
                self.walk(ta, ctx, depth + 1);
            }
        }
    }

    fn type_ref(&mut self, n: Node<'_>, ctx: Ctx) {
        let Some((name, recv)) = self.callee_parts(n) else { return };
        if recv.is_none() && (is_builtin(&name) || self.type_params.contains(&name)) {
            return;
        }
        if let Some(r) = &recv {
            let root = r.split('.').next().unwrap_or("");
            if is_builtin(root) {
                return;
            }
        }
        self.push_ref(RefKind::Type, name, recv, ctx.parent, span_of(n));
    }

    fn read_name(&mut self, name: String, receiver: Option<String>, ctx: Ctx, span: Span) {
        if self.refs.len() >= MAX_REFS {
            return;
        }
        let key = (ctx.parent, receiver, name);
        if self.read_seen.contains(&key) {
            return;
        }
        let (_, receiver, name) = key.clone();
        self.read_seen.insert(key);
        self.push_ref(RefKind::Read, name, receiver, ctx.parent, span);
    }

    /// Emits a `Read` for a value expression that names a non-local symbol.
    fn read(&mut self, e: Node<'_>, ctx: Ctx, depth: usize) {
        if depth >= self.max_depth {
            return;
        }
        match e.kind() {
            "identifier" => {
                let name = self.text(e);
                if name == "undefined" || self.is_local(&name) || is_builtin(&name) {
                    return;
                }
                self.read_name(name, None, ctx, span_of(e));
            }
            "member_expression" => {
                // Only plain chains: `a.b.c`, `this.x`, `a?.b`.
                let mut cur = e;
                let mut ok = false;
                for _ in 0..64 {
                    match cur.kind() {
                        "member_expression" => {
                            if !field(cur, "property").is_some_and(|p| {
                                matches!(p.kind(), "property_identifier" | "private_property_identifier")
                            }) {
                                break;
                            }
                            match field(cur, "object") {
                                Some(o) => cur = o,
                                None => break,
                            }
                        }
                        "non_null_expression" => match cur.named_child(0) {
                            Some(o) => cur = o,
                            None => break,
                        },
                        "this" => {
                            ok = true;
                            break;
                        }
                        "identifier" => {
                            let root = self.text(cur);
                            ok = !self.is_local(&root) && !is_builtin(&root);
                            break;
                        }
                        _ => break,
                    }
                }
                if !ok {
                    return;
                }
                let (Some(obj), Some(prop)) = (field(e, "object"), field(e, "property")) else { return };
                let name = self.text(prop);
                let recv = self.collapsed(obj, MAX_RECEIVER);
                self.read_name(name, Some(recv), ctx, span_of(e));
            }
            "parenthesized_expression"
            | "await_expression"
            | "non_null_expression"
            | "as_expression"
            | "satisfies_expression"
            | "spread_element" => {
                if let Some(c) = e.named_child(0) {
                    self.read(c, ctx, depth + 1);
                }
            }
            "ternary_expression" => {
                if let Some(c) = field(e, "consequence") {
                    self.read(c, ctx, depth + 1);
                }
                if let Some(c) = field(e, "alternative") {
                    self.read(c, ctx, depth + 1);
                }
            }
            "binary_expression" => {
                if let Some(c) = field(e, "left") {
                    self.read(c, ctx, depth + 1);
                }
                if let Some(c) = field(e, "right") {
                    self.read(c, ctx, depth + 1);
                }
            }
            _ => {}
        }
    }
}
