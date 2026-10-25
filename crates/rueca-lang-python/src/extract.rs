//! Syntax-tree walk producing [`FileFacts`] for one Python file.
//!
//! A single recursive visitor (depth-limited by `Budget::max_depth`) keeps a
//! definition stack so it can attribute every reference to its innermost
//! enclosing definition. Whether an identifier is a local binding comes from
//! the [`Locals`] table built by the vendored `queries/locals.scm` query (see
//! `locals.rs`). See the crate docs for the mapping rules.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::{self, Node};
use rueca_extract::{Budget, ExtractError, count_errors, first_line, line_count, span_of, text_of, type_receiver};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::is_builtin_name;
use crate::locals::{self, Locals};
use crate::text::{
    MAX_DOC, MAX_ONE_LINE, cleandoc, is_dotted_identifier, is_upper_case, one_line, string_literal_value,
    truncate_chars,
};
use crate::types::TypeTable;

/// Hard cap on references per file.
pub const MAX_REFS: usize = 20_000;

const SELF_RECEIVERS: &[&str] = &["self", "cls"];

pub(crate) fn extract(source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let locals = locals::build(&tree, source, budget.max_parse_time, budget.max_depth)?;

    let mut ex = Extractor {
        source,
        max_depth: budget.max_depth,
        defs: Vec::new(),
        refs: Vec::new(),
        imports: Vec::new(),
        scopes: Vec::new(),
        seen_refs: HashSet::new(),
        def_names: HashSet::new(),
        members: HashMap::new(),
        all: module_all(root, source),
        type_checking: 0,
        enc_override: None,
        locals,
        env: crate::bridge::EnvApi::scan(root, source),
        types: TypeTable::build(root, source, budget.max_depth),
    };
    ex.scopes.push(Scope { kind: ScopeKind::Module, def: None, self_name: None, init_class: None, node: root.id() });
    ex.visit_children(root, 0);

    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Module,
    Class,
    Function,
    Lambda,
}

struct Scope {
    kind: ScopeKind,
    /// Definition owning this scope (`None` for the module; a lambda inherits
    /// its parent's).
    def: Option<LocalDefId>,
    /// First parameter name of a method (`self`).
    self_name: Option<String>,
    /// For `__init__` methods: the class whose fields `self.x = …` creates.
    init_class: Option<LocalDefId>,
    /// The scope's node (module, class, function or lambda), the key of
    /// its bindings in [`TypeTable`].
    node: usize,
}

struct Extractor<'s> {
    source: &'s [u8],
    max_depth: usize,
    defs: Vec<Definition>,
    refs: Vec<Reference>,
    imports: Vec<Import>,
    scopes: Vec<Scope>,
    /// Dedup keys for `Read` / `Write`: (kind, enclosing, receiver, name).
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    /// Qualified names already defined (variables/fields are first-wins).
    def_names: HashSet<String>,
    /// Field names per class (dedup of class-body and `self.x` fields).
    members: HashMap<LocalDefId, HashSet<String>>,
    /// Literal `__all__`, if present.
    all: Option<HashSet<String>>,
    /// > 0 inside `if TYPE_CHECKING:`.
    type_checking: u32,
    /// When set, the enclosing definition for references (decorators, bases,
    /// annotations of a variable).
    enc_override: Option<Option<LocalDefId>>,
    /// Which identifiers are local bindings (from `queries/locals.scm`).
    locals: Locals,
    /// Env var APIs bound by the file's imports (the bridge, `bridge.rs`).
    env: crate::bridge::EnvApi,
    /// Annotated and constructed types of bindings (`types.rs`), for
    /// `$type:` receivers.
    types: TypeTable,
}

/// Identifier nodes of a pure attribute chain `a.b.c` (root first) and the
/// innermost attribute node (`a.b`), or `None` if the expression is not a
/// chain of identifiers.
fn chain<'t>(node: Node<'t>) -> Option<(Vec<Node<'t>>, Node<'t>)> {
    let mut idents = Vec::new();
    let mut cur = node;
    let mut innermost = node;
    loop {
        match cur.kind() {
            "identifier" => {
                idents.push(cur);
                break;
            }
            "attribute" => {
                idents.push(cur.child_by_field_name("attribute")?);
                innermost = cur;
                cur = cur.child_by_field_name("object")?;
            }
            _ => return None,
        }
    }
    idents.reverse();
    Some((idents, innermost))
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Literal `__all__ = [...]` / `(...)` (plus `+=` extensions) at module level.
fn module_all(root: Node<'_>, source: &[u8]) -> Option<HashSet<String>> {
    let mut all: Option<HashSet<String>> = None;
    let mut invalid = false;
    for stmt in named_children(root) {
        if stmt.kind() != "expression_statement" {
            continue;
        }
        for expr in named_children(stmt) {
            let augmented = match expr.kind() {
                "assignment" => false,
                "augmented_assignment" => true,
                _ => continue,
            };
            let Some(left) = expr.child_by_field_name("left") else { continue };
            if left.kind() != "identifier" || text_of(left, source) != "__all__" {
                continue;
            }
            let Some(right) = expr.child_by_field_name("right") else { continue };
            let strings = literal_strings(right, source);
            match (strings, augmented) {
                (Some(s), false) => all = Some(s),
                (Some(s), true) => {
                    if let Some(a) = all.as_mut() {
                        a.extend(s);
                    }
                }
                (None, _) => invalid = true,
            }
        }
    }
    if invalid { None } else { all }
}

fn literal_strings(node: Node<'_>, source: &[u8]) -> Option<HashSet<String>> {
    if !matches!(node.kind(), "list" | "tuple") {
        return None;
    }
    let mut out = HashSet::new();
    for item in named_children(node) {
        match item.kind() {
            "comment" => {}
            "string" => {
                out.insert(string_literal_value(&text_of(item, source))?.to_string());
            }
            _ => return None,
        };
    }
    Some(out)
}

impl<'s> Extractor<'s> {
    fn text(&self, node: Node<'_>) -> String {
        text_of(node, self.source).into_owned()
    }

    fn enclosing(&self) -> Option<LocalDefId> {
        match self.enc_override {
            Some(e) => e,
            None => self.scopes.last().and_then(|s| s.def),
        }
    }

    fn scope_kind(&self) -> ScopeKind {
        self.scopes.last().map(|s| s.kind).unwrap_or(ScopeKind::Module)
    }

    /// Whether an identifier node names a local binding (see `locals.rs`).
    fn is_local(&self, ident: Node<'_>) -> bool {
        self.locals.is_local(ident)
    }

    fn is_exported(&self, name: &str) -> bool {
        match &self.all {
            Some(all) => all.contains(name),
            None => !name.starts_with('_'),
        }
    }

    fn qualify(&self, parent: Option<LocalDefId>, name: &str) -> String {
        match parent.and_then(|p| self.defs.get(p as usize)) {
            Some(p) => format!("{}.{}", p.qualified_name, name),
            None => name.to_string(),
        }
    }

    fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.def_names.insert(def.qualified_name.clone());
        self.defs.push(def);
        (self.defs.len() - 1) as LocalDefId
    }

    fn push_ref(&mut self, kind: RefKind, name: String, receiver: Option<String>, span: Span) {
        if self.refs.len() >= MAX_REFS || name.is_empty() {
            return;
        }
        let enclosing = self.enclosing();
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

    /// Receiver text of an object expression: dotted for chains, otherwise
    /// the whitespace-collapsed source text truncated to 200 chars.
    fn receiver_text(&self, obj: Node<'_>) -> String {
        match chain(obj) {
            Some((idents, _)) => idents.iter().map(|n| self.text(*n)).collect::<Vec<_>>().join("."),
            None => truncate_chars(&one_line(obj, obj.start_byte(), obj.end_byte(), self.source), MAX_ONE_LINE),
        }
    }

    /// The type of a call's receiver when the file says it (`types.rs`): a
    /// local or module-level name with an annotation or a constructed /
    /// returned type, `self.attr` of the enclosing class, or a call of a
    /// method / function of this file with a return annotation
    /// (`self.make()`, `make()`, `Factory.create()`).
    fn receiver_type(&self, obj: Node<'_>) -> Option<String> {
        match obj.kind() {
            "identifier" => {
                let name = self.text(obj);
                if SELF_RECEIVERS.contains(&name.as_str()) || self.method_self(&name).is_some() {
                    return None;
                }
                if !self.is_local(obj) {
                    let module = self.scopes.first()?.node;
                    return self.types.var(module, &name).map(str::to_string);
                }
                let owner = self
                    .scopes
                    .iter()
                    .rev()
                    .filter(|s| matches!(s.kind, ScopeKind::Function | ScopeKind::Lambda))
                    .find(|s| self.types.binds(s.node, &name))?;
                self.types.var(owner.node, &name).map(str::to_string)
            }
            "attribute" => {
                let o = obj.child_by_field_name("object")?;
                let a = obj.child_by_field_name("attribute")?;
                if o.kind() != "identifier" {
                    return None;
                }
                let class = self.method_self(&self.text(o))?;
                self.types.attr(class, &self.text(a)).map(str::to_string)
            }
            "call" => {
                let f = obj.child_by_field_name("function")?;
                let (idents, _) = chain(f)?;
                let segs: Vec<String> = idents.iter().map(|n| self.text(*n)).collect();
                if let [s, m] = &segs[..]
                    && let Some(class) = self.method_self(s)
                {
                    return self.types.method_return(class, m).map(str::to_string);
                }
                if self.is_local(idents[0]) {
                    return None;
                }
                let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
                if let Some(r) = self.types.call_return(&segs) {
                    return Some(r.to_string());
                }
                // `Session().request()`: a call of a class-like name.
                let last = segs.last()?;
                crate::types::is_class_like(last).then(|| segs.join("."))
            }
            _ => None,
        }
    }

    /// When `name` is the `self` parameter of the enclosing method (also
    /// from a nested function or lambda), the class node of that method.
    fn method_self(&self, name: &str) -> Option<usize> {
        let i = self.scopes.iter().rposition(|s| s.kind == ScopeKind::Function && s.self_name.is_some())?;
        let s = &self.scopes[i];
        if s.self_name.as_deref() != Some(name) {
            return None;
        }
        let class = self.scopes.get(i.checked_sub(1)?)?;
        (class.kind == ScopeKind::Class).then_some(class.node)
    }

    /// `(name, receiver)` of a pure chain: `a.b.c` → (`c`, `a.b`).
    fn chain_name(&self, idents: &[Node<'_>]) -> (String, Option<String>) {
        let texts: Vec<String> = idents.iter().map(|n| self.text(*n)).collect();
        let (last, prefix) = texts.split_last().expect("chain is non-empty");
        let receiver = if prefix.is_empty() { None } else { Some(prefix.join(".")) };
        (last.clone(), receiver)
    }

    // -----------------------------------------------------------------------
    // Targets
    // -----------------------------------------------------------------------

    /// Identifier, attribute and subscript nodes of an assignment target.
    fn target_nodes<'t>(&self, node: Node<'t>, out: &mut Vec<Node<'t>>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "identifier" | "attribute" | "subscript" => out.push(node),
            _ => {
                for child in named_children(node) {
                    self.target_nodes(child, out, depth + 1);
                }
            }
        }
    }

    fn first_param_name(&self, params: Node<'_>) -> Option<String> {
        let first = named_children(params).into_iter().find(|p| p.kind() != "comment")?;
        match first.kind() {
            "identifier" => Some(self.text(first)),
            "typed_parameter" => {
                named_children(first).into_iter().find(|c| c.kind() == "identifier").map(|c| self.text(c))
            }
            "default_parameter" | "typed_default_parameter" => first.child_by_field_name("name").map(|n| self.text(n)),
            _ => None,
        }
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
            "function_definition" => self.visit_function(node, node, &[], depth),
            "class_definition" => self.visit_class(node, node, &[], depth),
            "decorated_definition" => self.visit_decorated(node, depth),
            "import_statement" => self.visit_import(node),
            "import_from_statement" | "future_import_statement" => self.visit_import_from(node),
            "assignment" => self.visit_assignment(node, depth),
            "augmented_assignment" => self.visit_augmented(node, depth),
            "call" => self.visit_call(node, depth),
            "subscript" => {
                if let Some(var) = self.env.subscript(node, self.source) {
                    self.push_ref(RefKind::EnvVar, var, None, span_of(node));
                }
                self.visit_children(node, depth)
            }
            "attribute" => self.visit_attribute(node, depth),
            "identifier" => self.read_identifier(node),
            "keyword_argument" => self.visit_field(node, "value", depth),
            "raise_statement" => self.visit_raise(node, depth),
            "if_statement" => self.visit_if(node, depth),
            "for_statement" | "for_in_clause" => {
                let left = node.child_by_field_name("left");
                for child in named_children(node) {
                    if Some(child) != left {
                        self.visit(child, depth + 1);
                    }
                }
            }
            "as_pattern" => {
                for child in named_children(node) {
                    if child.kind() != "as_pattern_target" {
                        self.visit(child, depth + 1);
                    }
                }
            }
            "named_expression" => self.visit_field(node, "value", depth),
            "lambda" => self.visit_lambda(node, depth),
            "string" => self.visit_string(node, depth),
            "type" => {
                let enc = self.enclosing();
                self.visit_type(node, enc, depth);
            }
            "type_alias_statement" => self.visit_type_alias(node, depth),
            "global_statement"
            | "nonlocal_statement"
            | "case_pattern"
            | "comment"
            | "dotted_name"
            | "parameters"
            | "lambda_parameters"
            | "decorator"
            | "import_prefix"
            | "relative_import"
            | "aliased_import"
            | "wildcard_import"
            | "keyword_separator"
            | "positional_separator"
            | "type_parameter"
            | "integer"
            | "float"
            | "true"
            | "false"
            | "none"
            | "ellipsis"
            | "escape_sequence"
            | "line_continuation" => {}
            _ => self.visit_children(node, depth),
        }
    }

    fn read_identifier(&mut self, node: Node<'_>) {
        let name = self.text(node);
        if is_builtin_name(&name) || self.is_local(node) {
            return;
        }
        self.push_ref(RefKind::Read, name, None, span_of(node));
    }

    /// `a.b.c` in value position.
    fn visit_attribute(&mut self, node: Node<'_>, depth: usize) {
        let Some((idents, innermost)) = chain(node) else {
            self.visit_field(node, "object", depth);
            return;
        };
        let root = self.text(idents[0]);
        if SELF_RECEIVERS.contains(&root.as_str()) {
            // `self.x.y` reads field `x` of the current instance.
            let name = self.text(idents[1]);
            self.push_ref(RefKind::Read, name, Some(root), span_of(innermost));
            return;
        }
        if is_builtin_name(&root) || self.is_local(idents[0]) {
            return;
        }
        let (name, receiver) = self.chain_name(&idents);
        self.push_ref(RefKind::Read, name, receiver, span_of(node));
    }

    fn visit_call(&mut self, node: Node<'_>, depth: usize) {
        if let Some(var) = self.env.call(node, self.source) {
            self.push_ref(RefKind::EnvVar, var, None, span_of(node));
        }
        if let Some(func) = node.child_by_field_name("function") {
            match func.kind() {
                "identifier" => {
                    let name = self.text(func);
                    self.push_ref(RefKind::Call, name, None, span_of(func));
                }
                "attribute" => {
                    let obj = func.child_by_field_name("object");
                    let attr = func.child_by_field_name("attribute");
                    if let (Some(obj), Some(attr)) = (obj, attr) {
                        let name = self.text(attr);
                        let receiver = match self.receiver_type(obj) {
                            Some(t) => type_receiver(&t),
                            None => self.receiver_text(obj),
                        };
                        self.push_ref(RefKind::Call, name, Some(receiver), span_of(func));
                        match chain(obj) {
                            // `self.repo.save()` also reads field `repo`.
                            Some((idents, innermost)) if idents.len() >= 2 => {
                                let root = self.text(idents[0]);
                                if SELF_RECEIVERS.contains(&root.as_str()) {
                                    let field = self.text(idents[1]);
                                    self.push_ref(RefKind::Read, field, Some(root), span_of(innermost));
                                }
                            }
                            Some(_) => {}
                            None => self.visit(obj, depth + 1),
                        }
                    } else {
                        self.visit(func, depth + 1);
                    }
                }
                _ => self.visit(func, depth + 1),
            }
        }
        self.visit_field(node, "arguments", depth);
    }

    fn visit_string(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for child in named_children(node) {
            match child.kind() {
                "interpolation" => {
                    for part in named_children(child) {
                        match part.kind() {
                            "format_specifier" => self.visit_string(part, depth + 1),
                            "type_conversion" | "comment" => {}
                            _ => self.visit(part, depth + 2),
                        }
                    }
                }
                "format_specifier" => self.visit_string(child, depth + 1),
                _ => {}
            }
        }
    }

    fn visit_lambda(&mut self, node: Node<'_>, depth: usize) {
        let def = self.scopes.last().and_then(|s| s.def);
        self.scopes.push(Scope { kind: ScopeKind::Lambda, def, self_name: None, init_class: None, node: node.id() });
        if let Some(params) = node.child_by_field_name("parameters") {
            for p in named_children(params) {
                if p.kind() == "default_parameter" {
                    self.visit_field(p, "value", depth + 1);
                }
            }
        }
        self.visit_field(node, "body", depth);
        self.scopes.pop();
    }

    fn visit_if(&mut self, node: Node<'_>, depth: usize) {
        let cond = node.child_by_field_name("condition");
        let consequence = node.child_by_field_name("consequence");
        let is_tc = cond.is_some_and(|c| {
            chain(c)
                .is_some_and(|(idents, _)| idents.last().is_some_and(|l| text_of(*l, self.source) == "TYPE_CHECKING"))
        });
        for child in named_children(node) {
            if Some(child) == consequence && is_tc {
                self.type_checking += 1;
                self.visit(child, depth + 1);
                self.type_checking -= 1;
            } else {
                self.visit(child, depth + 1);
            }
        }
    }

    fn visit_raise(&mut self, node: Node<'_>, depth: usize) {
        let cause = node.child_by_field_name("cause");
        for child in named_children(node) {
            if Some(child) == cause {
                self.visit(child, depth + 1);
                continue;
            }
            match child.kind() {
                "call" => {
                    let func = child.child_by_field_name("function");
                    match func {
                        Some(f) if chain(f).is_some() => self.throw_target(f),
                        Some(f) => self.visit(f, depth + 2),
                        None => {}
                    }
                    self.visit_field(child, "arguments", depth + 1);
                }
                "identifier" | "attribute" if chain(child).is_some() => self.throw_target(child),
                _ => self.visit(child, depth + 1),
            }
        }
    }

    fn throw_target(&mut self, expr: Node<'_>) {
        let Some((idents, _)) = chain(expr) else { return };
        let root = self.text(idents[0]);
        if self.is_local(idents[0]) || (idents.len() == 1 && SELF_RECEIVERS.contains(&root.as_str())) {
            return;
        }
        let (name, receiver) = self.chain_name(&idents);
        self.push_ref(RefKind::Throw, name, receiver, span_of(expr));
    }

    /// Type annotation: emits `Type` references for identifiers and dotted
    /// names (builtins skipped), including simple string forward references.
    fn visit_type(&mut self, node: Node<'_>, enc: Option<LocalDefId>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "identifier" => {
                let name = self.text(node);
                if !is_builtin_name(&name) && !self.locals.is_type_parameter(node) {
                    self.push_ref_enc(RefKind::Type, name, None, span_of(node), enc);
                }
            }
            "attribute" => match chain(node) {
                Some((idents, _)) => {
                    let (name, receiver) = self.chain_name(&idents);
                    self.push_ref_enc(RefKind::Type, name, receiver, span_of(node), enc);
                }
                None => self.with_enclosing(enc, |this| this.visit(node, depth)),
            },
            "member_type" => {
                let text = one_line(node, node.start_byte(), node.end_byte(), self.source).replace(' ', "");
                if is_dotted_identifier(&text) {
                    let (receiver, name) = match text.rsplit_once('.') {
                        Some((r, n)) => (Some(r.to_string()), n.to_string()),
                        None => (None, text.clone()),
                    };
                    self.push_ref_enc(RefKind::Type, name, receiver, span_of(node), enc);
                } else {
                    for child in named_children(node) {
                        self.visit_type(child, enc, depth + 1);
                    }
                }
            }
            "string" => {
                let raw = self.text(node);
                if let Some(value) = string_literal_value(&raw) {
                    let value = value.trim();
                    if is_dotted_identifier(value) {
                        let (receiver, name) = match value.rsplit_once('.') {
                            Some((r, n)) => (Some(r.to_string()), n.to_string()),
                            None => (None, value.to_string()),
                        };
                        if receiver.is_some() || !is_builtin_name(&name) {
                            self.push_ref_enc(RefKind::Type, name, receiver, span_of(node), enc);
                        }
                    }
                }
            }
            "call" | "lambda" => self.with_enclosing(enc, |this| this.visit(node, depth)),
            "none"
            | "ellipsis"
            | "integer"
            | "float"
            | "true"
            | "false"
            | "comment"
            | "keyword_argument"
            | "concatenated_string" => {}
            _ => {
                for child in named_children(node) {
                    self.visit_type(child, enc, depth + 1);
                }
            }
        }
    }

    fn push_ref_enc(
        &mut self,
        kind: RefKind,
        name: String,
        receiver: Option<String>,
        span: Span,
        enc: Option<LocalDefId>,
    ) {
        self.with_enclosing(enc, |this| this.push_ref(kind, name, receiver, span));
    }

    fn with_enclosing(&mut self, enc: Option<LocalDefId>, f: impl FnOnce(&mut Self)) {
        let saved = self.enc_override.replace(enc);
        f(self);
        self.enc_override = saved;
    }

    // -----------------------------------------------------------------------
    // Definitions
    // -----------------------------------------------------------------------

    /// One-line header `def f(a, b) -> X` / `class A(B)` (no trailing colon).
    fn header(&self, def: Node<'_>) -> Option<String> {
        let end = def.child_by_field_name("body").map(|b| b.start_byte()).unwrap_or(def.end_byte());
        let text = one_line(def, def.start_byte(), end, self.source);
        let text = text.trim().trim_end_matches(':').trim_end();
        if text.is_empty() { None } else { Some(truncate_chars(text, MAX_ONE_LINE)) }
    }

    fn docstring(&self, def: Node<'_>) -> Option<String> {
        let body = def.child_by_field_name("body")?;
        let first = named_children(body).into_iter().find(|c| c.kind() != "comment")?;
        if first.kind() != "expression_statement" {
            return None;
        }
        let parts = named_children(first);
        if parts.len() != 1 {
            return None;
        }
        let lit = parts[0];
        let strings = match lit.kind() {
            "string" => vec![lit],
            "concatenated_string" => named_children(lit).into_iter().filter(|c| c.kind() == "string").collect(),
            _ => return None,
        };
        let mut raw = String::new();
        for s in strings {
            let text = self.text(s);
            let prefix: String = text.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            if prefix.contains(['f', 'F', 'b', 'B']) {
                return None;
            }
            raw.push_str(string_literal_value(&text)?);
        }
        let doc = cleandoc(&raw);
        if doc.is_empty() { None } else { Some(truncate_chars(&doc, MAX_DOC)) }
    }

    fn decorator_text(&self, dec: Node<'_>) -> String {
        let expr = named_children(dec).into_iter().find(|c| c.kind() != "comment");
        let text = match expr {
            Some(e) => one_line(e, e.start_byte(), e.end_byte(), self.source),
            None => self.text(dec).trim_start_matches('@').trim().to_string(),
        };
        truncate_chars(&text, MAX_ONE_LINE)
    }

    fn visit_decorated(&mut self, node: Node<'_>, depth: usize) {
        let decorators: Vec<Node<'_>> = named_children(node).into_iter().filter(|c| c.kind() == "decorator").collect();
        match node.child_by_field_name("definition") {
            Some(def) if def.kind() == "function_definition" => self.visit_function(def, node, &decorators, depth),
            Some(def) if def.kind() == "class_definition" => self.visit_class(def, node, &decorators, depth),
            _ => self.visit_children(node, depth),
        }
    }

    fn visit_decorators(&mut self, decorators: &[Node<'_>], def_idx: LocalDefId, depth: usize) {
        self.with_enclosing(Some(def_idx), |this| {
            for dec in decorators {
                let Some(expr) = named_children(*dec).into_iter().find(|c| c.kind() != "comment") else { continue };
                let (callee, args) = if expr.kind() == "call" {
                    (expr.child_by_field_name("function"), expr.child_by_field_name("arguments"))
                } else {
                    (Some(expr), None)
                };
                if let Some(callee) = callee {
                    match chain(callee) {
                        Some((idents, _)) => {
                            let (name, receiver) = this.chain_name(&idents);
                            this.push_ref(RefKind::Decorator, name, receiver, span_of(expr));
                        }
                        None => this.visit(callee, depth + 2),
                    }
                }
                if let Some(args) = args {
                    this.visit(args, depth + 2);
                }
            }
        });
    }

    fn visit_function(&mut self, def: Node<'_>, outer: Node<'_>, decorators: &[Node<'_>], depth: usize) {
        let Some(name_node) = def.child_by_field_name("name") else {
            self.visit_children(def, depth);
            return;
        };
        let name = self.text(name_node);
        let (scope_kind, parent) = (self.scope_kind(), self.scopes.last().and_then(|s| s.def));
        let in_class = scope_kind == ScopeKind::Class;
        let in_test_class =
            in_class && parent.and_then(|p| self.defs.get(p as usize)).is_some_and(|p| p.name.starts_with("Test"));
        let is_test = name == "test" || name.starts_with("test_") || (in_test_class && name.starts_with("test"));
        let decorator_texts = decorators.iter().map(|d| self.decorator_text(*d)).collect();
        let idx = self.push_def(Definition {
            label: if in_class { Label::Method } else { Label::Function },
            qualified_name: self.qualify(parent, &name),
            exported: parent.is_none() && scope_kind == ScopeKind::Module && self.is_exported(&name),
            name,
            parent,
            span: span_of(outer),
            name_span: span_of(name_node),
            signature: self.header(def),
            doc: self.docstring(def),
            decorators: decorator_texts,
            bases: Vec::new(),
            implements: Vec::new(),
            is_test,
            is_declaration: false,
        });
        self.visit_decorators(decorators, idx, depth);

        let params = def.child_by_field_name("parameters");
        let body = def.child_by_field_name("body");
        let self_name = if in_class { params.and_then(|p| self.first_param_name(p)) } else { None };
        let init_class = if in_class && self.defs[idx as usize].name == "__init__" { parent } else { None };
        self.scopes.push(Scope { kind: ScopeKind::Function, def: Some(idx), self_name, init_class, node: def.id() });
        if let Some(p) = params {
            for param in named_children(p) {
                match param.kind() {
                    "typed_parameter" => {
                        if let Some(t) = param.child_by_field_name("type") {
                            self.visit_type(t, Some(idx), depth + 2);
                        }
                    }
                    "typed_default_parameter" => {
                        if let Some(t) = param.child_by_field_name("type") {
                            self.visit_type(t, Some(idx), depth + 2);
                        }
                        self.visit_field(param, "value", depth + 1);
                    }
                    "default_parameter" => self.visit_field(param, "value", depth + 1),
                    _ => {}
                }
            }
        }
        if let Some(rt) = def.child_by_field_name("return_type") {
            self.visit_type(rt, Some(idx), depth + 1);
        }
        if let Some(b) = body {
            self.visit_children(b, depth + 1);
        }
        self.scopes.pop();
    }

    fn visit_class(&mut self, def: Node<'_>, outer: Node<'_>, decorators: &[Node<'_>], depth: usize) {
        let Some(name_node) = def.child_by_field_name("name") else {
            self.visit_children(def, depth);
            return;
        };
        let name = self.text(name_node);
        let (scope_kind, parent) = (self.scope_kind(), self.scopes.last().and_then(|s| s.def));
        let superclasses = def.child_by_field_name("superclasses");
        let base_nodes: Vec<Node<'_>> = superclasses
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !matches!(c.kind(), "keyword_argument" | "list_splat" | "dictionary_splat" | "comment"))
            .collect();
        let bases = base_nodes
            .iter()
            .map(|b| truncate_chars(&one_line(*b, b.start_byte(), b.end_byte(), self.source), MAX_ONE_LINE))
            .collect();
        let decorator_texts = decorators.iter().map(|d| self.decorator_text(*d)).collect();
        let idx = self.push_def(Definition {
            label: Label::Class,
            qualified_name: self.qualify(parent, &name),
            exported: parent.is_none() && scope_kind == ScopeKind::Module && self.is_exported(&name),
            is_test: name.starts_with("Test"),
            is_declaration: false,
            name,
            parent,
            span: span_of(outer),
            name_span: span_of(name_node),
            signature: self.header(def),
            doc: self.docstring(def),
            decorators: decorator_texts,
            bases,
            implements: Vec::new(),
        });
        self.visit_decorators(decorators, idx, depth);
        for base in &base_nodes {
            self.visit_type(*base, Some(idx), depth + 2);
        }
        if let Some(sc) = superclasses {
            self.with_enclosing(Some(idx), |this| {
                for kw in named_children(sc) {
                    if matches!(kw.kind(), "keyword_argument" | "list_splat" | "dictionary_splat") {
                        this.visit(kw, depth + 2);
                    }
                }
            });
        }
        let body = def.child_by_field_name("body");
        self.scopes.push(Scope {
            kind: ScopeKind::Class,
            def: Some(idx),
            self_name: None,
            init_class: None,
            node: def.id(),
        });
        if let Some(b) = body {
            self.visit_children(b, depth + 1);
        }
        self.scopes.pop();
    }

    fn visit_type_alias(&mut self, node: Node<'_>, depth: usize) {
        let left = node.child_by_field_name("left");
        let name_node = left.and_then(|l| {
            let mut cur = l;
            loop {
                if cur.kind() == "identifier" {
                    return Some(cur);
                }
                cur = cur.named_child(0)?;
            }
        });
        let scope_kind = self.scope_kind();
        let mut enc = self.enclosing();
        if let Some(name_node) = name_node
            && matches!(scope_kind, ScopeKind::Module | ScopeKind::Class)
        {
            let parent = self.scopes.last().and_then(|s| s.def);
            let name = self.text(name_node);
            let idx = self.push_def(Definition {
                label: Label::TypeAlias,
                qualified_name: self.qualify(parent, &name),
                exported: parent.is_none() && self.is_exported(&name),
                name,
                parent,
                span: span_of(node),
                name_span: span_of(name_node),
                signature: Some(first_line(&self.text(node), MAX_ONE_LINE)),
                doc: None,
                decorators: Vec::new(),
                bases: Vec::new(),
                implements: Vec::new(),
                is_test: false,
                is_declaration: false,
            });
            enc = Some(idx);
        }
        if let Some(right) = node.child_by_field_name("right") {
            self.visit_type(right, enc, depth + 1);
        }
    }

    /// Statement node used as a variable's span: the outermost assignment of
    /// a chain, widened to its `expression_statement`.
    fn statement_of<'t>(node: Node<'t>) -> Node<'t> {
        let mut cur = node;
        while let Some(p) = cur.parent() {
            if matches!(p.kind(), "assignment" | "expression_statement") {
                cur = p;
            } else {
                break;
            }
        }
        cur
    }

    fn add_variable(&mut self, ident: Node<'_>, stmt: Node<'_>, class: Option<LocalDefId>) -> Option<LocalDefId> {
        let name = self.text(ident);
        let qualified_name = self.qualify(class, &name);
        if class.is_some() {
            let members = self.members.entry(class.unwrap_or_default()).or_default();
            if !members.insert(name.clone()) {
                return None;
            }
        }
        if self.def_names.contains(&qualified_name) {
            return None;
        }
        let label = match class {
            Some(_) => Label::Field,
            None if is_upper_case(&name) => Label::Constant,
            None => Label::Variable,
        };
        Some(self.push_def(Definition {
            label,
            exported: class.is_none() && self.is_exported(&name),
            qualified_name,
            name,
            parent: class,
            span: span_of(stmt),
            name_span: span_of(ident),
            signature: Some(first_line(&self.text(stmt), MAX_ONE_LINE)),
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            is_test: false,
            is_declaration: false,
        }))
    }

    fn visit_assignment(&mut self, node: Node<'_>, depth: usize) {
        let stmt = Self::statement_of(node);
        let scope_kind = self.scope_kind();
        let scope_def = self.scopes.last().and_then(|s| s.def);
        let mut targets = Vec::new();
        if let Some(left) = node.child_by_field_name("left") {
            self.target_nodes(left, &mut targets, depth + 1);
        }
        let mut created = None;
        for target in targets {
            match target.kind() {
                "identifier" => {
                    let def = match scope_kind {
                        ScopeKind::Module => self.add_variable(target, stmt, None),
                        ScopeKind::Class => self.add_variable(target, stmt, scope_def),
                        _ => None,
                    };
                    created = created.or(def);
                }
                "attribute" => {
                    let def = self.attribute_target(target, Some(stmt), depth);
                    created = created.or(def);
                }
                _ => self.visit(target, depth + 1),
            }
        }
        if let Some(t) = node.child_by_field_name("type") {
            let enc = created.or(self.enclosing());
            self.visit_type(t, enc, depth + 1);
        }
        self.visit_field(node, "right", depth);
    }

    fn visit_augmented(&mut self, node: Node<'_>, depth: usize) {
        if let Some(left) = node.child_by_field_name("left") {
            match left.kind() {
                // `x += 1` reads `x`; locals (the usual case) are filtered.
                "identifier" => self.read_identifier(left),
                "attribute" => {
                    self.attribute_target(left, None, depth);
                }
                _ => self.visit(left, depth + 1),
            }
        }
        self.visit_field(node, "right", depth);
    }

    /// Assignment to `obj.attr`: `Write` for `self`/`cls` receivers (plus a
    /// `Field` when assigning `self.x` in `__init__`), otherwise a read of
    /// the object.
    fn attribute_target(&mut self, target: Node<'_>, stmt: Option<Node<'_>>, depth: usize) -> Option<LocalDefId> {
        let obj = target.child_by_field_name("object")?;
        let attr = target.child_by_field_name("attribute")?;
        let obj_text = self.text(obj);
        if obj.kind() == "identifier" && SELF_RECEIVERS.contains(&obj_text.as_str()) {
            let name = self.text(attr);
            self.push_ref(RefKind::Write, name, Some(obj_text.clone()), span_of(target));
            let scope = self.scopes.last()?;
            if let (Some(class), Some(stmt)) = (scope.init_class, stmt)
                && scope.self_name.as_deref() == Some(obj_text.as_str())
            {
                return self.add_variable(attr, stmt, Some(class));
            }
            return None;
        }
        self.visit(obj, depth + 1);
        None
    }

    // -----------------------------------------------------------------------
    // Imports
    // -----------------------------------------------------------------------

    fn dotted(&self, node: Node<'_>) -> String {
        self.text(node).split_whitespace().collect::<String>().replace('\\', "")
    }

    fn imported_name(&self, node: Node<'_>) -> Option<(String, Option<String>)> {
        match node.kind() {
            "dotted_name" | "identifier" => Some((self.dotted(node), None)),
            "aliased_import" => {
                let name = node.child_by_field_name("name")?;
                let alias = node.child_by_field_name("alias").map(|a| self.text(a));
                Some((self.dotted(name), alias))
            }
            _ => None,
        }
    }

    fn visit_import(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        let names: Vec<Node<'_>> = node.children_by_field_name("name", &mut cursor).collect();
        for n in names {
            if let Some((module, alias)) = self.imported_name(n) {
                self.imports.push(Import {
                    module,
                    relative_level: 0,
                    names: Vec::new(),
                    module_alias: alias,
                    type_only: self.type_checking > 0,
                    reexport: false,
                    span: span_of(node),
                });
            }
        }
    }

    fn visit_import_from(&mut self, node: Node<'_>) {
        let (module, relative_level) = if node.kind() == "future_import_statement" {
            ("__future__".to_string(), 0)
        } else {
            match node.child_by_field_name("module_name") {
                Some(m) if m.kind() == "relative_import" => {
                    let mut level = 0u32;
                    let mut module = String::new();
                    for c in named_children(m) {
                        match c.kind() {
                            "import_prefix" => level += self.text(c).chars().filter(|ch| *ch == '.').count() as u32,
                            "dotted_name" => module = self.dotted(c),
                            _ => {}
                        }
                    }
                    (module, level)
                }
                Some(m) => (self.dotted(m), 0),
                None => return,
            }
        };
        let mut names = Vec::new();
        let mut cursor = node.walk();
        let name_nodes: Vec<Node<'_>> = node.children_by_field_name("name", &mut cursor).collect();
        for n in name_nodes {
            if let Some((name, alias)) = self.imported_name(n) {
                names.push(ImportedName { name, alias });
            }
        }
        if named_children(node).iter().any(|c| c.kind() == "wildcard_import") {
            names.push(ImportedName { name: "*".to_string(), alias: None });
        }
        self.imports.push(Import {
            module,
            relative_level,
            names,
            module_alias: None,
            type_only: self.type_checking > 0,
            reexport: false,
            span: span_of(node),
        });
    }
}
