//! References in expressions, patterns and types (see the crate docs,
//! *References*).

use rueca_extract::SELF_RECEIVER;
use rueca_extract::tree_sitter::Node;
use rueca_model::RefKind;

use crate::builtins::{is_builtin, is_deref_wrapper, is_primitive};
use crate::extract::{Cx, Extractor, MAX_REFS, is_comment, named_children};
use crate::types::{path_segments, ty_of};

/// Node kinds with nothing to visit.
fn is_inert(kind: &str) -> bool {
    matches!(
        kind,
        "lifetime"
            | "label"
            | "string_literal"
            | "raw_string_literal"
            | "char_literal"
            | "integer_literal"
            | "float_literal"
            | "boolean_literal"
            | "escape_sequence"
            | "self"
            | "crate"
            | "super"
            | "metavariable"
            | "attribute_item"
            | "inner_attribute_item"
            | "visibility_modifier"
            | "mutable_specifier"
            | "primitive_type"
            | "never_type"
            | "use_declaration"
            | "extern_crate_declaration"
            | "field_identifier"
            | "shorthand_field_identifier"
    ) || is_comment(kind)
}

/// Item kinds (definitions when met in the file's own text).
fn is_item(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_signature_item"
            | "struct_item"
            | "union_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "macro_definition"
            | "mod_item"
            | "foreign_mod_item"
    )
}

/// Type node kinds.
fn is_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "reference_type"
            | "pointer_type"
            | "array_type"
            | "tuple_type"
            | "function_type"
            | "dynamic_type"
            | "abstract_type"
            | "bounded_type"
            | "qualified_type"
            | "bracketed_type"
            | "type_arguments"
            | "trait_bounds"
    )
}

/// Whether a name looks like a type, a variant or a constant (`Foo`, `MAX`).
fn is_capitalized(name: &str) -> bool {
    name.starts_with(|c: char| c.is_uppercase())
}

impl Extractor<'_> {
    /// Expressions and statements.
    pub(crate) fn visit(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        let kind = node.kind();
        if is_inert(kind) {
            return;
        }
        if is_item(kind) {
            let c = self.container.clone();
            return self.item(cx, node, &c);
        }
        if is_type_kind(kind) {
            return self.visit_type(cx, node, depth + 1);
        }
        match kind {
            "identifier" => self.identifier_read(cx, node),
            "call_expression" => self.call(cx, node, depth),
            "macro_invocation" => self.macro_invocation(cx, node, depth),
            "field_expression" => self.field_read(cx, node, depth),
            "scoped_identifier" => self.path_read(cx, node),
            "struct_expression" => self.struct_literal(cx, node, depth),
            "generic_function" => {
                self.visit_field(cx, node, "function", depth);
                self.visit_type_field(cx, node, "type_arguments", depth);
            }
            "let_declaration" => self.let_declaration(cx, node, depth),
            "assignment_expression" | "compound_assignment_expr" => self.assignment(cx, node, depth),
            "type_cast_expression" => {
                self.visit_field(cx, node, "value", depth);
                self.visit_type_field(cx, node, "type", depth);
            }
            "parameter" => {
                self.visit_pattern_field(cx, node, "pattern", depth);
                self.visit_type_field(cx, node, "type", depth);
            }
            "let_condition" | "for_expression" => {
                self.visit_pattern_field(cx, node, "pattern", depth);
                self.visit_field(cx, node, "value", depth);
                self.visit_field(cx, node, "body", depth);
            }
            "match_pattern" => self.match_pattern(cx, node, depth),
            "tuple_struct_pattern"
            | "struct_pattern"
            | "tuple_pattern"
            | "slice_pattern"
            | "or_pattern"
            | "ref_pattern"
            | "mut_pattern"
            | "captured_pattern"
            | "reference_pattern" => self.visit_pattern(cx, node, depth),
            "shorthand_field_initializer" => self.shorthand_field(cx, node),
            _ => self.visit_children(cx, node, depth),
        }
    }

    pub(crate) fn visit_children(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            self.visit(cx, c, depth + 1);
        }
    }

    fn visit_field(&mut self, cx: Cx<'_>, node: Node<'_>, field: &str, depth: usize) {
        if let Some(c) = node.child_by_field_name(field) {
            self.visit(cx, c, depth + 1);
        }
    }

    fn visit_type_field(&mut self, cx: Cx<'_>, node: Node<'_>, field: &str, depth: usize) {
        if let Some(c) = node.child_by_field_name(field) {
            self.visit_type(cx, c, depth + 1);
        }
    }

    fn visit_pattern_field(&mut self, cx: Cx<'_>, node: Node<'_>, field: &str, depth: usize) {
        if let Some(c) = node.child_by_field_name(field) {
            self.visit_pattern(cx, c, depth + 1);
        }
    }

    /// `left = right` / `left += right`. In a macro argument `name = value`
    /// is a named argument (`format!("{x}", x = 1)`, tracing fields), not a
    /// write.
    fn assignment(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        if let Some(l) = node.child_by_field_name("left").filter(|_| !cx.snippet) {
            self.write_target(cx, l, depth);
        }
        self.visit_field(cx, node, "right", depth);
    }

    /// A match arm's pattern and its `if` guard.
    fn match_pattern(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        let guard = node.child_by_field_name("condition").map(|c| c.id());
        for c in named_children(node) {
            if Some(c.id()) == guard {
                self.visit(cx, c, depth + 1);
            } else {
                self.visit_pattern(cx, c, depth + 1);
            }
        }
    }

    /// `T { x }`: `x` is a read of the local (or item) `x`.
    fn shorthand_field(&mut self, cx: Cx<'_>, node: Node<'_>) {
        if let Some(id) = named_children(node).first() {
            self.identifier_read(cx, *id);
        }
    }

    fn let_declaration(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        self.visit_pattern_field(cx, node, "pattern", depth);
        self.visit_type_field(cx, node, "type", depth);
        self.visit_field(cx, node, "value", depth);
        self.visit_field(cx, node, "alternative", depth);
    }

    // -- names ---------------------------------------------------------------

    /// Whether `name` at `node` is a local binding.
    fn is_local(&self, cx: Cx<'_>, node: Node<'_>, name: &str) -> bool {
        self.binding(cx, node, name).is_some()
    }

    /// A non-local identifier in value position: a read of an item.
    fn identifier_read(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let name = cx.text(node);
        if name == "_" || name == "Self" || is_builtin(name) || self.is_local(cx, node, name) {
            return;
        }
        let span = self.span(cx, node);
        self.push_ref(RefKind::Read, name, None, span);
    }

    /// `a::b::c` in value position: a read of `c` through `a::b`.
    fn path_read(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = cx.text(name_node).to_string();
        let span = self.span(cx, node);
        let receiver = self.qualifier(cx, node);
        if receiver.is_none() && is_builtin(&name) {
            return;
        }
        self.push_ref(RefKind::Read, &name, receiver, span);
    }

    /// Receiver text of the qualifier of a scoped path (`a::b` of
    /// `a::b::c`, `<T as Tr>` of `<T as Tr>::f`); `None` without one.
    fn qualifier(&mut self, cx: Cx<'_>, node: Node<'_>) -> Option<String> {
        let path = node.child_by_field_name("path")?;
        if let Some(q) = named_children(path).into_iter().find(|q| q.kind() == "qualified_type") {
            return Some(self.qualified_receiver(cx, q));
        }
        if matches!(path.kind(), "bracketed_type" | "generic_type") && path_segments(cx, path).is_none() {
            let t = ty_of(cx, path)?;
            return Some(self.type_receiver(cx, &t, path));
        }
        match path_segments(cx, path) {
            Some(segs) => self.path_receiver(cx, &segs, path),
            None => {
                let t = ty_of(cx, path)?;
                Some(self.type_receiver(cx, &t, path))
            }
        }
    }

    /// `<T as Tr>`: the concrete type `T` (its impl of `Tr`, or the trait's
    /// default through it), the trait when `T` is generic or `Self`.
    fn qualified_receiver(&mut self, cx: Cx<'_>, q: Node<'_>) -> String {
        let concrete = q.child_by_field_name("type").and_then(|t| {
            let ty = ty_of(cx, t)?;
            let generic = ty.path.len() == 1 && (ty.path[0] == "Self" || self.is_type_param(cx, t, &ty.path[0]));
            (!generic).then_some((ty, t))
        });
        let chosen = concrete.or_else(|| q.child_by_field_name("alias").and_then(|a| ty_of(cx, a).map(|ty| (ty, a))));
        match chosen {
            Some((ty, n)) => self.type_receiver(cx, &ty, n),
            None => "(?)".to_string(),
        }
    }

    /// `x.f` not called: a read of field `f` when `x` is `self`.
    fn field_read(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        let (Some(value), Some(field)) = (node.child_by_field_name("value"), node.child_by_field_name("field")) else {
            return;
        };
        if value.kind() == "self" {
            if field.kind() == "field_identifier" {
                let span = self.span(cx, field);
                self.push_ref(RefKind::Read, cx.text(field), Some(SELF_RECEIVER.to_string()), span);
            }
            return;
        }
        self.visit(cx, value, depth + 1);
    }

    /// Assignment target: a non-local name or a field of `self` is a
    /// `Write`; other targets are visited.
    fn write_target(&mut self, cx: Cx<'_>, target: Node<'_>, depth: usize) {
        match target.kind() {
            "identifier" => {
                let name = cx.text(target);
                if !self.is_local(cx, target, name) && !is_builtin(name) && name != "_" {
                    let span = self.span(cx, target);
                    self.push_ref(RefKind::Write, name, None, span);
                }
            }
            "field_expression" => match self_field(target) {
                Some(field) => {
                    let span = self.span(cx, field);
                    self.push_ref(RefKind::Write, cx.text(field), Some(SELF_RECEIVER.to_string()), span);
                }
                None => self.visit(cx, target, depth + 1),
            },
            _ => self.visit(cx, target, depth + 1),
        }
    }

    /// `T { a: x, ..base }`: a reference to the type `T`; values visited.
    fn struct_literal(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        if let Some(name) = node.child_by_field_name("name") {
            self.type_name_ref(cx, name, depth);
        }
        let Some(body) = node.child_by_field_name("body") else { return };
        for init in named_children(body) {
            match init.kind() {
                "field_initializer" => self.visit_field(cx, init, "value", depth + 1),
                _ => self.visit(cx, init, depth + 1),
            }
        }
    }

    /// The type named in a struct expression or pattern.
    fn type_name_ref(&mut self, cx: Cx<'_>, name: Node<'_>, depth: usize) {
        match name.kind() {
            "type_identifier" | "identifier" if cx.text(name) == "Self" => {}
            "generic_type_with_turbofish" => {
                self.visit_type_field(cx, name, "type", depth);
                self.visit_type_field(cx, name, "type_arguments", depth);
            }
            _ => self.visit_type(cx, name, depth + 1),
        }
    }

    // -- calls ---------------------------------------------------------------

    fn call(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        let args = node.child_by_field_name("arguments");
        if let Some(func) = node.child_by_field_name("function") {
            self.callee(cx, func, args, depth);
        }
        if let Some(a) = args {
            self.visit_children(cx, a, depth);
        }
    }

    /// The function part of a call.
    fn callee(&mut self, cx: Cx<'_>, func: Node<'_>, args: Option<Node<'_>>, depth: usize) {
        match func.kind() {
            "identifier" => self.bare_call(cx, func, args),
            "scoped_identifier" => self.path_call(cx, func, args),
            "field_expression" => self.method_call(cx, func, depth),
            "generic_function" => {
                self.visit_type_field(cx, func, "type_arguments", depth);
                if let Some(inner) = func.child_by_field_name("function") {
                    self.callee(cx, inner, args, depth);
                }
            }
            _ => self.visit(cx, func, depth + 1),
        }
    }

    /// `f(…)`: a call of an item; `F(…)` constructs a tuple struct or
    /// variant (a read); calls of local closures are not references.
    fn bare_call(&mut self, cx: Cx<'_>, func: Node<'_>, args: Option<Node<'_>>) {
        let name = cx.text(func);
        if self.is_local(cx, func, name) || is_builtin(name) {
            return;
        }
        let span = self.span(cx, func);
        if self.file.env_fns.contains(name) {
            self.env_read(cx, args, func);
        }
        let kind = if is_capitalized(name) { RefKind::Read } else { RefKind::Call };
        self.push_ref(kind, name, None, span);
    }

    /// `a::b::f(…)`, `T::new(…)`, `Self::f(…)`, `E::V(…)`, `<T as Tr>::f(…)`.
    fn path_call(&mut self, cx: Cx<'_>, func: Node<'_>, args: Option<Node<'_>>) {
        let Some(name_node) = func.child_by_field_name("name") else { return };
        let name = cx.text(name_node).to_string();
        if self.is_env_path(cx, func) {
            self.env_read(cx, args, func);
        }
        let receiver = self.qualifier(cx, func);
        if receiver.is_none() && (is_builtin(&name) || self.is_local(cx, name_node, &name)) {
            return;
        }
        let kind = if is_capitalized(&name) { RefKind::Read } else { RefKind::Call };
        let span = self.span(cx, func);
        self.push_ref(kind, &name, receiver, span);
    }

    /// `x.m(…)`: a method call with the receiver typed from the file.
    fn method_call(&mut self, cx: Cx<'_>, func: Node<'_>, depth: usize) {
        let (Some(value), Some(field)) = (func.child_by_field_name("value"), func.child_by_field_name("field")) else {
            return;
        };
        if field.kind() != "field_identifier" {
            // `t.0(…)`: a call of a tuple field.
            return self.visit(cx, value, depth + 1);
        }
        let receiver = self.receiver_of(cx, value);
        let span = self.span(cx, field);
        self.push_ref(RefKind::Call, cx.text(field), Some(receiver), span);
        self.visit(cx, value, depth + 1);
    }

    // -- env reads -----------------------------------------------------------

    /// `std::env::var`, `env::var` with `use std::env`, `var_os` likewise.
    fn is_env_path(&self, cx: Cx<'_>, func: Node<'_>) -> bool {
        let Some(segs) = path_segments(cx, func) else { return false };
        let s: Vec<&str> = segs.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
        match s.as_slice() {
            ["std" | "core", "env", f] => matches!(*f, "var" | "var_os"),
            [m, f] => self.file.env_mods.contains(*m) && matches!(*f, "var" | "var_os"),
            _ => false,
        }
    }

    /// An `EnvVar` read of the literal first argument of an env call.
    fn env_read(&mut self, cx: Cx<'_>, args: Option<Node<'_>>, func: Node<'_>) {
        let Some(lit) = args.and_then(|a| named_children(a).into_iter().next()).and_then(|a| string_value(cx, a))
        else {
            return;
        };
        let span = self.span(cx, func.parent().unwrap_or(func));
        self.push_ref(RefKind::EnvVar, &lit, None, span);
    }

    // -- patterns ------------------------------------------------------------

    /// Patterns: paths in them are references (`E::A`, `Some(Foo { .. })`),
    /// bindings are not.
    pub(crate) fn visit_pattern(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "identifier" => self.pattern_identifier(cx, node),
            "scoped_identifier" => self.path_read(cx, node),
            "tuple_struct_pattern" | "struct_pattern" => self.constructor_pattern(cx, node, depth),
            "range_pattern" | "macro_invocation" => self.visit(cx, node, depth + 1),
            k if is_inert(k) => {}
            _ => {
                for c in named_children(node) {
                    self.visit_pattern(cx, c, depth + 1);
                }
            }
        }
    }

    /// An identifier in a pattern: an upper-case one is a path (a constant,
    /// a unit struct or variant), a lower-case one a binding.
    fn pattern_identifier(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let name = cx.text(node);
        if is_capitalized(name) && !is_builtin(name) {
            let span = self.span(cx, node);
            self.push_ref(RefKind::Read, name, None, span);
        }
    }

    /// `Path(a, b)` / `Path { a, b: c }` patterns: the path is a reference,
    /// the sub-patterns are visited.
    fn constructor_pattern(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        let path = node.child_by_field_name("type");
        match path {
            Some(t) if node.kind() == "struct_pattern" => self.type_name_ref(cx, t, depth),
            Some(t) => self.visit_pattern(cx, t, depth + 1),
            None => {}
        }
        let path_id = path.map(|p| p.id());
        for c in named_children(node).into_iter().filter(|c| Some(c.id()) != path_id) {
            if c.kind() == "field_pattern" {
                self.visit_pattern_field(cx, c, "pattern", depth);
            } else {
                self.visit_pattern(cx, c, depth + 1);
            }
        }
    }

    // -- types ---------------------------------------------------------------

    /// Type references in a type expression (and expressions inside it,
    /// such as array lengths).
    pub(crate) fn visit_type(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        if depth > self.max_depth || self.refs.len() >= MAX_REFS {
            return;
        }
        match node.kind() {
            "type_identifier" => self.named_type_ref(cx, node),
            "scoped_type_identifier" => self.scoped_type_ref(cx, node),
            "lifetime" | "primitive_type" | "removed_trait_bound" | "use_bounds" | "never_type" => {}
            "type_binding" => self.visit_type_field(cx, node, "type", depth),
            "where_predicate" => {
                self.visit_type_field(cx, node, "left", depth);
                self.visit_type_field(cx, node, "bounds", depth);
            }
            "type_parameter" => {
                self.visit_type_field(cx, node, "bounds", depth);
                self.visit_type_field(cx, node, "default_type", depth);
            }
            "const_parameter" => {
                self.visit_type_field(cx, node, "type", depth);
                self.visit_field(cx, node, "value", depth);
            }
            "parameter" | "self_parameter" | "variadic_parameter" => self.visit_type_field(cx, node, "type", depth),
            "array_type" => {
                self.visit_type_field(cx, node, "element", depth);
                self.visit_field(cx, node, "length", depth);
            }
            "identifier" => self.identifier_read(cx, node),
            k if is_comment(k) => {}
            k if is_type_kind(k)
                || matches!(
                    k,
                    "type_parameters"
                        | "parameters"
                        | "where_clause"
                        | "higher_ranked_trait_bound"
                        | "constrained_type_parameter"
                ) =>
            {
                for c in named_children(node) {
                    self.visit_type(cx, c, depth + 1);
                }
            }
            _ => self.visit(cx, node, depth + 1),
        }
    }

    /// A reference to the type named by `node` (not a primitive, `Self`, a
    /// generic parameter, nor a standard type outside field types).
    fn named_type_ref(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let name = cx.text(node);
        if name == "_" || name == "Self" || is_primitive(name) || self.is_type_param(cx, node, name) {
            return;
        }
        let std_ok = self.std_types && !is_deref_wrapper(name);
        if is_builtin(name) && !std_ok {
            return;
        }
        let span = self.span(cx, node);
        self.push_ref(RefKind::Type, name, None, span);
    }

    fn scoped_type_ref(&mut self, cx: Cx<'_>, node: Node<'_>) {
        let Some(name) = node.child_by_field_name("name") else { return };
        let name = cx.text(name).to_string();
        let receiver = self.qualifier(cx, node);
        let span = self.span(cx, node);
        self.push_ref(RefKind::Type, &name, receiver, span);
    }
}

/// Value of a string literal without escapes (`"X"`, `r"X"`).
pub(crate) fn string_value(cx: Cx<'_>, node: Node<'_>) -> Option<String> {
    if !matches!(node.kind(), "string_literal" | "raw_string_literal") {
        return None;
    }
    let parts = named_children(node);
    if parts.iter().any(|c| c.kind() != "string_content") {
        return None;
    }
    Some(parts.iter().map(|c| cx.text(*c)).collect())
}

/// The field of `self.field`.
fn self_field(expr: Node<'_>) -> Option<Node<'_>> {
    expr.child_by_field_name("value").filter(|v| v.kind() == "self")?;
    expr.child_by_field_name("field")
}
