//! Syntax-tree walk producing [`FileFacts`] for one Go file.
//!
//! A recursive visitor (depth-limited by `Budget::max_depth`) keeps the
//! enclosing definition so every reference is attributed to it. Whether an
//! identifier is a local binding, and what type a local has, comes from the
//! [`Locals`] table built by the vendored `queries/locals.scm` query (see
//! `locals.rs`). See the crate docs for the mapping rules.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::{self, Node};
use rueca_extract::{Budget, ExtractError, SELF_RECEIVER, count_errors, line_count, span_of, type_receiver};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::builtins::{is_predeclared, is_predeclared_type};
use crate::locals::{self, DefKind, FileTypes, Locals, text, type_text};
use crate::modules::conventional_name;

/// Hard cap on references per file.
pub const MAX_REFS: usize = 20_000;

/// Longest signature / receiver text kept, in chars.
const MAX_ONE_LINE: usize = 200;

/// Longest doc comment kept, in chars.
const MAX_DOC: usize = 1000;

pub(crate) fn extract(path: &str, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let tree = rueca_extract::parse(&language, source, budget)?;
    let root = tree.root_node();
    let mut locals = locals::build(&tree, source, budget.max_parse_time)?;
    let file_types = file_types(root, source, &locals);
    locals::infer_types(&mut locals, root, source, &file_types);

    let mut ex = Extractor {
        source,
        test_file: path.ends_with("_test.go"),
        max_depth: budget.max_depth,
        locals,
        file_types,
        struct_fields: HashMap::new(),
        top_names: HashSet::new(),
        imports: Vec::new(),
        import_names: HashSet::new(),
        os_names: HashSet::new(),
        viper_names: HashSet::new(),
        viper_config: None,
        config_refs: Vec::new(),
        qualifiers: HashSet::new(),
        defs: Vec::new(),
        refs: Vec::new(),
        seen_refs: HashSet::new(),
        enclosing: None,
        recv_type: None,
    };
    ex.prepass(root);
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        ex.top_level(child);
    }
    ex.finish_imports();
    ex.finish_config_refs();

    Ok(FileFacts {
        definitions: ex.defs,
        references: ex.refs,
        imports: ex.imports,
        parse_errors: count_errors(&tree),
        line_count: line_count(source),
    })
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|c| c.kind() != "comment").collect()
}

fn children_by_field<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor).filter(|c| c.is_named()).collect()
}

/// Whitespace-collapsed text, truncated to `max` chars (with `…`).
fn one_line(text: &str, max: usize) -> String {
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

/// Exported by Go's rule: the name starts with an upper-case letter.
pub(crate) fn is_exported(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

/// `go test` function names: `Test`, `Benchmark`, `Fuzz`, `Example`
/// followed by nothing or by a character that is not a lower-case letter.
pub(crate) fn is_test_name(name: &str) -> bool {
    ["Test", "Benchmark", "Fuzz", "Example"]
        .iter()
        .any(|p| name.strip_prefix(p).is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_lowercase())))
}

/// Value of a string literal without escapes (`"X"`, `` `X` ``).
fn string_value(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "interpreted_string_literal" => {
            let inner = named_children(node);
            if inner.iter().any(|c| c.kind() != "interpreted_string_literal_content") {
                return None;
            }
            Some(inner.iter().map(|c| text(*c, source)).collect())
        }
        "raw_string_literal" => Some(named_children(node).iter().map(|c| text(*c, source)).collect()),
        _ => None,
    }
}

/// The doc comment of a declaration: the `//` or `/* */` comments directly
/// above it (no blank line in between), delimiters stripped; directive
/// lines (`//go:generate`, `//nolint:…`) are dropped.
fn doc_of(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut row = node.start_position().row;
    let mut cur = node.prev_sibling();
    while let Some(c) = cur {
        if c.kind() != "comment" || c.end_position().row + 1 != row {
            break;
        }
        // A trailing comment of the previous line (`a int // note`) is not
        // part of this declaration's doc.
        if c.prev_sibling().is_some_and(|p| p.kind() != "comment" && p.end_position().row == c.start_position().row) {
            break;
        }
        let mut block = comment_lines(text(c, source));
        block.reverse();
        lines.extend(block);
        row = c.start_position().row;
        cur = c.prev_sibling();
    }
    lines.reverse();
    let doc = lines.join("\n").trim().to_string();
    (!doc.is_empty()).then(|| truncate(&doc, MAX_DOC))
}

/// The text lines of one comment, delimiters stripped; a directive line
/// (`//go:generate`, `//nolint:…`) has none.
fn comment_lines(t: &str) -> Vec<String> {
    if let Some(body) = t.strip_prefix("/*") {
        return body
            .trim_end_matches("*/")
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim().to_string())
            .collect();
    }
    let body = t.trim_start_matches("//");
    let directive = !body.starts_with(' ')
        && body
            .split_once(':')
            .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()));
    if directive { Vec::new() } else { vec![body.strip_prefix(' ').unwrap_or(body).trim_end().to_string()] }
}

/// Type of each package-level `var` and the first result type of each
/// function declared in the file (inputs of type inference).
fn file_types(root: Node<'_>, source: &[u8], locals: &Locals) -> FileTypes {
    let mut ft = FileTypes::default();
    for child in named_children(root).into_iter().filter(|c| c.kind() == "function_declaration") {
        function_result_type(child, source, locals, &mut ft);
    }
    for child in named_children(root).into_iter().filter(|c| c.kind() == "var_declaration") {
        for spec in specs_of(child, "var_spec", "var_spec_list") {
            var_spec_types(spec, source, locals, &mut ft);
        }
    }
    ft
}

/// Records the first result type of a function declaration.
fn function_result_type(func: Node<'_>, source: &[u8], locals: &Locals, ft: &mut FileTypes) {
    let (Some(name), Some(result)) = (func.child_by_field_name("name"), func.child_by_field_name("result")) else {
        return;
    };
    let pos = result.start_byte() as u32;
    let tp = |t: &str| locals.is_type_param(t, pos);
    let first = if result.kind() == "parameter_list" {
        named_children(result).first().and_then(|p| p.child_by_field_name("type"))
    } else {
        Some(result)
    };
    if let Some(ty) = first.and_then(|t| type_text(t, source, &tp)) {
        ft.results.insert(text(name, source).to_string(), ty);
    }
}

/// Records the declared (or inferred) type of each name of a package-level
/// `var` spec.
fn var_spec_types(spec: Node<'_>, source: &[u8], locals: &Locals, ft: &mut FileTypes) {
    let no_tp = |_: &str| false;
    let names = children_by_field(spec, "name");
    let values = spec.child_by_field_name("value").map(named_children).unwrap_or_default();
    for (i, n) in names.iter().enumerate() {
        let ty = match spec.child_by_field_name("type") {
            Some(t) => type_text(t, source, &no_tp),
            None if values.len() == names.len() => locals.expr_type(values[i], source, ft),
            None => None,
        };
        if let Some(ty) = ty {
            ft.vars.insert(text(*n, source).to_string(), ty);
        }
    }
}

/// The `spec` children of a declaration, flattening one `list` level
/// (`var ( a = 1; b = 2 )`).
fn specs_of<'t>(decl: Node<'t>, spec: &str, list: &str) -> Vec<Node<'t>> {
    let mut specs = Vec::new();
    for c in named_children(decl) {
        if c.kind() == spec {
            specs.push(c);
        } else if c.kind() == list {
            specs.extend(named_children(c).into_iter().filter(|s| s.kind() == spec));
        }
    }
    specs
}

struct Extractor<'s> {
    source: &'s [u8],
    test_file: bool,
    max_depth: usize,
    locals: Locals,
    file_types: FileTypes,
    /// Field types of the struct types declared in the file.
    struct_fields: HashMap<String, HashMap<String, String>>,
    /// Package-level names declared in the file.
    top_names: HashSet<String>,
    imports: Vec<Import>,
    /// Binding names of the file's imports (aliases and conventional names).
    import_names: HashSet<String>,
    /// Binding names of `import "os"` and `github.com/spf13/viper`.
    os_names: HashSet<String>,
    viper_names: HashSet<String>,
    /// Literal of the first `viper.SetConfigName("…")` of the file.
    viper_config: Option<(u32, String)>,
    /// Indexes of `ConfigKey` references (namespace fixed after the walk).
    config_refs: Vec<usize>,
    /// Non-local identifiers used as qualifiers (`x` in `x.Y`).
    qualifiers: HashSet<String>,
    defs: Vec<Definition>,
    refs: Vec<Reference>,
    /// Dedup keys for `Read` / `Write` / `Type`: (kind, enclosing, receiver, name).
    seen_refs: HashSet<(u8, Option<LocalDefId>, Option<String>, String)>,
    enclosing: Option<LocalDefId>,
    /// Receiver type of the method being visited (`Server` in `func (s *Server)`).
    recv_type: Option<String>,
}

impl<'s> Extractor<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.source)
    }

    // -- pre-pass ------------------------------------------------------------

    /// Imports and the package-level names / struct field types of the file.
    fn prepass(&mut self, root: Node<'_>) {
        for child in named_children(root) {
            match child.kind() {
                "import_declaration" => {
                    for spec in specs_of(child, "import_spec", "import_spec_list") {
                        self.import_spec(spec);
                    }
                }
                "function_declaration" | "type_declaration" | "var_declaration" | "const_declaration" => {
                    self.collect_top_names(child);
                }
                _ => {}
            }
        }
    }

    fn import_spec(&mut self, spec: Node<'_>) {
        let Some(path) = spec.child_by_field_name("path").and_then(|p| string_value(p, self.source)) else {
            return;
        };
        let name = spec.child_by_field_name("name");
        let (names, alias) = match name.map(|n| (n.kind(), self.text(n))) {
            Some(("dot", _)) => (vec![ImportedName { name: "*".into(), alias: None }], None),
            Some(("blank_identifier", _)) => (Vec::new(), None),
            Some((_, a)) => (Vec::new(), Some(a.to_string())),
            None => (Vec::new(), conventional_name(&path)),
        };
        if let Some(a) = &alias {
            self.import_names.insert(a.clone());
            if path == "os" {
                self.os_names.insert(a.clone());
            }
            if path == "github.com/spf13/viper" {
                self.viper_names.insert(a.clone());
            }
        }
        self.imports.push(Import {
            module: path,
            relative_level: 0,
            names,
            module_alias: alias,
            type_only: false,
            reexport: false,
            span: span_of(spec),
        });
    }

    fn collect_top_names(&mut self, decl: Node<'_>) {
        match decl.kind() {
            "function_declaration" => {
                if let Some(n) = decl.child_by_field_name("name") {
                    self.top_names.insert(self.text(n).to_string());
                }
            }
            "type_declaration" => self.collect_type_names(decl),
            _ => self.collect_value_names(decl),
        }
    }

    /// Names (and struct field types) of the specs of a `type` declaration.
    fn collect_type_names(&mut self, decl: Node<'_>) {
        for spec in named_children(decl) {
            let Some(n) = spec.child_by_field_name("name") else { continue };
            let name = self.text(n).to_string();
            if let Some(st) = spec.child_by_field_name("type").filter(|t| t.kind() == "struct_type") {
                let fields = self.struct_field_types(st);
                self.struct_fields.insert(name.clone(), fields);
            }
            self.top_names.insert(name);
        }
    }

    /// Names declared by a package-level `var` / `const` declaration.
    fn collect_value_names(&mut self, decl: Node<'_>) {
        let mut stack = vec![decl];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "var_spec" | "const_spec" => {
                    for name in children_by_field(n, "name") {
                        self.top_names.insert(self.text(name).to_string());
                    }
                }
                "var_declaration" | "const_declaration" | "var_spec_list" => stack.extend(named_children(n)),
                _ => {}
            }
        }
    }

    /// Field name → declared type of a struct type (embedded fields are
    /// named after their type).
    fn struct_field_types(&self, st: Node<'_>) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let no_tp = |_: &str| false;
        let Some(list) = named_children(st).into_iter().find(|c| c.kind() == "field_declaration_list") else {
            return out;
        };
        for field in named_children(list).into_iter().filter(|f| f.kind() == "field_declaration") {
            let Some(ty) = field.child_by_field_name("type") else { continue };
            let Some(t) = type_text(ty, self.source, &no_tp) else { continue };
            let names = children_by_field(field, "name");
            if names.is_empty() {
                let short = t.rsplit('.').next().unwrap_or(&t).to_string();
                out.insert(short, t);
            } else {
                for n in names {
                    out.insert(self.text(n).to_string(), t.clone());
                }
            }
        }
        out
    }

    // -- definitions ---------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn push_def(
        &mut self,
        label: Label,
        name: &str,
        qualified_name: String,
        parent: Option<LocalDefId>,
        span: Span,
        name_span: Span,
        signature: Option<String>,
        doc: Option<String>,
    ) -> LocalDefId {
        let is_test = self.test_file && label == Label::Function && is_test_name(name);
        self.defs.push(Definition {
            label,
            name: name.to_string(),
            qualified_name,
            parent,
            span,
            name_span,
            signature,
            doc,
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: is_exported(name),
            is_test,
            is_declaration: false,
        });
        (self.defs.len() - 1) as LocalDefId
    }

    /// Signature of a declaration: its text up to the body, one line.
    fn signature_until(&self, node: Node<'_>, body: Option<Node<'_>>) -> String {
        let end = body.map_or(node.end_byte(), |b| b.start_byte());
        let t = std::str::from_utf8(&self.source[node.start_byte()..end]).unwrap_or("");
        one_line(t, MAX_ONE_LINE)
    }

    fn top_level(&mut self, node: Node<'_>) {
        match node.kind() {
            "package_clause" => self.package_clause(node),
            "import_declaration" | "comment" => {}
            "function_declaration" => self.function(node),
            "method_declaration" => self.method(node),
            "type_declaration" => self.type_declaration(node),
            "var_declaration" | "const_declaration" => self.value_declaration(node),
            _ => self.visit(node, 1),
        }
    }

    /// `package x_test` (an external test package) is recorded as a
    /// `Package` definition so `module_name_for` can tell it apart.
    fn package_clause(&mut self, node: Node<'_>) {
        let Some(id) = named_children(node).into_iter().find(|c| c.kind() == "package_identifier") else { return };
        let name = self.text(id);
        if !name.ends_with("_test") {
            return;
        }
        let d = self.push_def(
            Label::Package,
            name,
            name.to_string(),
            None,
            span_of(node),
            span_of(id),
            Some(format!("package {name}")),
            None,
        );
        self.defs[d as usize].is_test = true;
        self.defs[d as usize].exported = false;
    }

    fn function(&mut self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else { return self.visit(node, 1) };
        let name = self.text(name_node);
        let body = node.child_by_field_name("body");
        let sig = self.signature_until(node, body);
        let d = self.push_def(
            Label::Function,
            name,
            name.to_string(),
            None,
            span_of(node),
            span_of(name_node),
            Some(sig),
            doc_of(node, self.source),
        );
        self.with_enclosing(Some(d), None, |ex| ex.callable_parts(node));
    }

    fn method(&mut self, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else { return self.visit(node, 1) };
        let name = self.text(name_node);
        let recv = node.child_by_field_name("receiver").and_then(|r| self.receiver_type_name(r));
        let qn = match &recv {
            Some(t) => format!("{t}.{name}"),
            None => name.to_string(),
        };
        let body = node.child_by_field_name("body");
        let sig = self.signature_until(node, body);
        let d = self.push_def(
            Label::Method,
            name,
            qn,
            None,
            span_of(node),
            span_of(name_node),
            Some(sig),
            doc_of(node, self.source),
        );
        self.with_enclosing(Some(d), recv, |ex| ex.callable_parts(node));
    }

    /// `T` of a receiver `(r *T)`, `(r T[K])`, `(T)`.
    fn receiver_type_name(&self, list: Node<'_>) -> Option<String> {
        let param = named_children(list).into_iter().find(|p| p.kind() == "parameter_declaration")?;
        let mut ty = param.child_by_field_name("type")?;
        loop {
            match ty.kind() {
                "pointer_type" | "parenthesized_type" => ty = *named_children(ty).first()?,
                "generic_type" => ty = ty.child_by_field_name("type")?,
                "type_identifier" => return Some(self.text(ty).to_string()),
                _ => return None,
            }
        }
    }

    fn with_enclosing(&mut self, d: Option<LocalDefId>, recv: Option<String>, f: impl FnOnce(&mut Self)) {
        let saved = (self.enclosing, self.recv_type.take());
        self.enclosing = d;
        self.recv_type = recv.or(saved.1.clone());
        f(self);
        self.enclosing = saved.0;
        self.recv_type = saved.1;
    }

    /// Type parameters, parameters, results and body of a function, method
    /// or function literal. The receiver's own type is not a reference.
    fn callable_parts(&mut self, node: Node<'_>) {
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            self.visit_type(tp, 1);
        }
        for field in ["parameters", "result"] {
            if let Some(p) = node.child_by_field_name(field) {
                self.visit_type(p, 1);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, 1);
        }
    }

    fn type_declaration(&mut self, node: Node<'_>) {
        let specs: Vec<Node<'_>> =
            named_children(node).into_iter().filter(|s| matches!(s.kind(), "type_spec" | "type_alias")).collect();
        let single = specs.len() == 1;
        for spec in specs {
            self.type_spec(spec, if single { Some(node) } else { None });
        }
    }

    /// One `type` spec. `decl` is the whole declaration when it has a
    /// single spec (its span and doc are used).
    fn type_spec(&mut self, spec: Node<'_>, decl: Option<Node<'_>>) {
        let Some(name_node) = spec.child_by_field_name("name") else { return };
        let name = self.text(name_node).to_string();
        let ty = spec.child_by_field_name("type");
        let label = match (spec.kind(), ty.map(|t| t.kind())) {
            ("type_spec", Some("struct_type")) => Label::Struct,
            ("type_spec", Some("interface_type")) => Label::Interface,
            _ => Label::TypeAlias,
        };
        let outer = decl.unwrap_or(spec);
        let body = ty.filter(|_| matches!(label, Label::Struct | Label::Interface));
        let sig = {
            let end = body.map_or(spec.end_byte(), |b| {
                // `type T struct {` → stop before the field list.
                named_children(b).first().map_or(b.end_byte(), |l| l.start_byte())
            });
            let t = std::str::from_utf8(&self.source[spec.start_byte()..end]).unwrap_or("");
            one_line(&format!("type {}", t.trim_end().trim_end_matches('{').trim_end()), MAX_ONE_LINE)
        };
        let doc = doc_of(spec, self.source).or_else(|| decl.and_then(|d| doc_of(d, self.source)));
        let d = self.push_def(label, &name, name.clone(), None, span_of(outer), span_of(name_node), Some(sig), doc);
        self.with_enclosing(Some(d), None, |ex| {
            if let Some(tp) = spec.child_by_field_name("type_parameters") {
                ex.visit_type(tp, 1);
            }
            if spec.kind() == "type_alias"
                && let Some(t) = ty
                && matches!(t.kind(), "type_identifier" | "qualified_type" | "generic_type")
                && !(t.kind() == "type_identifier" && is_predeclared(ex.text(t)))
            {
                // `type A = B`: A is B, so B's members are found through A.
                let base = ex.text(t).to_string();
                ex.defs[d as usize].bases.push(base);
            }
            match (label, ty) {
                (Label::Struct, Some(st)) => ex.struct_members(d, &name, st),
                (Label::Interface, Some(it)) => ex.interface_members(d, &name, it),
                (_, Some(t)) => ex.visit_type(t, 1),
                _ => {}
            }
        });
    }

    fn struct_members(&mut self, owner: LocalDefId, owner_name: &str, st: Node<'_>) {
        let Some(list) = named_children(st).into_iter().find(|c| c.kind() == "field_declaration_list") else {
            return;
        };
        for field in named_children(list).into_iter().filter(|f| f.kind() == "field_declaration") {
            let Some(ty) = field.child_by_field_name("type") else { continue };
            let names = children_by_field(field, "name");
            let sig = {
                let end = field.child_by_field_name("tag").map_or(field.end_byte(), |t| t.start_byte());
                let t = std::str::from_utf8(&self.source[field.start_byte()..end]).unwrap_or("");
                one_line(t, MAX_ONE_LINE)
            };
            let doc = doc_of(field, self.source);
            if names.is_empty() {
                // Embedded field: a base (method promotion) and a field named
                // after the type.
                let base = self.text(ty).trim_start_matches('*').trim().to_string();
                let short = embedded_name(&base);
                if short.is_empty() {
                    continue;
                }
                self.defs[owner as usize].bases.push(base);
                let qn = format!("{owner_name}.{short}");
                let fd =
                    self.push_def(Label::Field, &short, qn, Some(owner), span_of(field), span_of(ty), Some(sig), doc);
                // Type arguments of an embedded generic type are references.
                self.with_enclosing(Some(fd), None, |ex| {
                    if let Some(args) = generic_args(ty) {
                        ex.visit_type(args, 1);
                    }
                });
                continue;
            }
            let mut first = None;
            for n in names {
                let fname = self.text(n).to_string();
                let qn = format!("{owner_name}.{fname}");
                let fd = self.push_def(
                    Label::Field,
                    &fname,
                    qn,
                    Some(owner),
                    span_of(field),
                    span_of(n),
                    Some(sig.clone()),
                    doc.clone(),
                );
                first.get_or_insert(fd);
            }
            self.with_enclosing(first, None, |ex| ex.visit_type(ty, 1));
        }
    }

    fn interface_members(&mut self, owner: LocalDefId, owner_name: &str, it: Node<'_>) {
        for elem in named_children(it) {
            match elem.kind() {
                "method_elem" => self.interface_method(owner, owner_name, elem),
                "type_elem" => self.interface_type_elem(owner, elem),
                _ => {}
            }
        }
    }

    /// A method of an interface: a `Method` whose signature types are
    /// references.
    fn interface_method(&mut self, owner: LocalDefId, owner_name: &str, elem: Node<'_>) {
        let Some(n) = elem.child_by_field_name("name") else { return };
        let mname = self.text(n).to_string();
        let qn = format!("{owner_name}.{mname}");
        let sig = one_line(self.text(elem), MAX_ONE_LINE);
        let md = self.push_def(
            Label::Method,
            &mname,
            qn,
            Some(owner),
            span_of(elem),
            span_of(n),
            Some(sig),
            doc_of(elem, self.source),
        );
        self.with_enclosing(Some(md), None, |ex| ex.visit_signature_types(elem, 1));
    }

    /// The `parameters` and `result` types of a method element.
    fn visit_signature_types(&mut self, node: Node<'_>, depth: usize) {
        for field in ["parameters", "result"] {
            if let Some(p) = node.child_by_field_name(field) {
                self.visit_type(p, depth);
            }
        }
    }

    /// A type element of an interface: a single named type is an embedded
    /// interface (a base); unions and `~T` terms are constraints (references).
    fn interface_type_elem(&mut self, owner: LocalDefId, elem: Node<'_>) {
        let terms = named_children(elem);
        let embedded = terms.len() == 1
            && matches!(terms[0].kind(), "type_identifier" | "qualified_type" | "generic_type")
            && !self.text(elem).contains('|');
        let name = terms.first().map(|t| self.text(*t).to_string()).unwrap_or_default();
        let builtin = terms.len() == 1 && terms[0].kind() == "type_identifier" && is_predeclared(&name);
        if !embedded || builtin {
            self.visit_type(elem, 1);
            return;
        }
        self.defs[owner as usize].bases.push(name);
        if let Some(args) = generic_args(terms[0]) {
            self.visit_type(args, 1);
        }
    }

    /// Package-level `var` / `const`: one `Variable` / `Constant` per name.
    fn value_declaration(&mut self, node: Node<'_>) {
        let (label, keyword) =
            if node.kind() == "const_declaration" { (Label::Constant, "const") } else { (Label::Variable, "var") };
        let specs = value_specs(node);
        let single = specs.len() == 1;
        for spec in specs {
            let doc = doc_of(spec, self.source).or_else(|| single.then(|| doc_of(node, self.source)).flatten());
            let sig = {
                let t = self.text(spec);
                one_line(&format!("{keyword} {}", t.lines().next().unwrap_or("")), MAX_ONE_LINE)
            };
            let span = if single { span_of(node) } else { span_of(spec) };
            let mut first = None;
            for n in children_by_field(spec, "name") {
                let name = self.text(n);
                if name == "_" {
                    continue;
                }
                let d = self.push_def(
                    label,
                    name,
                    name.to_string(),
                    None,
                    span,
                    span_of(n),
                    Some(sig.clone()),
                    doc.clone(),
                );
                first.get_or_insert(d);
            }
            self.with_enclosing(first, None, |ex| ex.visit_spec_parts(spec, 1));
        }
    }

    /// The type (a reference) and the values (expressions) of a `var` /
    /// `const` spec.
    fn visit_spec_parts(&mut self, spec: Node<'_>, depth: usize) {
        if let Some(t) = spec.child_by_field_name("type") {
            self.visit_type(t, depth);
        }
        if let Some(v) = spec.child_by_field_name("value") {
            self.visit(v, depth);
        }
    }

    // -- references ----------------------------------------------------------

    fn push_ref(&mut self, kind: RefKind, name: &str, receiver: Option<String>, span: Span, arity: Option<u32>) {
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
        self.refs.push(Reference { kind, name: name.to_string(), receiver, enclosing, span, arity });
    }

    /// Type parameters and local types are not references.
    fn is_type_param(&self, name: &str, pos: u32) -> bool {
        self.locals.lookup(name, pos).is_some_and(|b| matches!(b.kind, DefKind::TypeParameter | DefKind::Type))
    }

    /// Type references in a type expression (and expressions inside it,
    /// such as array lengths).
    fn visit_type(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "type_identifier" => self.named_type_ref(node, node.start_byte() as u32, RefKind::Type),
            "qualified_type" => self.qualified_type_ref(node, RefKind::Type),
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration"
            | "field_declaration" => {
                if let Some(t) = node.child_by_field_name("type") {
                    self.visit_type(t, depth + 1);
                }
            }
            "method_elem" => self.visit_signature_types(node, depth + 1),
            "comment" | "field_identifier" | "identifier" | "raw_string_literal" | "interpreted_string_literal" => {}
            k if is_type_container(k) => {
                for c in named_children(node) {
                    self.visit_type(c, depth + 1);
                }
            }
            // Expressions inside types (array lengths).
            _ => self.visit(node, depth + 1),
        }
    }

    /// A reference of `kind` to the type named by identifier `t` (not a
    /// predeclared type nor a type parameter in scope at `pos`).
    fn named_type_ref(&mut self, t: Node<'_>, pos: u32, kind: RefKind) {
        let name = self.text(t);
        if !is_predeclared_type(name) && !self.is_type_param(name, pos) {
            self.push_ref(kind, name, None, span_of(t), None);
        }
    }

    /// A reference of `kind` to `pkg.Name` through the qualifier `pkg`.
    fn qualified_type_ref(&mut self, t: Node<'_>, kind: RefKind) {
        let (Some(pkg), Some(name)) = (t.child_by_field_name("package"), t.child_by_field_name("name")) else {
            return;
        };
        let pkg = self.text(pkg).to_string();
        self.qualifiers.insert(pkg.clone());
        let name = self.text(name);
        self.push_ref(kind, name, Some(pkg), span_of(t), None);
    }

    /// Expressions and statements.
    fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            "comment" | "field_identifier" | "package_identifier" | "label_name" | "blank_identifier" => {}
            "identifier" => self.identifier_read(node),
            "call_expression" => self.call(node, depth),
            "composite_literal" => self.composite_literal(node, depth),
            "selector_expression" => self.selector_read(node, depth),
            "func_literal" => {
                let enc = self.enclosing;
                self.with_enclosing(enc, None, |ex| ex.callable_parts(node));
            }
            "short_var_declaration" => self.visit_field(node, "right", depth + 1),
            "var_spec" | "const_spec" => self.visit_spec_parts(node, depth + 1),
            "type_declaration" => {
                // Local types: their field and underlying types are references.
                for spec in named_children(node) {
                    if let Some(t) = spec.child_by_field_name("type") {
                        self.visit_type(t, depth + 1);
                    }
                }
            }
            "assignment_statement" => self.assignment(node, true, depth),
            "inc_statement" | "dec_statement" => {
                for target in named_children(node) {
                    self.write_target(target, depth);
                }
            }
            "range_clause" | "receive_statement" => {
                let declares = {
                    let mut c = node.walk();
                    node.children(&mut c).any(|ch| ch.kind() == ":=")
                };
                self.assignment(node, !declares, depth);
            }
            "type_switch_statement" => self.type_switch(node, depth),
            "type_case" => self.type_case(node, depth),
            "type_assertion_expression" | "type_conversion_expression" => {
                self.visit_field(node, "operand", depth + 1);
                if let Some(t) = node.child_by_field_name("type") {
                    self.visit_type(t, depth + 1);
                }
            }
            "keyed_element" => self.keyed_element(node, depth),
            "parameter_declaration" | "variadic_parameter_declaration" => self.visit_type(node, depth + 1),
            k if is_type_kind(k) => self.visit_type(node, depth + 1),
            _ => {
                for c in named_children(node) {
                    self.visit(c, depth + 1);
                }
            }
        }
    }

    /// Visits the `field` child of `node` (if any) as an expression.
    fn visit_field(&mut self, node: Node<'_>, field: &str, depth: usize) {
        if let Some(c) = node.child_by_field_name(field) {
            self.visit(c, depth);
        }
    }

    /// `left = right` (and range / receive clauses): the `left` targets are
    /// writes when `writes`; `right` is visited.
    fn assignment(&mut self, node: Node<'_>, writes: bool, depth: usize) {
        if let Some(l) = node.child_by_field_name("left")
            && writes
        {
            for target in named_children(l) {
                self.write_target(target, depth);
            }
        }
        self.visit_field(node, "right", depth + 1);
    }

    /// `switch x := v.(type)`: every child but the alias.
    fn type_switch(&mut self, node: Node<'_>, depth: usize) {
        let alias = node.child_by_field_name("alias").map(|a| a.id());
        for c in named_children(node) {
            if alias == Some(c.id()) {
                continue;
            }
            self.visit(c, depth + 1);
        }
    }

    /// `case T1, T2:` of a type switch: the types are references, the
    /// statements visited.
    fn type_case(&mut self, node: Node<'_>, depth: usize) {
        for t in children_by_field(node, "type") {
            self.visit_type(t, depth + 1);
        }
        for c in named_children(node).into_iter().filter(|c| c.kind() == "statement_list") {
            self.visit(c, depth + 1);
        }
    }

    /// A non-local identifier in value position: a read of a package-level
    /// name (`Read`).
    fn identifier_read(&mut self, node: Node<'_>) {
        let name = self.text(node);
        if name == "_" || is_predeclared(name) || self.locals.is_local(node, self.source) {
            return;
        }
        self.push_ref(RefKind::Read, name, None, span_of(node), None);
    }

    /// `T{…}` → `New` of the named type; element values are visited.
    fn composite_literal(&mut self, node: Node<'_>, depth: usize) {
        if let Some(ty) = node.child_by_field_name("type") {
            self.literal_type(ty, depth);
        }
        if let Some(body) = node.child_by_field_name("body") {
            let map_literal = node.child_by_field_name("type").is_some_and(|t| t.kind() == "map_type");
            self.literal_body(body, map_literal, depth + 1);
        }
    }

    /// The type of a composite literal: `New` of a named (possibly generic)
    /// type, whose type arguments are references; other types are visited.
    fn literal_type(&mut self, ty: Node<'_>, depth: usize) {
        let pos = ty.start_byte() as u32;
        let named = match ty.kind() {
            "generic_type" => ty.child_by_field_name("type").map(|t| (t, generic_args(ty))),
            _ => Some((ty, None)),
        };
        let args = match named {
            Some((t, args)) if t.kind() == "type_identifier" => {
                self.named_type_ref(t, pos, RefKind::New);
                args
            }
            Some((t, args)) if t.kind() == "qualified_type" => {
                self.qualified_type_ref(t, RefKind::New);
                args
            }
            _ => {
                self.visit_type(ty, depth + 1);
                return;
            }
        };
        if let Some(a) = args {
            self.visit_type(a, depth + 1);
        }
    }

    fn literal_body(&mut self, body: Node<'_>, map_literal: bool, depth: usize) {
        for el in named_children(body) {
            if el.kind() == "keyed_element" && map_literal {
                for part in named_children(el) {
                    self.visit(part, depth + 1);
                }
            } else {
                self.visit(el, depth + 1);
            }
        }
    }

    /// `Key: value` in a struct literal: the key is a field name, not a
    /// reference (map literal keys are visited as expressions).
    fn keyed_element(&mut self, node: Node<'_>, depth: usize) {
        let parts = named_children(node);
        for (i, p) in parts.iter().enumerate() {
            let bare_key = i == 0
                && parts.len() == 2
                && named_children(*p).first().is_some_and(|k| k.kind() == "identifier")
                && named_children(*p).len() == 1;
            if bare_key {
                continue;
            }
            self.visit(*p, depth + 1);
        }
    }

    /// Assignment target: package-level variables and `recv.field` are
    /// `Write`s; other targets are visited as expressions.
    fn write_target(&mut self, target: Node<'_>, depth: usize) {
        match target.kind() {
            "identifier" => {
                let name = self.text(target);
                if name != "_" && !self.locals.is_local(target, self.source) && !is_predeclared(name) {
                    self.push_ref(RefKind::Write, name, None, span_of(target), None);
                }
            }
            "selector_expression" => {
                let (Some(op), Some(field)) =
                    (target.child_by_field_name("operand"), target.child_by_field_name("field"))
                else {
                    return;
                };
                if self.is_receiver_var(op) {
                    let f = self.text(field);
                    self.push_ref(RefKind::Write, f, Some(SELF_RECEIVER.to_string()), span_of(field), None);
                } else {
                    self.visit(op, depth + 1);
                }
            }
            _ => self.visit(target, depth + 1),
        }
    }

    /// Whether `node` is the identifier of the current method's receiver.
    fn is_receiver_var(&self, node: Node<'_>) -> bool {
        node.kind() == "identifier"
            && self.locals.lookup(self.text(node), node.start_byte() as u32).is_some_and(|b| b.receiver)
    }

    /// `x.f` not called: `pkg.Name` is a read of `Name` through `pkg`;
    /// `recv.field` a read of the field through `$self`.
    fn selector_read(&mut self, node: Node<'_>, depth: usize) {
        let (Some(op), Some(field)) = (node.child_by_field_name("operand"), node.child_by_field_name("field")) else {
            return;
        };
        let f = self.text(field);
        if op.kind() == "identifier" {
            let name = self.text(op);
            match self.locals.lookup(name, op.start_byte() as u32) {
                Some(b) if b.receiver => {
                    self.push_ref(RefKind::Read, f, Some(SELF_RECEIVER.to_string()), span_of(field), None);
                }
                Some(_) => {}
                None if is_predeclared(name) => {}
                None => {
                    self.qualifiers.insert(name.to_string());
                    if self.top_names.contains(name) {
                        // A package-level variable or type of this file.
                        self.push_ref(RefKind::Read, name, None, span_of(op), None);
                    } else {
                        self.push_ref(RefKind::Read, f, Some(name.to_string()), span_of(field), None);
                    }
                }
            }
            return;
        }
        self.visit(op, depth + 1);
    }

    /// The receiver text of a call `operand.M()` (crate docs, *Receivers*).
    fn receiver_for(&mut self, op: Node<'_>) -> String {
        if op.kind() == "identifier" {
            return self.identifier_receiver(op);
        }
        // `NewT().M()`, `T{}.M()`, `x.(*T).M()`: the operand itself is typed.
        if let Some(t) = self.locals.expr_type(op, self.source, &self.file_types) {
            return type_receiver(&t);
        }
        // A chain `root.a.b()…`: follow declared field types from a typed
        // root while they are known.
        let (root, segs) = self.receiver_chain(op);
        if segs.is_empty() {
            return self.raw_receiver(op);
        }
        let ty = if root.kind() == "identifier" {
            let name = self.text(root);
            if !self.locals.is_local(root, self.source) {
                self.qualifiers.insert(name.to_string());
                if self.import_names.contains(name) {
                    // `pkg.F().M()`: through a package, written without the
                    // calls so a third-party package makes it external.
                    let rest: Vec<&str> = segs.iter().flatten().copied().collect();
                    return format!("{name}.{}", rest.join("."));
                }
            }
            self.ident_type(root)
        } else {
            self.locals.expr_type(root, self.source, &self.file_types)
        };
        self.follow_chain(ty, &segs).unwrap_or_else(|| self.raw_receiver(op))
    }

    /// The receiver of a call on an identifier operand.
    fn identifier_receiver(&mut self, op: Node<'_>) -> String {
        let name = self.text(op);
        match self.locals.lookup(name, op.start_byte() as u32) {
            Some(b) if b.receiver => SELF_RECEIVER.to_string(),
            Some(b) => match self.ident_type(op) {
                Some(t) => type_receiver(&t),
                None if b.origin.is_some() => b.origin.clone().unwrap_or_default(),
                // An untyped local named like an import or a package
                // name must not bind to it.
                None if self.import_names.contains(name) || self.top_names.contains(name) => format!("({name})"),
                None => name.to_string(),
            },
            None => {
                self.qualifiers.insert(name.to_string());
                match self.ident_type(op) {
                    Some(t) => type_receiver(&t),
                    None => name.to_string(),
                }
            }
        }
    }

    /// The root of a chain `root.a.b()[i]…` and its segments in source
    /// order: `Some(field)` for a selector, `None` for a call or an index.
    fn receiver_chain<'t>(&self, op: Node<'t>) -> (Node<'t>, Vec<Option<&'s str>>) {
        let mut segs: Vec<Option<&'s str>> = Vec::new();
        let mut root = op;
        loop {
            let (seg, next) = match root.kind() {
                "selector_expression" => {
                    let (Some(o), Some(f)) = (root.child_by_field_name("operand"), root.child_by_field_name("field"))
                    else {
                        break;
                    };
                    (Some(self.text(f)), o)
                }
                "call_expression" | "index_expression" => {
                    let next = root.child_by_field_name("function").or_else(|| root.child_by_field_name("operand"));
                    let Some(next) = next else { break };
                    (None, next)
                }
                "parenthesized_expression" => match named_children(root).as_slice() {
                    [inner] => {
                        root = *inner;
                        continue;
                    }
                    _ => break,
                },
                _ => break,
            };
            segs.push(seg);
            root = next;
        }
        segs.reverse();
        (root, segs)
    }

    /// The receiver after following `segs` through declared field types
    /// from the root type `ty`; `None` when the chain cannot be typed.
    fn follow_chain(&self, mut ty: Option<String>, segs: &[Option<&str>]) -> Option<String> {
        for (i, seg) in segs.iter().enumerate() {
            let t = ty.as_deref()?;
            if t.contains('.') {
                // A package-qualified type: write the rest of the chain after
                // it, so a chain through a third-party type is external.
                let rest: Vec<&str> = segs[i..].iter().flatten().copied().collect();
                return Some(if rest.is_empty() { type_receiver(t) } else { format!("{t}.{}", rest.join(".")) });
            }
            ty = seg.and_then(|field| self.struct_fields.get(t).and_then(|fs| fs.get(field)).cloned());
        }
        ty.map(|t| type_receiver(&t))
    }

    /// Declared type of an identifier use: the method receiver's type, a
    /// typed local (`err` is `error` by convention when untyped), a typed
    /// package-level variable of this file.
    fn ident_type(&self, ident: Node<'_>) -> Option<String> {
        let name = self.text(ident);
        match self.locals.lookup(name, ident.start_byte() as u32) {
            Some(b) if b.receiver => self.recv_type.clone(),
            Some(b) => self
                .locals
                .type_of_use(b, ident, self.source)
                .or_else(|| (name == "err" && b.kind != DefKind::TypeSwitch).then(|| "error".to_string())),
            None if self.import_names.contains(name) => None,
            None => self.file_types.vars.get(name).cloned(),
        }
    }

    fn raw_receiver(&self, op: Node<'_>) -> String {
        one_line(self.text(op), MAX_ONE_LINE)
    }

    fn call(&mut self, node: Node<'_>, depth: usize) {
        let Some(func) = node.child_by_field_name("function") else { return };
        let args = node.child_by_field_name("arguments");
        let arg_nodes = args.map(named_children).unwrap_or_default();
        let arity = Some(arg_nodes.len() as u32);
        if let Some(ta) = node.child_by_field_name("type_arguments") {
            self.visit_type(ta, depth + 1);
        }
        let func = self.callee(func, depth);
        match func.kind() {
            "identifier" => {
                let name = self.text(func);
                if !self.locals.is_local(func, self.source) {
                    self.push_ref(RefKind::Call, name, None, span_of(func), arity);
                }
                if name == "new" || name == "make" {
                    self.new_or_make_args(&arg_nodes, depth);
                    return;
                }
            }
            "selector_expression" => {
                let (Some(op), Some(field)) = (func.child_by_field_name("operand"), func.child_by_field_name("field"))
                else {
                    return;
                };
                self.method_call(node, op, field, &arg_nodes, arity, depth);
            }
            _ => self.visit(func, depth + 1),
        }
        for a in arg_nodes {
            self.visit(a, depth + 1);
        }
    }

    /// The called function under parentheses and generic instantiations:
    /// `(f)(x)`, `F[int](x)` (of a non-local generic function); the type
    /// arguments of an instantiation are references.
    fn callee<'t>(&mut self, mut func: Node<'t>, depth: usize) -> Node<'t> {
        loop {
            match func.kind() {
                "parenthesized_expression" => match named_children(func).as_slice() {
                    [inner] => func = *inner,
                    _ => return func,
                },
                "index_expression" | "type_instantiation_expression" => {
                    let Some(o) = self.instantiated_operand(func) else { return func };
                    for c in named_children(func).into_iter().skip(1) {
                        self.visit_type(c, depth + 1);
                    }
                    func = o;
                }
                _ => return func,
            }
        }
    }

    /// The operand of `f[T]` when it is the instantiation of a non-local
    /// generic function (or of `pkg.F`): only an index that is a type makes
    /// it one, `handlers[i](c)` calls a slice element.
    fn instantiated_operand<'t>(&self, func: Node<'t>) -> Option<Node<'t>> {
        let operand = func.child_by_field_name("operand").or_else(|| named_children(func).first().copied());
        let index_is_type = named_children(func)
            .iter()
            .skip(1)
            .all(|c| is_type_kind(c.kind()) || (c.kind() == "identifier" && is_predeclared_type(self.text(*c))));
        operand.filter(|o| {
            index_is_type
                && match o.kind() {
                    "identifier" => !self.locals.is_local(*o, self.source),
                    "selector_expression" => true,
                    _ => false,
                }
        })
    }

    /// The arguments of `new(T)` / `make(T, …)`: the first is a type, the
    /// rest are expressions.
    fn new_or_make_args(&mut self, arg_nodes: &[Node<'_>], depth: usize) {
        if let Some(first) = arg_nodes.first() {
            if first.kind() == "identifier" && !self.locals.is_local(*first, self.source) {
                self.named_type_ref(*first, first.start_byte() as u32, RefKind::Type);
            } else if first.kind() == "selector_expression" {
                self.selector_read(*first, depth + 1);
            } else {
                self.visit_type(*first, depth + 1);
            }
        }
        for a in arg_nodes.iter().skip(1) {
            self.visit(*a, depth + 1);
        }
    }

    /// `op.name(args)`: a `Call` through the receiver of `op`, the env /
    /// config bridges of a package qualifier, and the operand expression.
    fn method_call(
        &mut self,
        call: Node<'_>,
        op: Node<'_>,
        field: Node<'_>,
        arg_nodes: &[Node<'_>],
        arity: Option<u32>,
        depth: usize,
    ) {
        let name = self.text(field);
        let receiver = self.receiver_for(op);
        self.push_ref(RefKind::Call, name, Some(receiver), span_of(field), arity);
        match op.kind() {
            "identifier" => {
                if !self.locals.is_local(op, self.source) {
                    self.bridge(self.text(op), name, arg_nodes, call);
                }
            }
            "selector_expression" => self.selector_read(op, depth + 1),
            _ => self.visit(op, depth + 1),
        }
    }

    /// Env var and config key reads (RFC 0003 §7.2): `os.Getenv("X")`,
    /// `os.LookupEnv("X")`, `viper.GetString("a.b")` with `os` / `viper`
    /// bound by this file's imports and a literal first argument.
    fn bridge(&mut self, qualifier: &str, method: &str, args: &[Node<'_>], call: Node<'_>) {
        let Some(lit) = args.first().and_then(|a| string_value(*a, self.source)) else { return };
        if lit.is_empty() {
            return;
        }
        let span = span_of(call);
        if self.os_names.contains(qualifier) && matches!(method, "Getenv" | "LookupEnv") {
            self.push_ref(RefKind::EnvVar, &lit, None, span, None);
        } else if self.viper_names.contains(qualifier) {
            if method == "SetConfigName" {
                let pos = call.start_byte() as u32;
                if self.viper_config.as_ref().is_none_or(|(p, _)| pos < *p) {
                    self.viper_config = Some((pos, lit));
                }
            } else if method.starts_with("Get") || matches!(method, "IsSet" | "UnmarshalKey" | "Sub") {
                self.config_refs.push(self.refs.len());
                self.push_ref(RefKind::ConfigKey, &lit, Some("viper".into()), span, None);
            }
        }
    }

    fn finish_config_refs(&mut self) {
        let Some((_, name)) = &self.viper_config else { return };
        let ns = format!("viper:{name}");
        for &i in &self.config_refs {
            if let Some(r) = self.refs.get_mut(i) {
                r.receiver = Some(ns.clone());
            }
        }
    }

    /// Binding names of unaliased imports whose conventional name is never
    /// used in the file (crate docs, *Imports*): Go rejects unused imports,
    /// so the package is bound under another name. A qualifier that no
    /// import, local or package-level name explains and that the import
    /// path contains (`jsoniter` for `github.com/json-iterator/go`) is that
    /// name; failing that, a single unexplained qualifier for a single
    /// unmatched import.
    fn finish_imports(&mut self) {
        let explicit: HashSet<String> = self
            .imports
            .iter()
            .filter_map(|i| i.module_alias.clone())
            .filter(|a| self.qualifiers.contains(a))
            .collect();
        let mut unmatched: Vec<usize> = self
            .imports
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                i.names.is_empty()
                    && i.module_alias.as_ref().is_none_or(|a| !self.qualifiers.contains(a))
                    && !self.is_aliased_in_source(i)
            })
            .map(|(k, _)| k)
            .collect();
        if unmatched.is_empty() {
            return;
        }
        let mut free: Vec<String> = self
            .qualifiers
            .iter()
            .filter(|q| !explicit.contains(*q) && !self.top_names.contains(*q) && !is_predeclared(q))
            .cloned()
            .collect();
        free.sort();
        let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let mut assigned = Vec::new();
        for &k in &unmatched {
            let path = norm(&self.imports[k].module);
            let cands: Vec<&String> =
                free.iter().filter(|q| q.len() >= 2 && path.contains(&q.to_lowercase())).collect();
            if let [only] = cands.as_slice() {
                assigned.push((k, (*only).clone()));
            }
        }
        for (k, q) in &assigned {
            free.retain(|f| f != q);
            self.imports[*k].module_alias = Some(q.clone());
        }
        unmatched.retain(|k| !assigned.iter().any(|(a, _)| a == k));
        if let ([k], [q]) = (unmatched.as_slice(), free.as_slice()) {
            self.imports[*k].module_alias = Some(q.clone());
        }
    }

    /// Whether an import spec had an explicit name in the source (aliases
    /// are kept even when unused, e.g. in files with parse errors).
    fn is_aliased_in_source(&self, imp: &Import) -> bool {
        let start = imp.span.start_byte as usize;
        let end = imp.span.end_byte as usize;
        let t = std::str::from_utf8(&self.source[start..end.min(self.source.len())]).unwrap_or("");
        !t.trim_start().starts_with(['"', '`'])
    }
}

/// The specs of a package-level `var` / `const` declaration, in source
/// order (one `var ( … )` list level flattened).
fn value_specs(node: Node<'_>) -> Vec<Node<'_>> {
    let mut specs = Vec::new();
    for c in named_children(node) {
        match c.kind() {
            "var_spec" | "const_spec" => specs.push(c),
            "var_spec_list" => specs.extend(named_children(c).into_iter().filter(|s| s.kind() == "var_spec")),
            _ => {}
        }
    }
    specs
}

/// Last name of an embedded type text: `*pkg.Base[T]` → `Base`.
fn embedded_name(text: &str) -> String {
    let t = text.trim_start_matches('*');
    let t = t.split(['[', '(']).next().unwrap_or(t);
    t.rsplit('.').next().unwrap_or(t).trim().to_string()
}

/// `type_arguments` of a generic type node (through a pointer).
fn generic_args(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "generic_type" => node.child_by_field_name("type_arguments"),
        "pointer_type" => named_children(node).first().and_then(|n| generic_args(*n)),
        _ => None,
    }
}

/// Type nodes and the lists inside them whose children are types.
fn is_type_container(kind: &str) -> bool {
    is_type_kind(kind) || matches!(kind, "parameter_list" | "type_parameter_list" | "field_declaration_list")
}

fn is_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "qualified_type"
            | "generic_type"
            | "pointer_type"
            | "slice_type"
            | "array_type"
            | "implicit_length_array_type"
            | "map_type"
            | "channel_type"
            | "function_type"
            | "struct_type"
            | "interface_type"
            | "parenthesized_type"
            | "negated_type"
            | "type_elem"
            | "type_arguments"
            | "type_constraint"
    )
}
