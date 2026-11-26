//! Expressions: calls, receivers, property reads and writes, types, and the
//! code ↔ config bridge (see the crate docs, *References*).

use rueca_extract::tree_sitter::Node;
use rueca_extract::{member_receiver, span_of, type_receiver, with_arg_types};
use rueca_model::RefKind;

use crate::extract::{Extractor, last_segment};
use crate::locals::name_path;
use crate::types::{
    child_of_kind, class_of_type, declared_type, is_type_like, is_type_node, named_children, strip_type_text,
};

const MAX_RECEIVER: usize = 200;
/// Receivers typed through initializers nested at most.
const MAX_RECV_DEPTH: usize = 4;
/// Prefix of a call-result receiver (`$ret:<receiver>#<name>`, the Rust
/// pack's form; `rueca-resolve` reads the callee's declared return type).
const RET_PREFIX: &str = "$ret:";
/// Scope functions that return their receiver.
const RETURNS_RECEIVER: &[&str] = &["also", "apply"];
/// Scope functions whose lambda has the receiver as `this`.
const THIS_SCOPES: &[&str] = &["apply", "run"];
/// Scope functions whose lambda has the receiver as `it`.
const IT_SCOPES: &[&str] = &["also", "let"];
/// Implicit lambda receiver (`$with:<T>`, see `rueca-resolve`).
const WITH_PREFIX: &str = "$with:";
/// Spring `@Value` lives here.
const SPRING_VALUE: &str = "org.springframework.beans.factory.annotation.Value";
/// Spring's environment abstraction.
const SPRING_ENV_TYPES: &[&str] = &[
    "org.springframework.core.env.ConfigurableEnvironment",
    "org.springframework.core.env.Environment",
    "org.springframework.core.env.PropertyResolver",
    "org.springframework.core.env.StandardEnvironment",
];
const SPRING_ENV_METHODS: &[&str] = &["containsProperty", "getProperty", "getRequiredProperty"];

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Skips parentheses and `!!` around an expression.
fn strip(mut v: Node<'_>) -> Node<'_> {
    for _ in 0..16 {
        let next = match v.kind() {
            "parenthesized_expression" => v.named_child(0),
            "postfix_expression" if v.named_child_count() == 1 => v.named_child(0),
            _ => None,
        };
        match next {
            Some(n) => v = n,
            None => return v,
        }
    }
    v
}

/// `${a.b}` / `${a.b:default}` placeholders of a Spring `@Value` text.
pub(crate) fn placeholder_keys(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        rest = &rest[i + 2..];
        let Some(end) = rest.find('}') else { break };
        let key = rest[..end].split(':').next().unwrap_or("").trim();
        if !key.is_empty() && !key.contains("${") {
            out.push(key.to_string());
        }
        rest = &rest[end + 1..];
    }
    out
}

/// The text of a string literal without interpolations (`None` when it
/// interpolates).
fn string_value(node: Node<'_>, source: &[u8]) -> Option<String> {
    let node = strip(node);
    if node.kind() != "string_literal" {
        return None;
    }
    let mut out = String::new();
    for c in named_children(node) {
        match c.kind() {
            "string_content" => out.push_str(crate::types::text(c, source)),
            "character_escape_seq" => out.push_str(crate::types::text(c, source)),
            _ => return None,
        }
    }
    Some(out)
}

/// The argument count of a call suffix: value arguments plus a trailing lambda.
fn arity(suffix: Option<Node<'_>>) -> Option<u32> {
    let s = suffix?;
    let mut n = 0u32;
    for c in named_children(s) {
        match c.kind() {
            "value_arguments" => n += named_children(c).iter().filter(|a| a.kind() == "value_argument").count() as u32,
            "annotated_lambda" => n += 1,
            _ => {}
        }
    }
    Some(n)
}

impl<'s> Extractor<'s> {
    pub(crate) fn visit_expr(&mut self, node: Node<'_>, depth: usize) {
        match node.kind() {
            "call_expression" => self.visit_call_chain(node, depth),
            "navigation_expression" => self.visit_navigation(node, depth),
            "simple_identifier" | "interpolated_identifier" => self.visit_identifier(node),
            "assignment" => self.visit_assignment(node, depth),
            "callable_reference" => self.visit_callable_ref(node),
            "annotation" => self.visit_annotation(node, false, depth),
            "lambda_parameters" | "for_statement" | "when_subject" | "catch_block" => self.visit_binders(node, depth),
            "this_expression" | "super_expression" | "label" => {}
            "when_expression" => self.visit_when(node, depth),
            "if_expression" => self.visit_if(node, depth),
            k if is_type_node(k) => self.visit_type(node, depth),
            _ => self.visit_children(node, depth),
        }
    }

    /// Constructs that declare locals: their declared types, then the rest.
    fn visit_binders(&mut self, node: Node<'_>, depth: usize) {
        for c in named_children(node) {
            match c.kind() {
                "variable_declaration" | "multi_variable_declaration" => self.visit_declared_types(c, depth),
                "simple_identifier" if node.kind() == "catch_block" => {}
                _ => self.visit(c, depth + 1),
            }
        }
    }

    // -- types -----------------------------------------------------------------------

    /// `Type` references of a type node: the class it names (receiver = its
    /// qualifier) and its generic arguments; type parameters in scope are
    /// skipped.
    pub(crate) fn visit_type(&mut self, ty: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match ty.kind() {
            "user_type" => self.visit_user_type(ty, depth),
            "type_arguments" | "type_projection" | "function_type_parameters" => {
                named_children(ty).into_iter().for_each(|c| self.visit_type(c, depth + 1))
            }
            "annotation" => self.visit_annotation(ty, false, depth),
            k if is_type_node(k) || k == "receiver_type" => {
                named_children(ty).into_iter().for_each(|c| self.visit_type(c, depth + 1))
            }
            _ => {}
        }
    }

    fn visit_user_type(&mut self, ty: Node<'_>, depth: usize) {
        let path = strip_type_text(self.text(ty));
        let (receiver, name) = match path.rsplit_once('.') {
            Some((r, n)) => (Some(r.to_string()), n.to_string()),
            None => (None, path.clone()),
        };
        if !(receiver.is_none() && self.type_params.contains(&name)) {
            self.push_ref(RefKind::Type, &name, receiver, span_of(ty), None);
        }
        for c in named_children(ty).into_iter().filter(|c| c.kind() == "type_arguments") {
            self.visit_type(c, depth + 1);
        }
    }

    // -- calls -----------------------------------------------------------------------

    /// `a.b().c().d()` nests to the left: walk the call spine iteratively,
    /// innermost call first.
    fn visit_call_chain(&mut self, node: Node<'_>, depth: usize) {
        let mut chain = vec![node];
        let mut cur = node;
        while let Some(inner) = self.receiver_call(cur) {
            chain.push(inner);
            cur = inner;
        }
        for call in chain.into_iter().rev() {
            if self.refs.len() >= crate::MAX_REFS {
                return;
            }
            self.visit_one_call(call, depth);
        }
    }

    /// The call a call's receiver is (`x.f()` in `x.f().g()`).
    fn receiver_call<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let callee = call.named_child(0)?;
        if callee.kind() != "navigation_expression" {
            return None;
        }
        let recv = strip(callee.named_child(0)?);
        (recv.kind() == "call_expression").then_some(recv)
    }

    fn visit_one_call(&mut self, node: Node<'_>, depth: usize) {
        let Some(callee) = node.named_child(0) else { return };
        let suffix = child_of_kind(node, "call_suffix");
        let mark = self.lambda_recv.len();
        if let Some(s) = suffix {
            self.push_scope_receiver(callee, s);
        }
        self.visit_one_call_parts(callee, suffix, depth);
        self.lambda_recv.truncate(mark);
    }

    /// `x.apply { … }`, `x.run { … }`, `with(x) { … }` (`this`) and
    /// `x.let { … }`, `x.also { … }` (`it`) with a typed `x`: the lambda's
    /// implicit receiver.
    fn push_scope_receiver(&mut self, callee: Node<'_>, suffix: Node<'_>) {
        let Some(lambda) = child_of_kind(suffix, "annotated_lambda").and_then(|l| child_of_kind(l, "lambda_literal"))
        else {
            return;
        };
        let (name, object) = match callee.kind() {
            "simple_identifier" => {
                let arg = child_of_kind(suffix, "value_arguments")
                    .and_then(|a| child_of_kind(a, "value_argument"))
                    .and_then(|a| a.named_children(&mut a.walk()).last());
                (self.text(callee), arg)
            }
            "navigation_expression" => {
                let id = callee.named_child(1).and_then(|n| n.named_child(0)).map(|n| self.text(n)).unwrap_or("");
                (id, callee.named_child(0))
            }
            _ => return,
        };
        let it = IT_SCOPES.contains(&name) && callee.kind() == "navigation_expression";
        let this = (THIS_SCOPES.contains(&name) && callee.kind() == "navigation_expression")
            || (name == "with" && callee.kind() == "simple_identifier");
        if !(it || this) || (it && child_of_kind(lambda, "lambda_parameters").is_some()) {
            return;
        }
        let Some(object) = object else { return };
        let r = self.receiver(object);
        if let Some(class) = r.strip_prefix("$type:") {
            self.lambda_recv.push((lambda.start_byte(), lambda.end_byte(), class.to_string(), it));
        }
    }

    /// The innermost scope-function lambda receiver at `pos` (`it` or `this`).
    fn lambda_receiver(&self, pos: usize, it: bool) -> Option<&str> {
        self.lambda_recv.iter().rev().find(|l| l.0 <= pos && pos < l.1 && l.3 == it).map(|l| l.2.as_str())
    }

    fn visit_one_call_parts(&mut self, callee: Node<'_>, suffix: Option<Node<'_>>, depth: usize) {
        match callee.kind() {
            "simple_identifier" => self.visit_bare_call(callee, suffix),
            "navigation_expression" => self.visit_member_call(callee, suffix, depth),
            "object_literal" => {
                let body = suffix
                    .and_then(|s| child_of_kind(s, "annotated_lambda"))
                    .and_then(|l| child_of_kind(l, "lambda_literal"))
                    .and_then(|l| child_of_kind(l, "statements"));
                self.visit_object_literal(callee, body, depth);
                if body.is_some() {
                    return;
                }
            }
            _ => self.visit(callee, depth + 1),
        }
        if let Some(s) = suffix {
            self.visit_call_suffix(s, depth);
        }
    }

    fn visit_call_suffix(&mut self, suffix: Node<'_>, depth: usize) {
        for c in named_children(suffix) {
            match c.kind() {
                "type_arguments" => self.visit_type(c, depth + 1),
                _ => self.visit(c, depth + 1),
            }
        }
    }

    /// `f(…)`: a call of a function, a class (construction) or, for a local
    /// holding a function value, nothing.
    fn visit_bare_call(&mut self, callee: Node<'_>, suffix: Option<Node<'_>>) {
        let name = self.text(callee);
        if self.locals.is_local(name, callee.start_byte() as u32) {
            return;
        }
        let named = self.with_suffix_args(name, suffix);
        let receiver = self.lambda_receiver(callee.start_byte(), false).map(|c| format!("{WITH_PREFIX}{c}"));
        self.push_ref(RefKind::Call, &named, receiver, span_of(callee), arity(suffix));
    }

    /// `x.f(…)`: a member call with a receiver (crate docs).
    fn visit_member_call(&mut self, callee: Node<'_>, suffix: Option<Node<'_>>, depth: usize) {
        let (Some(object), Some(nav)) = (callee.named_child(0), callee.named_child(1)) else {
            return self.visit_children(callee, depth);
        };
        let Some(id) = nav.named_child(0).filter(|c| c.kind() == "simple_identifier") else {
            return self.visit_children(callee, depth);
        };
        let name = self.text(id).to_string();
        let receiver = self.receiver(object);
        let args = suffix.and_then(|s| child_of_kind(s, "value_arguments"));
        let first = args.and_then(|a| child_of_kind(a, "value_argument")).and_then(|a| a.named_child(0));
        let first = first.and_then(|f| string_value(f, self.source));
        let bridged = self.bridge_getenv(&name, &receiver, first.as_deref(), id)
            || self.bridge_spring_env(&name, object, first.as_deref(), id);
        if !bridged {
            let named = self.with_suffix_args(&name, suffix);
            self.push_ref(RefKind::Call, &named, Some(receiver), span_of(id), arity(suffix));
        }
        self.visit_receiver_object(object, depth);
    }

    /// Visits a receiver expression unless the call spine already did.
    fn visit_receiver_object(&mut self, object: Node<'_>, depth: usize) {
        let o = strip(object);
        match o.kind() {
            "call_expression" | "this_expression" | "super_expression" => {}
            _ => self.visit(o, depth + 1),
        }
    }

    /// A call name carrying its argument types (`with_arg_types`).
    pub(crate) fn with_args(&mut self, name: &str, args: Option<Node<'_>>) -> String {
        let Some(args) = args else { return name.to_string() };
        with_arg_types(name, &self.value_arg_types(args))
    }

    /// [`Self::with_args`] for a call suffix: a trailing lambda is a last
    /// `Function` argument.
    fn with_suffix_args(&mut self, name: &str, suffix: Option<Node<'_>>) -> String {
        let Some(s) = suffix else { return name.to_string() };
        let mut types = child_of_kind(s, "value_arguments").map(|a| self.value_arg_types(a)).unwrap_or_default();
        if child_of_kind(s, "annotated_lambda").is_some() {
            types.push(Some("Function".to_string()));
        }
        with_arg_types(name, &types)
    }

    fn value_arg_types(&mut self, args: Node<'_>) -> Vec<Option<String>> {
        let exprs: Vec<Option<Node<'_>>> = named_children(args)
            .into_iter()
            .filter(|a| a.kind() == "value_argument")
            .map(|a| a.named_children(&mut a.walk()).last())
            .collect();
        exprs.into_iter().map(|e| e.and_then(|e| self.arg_type(e))).collect()
    }

    /// Static type of an argument when the file knows it.
    fn arg_type(&mut self, e: Node<'_>) -> Option<String> {
        let e = strip(e);
        let fixed = match e.kind() {
            "string_literal" => Some("String"),
            "integer_literal" | "hex_literal" | "bin_literal" => Some("Int"),
            "long_literal" => Some("Long"),
            "real_literal" => Some(if self.text(e).ends_with(['f', 'F']) { "Float" } else { "Double" }),
            "boolean_literal" => Some("Boolean"),
            "character_literal" => Some("Char"),
            _ => None,
        };
        if let Some(f) = fixed {
            return Some(f.to_string());
        }
        match e.kind() {
            "simple_identifier" => self.identifier_arg_type(e),
            "call_expression" => self.call_arg_type(e),
            "lambda_literal" | "anonymous_function" => Some("Function".to_string()),
            "spread_expression" => Some("*".to_string()),
            "callable_reference" if child_of_kind(e, "simple_identifier").is_some() => Some("Function".to_string()),
            // `X::class`
            "callable_reference" if child_of_kind(e, "simple_identifier").is_none() => Some("KClass".to_string()),
            // `X::class.java`
            "navigation_expression" => {
                let head = e.named_child(0).filter(|h| h.kind() == "callable_reference");
                (head.is_some() && self.text(e).ends_with(".java")).then(|| "Class".to_string())
            }
            "prefix_expression" => e.named_child(0).and_then(|v| self.arg_type(v)),
            "additive_expression" => {
                let string = named_children(e).iter().any(|c| strip(*c).kind() == "string_literal");
                string.then(|| "String".to_string())
            }
            _ => None,
        }
    }

    /// Type of a call argument: a construction, a `String` result, a
    /// function of this file with a known return class, else `^name` (the
    /// resolver reads the return type the project's callables named so
    /// declare).
    fn call_arg_type(&mut self, e: Node<'_>) -> Option<String> {
        if let Some(c) = crate::locals::call_class(e, self.source) {
            return Some(last_segment(&c).to_string());
        }
        let callee = e.named_child(0)?;
        let (name, recv) = match callee.kind() {
            "simple_identifier" => (self.text(callee), None),
            "navigation_expression" => {
                let id = callee.named_child(1)?.named_child(0).filter(|c| c.kind() == "simple_identifier")?;
                (self.text(id), callee.named_child(0))
            }
            _ => return None,
        };
        if recv.is_none()
            && let Some(c) = self.function_return(name)
        {
            return Some(last_segment(&c).to_string());
        }
        // `x.f()` on a typed `x` (`^$type:X#f`), a call result
        // (`^$ret:…#f`) or a type path (`^Foo#of`): the callee's return type.
        let typed = recv.map(|r| self.receiver(r)).filter(|r| {
            let typed = r.starts_with("$type:") || r.starts_with(RET_PREFIX) || is_type_like(r);
            typed && !r.contains([',', ' ', '(', '{', '"'])
        });
        Some(match typed {
            Some(r) => format!("^{r}#{name}"),
            None => format!("^{name}"),
        })
    }

    /// Type of a local or of a property of an enclosing class / the file.
    fn identifier_arg_type(&self, e: Node<'_>) -> Option<String> {
        let name = self.text(e);
        if let Some(b) = self.locals.lookup(name, e.start_byte() as u32) {
            return b.ty.clone();
        }
        let class = self.property_class(name).or_else(|| self.file_props.get(name).cloned()).flatten()?;
        Some(last_segment(&class).to_string())
    }

    // -- receivers ---------------------------------------------------------------------

    fn raw(&self, node: Node<'_>) -> String {
        let s = collapse(self.text(node));
        if s.chars().count() <= MAX_RECEIVER { s } else { s.chars().take(MAX_RECEIVER).collect() }
    }

    /// The receiver text of a call / property access on `object`.
    pub(crate) fn receiver(&mut self, object: Node<'_>) -> String {
        let o = strip(object);
        match o.kind() {
            "this_expression" => self.this_receiver(o),
            "super_expression" => self.super_receiver(o),
            "simple_identifier" => self.identifier_receiver(o),
            "navigation_expression" => self.navigation_receiver(o),
            "string_literal" => type_receiver("String"),
            "integer_literal" | "hex_literal" | "bin_literal" => type_receiver("Int"),
            "long_literal" => type_receiver("Long"),
            "real_literal" => type_receiver("Double"),
            "boolean_literal" => type_receiver("Boolean"),
            "character_literal" => type_receiver("Char"),
            "call_expression" => self.call_receiver(o),
            "as_expression" => {
                let class = declared_type(o).and_then(|t| class_of_type(t, self.source));
                class.map_or_else(|| self.raw(o), |c| type_receiver(&c))
            }
            _ => self.raw(o),
        }
    }

    /// `this`, or `$type:Outer` for `this@Outer` naming an outer class;
    /// in a scope-function lambda, its receiver.
    fn this_receiver(&self, o: Node<'_>) -> String {
        let label = child_of_kind(o, "type_identifier").map(|l| self.text(l));
        if label.is_none()
            && let Some(c) = self.lambda_receiver(o.start_byte(), false)
        {
            return format!("{WITH_PREFIX}{c}");
        }
        match label {
            Some(l) if self.classes.last().is_none_or(|c| c.name != l) && self.declared_types.contains(l) => {
                type_receiver(l)
            }
            _ => "this".to_string(),
        }
    }

    /// `super`, or `$type:T` for `super<T>`.
    fn super_receiver(&self, o: Node<'_>) -> String {
        match child_of_kind(o, "user_type").and_then(|t| class_of_type(t, self.source)) {
            Some(t) => type_receiver(&t),
            None => "super".to_string(),
        }
    }

    /// Class of a property of an enclosing class of this file, innermost
    /// first: `Some(None)` when the property exists with an unknown class.
    fn property_class(&self, name: &str) -> Option<Option<String>> {
        self.classes.iter().rev().find_map(|c| c.props.get(name).cloned())
    }

    /// The return class of a function the file declares (methods of the
    /// enclosing classes first, then top-level functions).
    fn function_return(&self, name: &str) -> Option<String> {
        self.classes
            .iter()
            .rev()
            .find_map(|c| c.methods.get(name).cloned())
            .or_else(|| self.file_functions.get(name).cloned())
            .flatten()
    }

    fn identifier_receiver(&mut self, o: Node<'_>) -> String {
        let name = self.text(o);
        let pos = o.start_byte();
        if let Some(c) = self.cast_at(name, pos) {
            return type_receiver(&c);
        }
        if name == "it"
            && let Some(c) = self.lambda_receiver(pos, true)
        {
            return type_receiver(c);
        }
        if let Some(b) = self.locals.lookup(name, pos as u32) {
            let class = b.class.clone().or_else(|| b.init_call.as_deref().and_then(|f| self.function_return(f)));
            let class = class.and_then(|c| self.bound_or_class(c));
            let init = b.init;
            return match class {
                Some(c) => type_receiver(&c),
                None => init.and_then(|r| self.typed_initializer(r)).unwrap_or_else(|| name.to_string()),
            };
        }
        if let Some(class) = self.property_class(name) {
            let init = self.classes.iter().rev().find_map(|c| c.prop_inits.get(name).copied());
            return match class {
                Some(c) => type_receiver(&c),
                None => init.and_then(|r| self.typed_initializer(r)).unwrap_or_else(|| member_receiver(name)),
            };
        }
        match self.file_props.get(name).cloned() {
            Some(Some(c)) => type_receiver(&c),
            Some(None) => {
                let init = self.file_prop_inits.get(name).copied();
                init.and_then(|r| self.typed_initializer(r)).unwrap_or_else(|| name.to_string())
            }
            None => name.to_string(),
        }
    }

    /// The class a smart cast gives `name` at byte `pos` (`is T` in an
    /// enclosing `when` entry or `if`).
    fn cast_at(&self, name: &str, pos: usize) -> Option<String> {
        self.casts.iter().rev().find(|c| c.0 == name && c.2 <= pos && pos < c.3).map(|c| c.1.clone())
    }

    /// The receiver an initializer expression stands for, when it is typed
    /// (`$type:` / `$ret:`).
    fn typed_initializer(&mut self, (start, end): (usize, usize)) -> Option<String> {
        if self.recv_depth >= MAX_RECV_DEPTH {
            return None;
        }
        let node = self.root.descendant_for_byte_range(start, end)?;
        self.recv_depth += 1;
        let r = self.receiver(node);
        self.recv_depth -= 1;
        r.starts_with('$').then_some(r)
    }

    fn navigation_receiver(&mut self, o: Node<'_>) -> String {
        let (Some(head), Some(nav)) = (o.named_child(0), o.named_child(1)) else { return self.raw(o) };
        let field = nav.named_child(0).filter(|c| c.kind() == "simple_identifier").map(|c| self.text(c));
        if strip(head).kind() == "this_expression"
            && let Some(f) = field
        {
            return match self.property_class(f) {
                Some(Some(c)) => type_receiver(&c),
                _ => member_receiver(f),
            };
        }
        match name_path(o, self.source) {
            Some(p) if !self.path_head_is_value(&p, o) => p,
            _ => self.raw(o),
        }
    }

    /// Whether a dotted path starts with a local or a property (a value,
    /// not a package or class).
    fn path_head_is_value(&self, path: &str, at: Node<'_>) -> bool {
        let head = path.split('.').next().unwrap_or(path);
        self.locals.is_local(head, at.start_byte() as u32)
            || self.property_class(head).is_some()
            || self.file_props.contains_key(head)
    }

    /// `Foo(…)` (construction) → `$type:Foo`; `own(…)` / `this.own(…)` of a
    /// function with a known return class → `$type:R`.
    fn call_receiver(&mut self, call: Node<'_>) -> String {
        let Some(callee) = call.named_child(0) else { return self.raw(call) };
        let own = match callee.kind() {
            "simple_identifier" => Some(self.text(callee)),
            "navigation_expression" if callee.named_child(0).is_some_and(|h| strip(h).kind() == "this_expression") => {
                callee.named_child(1).and_then(|n| n.named_child(0)).map(|n| self.text(n))
            }
            _ => None,
        };
        if let Some(name) = own {
            if is_type_like(name) && !self.locals.is_local(name, callee.start_byte() as u32) {
                return type_receiver(name);
            }
            if let Some(c) = self.function_return(name) {
                return type_receiver(&c);
            }
        }
        match crate::locals::call_path(call, self.source) {
            Some(p) if is_type_like(last_segment(&p)) && !self.path_head_is_value(&p, call) => type_receiver(&p),
            _ => self.ret_receiver(call).unwrap_or_else(|| self.raw(call)),
        }
    }

    /// `$ret:<receiver>#<name>` for the value of call `call` (`x.f()` →
    /// `$ret:$type:X#f`); `x.apply { … }` / `x.also { … }` are `x`.
    fn ret_receiver(&mut self, call: Node<'_>) -> Option<String> {
        let callee = call.named_child(0)?;
        let (recv, name) = match callee.kind() {
            "simple_identifier" => {
                let name = self.text(callee);
                if self.locals.is_local(name, callee.start_byte() as u32) {
                    return None;
                }
                (String::new(), name)
            }
            "navigation_expression" => {
                let id = callee.named_child(1)?.named_child(0).filter(|c| c.kind() == "simple_identifier")?;
                let object = callee.named_child(0)?;
                let name = self.text(id);
                if RETURNS_RECEIVER.contains(&name) {
                    return Some(self.receiver(object));
                }
                (self.receiver(object), name)
            }
            _ => return None,
        };
        let spec = format!("{RET_PREFIX}{recv}#{name}");
        (spec.len() <= MAX_RECEIVER && !recv.contains(char::is_whitespace)).then_some(spec)
    }

    // -- smart casts ---------------------------------------------------------------

    /// `when (x) { is T -> … }` / `when (val x = …)`: in an entry whose only
    /// condition is `is T`, `x` holds a `T`.
    fn visit_when(&mut self, node: Node<'_>, depth: usize) {
        let subject = child_of_kind(node, "when_subject").and_then(|s| self.when_subject_name(s));
        for c in named_children(node) {
            let cast = match (c.kind(), &subject) {
                ("when_entry", Some(name)) => self.entry_cast(c).map(|t| (name.clone(), t)),
                _ => None,
            };
            let mark = self.casts.len();
            if let (Some((name, class)), Some(body)) = (cast, child_of_kind(c, "control_structure_body")) {
                self.casts.push((name, class, body.start_byte(), body.end_byte()));
            }
            self.visit(c, depth + 1);
            self.casts.truncate(mark);
        }
    }

    fn when_subject_name(&self, s: Node<'_>) -> Option<String> {
        if let Some(v) = child_of_kind(s, "variable_declaration") {
            return child_of_kind(v, "simple_identifier").map(|n| self.text(n).to_string());
        }
        let e = s.named_child(0).map(strip)?;
        (e.kind() == "simple_identifier").then(|| self.text(e).to_string())
    }

    /// The class of a `when` entry whose only condition is `is T`.
    fn entry_cast(&self, entry: Node<'_>) -> Option<String> {
        let conds: Vec<Node<'_>> = named_children(entry).into_iter().filter(|c| c.kind() == "when_condition").collect();
        let [cond] = conds.as_slice() else { return None };
        let test = child_of_kind(*cond, "type_test")?;
        if crate::types::has_keyword(test, "!is") {
            return None;
        }
        declared_type(test).and_then(|t| class_of_type(t, self.source))
    }

    /// `if (x is T) …`: `x` holds a `T` in the consequence.
    fn visit_if(&mut self, node: Node<'_>, depth: usize) {
        let cond = node.child_by_field_name("condition").map(strip);
        let cast = cond.filter(|c| c.kind() == "check_expression").and_then(|c| self.check_cast(c));
        let body = node.child_by_field_name("consequence");
        for c in named_children(node) {
            let mark = self.casts.len();
            if let (Some((name, class)), Some(b)) = (&cast, body)
                && c.id() == b.id()
            {
                self.casts.push((name.clone(), class.clone(), b.start_byte(), b.end_byte()));
            }
            self.visit(c, depth + 1);
            self.casts.truncate(mark);
        }
    }

    /// `x is T` → (`x`, `T`).
    fn check_cast(&self, check: Node<'_>) -> Option<(String, String)> {
        if crate::types::has_keyword(check, "!is") {
            return None;
        }
        let x = check.named_child(0).map(strip).filter(|x| x.kind() == "simple_identifier")?;
        let class = declared_type(check).and_then(|t| class_of_type(t, self.source))?;
        Some((self.text(x).to_string(), class))
    }

    // -- property access -----------------------------------------------------------

    /// `a.b` outside a call: a `Read` of `b` when the receiver is typed or
    /// `this`; the receiver expression is visited.
    fn visit_navigation(&mut self, node: Node<'_>, depth: usize) {
        let (Some(object), Some(nav)) = (node.named_child(0), node.named_child(1)) else {
            return self.visit_children(node, depth);
        };
        let Some(id) = nav.named_child(0).filter(|c| c.kind() == "simple_identifier") else {
            self.visit(object, depth + 1);
            return;
        };
        if let Some(r) = self.read_receiver(object) {
            let name = self.text(id).to_string();
            self.push_ref(RefKind::Read, &name, Some(r), span_of(id), None);
        }
        match strip(object).kind() {
            "this_expression" | "super_expression" => {}
            _ => self.visit(object, depth + 1),
        }
    }

    /// The receiver of a property read worth reporting: `this`, typed
    /// values and type paths (`Color.RED`).
    fn read_receiver(&mut self, object: Node<'_>) -> Option<String> {
        let o = strip(object);
        let r = match o.kind() {
            "this_expression" => self.this_receiver(o),
            "simple_identifier" => self.identifier_receiver(o),
            "navigation_expression" => self.navigation_receiver(o),
            _ => return None,
        };
        let typed = r == "this" || r.starts_with('$') || is_type_like(last_segment(&r));
        typed.then_some(r)
    }

    /// A bare identifier: a `Read` of a property of an enclosing class or of
    /// the file (unless a local shadows it) or of an `UPPER_CASE` constant.
    fn visit_identifier(&mut self, node: Node<'_>) {
        let name = self.text(node);
        if self.locals.is_local(name, node.start_byte() as u32) {
            return;
        }
        let screaming = name.len() > 1 && name.chars().all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_');
        if self.property_class(name).is_some() || self.file_props.contains_key(name) || screaming {
            let name = name.to_string();
            self.push_ref(RefKind::Read, &name, None, span_of(node), None);
        }
    }

    /// `x = …`, `this.x = …`, `a.b += …`: a `Write` of the target, then the
    /// value.
    fn visit_assignment(&mut self, node: Node<'_>, depth: usize) {
        let Some(target) = node.named_child(0) else { return };
        if target.kind() == "directly_assignable_expression" {
            self.visit_assign_target(target, depth);
        } else {
            self.visit(target, depth + 1);
        }
        for c in named_children(node).into_iter().skip(1) {
            self.visit(c, depth + 1);
        }
    }

    fn visit_assign_target(&mut self, target: Node<'_>, depth: usize) {
        let kids = named_children(target);
        match kids.as_slice() {
            [id] if id.kind() == "simple_identifier" => {
                let name = self.text(*id);
                let known = self.property_class(name).is_some() || self.file_props.contains_key(name);
                if known && !self.locals.is_local(name, id.start_byte() as u32) {
                    let name = name.to_string();
                    self.push_ref(RefKind::Write, &name, None, span_of(*id), None);
                }
            }
            [object, nav] if nav.kind() == "navigation_suffix" => {
                if let Some(id) = nav.named_child(0).filter(|c| c.kind() == "simple_identifier")
                    && let Some(r) = self.read_receiver(*object)
                {
                    let name = self.text(id).to_string();
                    self.push_ref(RefKind::Write, &name, Some(r), span_of(id), None);
                }
                if !matches!(strip(*object).kind(), "this_expression" | "super_expression") {
                    self.visit(*object, depth + 1);
                }
            }
            _ => self.visit_children(target, depth),
        }
    }

    /// `::f`, `Foo::bar`, `x::bar`, `::Foo`: a `Read` of the callable (a
    /// reference, not a call).
    fn visit_callable_ref(&mut self, node: Node<'_>) {
        let Some(id) = named_children(node).into_iter().rev().find(|c| c.kind() == "simple_identifier") else {
            // `X::class`: a class literal.
            if let Some(t) = child_of_kind(node, "type_identifier").or_else(|| child_of_kind(node, "user_type")) {
                let path = strip_type_text(self.text(t));
                let (receiver, name) = match path.rsplit_once('.') {
                    Some((r, n)) => (Some(r.to_string()), n.to_string()),
                    None => (None, path.clone()),
                };
                self.push_ref(RefKind::Type, &name, receiver, span_of(t), None);
            }
            return;
        };
        let name = self.text(id).to_string();
        let receiver = named_children(node).into_iter().find(|c| c.id() != id.id()).map(|r| {
            if is_type_node(r.kind()) || r.kind() == "type_identifier" {
                strip_type_text(self.text(r))
            } else {
                self.receiver(r)
            }
        });
        self.push_ref(RefKind::Read, &name, receiver, span_of(id), None);
    }

    // -- code ↔ config bridge (RFC 0003 §7.2) ------------------------------------------

    /// `System.getenv("X")` → an `EnvVar` read of `X` (not when the file
    /// declares or imports another `System`); `true` when bridged.
    fn bridge_getenv(&mut self, name: &str, receiver: &str, first: Option<&str>, at: Node<'_>) -> bool {
        let system = receiver == "System" || receiver == "java.lang.System";
        let shadowed = self.declared_types.contains("System")
            || self.imported.get("System").is_some_and(|q| q != "java.lang.System");
        let Some(var) = first.filter(|v| !v.is_empty()) else { return false };
        if name != "getenv" || !system || shadowed {
            return false;
        }
        self.push_ref(RefKind::EnvVar, var, None, span_of(at), None);
        true
    }

    /// `env.getProperty("a.b")` on a receiver declared as a Spring
    /// `Environment` → a `ConfigKey` read (`spring`).
    fn bridge_spring_env(&mut self, name: &str, object: Node<'_>, first: Option<&str>, at: Node<'_>) -> bool {
        let Some(key) = first.filter(|k| !k.is_empty()) else { return false };
        if !SPRING_ENV_METHODS.contains(&name) {
            return false;
        }
        let o = strip(object);
        if o.kind() != "simple_identifier" {
            return false;
        }
        let v = self.text(o);
        let class = match self.locals.lookup(v, o.start_byte() as u32) {
            Some(b) => b.class.clone(),
            None => self.property_class(v).flatten(),
        };
        let spring = class.is_some_and(|c| {
            let fq = self.imported.get(&c).cloned().unwrap_or(c);
            SPRING_ENV_TYPES.contains(&fq.as_str())
        });
        if !spring {
            return false;
        }
        self.push_ref(RefKind::ConfigKey, key, Some("spring".to_string()), span_of(at), None);
        true
    }

    /// `@Value("\${a.b}")` with `Value` imported from Spring → `ConfigKey`
    /// reads of every placeholder, attributed to the current definition.
    pub(crate) fn visit_bridge_annotations(&mut self, annotations: &[Node<'_>]) {
        if self.imported.get("Value").map(String::as_str) != Some(SPRING_VALUE) {
            return;
        }
        for a in annotations {
            let Some(inv) = child_of_kind(*a, "constructor_invocation") else { continue };
            if self.annotation_name(*a).as_deref() != Some("Value") {
                continue;
            }
            let Some(args) = child_of_kind(inv, "value_arguments") else { continue };
            for arg in named_children(args).into_iter().filter(|c| c.kind() == "value_argument") {
                let Some(v) = arg.named_children(&mut arg.walk()).last().and_then(|e| string_value(e, self.source))
                else {
                    continue;
                };
                for key in placeholder_keys(&v.replace("\\$", "$")) {
                    self.push_ref(RefKind::ConfigKey, &key, Some("spring".to_string()), span_of(*a), None);
                }
            }
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(placeholder_keys("${a.b}"), vec!["a.b"]);
        assert_eq!(placeholder_keys("${a.b:x}-${c}"), vec!["a.b", "c"]);
        assert!(placeholder_keys("plain").is_empty());
    }
}
