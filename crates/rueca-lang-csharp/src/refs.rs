//! Expressions, references, receivers and type references of the C#
//! extractor (crate docs, *References*).

use rueca_extract::tree_sitter::Node;
use rueca_extract::{span_of, type_receiver, with_arg_types};
use rueca_model::RefKind;

use crate::extract::{
    Extractor, MAX_RECEIVER, MAX_REFS, arity_of, attribute_simple_name, collapse, is_comment, truncate_chars,
};
use crate::types::{children_with_fields, class_of_type, named_children, strip_type_text, suffix_type};

/// Prefix of a receiver typed from a callee's declared return type
/// (`$ret:<receiver>#<name>`, `?` = awaited).
pub(crate) const RET_PREFIX: &str = "$ret:";
/// Calls of a chain typed through `$ret:` receivers.
const MAX_RET_DEPTH: usize = 3;

/// Skips parentheses around an expression.
fn unwrap_parens(mut n: Node<'_>) -> Node<'_> {
    while n.kind() == "parenthesized_expression" {
        match n.named_child(0) {
            Some(c) => n = c,
            None => break,
        }
    }
    n
}

/// Static type of a literal argument, in overload-suffix form.
fn literal_type(kind: &str, t: &str) -> Option<&'static str> {
    let ty = match kind {
        "string_literal" | "verbatim_string_literal" | "raw_string_literal" | "interpolated_string_expression" => {
            "string"
        }
        "character_literal" => "char",
        "boolean_literal" => "bool",
        "integer_literal" => {
            let t = t.to_ascii_lowercase();
            if t.ends_with("ul") || t.ends_with("lu") {
                "ulong"
            } else if t.ends_with('l') {
                "long"
            } else if t.ends_with('u') {
                "uint"
            } else {
                "int"
            }
        }
        "real_literal" => match t.chars().last().map(|c| c.to_ascii_lowercase()) {
            Some('f') => "float",
            Some('m') => "decimal",
            _ => "double",
        },
        "typeof_expression" => "Type",
        _ => return None,
    };
    Some(ty)
}

/// Whether a node is a string literal of any form.
pub(crate) fn is_string_node(kind: &str) -> bool {
    matches!(
        kind,
        "string_literal" | "verbatim_string_literal" | "raw_string_literal" | "interpolated_string_expression"
    )
}

/// Content of a string literal without quotes and escapes processing;
/// `None` for interpolated strings with interpolations.
pub(crate) fn string_value(node: Node<'_>, source: &[u8]) -> Option<String> {
    let t = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).ok()?;
    match node.kind() {
        "string_literal" => Some(t.strip_prefix('"')?.strip_suffix('"')?.replace("\\\\", "\\")),
        "verbatim_string_literal" => Some(t.strip_prefix("@\"")?.strip_suffix('"')?.replace("\"\"", "\"")),
        "raw_string_literal" => Some(t.trim_matches('"').trim().to_string()),
        "interpolated_string_expression" if !named_children(node).iter().any(|c| c.kind() == "interpolation") => {
            Some(t.trim_start_matches(['$', '@']).trim_matches('"').to_string())
        }
        _ => None,
    }
}

/// The expression of an `argument` (named arguments and `ref` / `out`
/// modifiers skipped).
pub(crate) fn argument_expr(arg: Node<'_>) -> Option<Node<'_>> {
    let name = arg.child_by_field_name("name");
    let named =
        named_children(arg).into_iter().find(|c| Some(*c) != name && c.kind() != "modifier" && !is_comment(c.kind()));
    // `this` / `base` are keywords: unnamed nodes.
    named.or_else(|| {
        let mut cursor = arg.walk();
        arg.children(&mut cursor).find(|c| matches!(c.kind(), "this" | "base"))
    })
}

/// The `argument` nodes of an `argument_list`.
pub(crate) fn arguments(args: Node<'_>) -> Vec<Node<'_>> {
    named_children(args).into_iter().filter(|c| c.kind() == "argument").collect()
}

impl<'s> Extractor<'s> {
    // -----------------------------------------------------------------------
    // Argument types
    // -----------------------------------------------------------------------

    /// Static type of an argument expression in overload-suffix form, when
    /// the file says it.
    pub(crate) fn arg_type(&self, node: Node<'_>) -> Option<String> {
        if let Some(t) = literal_type(node.kind(), self.text(node)) {
            return Some(t.to_string());
        }
        let ty = match node.kind() {
            "parenthesized_expression" => return self.arg_type(node.named_child(0)?),
            "cast_expression" | "object_creation_expression" | "declaration_expression" => {
                suffix_type(self.text(node.child_by_field_name("type")?))
            }
            "as_expression" => suffix_type(self.text(node.child_by_field_name("right")?)),
            "this" | "this_expression" => self.types.last()?.name.clone(),
            "identifier" => self.identifier_arg_type(node)?,
            "member_access_expression" => self.this_member_arg_type(node)?,
            "binary_expression" => self.binary_arg_type(node)?,
            "invocation_expression" => self.call_arg_type(node)?,
            _ => return None,
        };
        // A value of a type parameter is of an unknown type.
        let head = ty.split(['[', '.']).next().unwrap_or(&ty);
        (!ty.is_empty() && !self.is_type_param(head)).then_some(ty)
    }

    /// A local or parameter with a declared type, else a field / property
    /// of an enclosing type.
    fn identifier_arg_type(&self, node: Node<'_>) -> Option<String> {
        let t = self.text(node);
        match self.locals.lookup(t, node.start_byte() as u32) {
            Some(b) => b.ty.clone(),
            None => self.field_type(t),
        }
    }

    /// `this.x`: the declared type of the field / property.
    fn this_member_arg_type(&self, node: Node<'_>) -> Option<String> {
        let obj = node.child_by_field_name("expression")?;
        let name = node.child_by_field_name("name")?;
        if obj.kind() != "this" {
            return None;
        }
        self.types.last()?.field_types.get(self.text(name)).cloned()
    }

    /// Comparisons and logical operators are `bool`; `+` with a `string`
    /// operand is a concatenation.
    fn binary_arg_type(&self, node: Node<'_>) -> Option<String> {
        let op = node.child_by_field_name("operator").map(|o| o.kind())?;
        match op {
            "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Some("bool".to_string()),
            "+" => {
                let l = node.child_by_field_name("left").and_then(|l| self.arg_type(l));
                let r = node.child_by_field_name("right").and_then(|r| self.arg_type(r));
                (l.as_deref() == Some("string") || r.as_deref() == Some("string")).then(|| "string".to_string())
            }
            _ => None,
        }
    }

    /// A call of a method of an enclosing type of this file: its declared
    /// return type; `nameof(…)` is a `string`.
    fn call_arg_type(&self, node: Node<'_>) -> Option<String> {
        let f = node.child_by_field_name("function")?;
        let name = match f.kind() {
            "identifier" => self.text(f),
            "member_access_expression" if f.child_by_field_name("expression").is_some_and(|o| o.kind() == "this") => {
                self.text(f.child_by_field_name("name")?)
            }
            _ => return None,
        };
        if name == "nameof" {
            return Some("string".to_string());
        }
        Some(self.method_returns_of(name)?.ty)
    }

    /// `name` with the static types of the arguments of `args` (see
    /// `rueca_extract::with_arg_types`).
    pub(crate) fn with_args(&self, name: &str, args: Option<Node<'_>>) -> String {
        let Some(args) = args else { return name.to_string() };
        let types: Vec<Option<String>> =
            arguments(args).into_iter().map(|a| argument_expr(a).and_then(|e| self.arg_type(e))).collect();
        with_arg_types(name, &types)
    }

    // -----------------------------------------------------------------------
    // Receivers
    // -----------------------------------------------------------------------

    /// `$type:` receiver for a class text, unless it is a type parameter
    /// (a constrained one gives its first class constraint).
    pub(crate) fn typed(&self, class: Option<String>) -> Option<String> {
        let c = class?;
        let head = c.split('.').next().unwrap_or(&c);
        if let Some(i) = self.type_params.iter().rposition(|t| t == head) {
            // A constrained type parameter (`where T : IEntity`): its bound.
            let bound = self.type_bounds.get(i).cloned().flatten()?;
            let bound_head = bound.split('.').next().unwrap_or(&bound);
            return (!self.is_type_param(bound_head) && c == head).then(|| type_receiver(&bound));
        }
        let c = match self.aliases.get(head) {
            Some(target) if c == head => target.clone(),
            _ => c,
        };
        Some(type_receiver(&c))
    }

    /// Raw receiver text: whitespace collapsed, bounded.
    pub(crate) fn raw(&self, node: Node<'_>) -> String {
        truncate_chars(collapse(self.text(node)), MAX_RECEIVER)
    }

    /// The identifiers of a dotted path (`a`, `a.b.c`), or `None`.
    pub(crate) fn identifier_path(&self, node: Node<'_>) -> Option<Vec<&'s str>> {
        match node.kind() {
            "identifier" => Some(vec![self.text(node)]),
            "member_access_expression" | "qualified_name" => {
                let (obj, name) = match node.kind() {
                    "qualified_name" => (node.child_by_field_name("qualifier")?, node.child_by_field_name("name")?),
                    _ => (node.child_by_field_name("expression")?, node.child_by_field_name("name")?),
                };
                if name.kind() != "identifier" {
                    return None;
                }
                let mut v = self.identifier_path(obj)?;
                v.push(self.text(name));
                Some(v)
            }
            _ => None,
        }
    }

    /// The receiver text of a call / member access on `object` (crate
    /// docs, *References*).
    pub(crate) fn receiver(&self, object: Node<'_>) -> String {
        match object.kind() {
            "this" => "this".to_string(),
            "base" => "base".to_string(),
            "identifier" => self.identifier_receiver(object),
            "member_access_expression" => self.member_receiver(object),
            "parenthesized_expression" | "postfix_unary_expression" => match object.named_child(0) {
                Some(inner) => self.receiver(inner),
                None => self.raw(object),
            },
            "cast_expression" | "object_creation_expression" => {
                let class = object.child_by_field_name("type").and_then(|t| class_of_type(t, self.source));
                self.typed_or_raw(class, object)
            }
            "as_expression" => {
                let class = object.child_by_field_name("right").and_then(|t| class_of_type(t, self.source));
                self.typed_or_raw(class, object)
            }
            k if is_string_node(k) => type_receiver("string"),
            "predefined_type" => self.text(object).to_string(),
            "generic_name" | "qualified_name" | "alias_qualified_name" => strip_type_text(self.text(object)),
            "invocation_expression" => self.call_receiver(object, false, 0),
            "await_expression" => match object.named_child(0).map(unwrap_parens) {
                Some(inner) if inner.kind() == "invocation_expression" => self.call_receiver(inner, true, 0),
                _ => self.raw(object),
            },
            _ => self.raw(object),
        }
    }

    /// The receiver of a call's result: the declared return class of an own
    /// method, else `$ret:<receiver>#<name>` (`?` when awaited) for the
    /// resolver to type from the callee's signature (crate docs,
    /// *References*); chains deeper than [`MAX_RET_DEPTH`] are raw.
    fn call_receiver(&self, call: Node<'_>, awaited: bool, depth: usize) -> String {
        if !awaited && let Some(c) = self.own_call_return_class(call) {
            return self.typed(Some(c)).unwrap_or_else(|| self.raw(call));
        }
        let Some(function) = call.child_by_field_name("function") else { return self.raw(call) };
        let Some(name) = self.called_name(function) else { return self.raw(call) };
        if depth >= MAX_RET_DEPTH {
            return self.raw(call);
        }
        let inner = match self.call_object(function) {
            None => String::new(),
            Some(o) if o.kind() == "invocation_expression" => self.call_receiver(o, false, depth + 1),
            Some(o) => self.receiver(o),
        };
        if inner.contains('#') && !inner.starts_with(RET_PREFIX) {
            return self.raw(call);
        }
        let mark = if awaited { "?" } else { "" };
        format!("{RET_PREFIX}{inner}#{}{mark}", self.text(name))
    }

    /// The return class of a call of an own method (`M()`, `this.M()`).
    fn own_call_return_class(&self, call: Node<'_>) -> Option<String> {
        let f = call.child_by_field_name("function")?;
        let name = match f.kind() {
            "identifier" => self.text(f),
            "generic_name" => self.text(f.named_child(0)?),
            "member_access_expression" if f.child_by_field_name("expression").is_some_and(|o| o.kind() == "this") => {
                let n = f.child_by_field_name("name")?;
                if n.kind() == "generic_name" { self.text(n.named_child(0)?) } else { self.text(n) }
            }
            _ => return None,
        };
        self.method_returns_of(name)?.class
    }

    fn typed_or_raw(&self, class: Option<String>, object: Node<'_>) -> String {
        self.typed(class).unwrap_or_else(|| self.raw(object))
    }

    /// Receiver of a bare identifier: the class of a local, of a field /
    /// property or a visible type, else the name itself.
    fn identifier_receiver(&self, object: Node<'_>) -> String {
        let name = self.text(object);
        let class = if let Some(b) = self.locals.lookup(name, object.start_byte() as u32) {
            b.class.clone()
        } else if let Some(class) = self.field_class(name) {
            class
        } else if self.visible_type(name).is_some() {
            Some(name.to_string())
        } else {
            return name.to_string();
        };
        self.typed(class).unwrap_or_else(|| name.to_string())
    }

    /// Receiver of `this.x` (the field's class), `Outer.Inner` (a type of
    /// this file), or a dotted path (`A.B.Type`, `x.y`).
    fn member_receiver(&self, object: Node<'_>) -> String {
        let obj = object.child_by_field_name("expression");
        let name = object.child_by_field_name("name");
        if let (Some(o), Some(n)) = (obj, name)
            && o.kind() == "this"
        {
            let class = self.types.last().and_then(|t| t.fields.get(self.text(n)).cloned()).flatten();
            return self.typed_or_raw(class, object);
        }
        match self.identifier_path(object) {
            Some(path)
                if !self.locals.is_local(path[0], object.start_byte() as u32)
                    && self.field_class(path[0]).is_none() =>
            {
                let t = path.join(".");
                if self.visible_type(path[0]).is_some() { self.typed(Some(t.clone())).unwrap_or(t) } else { t }
            }
            _ => self.raw(object),
        }
    }

    // -----------------------------------------------------------------------
    // Types
    // -----------------------------------------------------------------------

    /// Name and receiver of a named type node (`Foo`, `A.B.Foo`,
    /// `Outer.Inner<T>`), for `Type` / `New` / `Throw` references; `None`
    /// for keywords, arrays and type parameters. A qualifier that is a type
    /// visible here is a `$type:` receiver.
    pub(crate) fn type_name(&self, ty: Node<'_>) -> Option<(String, Option<String>)> {
        let class = match ty.kind() {
            "identifier" | "qualified_name" | "generic_name" | "alias_qualified_name" => {
                class_of_type(ty, self.source)?
            }
            "nullable_type" | "ref_type" | "scoped_type" => return self.type_name(ty.child_by_field_name("type")?),
            _ => return None,
        };
        if matches!(class.as_str(), "string" | "object") {
            return None;
        }
        let head = class.split('.').next().unwrap_or(&class).to_string();
        if self.is_type_param(&head) {
            return None;
        }
        Some(match class.rsplit_once('.') {
            Some((q, n)) => {
                let receiver = if self.visible_type(&head).is_some() {
                    self.typed(Some(q.to_string())).unwrap_or_else(|| q.to_string())
                } else {
                    q.to_string()
                };
                (n.to_string(), Some(receiver))
            }
            None => (class, None),
        })
    }

    /// `Type` references for every class a type node mentions (generic
    /// arguments, array elements, tuple elements, nullable and pointer
    /// targets).
    pub(crate) fn visit_type(&mut self, ty: Node<'_>, depth: usize) {
        self.visit_type_as(ty, RefKind::Type, depth);
    }

    pub(crate) fn visit_type_as(&mut self, ty: Node<'_>, kind: RefKind, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match ty.kind() {
            "identifier" | "qualified_name" | "generic_name" | "alias_qualified_name" => {
                if let Some((name, receiver)) = self.type_name(ty) {
                    self.push_ref(kind, &name, receiver, span_of(ty), None);
                }
                self.visit_type_args(ty, depth + 1);
            }
            "nullable_type" | "array_type" | "pointer_type" | "ref_type" | "scoped_type" => {
                if let Some(t) = ty.child_by_field_name("type") {
                    self.visit_type_as(t, kind, depth + 1);
                }
            }
            "tuple_type" => {
                for e in named_children(ty) {
                    if let Some(t) = e.child_by_field_name("type") {
                        self.visit_type(t, depth + 1);
                    }
                }
            }
            "type_argument_list" => {
                for c in named_children(ty) {
                    self.visit_type(c, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// `Type` references for the generic arguments of a type, not the type
    /// itself.
    pub(crate) fn visit_type_args(&mut self, ty: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for c in named_children(ty) {
            match c.kind() {
                "type_argument_list" => self.visit_type_as(c, RefKind::Type, depth + 1),
                "generic_name" | "qualified_name" => self.visit_type_args(c, depth + 1),
                _ => {}
            }
        }
    }

    /// An attribute: a `Decorator` reference when it annotates a definition
    /// (`as_decorator`), else a `Type` reference, to the attribute class
    /// (`[Foo]` names `FooAttribute`); its arguments are visited.
    pub(crate) fn visit_attribute(&mut self, node: Node<'_>, as_decorator: bool, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let full = strip_type_text(self.text(name_node));
        let (receiver, name) = match full.rsplit_once('.') {
            Some((q, n)) => (Some(q.to_string()), n.to_string()),
            None => (None, full.clone()),
        };
        let class = format!("{}Attribute", attribute_simple_name(&name));
        let kind = if as_decorator { RefKind::Decorator } else { RefKind::Type };
        self.push_ref(kind, &class, receiver, span_of(node), None);
        self.visit_type_args(name_node, depth + 1);
        for list in named_children(node).into_iter().filter(|c| c.kind() == "attribute_argument_list") {
            for a in named_children(list) {
                let skip = a.child_by_field_name("name");
                for c in named_children(a).into_iter().filter(|c| Some(*c) != skip) {
                    self.visit(c, depth + 1);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Expressions and statements
    // -----------------------------------------------------------------------

    pub(crate) fn visit_children(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            self.visit(c, depth + 1);
        }
    }

    /// Visits a node. Kept small (the recursion goes through it for every
    /// level of the tree): long left-nested chains are walked iteratively,
    /// every other construct is handled by [`Self::visit_construct`].
    pub(crate) fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            k if is_comment(k) => {}
            "binary_expression" => self.visit_binary(node, depth),
            "invocation_expression" => self.visit_call(node, depth),
            _ => {
                if !self.visit_construct(node, depth) {
                    self.visit_fields(node, depth);
                }
            }
        }
    }

    /// A node's children by field: types (`type`, `returns`) as types,
    /// declared names skipped, the rest as expressions.
    fn visit_fields(&mut self, node: Node<'_>, depth: usize) {
        for (c, field) in children_with_fields(node).into_iter().filter(|(c, _)| c.is_named()) {
            match field {
                Some("type" | "returns") => self.visit_type(c, depth + 1),
                Some("name") if c.kind() == "identifier" => {}
                _ => self.visit(c, depth + 1),
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
    /// generically (its children by field).
    #[inline(never)]
    fn visit_construct(&mut self, node: Node<'_>, depth: usize) -> bool {
        match node.kind() {
            "member_access_expression" => self.visit_member_access(node, depth),
            "conditional_access_expression" => self.visit_conditional_access(node, depth),
            "element_access_expression" => self.visit_element_access(node, depth),
            "identifier" => self.visit_identifier(node),
            "object_creation_expression" => self.visit_new(node, depth),
            "implicit_object_creation_expression" => self.visit_implicit_new(node, depth),
            "assignment_expression" => self.visit_assignment(node, depth),
            "local_function_statement" => self.visit_local_function(node, depth),
            "variable_declaration" => self.visit_variable_declaration(node, depth),
            "throw_statement" | "throw_expression" => self.visit_throw(node, depth),
            "lambda_expression" | "anonymous_method_expression" => self.visit_lambda(node, depth),
            "attribute_list" | "global_attribute" => {
                for a in named_children(node).into_iter().filter(|a| a.kind() == "attribute") {
                    self.visit_attribute(a, false, depth + 1);
                }
            }
            "is_expression" | "as_expression" => {
                self.visit_field_child(node, "left", depth);
                if let Some(r) = node.child_by_field_name("right") {
                    self.visit_type(r, depth + 1);
                }
            }
            "generic_name" | "type_argument_list" | "qualified_name" | "array_type" | "nullable_type" => {
                self.visit_type(node, depth);
            }
            "parameter_list" | "bracketed_parameter_list" => self.visit_parameters(node, depth),
            "predefined_type" | "this" | "base" | "implicit_type" | "modifier" | "label_name" => {}
            "goto_statement" | "break_statement" | "continue_statement" => {}
            k if crate::extract::is_type_decl(k) => self.visit_type_decl(node, "", depth),
            _ => return false,
        }
        true
    }

    pub(crate) fn visit_field_child(&mut self, node: Node<'_>, field: &str, depth: usize) {
        if let Some(c) = node.child_by_field_name(field) {
            self.visit(c, depth + 1);
        }
    }

    /// A local declaration: the type and the declarators' initializers.
    fn visit_variable_declaration(&mut self, node: Node<'_>, depth: usize) {
        if let Some(t) = node.child_by_field_name("type") {
            self.visit_type(t, depth + 1);
        }
        for d in named_children(node).into_iter().filter(|c| c.kind() == "variable_declarator") {
            self.visit_declarator(d, depth);
        }
    }

    /// A declarator's initializer (and array size).
    pub(crate) fn visit_declarator(&mut self, d: Node<'_>, depth: usize) {
        let name = d.child_by_field_name("name");
        for c in named_children(d) {
            if Some(c) != name && c.kind() != "tuple_pattern" {
                self.visit(c, depth + 1);
            }
        }
    }

    fn visit_throw(&mut self, node: Node<'_>, depth: usize) {
        if let Some(e) = node.named_child(0)
            && e.kind() == "object_creation_expression"
            && let Some(t) = e.child_by_field_name("type")
            && let Some((name, receiver)) = self.type_name(t)
        {
            self.push_ref(RefKind::Throw, &name, receiver, span_of(t), None);
        }
        self.visit_children(node, depth);
    }

    fn visit_lambda(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            match c.kind() {
                "parameter_list" => self.visit_parameters(c, depth + 1),
                "attribute_list" => {
                    self.visit_construct(c, depth + 1);
                }
                "modifier" | "implicit_parameter" | "identifier" => {}
                _ if Some(c) == node.child_by_field_name("type") => self.visit_type(c, depth + 1),
                _ => self.visit(c, depth + 1),
            }
        }
    }

    /// A bare identifier in an expression: a `Read` of a field or property
    /// of an enclosing type, unless it is a local; in a type whose members
    /// may be declared elsewhere (partial, or with a base class), also of
    /// an upper-case name that is not a type of this file (an inherited or
    /// other-part member, found by implicit `this`).
    pub(crate) fn visit_identifier(&mut self, node: Node<'_>) {
        let name = self.text(node);
        if self.locals.is_local(name, node.start_byte() as u32) {
            return;
        }
        if self.field_class(name).is_some() || self.member_elsewhere(name) {
            self.push_ref(RefKind::Read, name, None, span_of(node), None);
        }
    }

    /// Whether `name` may be a member of the innermost type declared in
    /// another file (see [`Self::visit_identifier`]).
    fn member_elsewhere(&self, name: &str) -> bool {
        self.types.last().is_some_and(|t| t.members_elsewhere)
            && name.starts_with(|c: char| c.is_ascii_uppercase())
            && self.visible_type(name).is_none()
            && !self.is_type_param(name)
            && !self.aliases.contains_key(name)
            && !crate::is_builtin_name(name)
    }

    fn visit_assignment(&mut self, node: Node<'_>, depth: usize) {
        if let Some(left) = node.child_by_field_name("left") {
            match left.kind() {
                "identifier" => self.write_identifier(left),
                "member_access_expression" => self.write_member(left, depth),
                _ => self.visit(left, depth + 1),
            }
        }
        self.visit_field_child(node, "right", depth);
    }

    /// An assigned bare identifier: a `Write` of a field / property of an
    /// enclosing type, unless it is a local.
    fn write_identifier(&mut self, left: Node<'_>) {
        let name = self.text(left);
        if !self.locals.is_local(name, left.start_byte() as u32) && self.field_class(name).is_some() {
            self.push_ref(RefKind::Write, name, None, span_of(left), None);
        }
    }

    /// An assigned `x.member`: a `Write` on a typed receiver, then the receiver.
    fn write_member(&mut self, left: Node<'_>, depth: usize) {
        let (Some(o), Some(f)) = (left.child_by_field_name("expression"), left.child_by_field_name("name")) else {
            return;
        };
        let receiver = self.receiver(o);
        if receiver == "this" || receiver.starts_with("$type:") {
            self.push_ref(RefKind::Write, self.text(f), Some(receiver), span_of(f), None);
        }
        match o.kind() {
            "identifier" => self.visit_identifier(o),
            "this" | "base" => {}
            _ => self.visit(o, depth + 1),
        }
    }

    // -----------------------------------------------------------------------
    // Calls
    // -----------------------------------------------------------------------

    /// The object a call's function is invoked on (`x` in `x.M()` /
    /// `x?.M()`), if any.
    fn call_object<'t>(&self, function: Node<'t>) -> Option<Node<'t>> {
        match function.kind() {
            "member_access_expression" => function.child_by_field_name("expression"),
            "conditional_access_expression" => function.child_by_field_name("condition"),
            _ => None,
        }
    }

    /// A call chain `x.A().B().C()` nests through its receivers: visited
    /// iteratively, innermost call first (source order).
    fn visit_call(&mut self, node: Node<'_>, depth: usize) {
        let mut chain = vec![node];
        let mut cur = node;
        while let Some(o) = cur.child_by_field_name("function").and_then(|f| self.call_object(f))
            && o.kind() == "invocation_expression"
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

    /// The called name node of a call's function (`M` of `x.M<T>()`).
    fn called_name<'t>(&self, function: Node<'t>) -> Option<Node<'t>> {
        let n = match function.kind() {
            "identifier" => function,
            "generic_name" => return function.named_child(0),
            "member_access_expression" => function.child_by_field_name("name")?,
            "conditional_access_expression" => named_children(function)
                .into_iter()
                .find(|c| c.kind() == "member_binding_expression")?
                .child_by_field_name("name")?,
            _ => return None,
        };
        if n.kind() == "generic_name" { n.named_child(0) } else { Some(n) }
    }

    fn visit_one_call(&mut self, node: Node<'_>, depth: usize) {
        let Some(function) = node.child_by_field_name("function") else { return self.visit_children(node, depth) };
        let args = node.child_by_field_name("arguments");
        let Some(name_node) = self.called_name(function) else {
            self.visit(function, depth + 1);
            if let Some(a) = args {
                self.visit_children(a, depth);
            }
            return;
        };
        let name = self.text(name_node).to_string();
        let object = self.call_object(function);
        if object.is_none() && name == "nameof" {
            return;
        }
        let local_delegate = object.is_none() && self.locals.is_local(&name, name_node.start_byte() as u32);
        let receiver = object.map(|o| self.receiver(o));
        let bridged = self.bridge_call(&name, receiver.as_deref(), object, args, node);
        if !bridged && !local_delegate {
            let named = self.with_args(&name, args);
            self.push_ref(RefKind::Call, &named, receiver, span_of(name_node), args.map(arity_of));
        }
        if let Some(o) = object {
            match o.kind() {
                "identifier" => self.visit_identifier(o),
                "this" | "base" | "invocation_expression" | "predefined_type" => {}
                _ => self.visit(o, depth + 1),
            }
        }
        self.visit_name_type_args(function, depth);
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
    }

    /// Generic arguments of a called name (`M<Foo>()`).
    fn visit_name_type_args(&mut self, function: Node<'_>, depth: usize) {
        let n = match function.kind() {
            "generic_name" => Some(function),
            "member_access_expression" => function.child_by_field_name("name"),
            _ => None,
        };
        if let Some(g) = n.filter(|n| n.kind() == "generic_name") {
            self.visit_type_args(g, depth + 1);
        }
    }

    /// `x.f`, `this.f`, `Type.Member`, `a.b.Type.Member`: the `Read` of the
    /// member (typed receivers, `this`, type paths) and of a head field.
    fn visit_member_access(&mut self, node: Node<'_>, depth: usize) {
        let (Some(obj), Some(field)) = (node.child_by_field_name("expression"), node.child_by_field_name("name"))
        else {
            return self.visit_children(node, depth);
        };
        let name = if field.kind() == "generic_name" { field.named_child(0).unwrap_or(field) } else { field };
        let name_text = self.text(name);
        match obj.kind() {
            "this" | "base" => {
                let r = self.text(obj).to_string();
                self.push_ref(RefKind::Read, name_text, Some(r), span_of(name), None);
            }
            "identifier" | "member_access_expression" => self.visit_path_access(obj, name, depth),
            "predefined_type" => {}
            _ => {
                let receiver = self.receiver(obj);
                if receiver.starts_with("$type:") {
                    self.push_ref(RefKind::Read, name_text, Some(receiver), span_of(name), None);
                }
                self.visit(obj, depth + 1);
            }
        }
        if field.kind() == "generic_name" {
            self.visit_type_args(field, depth + 1);
        }
    }

    /// `x.f` (a typed local or field) and `Type.Member` paths.
    fn visit_path_access(&mut self, obj: Node<'_>, field: Node<'_>, depth: usize) {
        let Some(path) = self.identifier_path(obj) else { return self.visit(obj, depth + 1) };
        let head = path[0];
        let head_local = self.locals.is_local(head, obj.start_byte() as u32);
        let head_field = !head_local && self.field_class(head).is_some();
        if path.len() == 1 && !head_local && self.member_elsewhere(head) {
            self.push_ref(RefKind::Read, head, None, span_of(obj), None);
        }
        if path.len() == 1 && (head_local || head_field) {
            self.read_on_value(obj, field, head_field);
        } else if head_local || head_field {
            self.visit(obj, depth + 1);
        } else if path.last().is_some_and(|s| s.starts_with(|c: char| c.is_ascii_uppercase())) {
            self.read_on_type_path(&path, field);
        }
    }

    /// `x.f` on a local or a field `x`: the `Read` of `f` on a typed
    /// receiver, and of the field `x`.
    fn read_on_value(&mut self, obj: Node<'_>, field: Node<'_>, head_field: bool) {
        let receiver = self.receiver(obj);
        if receiver.starts_with("$type:") {
            self.push_ref(RefKind::Read, self.text(field), Some(receiver), span_of(field), None);
        }
        if head_field {
            self.push_ref(RefKind::Read, self.text(obj), None, span_of(obj), None);
        }
    }

    /// `Type.Member`, `A.B.Type.Member`, `Outer.Inner.Member`: the `Read` of
    /// the member on the path (typed when its head is a type of the file).
    fn read_on_type_path(&mut self, path: &[&str], field: Node<'_>) {
        let q = path.join(".");
        let receiver = if self.visible_type(path[0]).is_some() { self.typed(Some(q.clone())).unwrap_or(q) } else { q };
        self.push_ref(RefKind::Read, self.text(field), Some(receiver), span_of(field), None);
    }

    /// `x?.f` (not a call): the receiver, and the member on a typed receiver.
    fn visit_conditional_access(&mut self, node: Node<'_>, depth: usize) {
        let cond = node.child_by_field_name("condition");
        if let Some(c) = cond {
            let receiver = self.receiver(c);
            let binding = named_children(node).into_iter().find(|b| b.kind() == "member_binding_expression");
            if let Some(n) = binding.and_then(|b| b.child_by_field_name("name"))
                && receiver.starts_with("$type:")
            {
                self.push_ref(RefKind::Read, self.text(n), Some(receiver), span_of(n), None);
            }
            self.visit(c, depth + 1);
        }
        for c in named_children(node) {
            if Some(c) != cond && c.kind() != "member_binding_expression" {
                self.visit(c, depth + 1);
            }
        }
    }

    /// `x[k]`: a configuration read when `x` is an `IConfiguration`, then
    /// the parts.
    fn visit_element_access(&mut self, node: Node<'_>, depth: usize) {
        self.bridge_indexer(node);
        self.visit_children(node, depth);
    }

    // -----------------------------------------------------------------------
    // Object creation
    // -----------------------------------------------------------------------

    fn visit_new(&mut self, node: Node<'_>, depth: usize) {
        let args = node.child_by_field_name("arguments");
        let ty = node.child_by_field_name("type");
        let mut created = None;
        if let Some(t) = ty {
            if let Some((name, receiver)) = self.type_name(t) {
                let named = self.with_args(&name, args);
                self.push_ref(RefKind::New, &named, receiver, span_of(t), Some(args.map_or(0, arity_of)));
            }
            self.visit_type_args(t, depth);
            created = class_of_type(t, self.source);
        }
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
        if let Some(init) = node.child_by_field_name("initializer") {
            self.visit_initializer(init, created, depth);
        }
    }

    /// `T x = new(…)`: a `New` of the declared type.
    fn visit_implicit_new(&mut self, node: Node<'_>, depth: usize) {
        let args = named_children(node).into_iter().find(|c| c.kind() == "argument_list");
        let declared = node
            .parent()
            .filter(|p| p.kind() == "variable_declarator")
            .and_then(|d| d.parent())
            .and_then(|decl| decl.child_by_field_name("type"))
            .filter(|t| t.kind() != "implicit_type");
        let mut created = None;
        if let Some(t) = declared {
            if let Some((name, receiver)) = self.type_name(t) {
                let named = self.with_args(&name, args);
                self.push_ref(RefKind::New, &named, receiver, span_of(node), Some(args.map_or(0, arity_of)));
            }
            created = class_of_type(t, self.source);
        }
        if let Some(a) = args {
            self.visit_children(a, depth);
        }
        if let Some(init) = named_children(node).into_iter().find(|c| c.kind() == "initializer_expression") {
            self.visit_initializer(init, created, depth);
        }
    }

    /// An object initializer `{ A = x, B = { … } }`: `Write`s of the
    /// created type's members, then the values.
    fn visit_initializer(&mut self, init: Node<'_>, created: Option<String>, depth: usize) {
        let receiver = self.typed(created);
        for c in named_children(init) {
            let left = c.child_by_field_name("left").filter(|l| l.kind() == "identifier");
            match (c.kind(), left, &receiver) {
                ("assignment_expression", Some(l), Some(r)) => {
                    self.push_ref(RefKind::Write, self.text(l), Some(r.clone()), span_of(l), None);
                    self.visit_field_child(c, "right", depth);
                }
                ("assignment_expression", Some(_), None) => self.visit_field_child(c, "right", depth),
                _ => self.visit(c, depth + 1),
            }
        }
    }
}
