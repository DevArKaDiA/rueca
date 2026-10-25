//! Syntax-tree walk producing [`FileFacts`] for one PHP file.
//!
//! A single recursive visitor (depth-limited by `Budget::max_depth`) keeps
//! the current namespace and `use` imports ([`NameCtx`]), the enclosing
//! definition and the enclosing class, so every name is reported fully
//! qualified (see `names.rs`) and every reference is attributed to its
//! innermost enclosing definition. Variable types for receiver typing come
//! from the [`Locals`] table built by the vendored `queries/locals.scm`. See
//! the crate docs for the mapping rules.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::{self, Node};
use rueca_extract::{Budget, ExtractError, count_errors, line_count, span_of, text_of};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::is_builtin_name;
use crate::locals::{self, Locals, single_class};
use crate::names::{NameCtx, NameKind, Resolved, relative_class};
use crate::text::{
    MAX_ONE_LINE, clean_docblock, doc_tag_class, docblock_of, one_line, plain_string_value, truncate_chars,
};

/// Hard cap on references per file.
pub const MAX_REFS: usize = 20_000;

/// Receiver used for `$this` / `self` / `static` inside an anonymous class,
/// whose members are not definitions.
const ANONYMOUS_CLASS: &str = "class@anonymous";

/// Pest functions that declare tests.
const PEST_FNS: &[&str] = &["describe", "it", "test"];

pub(crate) fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time, budget.max_depth)?;

    let mut ex = Extractor {
        source,
        max_depth: budget.max_depth,
        defs: Vec::new(),
        refs: Vec::new(),
        imports: Vec::new(),
        seen_refs: HashSet::new(),
        ctx: NameCtx::new(""),
        enc: None,
        class: None,
        anon_depth: 0,
        locals,
    };
    ex.visit_children(root, 0);

    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
    })
}

/// The class-like declaration being visited.
#[derive(Clone)]
struct ClassCtx {
    def: LocalDefId,
    /// Fully qualified name (no leading `\`).
    fqn: String,
    exported: bool,
    is_test: bool,
    /// Property name (without `$`) → fully qualified class of its declared
    /// type (`self` / `static` / `parent` kept as written).
    prop_types: HashMap<String, String>,
}

struct Extractor<'s> {
    source: &'s [u8],
    max_depth: usize,
    defs: Vec<Definition>,
    refs: Vec<Reference>,
    imports: Vec<Import>,
    /// Dedup keys for `Read` / `Write`: (kind, enclosing, receiver, name).
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    ctx: NameCtx,
    /// Innermost enclosing definition.
    enc: Option<LocalDefId>,
    class: Option<ClassCtx>,
    /// > 0 inside an anonymous class body.
    anon_depth: u32,
    locals: Locals,
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn is_name_node(node: Node<'_>) -> bool {
    matches!(node.kind(), "name" | "qualified_name" | "relative_name")
}

/// Fields that are part of a declaration but not of its signature.
fn is_attribute_or_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "attribute_list" | "comment")
}

/// The `visibility_modifier` of a member, lowercase (`public` when absent).
fn visibility(node: Node<'_>, source: &[u8]) -> String {
    let explicit = node
        .child_by_field_name("visibility")
        .or_else(|| named_children(node).into_iter().find(|c| c.kind() == "visibility_modifier"));
    explicit.map_or_else(|| "public".to_string(), |v| text_of(v, source).trim().to_ascii_lowercase())
}

impl<'s> Extractor<'s> {
    fn text(&self, node: Node<'_>) -> String {
        text_of(node, self.source).into_owned()
    }

    /// Name text with whitespace removed (`App \ Models` → `App\Models`).
    fn name_text(&self, node: Node<'_>) -> String {
        self.text(node).split_whitespace().collect()
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    fn push_ref(&mut self, kind: RefKind, name: String, receiver: Option<String>, span: Span) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        let enclosing = self.enc;
        let dedup = match kind {
            RefKind::Read => Some(0u8),
            RefKind::Write => Some(1u8),
            _ => None,
        };
        if let Some(k) = dedup
            && !self.seen_refs.insert((k, enclosing, receiver.clone(), name.clone()))
        {
            return;
        }
        self.refs.push(Reference { kind, name, receiver, enclosing, span, arity: None });
    }

    fn push_resolved(&mut self, kind: RefKind, resolved: &Resolved, span: Span) {
        let (name, receiver) = resolved.encode();
        self.push_ref(kind, name, Some(receiver), span);
    }

    /// A class name reference (`new X`, type, `instanceof`, `catch`,
    /// attribute, `X::class`). `self` / `static` / `parent` and builtin type
    /// keywords are skipped.
    fn class_ref(&mut self, kind: RefKind, name_node: Node<'_>) {
        let written = self.name_text(name_node);
        if let Some(fqn) = self.ctx.class_fqn(&written) {
            self.push_resolved(kind, &Resolved::Exact(fqn), span_of(name_node));
        }
    }

    fn with_enclosing(&mut self, enc: Option<LocalDefId>, f: impl FnOnce(&mut Self)) {
        let saved = std::mem::replace(&mut self.enc, enc);
        f(self);
        self.enc = saved;
    }

    /// Receiver text for `self` / `static` / `parent` (lowercase), or the
    /// anonymous-class placeholder for `self` / `static` inside one.
    fn relative_receiver(&self, rel: &'static str) -> String {
        if self.anon_depth > 0 && rel != "parent" { ANONYMOUS_CLASS.to_string() } else { rel.to_string() }
    }

    /// Receiver for a class written as `scope` in `X::m()` / `X::C` /
    /// `X::$p`. Returns `None` when the scope is not a class name (an
    /// expression), in which case the caller visits it.
    fn class_receiver(&self, scope: Node<'_>) -> Option<Option<String>> {
        match scope.kind() {
            "relative_scope" => {
                let rel = relative_class(self.text(scope).trim()).unwrap_or("static");
                Some(Some(self.relative_receiver(rel)))
            }
            "name" | "qualified_name" | "relative_name" => {
                let written = self.name_text(scope);
                if let Some(rel) = relative_class(&written) {
                    return Some(Some(self.relative_receiver(rel)));
                }
                Some(self.ctx.class_fqn(&written).map(|f| format!("\\{f}")))
            }
            "variable_name" => Some(Some(self.variable_receiver(scope, "::"))),
            _ => None,
        }
    }

    /// Receiver for a typed class: `\Fqn<suffix>` (`->` marks an instance),
    /// or the relative keyword.
    fn typed_receiver(&self, class_text: &str, suffix: &str) -> Option<String> {
        if let Some(rel) = relative_class(class_text.trim_start_matches('\\')) {
            return Some(self.relative_receiver(rel));
        }
        self.ctx.class_fqn(class_text).map(|f| format!("\\{f}{suffix}"))
    }

    /// Receiver for a `$variable`: `$this`, a typed receiver when every
    /// binding of the variable agrees on a class, else the variable text.
    fn variable_receiver(&self, var: Node<'_>, member_op: &str) -> String {
        let text = self.name_text(var);
        if text == "$this" {
            return if self.anon_depth > 0 { ANONYMOUS_CLASS.to_string() } else { text };
        }
        let suffix = if member_op == "::" { "" } else { "->" };
        self.locals.class_of(var, self.source).and_then(|c| self.typed_receiver(&c, suffix)).unwrap_or(text)
    }

    /// `$this->name` → `Some("name")`.
    fn this_property<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        if !matches!(node.kind(), "member_access_expression" | "nullsafe_member_access_expression") {
            return None;
        }
        let object = node.child_by_field_name("object")?;
        let name = node.child_by_field_name("name")?;
        (object.kind() == "variable_name" && self.text(object) == "$this" && name.kind() == "name").then_some(name)
    }

    /// Receiver text of a member call's object; records `$this->prop` reads
    /// and visits complex objects. Returns the receiver.
    fn object_receiver(&mut self, object: Node<'_>, depth: usize) -> String {
        if object.kind() == "variable_name" {
            return self.variable_receiver(object, "->");
        }
        if let Some(prop) = self.this_property(object) {
            let name = self.text(prop);
            if self.anon_depth == 0 {
                self.push_ref(RefKind::Read, format!("${name}"), Some("$this".into()), span_of(object));
                let typed = self.class.as_ref().and_then(|c| c.prop_types.get(&name)).cloned();
                if let Some(t) = typed {
                    return match relative_class(&t) {
                        Some(rel) => self.relative_receiver(rel),
                        None => format!("\\{t}->"),
                    };
                }
            }
            return truncate_chars(
                &one_line(object, object.start_byte(), object.end_byte(), &[], self.source),
                MAX_ONE_LINE,
            );
        }
        self.visit(object, depth + 1);
        truncate_chars(&one_line(object, object.start_byte(), object.end_byte(), &[], self.source), MAX_ONE_LINE)
    }

    // -----------------------------------------------------------------------
    // Visitor
    // -----------------------------------------------------------------------

    fn visit_children(&mut self, node: Node<'_>, depth: usize) {
        for child in named_children(node) {
            self.visit(child, depth + 1);
        }
    }

    fn visit_field(&mut self, node: Node<'_>, field: &str, depth: usize) {
        if let Some(c) = node.child_by_field_name(field) {
            self.visit(c, depth + 1);
        }
    }

    fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "namespace_definition" => self.visit_namespace(node, depth),
            "namespace_use_declaration" => self.visit_use(node),
            "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration" => {
                self.visit_class_like(node, depth)
            }
            "function_definition" => self.visit_function(node, depth),
            "const_declaration" => self.visit_const(node, depth),
            // Members outside a class only occur in error recovery.
            "method_declaration" => self.visit_method(node, depth),
            "property_declaration" => self.visit_property(node, depth),
            "enum_case" => self.visit_enum_case(node, depth),
            "property_hook" => {
                if let Some(list) = node.child_by_field_name("attributes") {
                    self.visit_attributes(list, None, depth);
                }
                if let Some(params) = node.child_by_field_name("parameters") {
                    self.visit_parameters(params, depth + 1);
                }
                self.visit_field(node, "body", depth);
            }
            "function_call_expression" => self.visit_call(node, depth),
            "member_call_expression" | "nullsafe_member_call_expression" => self.visit_member_call(node, depth),
            "scoped_call_expression" => self.visit_scoped_call(node, depth),
            "object_creation_expression" => self.visit_new(node, depth),
            "class_constant_access_expression" => self.visit_class_constant(node, depth),
            "scoped_property_access_expression" => self.visit_static_property(node, RefKind::Read, depth),
            "member_access_expression" | "nullsafe_member_access_expression" => {
                self.visit_member_access(node, RefKind::Read, depth)
            }
            "assignment_expression" | "reference_assignment_expression" => self.visit_assignment(node, depth),
            "augmented_assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.visit_target(left, depth);
                }
                self.visit_field(node, "right", depth);
            }
            "update_expression" => {
                for child in named_children(node) {
                    self.visit_target(child, depth);
                }
            }
            "binary_expression" => self.visit_binary(node, depth),
            "throw_expression" => self.visit_throw(node, depth),
            "catch_clause" => {
                if let Some(t) = node.child_by_field_name("type") {
                    self.visit_type(t, depth + 1);
                }
                self.visit_field(node, "body", depth);
            }
            "anonymous_function" | "arrow_function" => self.visit_closure(node, depth),
            "include_expression" | "include_once_expression" | "require_expression" | "require_once_expression" => {
                self.visit_include(node, depth)
            }
            "name" | "qualified_name" | "relative_name" => self.constant_ref(node),
            "argument" => {
                let label = node.child_by_field_name("name");
                for child in named_children(node) {
                    if Some(child) != label {
                        self.visit(child, depth + 1);
                    }
                }
            }
            "attribute_list" => self.visit_attributes(node, None, depth),
            "text"
            | "text_interpolation"
            | "php_tag"
            | "php_end_tag"
            | "comment"
            | "string_content"
            | "escape_sequence"
            | "integer"
            | "float"
            | "boolean"
            | "null"
            | "variable_name"
            | "dynamic_variable_name"
            | "primitive_type"
            | "cast_type"
            | "named_type"
            | "optional_type"
            | "union_type"
            | "intersection_type"
            | "disjunctive_normal_form_type"
            | "bottom_type"
            | "relative_scope"
            | "goto_statement"
            | "named_label_statement"
            | "declare_directive"
            | "global_declaration"
            | "use_declaration"
            | "visibility_modifier"
            | "static_modifier"
            | "abstract_modifier"
            | "final_modifier"
            | "readonly_modifier"
            | "var_modifier"
            | "reference_modifier"
            | "heredoc_start"
            | "heredoc_end"
            | "nowdoc_string"
            | "variadic_placeholder" => {}
            _ => self.visit_children(node, depth),
        }
    }

    /// A bare name in expression position: a constant (`MAX`, `\App\MAX`).
    fn constant_ref(&mut self, node: Node<'_>) {
        let written = self.name_text(node);
        let lower = written.to_ascii_lowercase();
        if matches!(lower.as_str(), "true" | "false" | "null")
            || (written.starts_with("__") && written.ends_with("__"))
            || (!written.contains('\\') && is_builtin_name(&written))
        {
            return;
        }
        if let Some(r) = self.ctx.resolve(&written, NameKind::Constant) {
            self.push_resolved(RefKind::Read, &r, span_of(node));
        }
    }

    // -- namespaces and imports ---------------------------------------------

    fn visit_namespace(&mut self, node: Node<'_>, depth: usize) {
        let name = node.child_by_field_name("name").map(|n| self.name_text(n)).unwrap_or_default();
        match node.child_by_field_name("body") {
            Some(body) => {
                let saved = std::mem::replace(&mut self.ctx, NameCtx::new(&name));
                self.visit_children(body, depth + 1);
                self.ctx = saved;
            }
            None => self.ctx = NameCtx::new(&name),
        }
    }

    fn visit_use(&mut self, node: Node<'_>) {
        let kind_of = |n: Node<'_>, this: &Self| match n.child_by_field_name("type").map(|t| this.text(t)) {
            Some(t) if t.eq_ignore_ascii_case("function") => Some(NameKind::Function),
            Some(t) if t.eq_ignore_ascii_case("const") => Some(NameKind::Constant),
            _ => None,
        };
        // `use function a, b;` marks the first clause only; the kind applies to all.
        let first_clause = named_children(node).into_iter().find(|c| c.kind() == "namespace_use_clause");
        let decl_kind = kind_of(node, self)
            .or_else(|| {
                first_clause.filter(|_| node.child_by_field_name("body").is_none()).and_then(|c| kind_of(c, self))
            })
            .unwrap_or(NameKind::Class);
        let (prefix, clauses) = match node.child_by_field_name("body") {
            Some(group) => {
                let prefix = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "namespace_name")
                    .map(|p| self.name_text(p))
                    .unwrap_or_default();
                (Some(prefix), named_children(group))
            }
            None => (None, named_children(node)),
        };
        for clause in clauses {
            if clause.kind() != "namespace_use_clause" {
                continue;
            }
            let kind = kind_of(clause, self).unwrap_or(decl_kind);
            let alias_node = clause.child_by_field_name("alias");
            let Some(target) = named_children(clause).into_iter().find(|c| is_name_node(*c) && Some(*c) != alias_node)
            else {
                continue;
            };
            let written = self.name_text(target);
            let fqn = match &prefix {
                Some(p) if !p.is_empty() => format!("{}\\{}", p.trim_matches('\\'), written.trim_start_matches('\\')),
                _ => written.trim_start_matches('\\').to_string(),
            };
            let short = fqn.rsplit('\\').next().unwrap_or(&fqn).to_string();
            let alias = alias_node.map(|a| self.text(a)).unwrap_or_else(|| short.clone());
            self.ctx.add_use(kind, &fqn, &alias);
            let import = match kind {
                NameKind::Class => Import {
                    module: fqn,
                    relative_level: 0,
                    names: Vec::new(),
                    module_alias: Some(alias),
                    type_only: false,
                    reexport: false,
                    span: span_of(node),
                },
                NameKind::Function | NameKind::Constant => {
                    let ns = fqn.rsplit_once('\\').map_or("", |(n, _)| n).to_string();
                    Import {
                        module: ns,
                        relative_level: 0,
                        names: vec![ImportedName { alias: (alias != short).then_some(alias), name: short }],
                        module_alias: None,
                        type_only: false,
                        reexport: false,
                        span: span_of(node),
                    }
                }
            };
            self.imports.push(import);
        }
    }

    /// `require` / `include` of a static path becomes an [`Import`];
    /// dynamic paths are visited as expressions.
    fn visit_include(&mut self, node: Node<'_>, depth: usize) {
        let Some(arg) = named_children(node).into_iter().find(|c| c.kind() != "comment") else { return };
        match self.static_path(arg, 0) {
            Some(path) if !path.is_empty() => self.imports.push(Import {
                module: path,
                relative_level: 0,
                names: Vec::new(),
                module_alias: None,
                type_only: false,
                reexport: false,
                span: span_of(node),
            }),
            _ => self.visit(arg, depth + 1),
        }
    }

    /// Statically known include path: string literals, `__DIR__`,
    /// `dirname(__DIR__ [, n])`, `dirname(__FILE__)` and
    /// `DIRECTORY_SEPARATOR`, joined with `.`. `__DIR__` stays as a prefix
    /// for [`crate::composer::resolve_path`].
    fn static_path(&self, node: Node<'_>, depth: usize) -> Option<String> {
        if depth > 16 {
            return None;
        }
        match node.kind() {
            "string" | "encapsed_string" => plain_string_value(self.text(node).trim()),
            "parenthesized_expression" => self.static_path(named_children(node).into_iter().next()?, depth + 1),
            "name" => match self.text(node).as_str() {
                "__DIR__" => Some("__DIR__".into()),
                "DIRECTORY_SEPARATOR" => Some("/".into()),
                _ => None,
            },
            "binary_expression" => {
                let op = node.child_by_field_name("operator")?;
                if op.kind() != "." {
                    return None;
                }
                let l = self.static_path(node.child_by_field_name("left")?, depth + 1)?;
                let r = self.static_path(node.child_by_field_name("right")?, depth + 1)?;
                Some(format!("{l}{r}"))
            }
            "function_call_expression" => {
                let f = node.child_by_field_name("function")?;
                if !self.text(f).trim_start_matches('\\').eq_ignore_ascii_case("dirname") {
                    return None;
                }
                let args: Vec<Node<'_>> = named_children(node.child_by_field_name("arguments")?)
                    .into_iter()
                    .filter(|a| a.kind() == "argument")
                    .collect();
                let first = named_children(*args.first()?).into_iter().next()?;
                let base = match self.text(first).as_str() {
                    "__DIR__" => "__DIR__".to_string(),
                    "__FILE__" => return (args.len() == 1).then(|| "__DIR__".to_string()),
                    _ => return None,
                };
                let levels = match args.get(1) {
                    Some(a) => self.text(*a).trim().parse::<usize>().ok()?.min(16),
                    None => 1,
                };
                Some(format!("{base}{}", "/..".repeat(levels)))
            }
            _ => None,
        }
    }

    // -- definitions ---------------------------------------------------------

    fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.defs.push(def);
        (self.defs.len() - 1) as LocalDefId
    }

    /// One-line header of a declaration: from its first keyword (after
    /// attributes and comments) to `end`, without trailing `{` / `;`.
    fn header(&self, decl: Node<'_>, end: usize) -> Option<String> {
        let start = named_or_anon_children(decl)
            .into_iter()
            .find(|c| !is_attribute_or_comment(*c))
            .map_or(decl.start_byte(), |c| c.start_byte());
        // Attributes of the declaration and of its parameters are not part
        // of the signature.
        let params = decl.child_by_field_name("parameters").map(named_children).unwrap_or_default();
        let attrs: Vec<Node<'_>> = named_children(decl)
            .into_iter()
            .chain(params.into_iter().flat_map(named_children))
            .filter(|c| c.kind() == "attribute_list")
            .collect();
        let text = one_line(decl, start, end, &attrs, self.source);
        let text = text.trim().trim_end_matches(['{', ';']).trim_end();
        if text.is_empty() { None } else { Some(truncate_chars(text, MAX_ONE_LINE)) }
    }

    fn body_start(decl: Node<'_>) -> usize {
        decl.child_by_field_name("body").map_or(decl.end_byte(), |b| b.start_byte())
    }

    /// The `/** … */` docblock immediately before `decl` (only whitespace in
    /// between), or right after its attributes.
    fn doc(&self, decl: Node<'_>) -> Option<String> {
        docblock_of(decl, self.source).and_then(|d| clean_docblock(&d))
    }

    fn attributes_of<'t>(&self, decl: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        for list in named_children(decl).into_iter().filter(|c| c.kind() == "attribute_list") {
            for group in named_children(list).into_iter().filter(|c| c.kind() == "attribute_group") {
                out.extend(named_children(group).into_iter().filter(|c| c.kind() == "attribute"));
            }
        }
        out
    }

    fn decorator_texts(&self, attrs: &[Node<'_>]) -> Vec<String> {
        attrs
            .iter()
            .map(|a| truncate_chars(&one_line(*a, a.start_byte(), a.end_byte(), &[], self.source), MAX_ONE_LINE))
            .collect()
    }

    /// Attributes: `Decorator` references when they decorate a definition
    /// (`decorated`), `Type` references otherwise; arguments are visited.
    fn visit_attributes(&mut self, list: Node<'_>, decorated: Option<LocalDefId>, depth: usize) {
        let attrs: Vec<Node<'_>> = named_children(list)
            .into_iter()
            .filter(|c| c.kind() == "attribute_group")
            .flat_map(named_children)
            .filter(|c| c.kind() == "attribute")
            .collect();
        self.visit_attribute_nodes(&attrs, decorated, depth);
    }

    fn visit_attribute_nodes(&mut self, attrs: &[Node<'_>], decorated: Option<LocalDefId>, depth: usize) {
        let enc = decorated.or(self.enc);
        self.with_enclosing(enc, |this| {
            for attr in attrs {
                if let Some(name) = named_children(*attr).into_iter().find(|c| is_name_node(*c)) {
                    let kind = if decorated.is_some() { RefKind::Decorator } else { RefKind::Type };
                    let written = this.name_text(name);
                    if let Some(fqn) = this.ctx.class_fqn(&written) {
                        this.push_resolved(kind, &Resolved::Exact(fqn), span_of(*attr));
                    }
                }
                if let Some(args) = attr.child_by_field_name("parameters") {
                    this.visit(args, depth + 2);
                }
            }
        });
    }

    /// `Type` references for every class in a type (`?A`, `A|B`, `A&B`,
    /// `(A&B)|null`), attributed to the current enclosing definition.
    fn visit_type(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "named_type" => {
                if let Some(n) = named_children(node).into_iter().find(|c| is_name_node(*c)) {
                    self.class_ref(RefKind::Type, n);
                }
            }
            "name" | "qualified_name" | "relative_name" => self.class_ref(RefKind::Type, node),
            "primitive_type" | "bottom_type" | "comment" => {}
            _ => {
                for child in named_children(node) {
                    self.visit_type(child, depth + 1);
                }
            }
        }
    }

    /// Fully qualified names (with leading `\`) of the classes written in a
    /// `base_clause` / `class_interface_clause` / trait `use`.
    fn clause_names(&self, clause: Node<'_>) -> Vec<String> {
        named_children(clause)
            .into_iter()
            .filter(|c| is_name_node(*c))
            .filter_map(|c| self.ctx.class_fqn(&self.name_text(c)))
            .map(|f| format!("\\{f}"))
            .collect()
    }

    fn visit_class_like(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.visit_children(node, depth);
            return;
        };
        let name = self.text(name_node);
        let fqn = self.ctx.declare(&name);
        let label = match node.kind() {
            "interface_declaration" => Label::Interface,
            "trait_declaration" => Label::Trait,
            "enum_declaration" => Label::Enum,
            _ => Label::Class,
        };
        let body = node.child_by_field_name("body");
        let mut bases = Vec::new();
        let mut implements = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "base_clause" => bases.extend(self.clause_names(child)),
                "class_interface_clause" => implements.extend(self.clause_names(child)),
                _ => {}
            }
        }
        // Traits used in the body are recorded after the parent class.
        let members = body.map(named_children).unwrap_or_default();
        for m in &members {
            if m.kind() == "use_declaration" {
                bases.extend(self.clause_names(*m));
            }
        }
        let extends_test_case =
            bases.first().is_some_and(|b| b.rsplit('\\').next().is_some_and(|s| s.ends_with("TestCase")));
        let is_test = label == Label::Class && (name.ends_with("Test") || extends_test_case);
        let attrs = self.attributes_of(node);
        let idx = self.push_def(Definition {
            label,
            name,
            qualified_name: fqn.clone(),
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: self.header(node, Self::body_start(node)),
            doc: self.doc(node),
            decorators: self.decorator_texts(&attrs),
            bases,
            implements,
            exported: true,
            is_test,
            is_declaration: false,
        });
        self.visit_attribute_nodes(&attrs, Some(idx), depth);

        let prop_types = self.property_types(&members);
        let saved_class = self.class.replace(ClassCtx { def: idx, fqn, exported: true, is_test, prop_types });
        let saved_enc = self.enc.replace(idx);
        for member in members {
            self.visit_member(member, depth + 2);
        }
        self.enc = saved_enc;
        self.class = saved_class;
    }

    /// Declared class of each property, from typed property declarations
    /// and promoted constructor parameters.
    fn property_types(&self, members: &[Node<'_>]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let add = |out: &mut HashMap<String, String>, this: &Self, var: Node<'_>, class: Option<String>| {
            let Some(class) = class else { return };
            let resolved = match relative_class(&class) {
                Some(rel) => Some(rel.to_string()),
                None => this.ctx.class_fqn(&class),
            };
            if let (Some(r), Some(n)) = (resolved, var_name_text(var, this.source)) {
                out.entry(n).or_insert(r);
            }
        };
        for m in members {
            match m.kind() {
                "property_declaration" => {
                    let elements = named_children(*m).into_iter().filter(|c| c.kind() == "property_element");
                    match m.child_by_field_name("type") {
                        Some(ty) => {
                            for el in elements {
                                if let Some(var) = el.child_by_field_name("name") {
                                    add(&mut out, self, var, single_class(ty, self.source));
                                }
                            }
                        }
                        // Untyped property: `/** @var Type */`.
                        None => {
                            let doc = docblock_of(*m, self.source);
                            let class = doc.as_deref().and_then(|d| doc_tag_class(d, "@var", None));
                            for el in elements {
                                if let Some(var) = el.child_by_field_name("name") {
                                    add(&mut out, self, var, class.clone());
                                }
                            }
                        }
                    }
                }
                "method_declaration" => {
                    let is_ctor =
                        m.child_by_field_name("name").is_some_and(|n| self.text(n).eq_ignore_ascii_case("__construct"));
                    let Some(params) = m.child_by_field_name("parameters").filter(|_| is_ctor) else { continue };
                    for p in named_children(params).into_iter().filter(|c| c.kind() == "property_promotion_parameter") {
                        if let (Some(var), Some(ty)) = (p.child_by_field_name("name"), p.child_by_field_name("type")) {
                            add(&mut out, self, var, single_class(ty, self.source));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn visit_member(&mut self, member: Node<'_>, depth: usize) {
        match member.kind() {
            "method_declaration" => self.visit_method(member, depth),
            "property_declaration" => self.visit_property(member, depth),
            "const_declaration" => self.visit_const(member, depth),
            "enum_case" => self.visit_enum_case(member, depth),
            "use_declaration" | "comment" => {}
            _ => self.visit(member, depth),
        }
    }

    fn member_exported(&self, member: Node<'_>) -> bool {
        self.class.as_ref().is_some_and(|c| c.exported) && visibility(member, self.source) == "public"
    }

    fn qualify_member(&self, name: &str) -> String {
        match &self.class {
            Some(c) => format!("{}::{name}", c.fqn),
            None => name.to_string(),
        }
    }

    fn visit_method(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.visit_children(node, depth);
            return;
        };
        let name = self.text(name_node);
        let attrs = self.attributes_of(node);
        let doc = self.doc(node);
        let in_test_class = self.class.as_ref().is_some_and(|c| c.is_test);
        let is_test = in_test_class
            && (name.starts_with("test")
                || doc.as_deref().is_some_and(has_test_tag)
                || attrs.iter().any(|a| {
                    named_children(*a)
                        .into_iter()
                        .find(|c| is_name_node(*c))
                        .is_some_and(|n| self.name_text(n).rsplit('\\').next() == Some("Test"))
                }));
        let idx = self.push_def(Definition {
            label: Label::Method,
            qualified_name: self.qualify_member(&name),
            name,
            parent: self.class.as_ref().map(|c| c.def).or(self.enc),
            span: span_of(node),
            name_span: span_of(name_node),
            signature: self.header(node, Self::body_start(node)),
            doc,
            decorators: self.decorator_texts(&attrs),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: self.member_exported(node),
            is_test,
            is_declaration: false,
        });
        self.visit_attribute_nodes(&attrs, Some(idx), depth);
        self.visit_callable(node, idx, depth);
    }

    /// Parameters, return type and body of a function or method `idx`.
    fn visit_callable(&mut self, node: Node<'_>, idx: LocalDefId, depth: usize) {
        self.with_enclosing(Some(idx), |this| {
            if let Some(params) = node.child_by_field_name("parameters") {
                this.visit_parameters(params, depth + 1);
            }
            if let Some(rt) = node.child_by_field_name("return_type") {
                this.visit_type(rt, depth + 1);
            }
            this.visit_field(node, "body", depth);
        });
    }

    /// Parameter types (`Type`), defaults, parameter attributes, and
    /// promoted constructor parameters (`Field`s of the enclosing class).
    fn visit_parameters(&mut self, params: Node<'_>, depth: usize) {
        for param in named_children(params) {
            match param.kind() {
                "simple_parameter" | "variadic_parameter" => {
                    if let Some(list) = param.child_by_field_name("attributes") {
                        self.visit_attributes(list, None, depth + 1);
                    }
                    if let Some(t) = param.child_by_field_name("type") {
                        self.visit_type(t, depth + 1);
                    }
                    self.visit_field(param, "default_value", depth);
                }
                "property_promotion_parameter" => self.visit_promoted(param, depth),
                _ => {}
            }
        }
    }

    fn visit_promoted(&mut self, param: Node<'_>, depth: usize) {
        let var = param.child_by_field_name("name").and_then(|n| {
            if n.kind() == "by_ref" {
                named_children(n).into_iter().find(|c| c.kind() == "variable_name")
            } else {
                Some(n)
            }
        });
        let name = var.and_then(|v| var_name_text(v, self.source));
        let (Some(var), Some(name), Some(class)) = (var, name, self.class.clone()) else {
            self.visit_children(param, depth);
            return;
        };
        let attrs = self.attributes_of(param);
        let field_name = format!("${name}");
        let idx = self.push_def(Definition {
            label: Label::Field,
            qualified_name: self.qualify_member(&field_name),
            name: field_name,
            parent: Some(class.def),
            span: span_of(param),
            name_span: span_of(var),
            signature: self.header(param, param.end_byte()),
            doc: None,
            decorators: self.decorator_texts(&attrs),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: class.exported && visibility(param, self.source) == "public",
            is_test: false,
            is_declaration: false,
        });
        self.visit_attribute_nodes(&attrs, Some(idx), depth);
        self.with_enclosing(Some(idx), |this| {
            if let Some(t) = param.child_by_field_name("type") {
                this.visit_type(t, depth + 1);
            }
        });
        self.visit_field(param, "default_value", depth);
        for hooks in named_children(param).into_iter().filter(|c| c.kind() == "property_hook_list") {
            self.with_enclosing(Some(idx), |this| this.visit_children(hooks, depth + 1));
        }
    }

    fn visit_property(&mut self, node: Node<'_>, depth: usize) {
        let attrs = self.attributes_of(node);
        let signature = self.header(node, node.end_byte());
        let doc = self.doc(node);
        let decorators = self.decorator_texts(&attrs);
        let exported = self.member_exported(node);
        let ty = node.child_by_field_name("type");
        let mut first = None;
        for el in named_children(node).into_iter().filter(|c| c.kind() == "property_element") {
            let Some(var) = el.child_by_field_name("name") else { continue };
            let Some(name) = var_name_text(var, self.source) else { continue };
            let field_name = format!("${name}");
            let idx = self.push_def(Definition {
                label: Label::Field,
                qualified_name: self.qualify_member(&field_name),
                name: field_name,
                parent: self.class.as_ref().map(|c| c.def).or(self.enc),
                span: span_of(node),
                name_span: span_of(var),
                signature: signature.clone(),
                doc: doc.clone(),
                decorators: decorators.clone(),
                bases: Vec::new(),
                implements: Vec::new(),
                exported,
                is_test: false,
                is_declaration: false,
            });
            first = first.or(Some(idx));
            self.with_enclosing(Some(idx), |this| {
                if let Some(t) = ty {
                    this.visit_type(t, depth + 1);
                }
                this.visit_field(el, "default_value", depth);
            });
        }
        if let Some(idx) = first {
            self.visit_attribute_nodes(&attrs, Some(idx), depth);
            for hooks in named_children(node).into_iter().filter(|c| c.kind() == "property_hook_list") {
                self.with_enclosing(Some(idx), |this| this.visit_children(hooks, depth + 1));
            }
        }
    }

    /// Class constants (`Class::NAME`) and namespace constants (`App\NAME`).
    fn visit_const(&mut self, node: Node<'_>, depth: usize) {
        let attrs = self.attributes_of(node);
        let signature = self.header(node, node.end_byte());
        let doc = self.doc(node);
        let in_class = self.class.is_some() && self.class.as_ref().map(|c| c.def) == self.enc;
        let exported = if in_class { self.member_exported(node) } else { true };
        let mut first = None;
        for el in named_children(node).into_iter().filter(|c| c.kind() == "const_element") {
            let parts = named_children(el);
            let Some(name_node) = parts.iter().copied().find(|c| c.kind() == "name") else { continue };
            let name = self.text(name_node);
            let qualified_name = if in_class { self.qualify_member(&name) } else { self.ctx.declare(&name) };
            let idx = self.push_def(Definition {
                label: Label::Constant,
                name,
                qualified_name,
                parent: self.enc,
                span: span_of(node),
                name_span: span_of(name_node),
                signature: signature.clone(),
                doc: doc.clone(),
                decorators: self.decorator_texts(&attrs),
                bases: Vec::new(),
                implements: Vec::new(),
                exported,
                is_test: false,
                is_declaration: false,
            });
            first = first.or(Some(idx));
            self.with_enclosing(Some(idx), |this| {
                if let Some(t) = node.child_by_field_name("type") {
                    this.visit_type(t, depth + 1);
                }
                for value in parts.iter().copied().filter(|c| *c != name_node) {
                    this.visit(value, depth + 2);
                }
            });
        }
        if let Some(idx) = first {
            self.visit_attribute_nodes(&attrs, Some(idx), depth);
        }
    }

    fn visit_enum_case(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = self.text(name_node);
        let attrs = self.attributes_of(node);
        let idx = self.push_def(Definition {
            label: Label::Constant,
            qualified_name: self.qualify_member(&name),
            name,
            parent: self.class.as_ref().map(|c| c.def).or(self.enc),
            span: span_of(node),
            name_span: span_of(name_node),
            signature: self.header(node, node.end_byte()),
            doc: self.doc(node),
            decorators: self.decorator_texts(&attrs),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: self.class.as_ref().is_none_or(|c| c.exported),
            is_test: false,
            is_declaration: false,
        });
        self.visit_attribute_nodes(&attrs, Some(idx), depth);
        self.with_enclosing(Some(idx), |this| this.visit_field(node, "value", depth));
    }

    fn visit_function(&mut self, node: Node<'_>, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else {
            self.visit_children(node, depth);
            return;
        };
        let name = self.text(name_node);
        let attrs = self.attributes_of(node);
        let idx = self.push_def(Definition {
            label: Label::Function,
            qualified_name: self.ctx.declare(&name),
            name,
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: self.header(node, Self::body_start(node)),
            doc: self.doc(node),
            decorators: self.decorator_texts(&attrs),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        });
        self.visit_attribute_nodes(&attrs, Some(idx), depth);
        // A function declared inside a method is not a member of the class.
        let saved = self.class.take();
        self.visit_callable(node, idx, depth);
        self.class = saved;
    }

    fn visit_closure(&mut self, node: Node<'_>, depth: usize) {
        if let Some(list) = node.child_by_field_name("attributes") {
            self.visit_attributes(list, None, depth);
        }
        if let Some(params) = node.child_by_field_name("parameters") {
            self.visit_parameters(params, depth + 1);
        }
        if let Some(rt) = node.child_by_field_name("return_type") {
            self.visit_type(rt, depth + 1);
        }
        self.visit_field(node, "body", depth);
    }

    // -- calls ---------------------------------------------------------------

    fn arguments<'t>(call: Node<'t>) -> Vec<Node<'t>> {
        call.child_by_field_name("arguments")
            .map(|a| named_children(a).into_iter().filter(|c| c.kind() == "argument").collect())
            .unwrap_or_default()
    }

    /// Value expression of an argument (skipping a named-argument label).
    fn argument_value<'t>(arg: Node<'t>) -> Option<Node<'t>> {
        let label = arg.child_by_field_name("name");
        named_children(arg).into_iter().find(|c| Some(*c) != label && c.kind() != "comment")
    }

    fn visit_call(&mut self, node: Node<'_>, depth: usize) {
        let Some(func) = node.child_by_field_name("function") else {
            self.visit_children(node, depth);
            return;
        };
        if is_name_node(func) {
            let written = self.name_text(func);
            let bare = written.trim_start_matches('\\');
            if func.kind() == "name" && PEST_FNS.contains(&bare) && self.pest_test(node, depth) {
                return;
            }
            if bare.eq_ignore_ascii_case("define") {
                self.define_constant(node);
            }
            if let Some(r) = self.ctx.resolve(&written, NameKind::Function) {
                self.push_resolved(RefKind::Call, &r, span_of(func));
            }
        } else {
            self.visit(func, depth + 1);
        }
        self.visit_field(node, "arguments", depth);
    }

    /// `define('NAME', value)` with a literal name declares a global constant.
    fn define_constant(&mut self, call: Node<'_>) {
        let args = Self::arguments(call);
        let Some(first) = args.first().and_then(|a| Self::argument_value(*a)) else { return };
        if !matches!(first.kind(), "string" | "encapsed_string") {
            return;
        }
        let Some(value) = plain_string_value(self.text(first).trim()) else { return };
        let fqn = value.trim_start_matches('\\').to_string();
        let name = fqn.rsplit('\\').next().unwrap_or(&fqn).to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return;
        }
        self.push_def(Definition {
            label: Label::Constant,
            name,
            qualified_name: fqn,
            parent: self.enc,
            span: span_of(call),
            name_span: span_of(first),
            signature: Some(truncate_chars(
                &one_line(call, call.start_byte(), call.end_byte(), &[], self.source),
                MAX_ONE_LINE,
            )),
            doc: call.parent().and_then(|stmt| self.doc(stmt)),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: false,
            is_declaration: false,
        });
    }

    /// Pest `it('…', fn)` / `test('…', fn)` / `describe('…', fn)`: a `Test`
    /// definition whose qualified name is the describe chain. Returns false
    /// when the call does not have that shape.
    fn pest_test(&mut self, node: Node<'_>, depth: usize) -> bool {
        let args = Self::arguments(node);
        let Some(title_node) = args.first().and_then(|a| Self::argument_value(*a)) else { return false };
        if !matches!(title_node.kind(), "string" | "encapsed_string") {
            return false;
        }
        let Some(title) = plain_string_value(self.text(title_node).trim()) else { return false };
        let closure = args
            .iter()
            .skip(1)
            .filter_map(|a| Self::argument_value(*a))
            .find(|v| matches!(v.kind(), "anonymous_function" | "arrow_function"));
        let Some(closure) = closure else { return false };
        let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return false;
        }
        let parent_test = self.enc.filter(|&e| self.defs[e as usize].label == Label::Test);
        let qualified_name = match parent_test {
            Some(p) => format!("{} > {title}", self.defs[p as usize].qualified_name),
            None => title.clone(),
        };
        let sig_end = closure.child_by_field_name("body").map_or(closure.end_byte(), |b| b.start_byte());
        let signature = truncate_chars(
            one_line(node, node.start_byte(), sig_end, &[], self.source).trim_end_matches(['{', ' ']).trim_end(),
            MAX_ONE_LINE,
        );
        let idx = self.push_def(Definition {
            label: Label::Test,
            name: title,
            qualified_name,
            parent: self.enc,
            span: span_of(node),
            name_span: span_of(title_node),
            signature: Some(signature),
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: false,
            is_test: true,
            is_declaration: false,
        });
        self.with_enclosing(Some(idx), |this| {
            for arg in args.iter().skip(1) {
                this.visit(*arg, depth + 2);
            }
        });
        true
    }

    fn visit_member_call(&mut self, node: Node<'_>, depth: usize) {
        let object = node.child_by_field_name("object");
        let name = node.child_by_field_name("name");
        let receiver = object.map(|o| self.object_receiver(o, depth));
        match name {
            Some(n) if n.kind() == "name" => {
                let method = self.text(n);
                self.push_ref(RefKind::Call, method, receiver, span_of(n));
            }
            Some(n) => self.visit(n, depth + 1),
            None => {}
        }
        self.visit_field(node, "arguments", depth);
    }

    fn visit_scoped_call(&mut self, node: Node<'_>, depth: usize) {
        let receiver = match node.child_by_field_name("scope") {
            Some(scope) => match self.class_receiver(scope) {
                Some(r) => r,
                None => {
                    self.visit(scope, depth + 1);
                    Some(truncate_chars(
                        &one_line(scope, scope.start_byte(), scope.end_byte(), &[], self.source),
                        MAX_ONE_LINE,
                    ))
                }
            },
            None => None,
        };
        match node.child_by_field_name("name") {
            Some(n) if n.kind() == "name" => {
                if let Some(receiver) = receiver {
                    let method = self.text(n);
                    self.push_ref(RefKind::Call, method, Some(receiver), span_of(n));
                }
            }
            Some(n) => self.visit(n, depth + 1),
            None => {}
        }
        self.visit_field(node, "arguments", depth);
    }

    fn visit_new(&mut self, node: Node<'_>, depth: usize) {
        for child in named_children(node) {
            match child.kind() {
                "name" | "qualified_name" | "relative_name" => {
                    let written = self.name_text(child);
                    match relative_class(&written) {
                        Some(rel) => {
                            let receiver = self.relative_receiver(rel);
                            self.push_ref(RefKind::New, "__construct".into(), Some(receiver), span_of(child));
                        }
                        None => self.class_ref(RefKind::New, child),
                    }
                }
                "anonymous_class" => self.visit_anonymous_class(child, depth + 1),
                "comment" => {}
                _ => self.visit(child, depth + 1),
            }
        }
    }

    /// `new class(...) extends A implements B { … }`: bases are `Type`
    /// references of the enclosing definition; members are not definitions
    /// and their code is attributed to the enclosing definition.
    fn visit_anonymous_class(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for child in named_children(node) {
            match child.kind() {
                "base_clause" | "class_interface_clause" => {
                    for n in named_children(child).into_iter().filter(|c| is_name_node(*c)) {
                        self.class_ref(RefKind::Type, n);
                    }
                }
                "attribute_list" => self.visit_attributes(child, None, depth),
                "arguments" => self.visit(child, depth + 1),
                "declaration_list" => {
                    self.anon_depth += 1;
                    let saved = self.class.take();
                    for member in named_children(child) {
                        match member.kind() {
                            "use_declaration" => {
                                for n in named_children(member).into_iter().filter(|c| is_name_node(*c)) {
                                    self.class_ref(RefKind::Type, n);
                                }
                            }
                            "method_declaration" => {
                                if let Some(list) = member.child_by_field_name("attributes") {
                                    self.visit_attributes(list, None, depth + 1);
                                }
                                if let Some(params) = member.child_by_field_name("parameters") {
                                    self.visit_parameters(params, depth + 2);
                                }
                                if let Some(rt) = member.child_by_field_name("return_type") {
                                    self.visit_type(rt, depth + 2);
                                }
                                self.visit_field(member, "body", depth + 1);
                            }
                            "property_declaration" => {
                                if let Some(t) = member.child_by_field_name("type") {
                                    self.visit_type(t, depth + 2);
                                }
                                for el in named_children(member).into_iter().filter(|c| c.kind() == "property_element")
                                {
                                    self.visit_field(el, "default_value", depth + 2);
                                }
                            }
                            "const_declaration" => {
                                for el in named_children(member).into_iter().filter(|c| c.kind() == "const_element") {
                                    for v in named_children(el).into_iter().skip(1) {
                                        self.visit(v, depth + 3);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    self.class = saved;
                    self.anon_depth -= 1;
                }
                _ => {}
            }
        }
    }

    /// `X::CONST`, `X::class`, `self::CASE`.
    fn visit_class_constant(&mut self, node: Node<'_>, depth: usize) {
        let parts: Vec<Node<'_>> = named_children(node).into_iter().filter(|c| c.kind() != "comment").collect();
        let (Some(&scope), Some(&member)) = (parts.first(), parts.last()) else { return };
        if parts.len() < 2 || member.kind() != "name" {
            self.visit_children(node, depth);
            return;
        }
        let member_name = self.text(member);
        if member_name.eq_ignore_ascii_case("class") {
            if is_name_node(scope) && relative_class(&self.name_text(scope)).is_none() {
                if let Some(fqn) = self.ctx.class_fqn(&self.name_text(scope)) {
                    self.push_resolved(RefKind::Read, &Resolved::Exact(fqn), span_of(node));
                }
            } else if !matches!(scope.kind(), "relative_scope" | "name" | "qualified_name" | "relative_name") {
                self.visit(scope, depth + 1);
            }
            return;
        }
        match self.class_receiver(scope) {
            Some(Some(receiver)) if !receiver.starts_with('$') => {
                self.push_ref(RefKind::Read, member_name, Some(receiver), span_of(node))
            }
            Some(_) => {}
            None => self.visit(scope, depth + 1),
        }
    }

    /// `X::$prop` / `self::$prop` as a read or a write target.
    fn visit_static_property(&mut self, node: Node<'_>, kind: RefKind, depth: usize) {
        let scope = node.child_by_field_name("scope");
        let name = node.child_by_field_name("name");
        let prop = name.filter(|n| n.kind() == "variable_name").map(|n| self.name_text(n));
        let receiver = match scope.map(|s| (s, self.class_receiver(s))) {
            Some((_, Some(r))) => r,
            Some((s, None)) => {
                self.visit(s, depth + 1);
                None
            }
            None => None,
        };
        match (receiver, prop) {
            (Some(r), Some(p)) if !r.starts_with('$') => self.push_ref(kind, p, Some(r), span_of(node)),
            _ => {}
        }
        if let Some(n) = name.filter(|n| n.kind() != "variable_name") {
            self.visit(n, depth + 1);
        }
    }

    /// `$this->prop` as a read or write; other objects are visited.
    fn visit_member_access(&mut self, node: Node<'_>, kind: RefKind, depth: usize) {
        if let Some(prop) = self.this_property(node) {
            if self.anon_depth == 0 {
                let name = format!("${}", self.text(prop));
                self.push_ref(kind, name, Some("$this".into()), span_of(node));
            }
            return;
        }
        if let Some(object) = node.child_by_field_name("object")
            && object.kind() != "variable_name"
        {
            self.visit(object, depth + 1);
        }
        if let Some(name) = node.child_by_field_name("name")
            && name.kind() != "name"
        {
            self.visit(name, depth + 1);
        }
    }

    /// Assignment target: writes to `$this->x` / `X::$y`; other targets are
    /// visited (subscripts read their base).
    fn visit_target(&mut self, target: Node<'_>, depth: usize) {
        match target.kind() {
            "member_access_expression" | "nullsafe_member_access_expression" => {
                self.visit_member_access(target, RefKind::Write, depth)
            }
            "scoped_property_access_expression" => self.visit_static_property(target, RefKind::Write, depth),
            "variable_name" | "list_literal" | "by_ref" => {
                for child in named_children(target) {
                    if !matches!(child.kind(), "variable_name" | "name" | "by_ref" | "list_literal") {
                        self.visit(child, depth + 1);
                    }
                }
            }
            _ => self.visit(target, depth + 1),
        }
    }

    fn visit_assignment(&mut self, node: Node<'_>, depth: usize) {
        if let Some(left) = node.child_by_field_name("left") {
            self.visit_target(left, depth);
        }
        self.visit_field(node, "right", depth);
    }

    fn visit_binary(&mut self, node: Node<'_>, depth: usize) {
        let is_instanceof = node.child_by_field_name("operator").is_some_and(|o| o.kind() == "instanceof");
        self.visit_field(node, "left", depth);
        let Some(right) = node.child_by_field_name("right") else { return };
        if is_instanceof && is_name_node(right) {
            self.class_ref(RefKind::Type, right);
        } else {
            self.visit(right, depth + 1);
        }
    }

    fn visit_throw(&mut self, node: Node<'_>, depth: usize) {
        for child in named_children(node) {
            if child.kind() == "object_creation_expression"
                && let Some(class) = named_children(child).into_iter().find(|c| is_name_node(*c))
                && relative_class(&self.name_text(class)).is_none()
            {
                self.class_ref(RefKind::Throw, class);
            }
            self.visit(child, depth + 1);
        }
    }
}

/// Children including anonymous tokens (`final`, `class`, `function`).
fn named_or_anon_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// `$name` → `name` for a `variable_name` node.
fn var_name_text(var: Node<'_>, source: &[u8]) -> Option<String> {
    if var.kind() != "variable_name" {
        return None;
    }
    let name = named_children(var).into_iter().find(|c| c.kind() == "name")?;
    Some(text_of(name, source).into_owned())
}

/// `@test` as a docblock tag.
fn has_test_tag(doc: &str) -> bool {
    doc.split(|c: char| c.is_whitespace()).any(|w| w == "@test")
}
