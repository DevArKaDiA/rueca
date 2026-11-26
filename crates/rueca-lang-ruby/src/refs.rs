//! References in expressions (crate docs, *References*).

use rueca_extract::span_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::RefKind;

use crate::extract::{Ctx, Extractor, const_path, literal_string, symbol_name};

/// Calls whose first symbol argument names a method called on the same
/// receiver (`send(:helper, x)`).
const SEND_LIKE: &[&str] = &["send", "public_send", "__send__"];

/// `ENV` methods that read one variable by name.
const ENV_READS: &[&str] = &["fetch", "key?", "has_key?", "include?", "member?", "[]", "values_at", "dig"];

/// Node kinds that hold no references.
const LEAVES: &[&str] = &[
    "integer",
    "float",
    "complex",
    "rational",
    "true",
    "false",
    "nil",
    "simple_symbol",
    "hash_key_symbol",
    "string_content",
    "escape_sequence",
    "comment",
    "heredoc_content",
    "heredoc_end",
    "character",
    "regex",
    "global_variable",
    "class_variable",
    "instance_variable",
    "self",
    "uninterpreted",
    "forward_argument",
    "forward_parameter",
    "block_parameters",
    "lambda_parameters",
    "method_parameters",
    "exception_variable",
    "left_assignment_list",
    "destructured_left_assignment",
    "rest_assignment",
    "undef",
    "empty_statement",
    "line",
    "file",
    "encoding",
];

impl Extractor<'_> {
    /// Visits an expression (or any subtree) for references.
    pub(crate) fn expr(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        if depth > self.budget_depth || LEAVES.contains(&node.kind()) {
            return;
        }
        match node.kind() {
            "identifier" => self.bare_identifier(node, ctx),
            "constant" | "scope_resolution" => self.constant_ref(node, ctx, RefKind::Read),
            "call" => self.call(node, ctx, depth),
            "super" | "zsuper" => self.super_call(node, ctx),
            "element_reference" => self.element_reference(node, ctx, depth),
            "assignment" | "operator_assignment" => self.assignment(node, ctx, depth),
            "rescue" => self.rescue(node, ctx, depth),
            "pair" => {
                if let Some(v) = node.child_by_field_name("value") {
                    self.expr(v, ctx, depth + 1);
                }
                if let Some(k) = node.child_by_field_name("key").filter(|k| k.kind() != "hash_key_symbol") {
                    self.expr(k, ctx, depth + 1);
                }
            }
            "method" | "singleton_method" | "class" | "module" | "singleton_class" | "alias" => {
                self.statement(node, ctx, depth + 1)
            }
            "body_statement" | "block_body" | "begin" | "then" | "else" | "do" | "ensure" => {
                self.statements(node, ctx, depth + 1)
            }
            "in_clause" => {
                // A pattern binds names; only guards and bodies are code.
                for f in ["guard", "body"] {
                    if let Some(c) = node.child_by_field_name(f) {
                        self.expr(c, ctx, depth + 1);
                    }
                }
            }
            "block_argument" => self.block_argument(node, ctx, depth),
            _ => self.walk_children(node, ctx, depth),
        }
    }

    pub(crate) fn walk_children(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for c in children {
            match c.kind() {
                "block" | "do_block" => {
                    if let Some(b) = c.child_by_field_name("body") {
                        self.statements(b, ctx, depth + 1);
                    }
                }
                _ => self.statement(c, ctx, depth + 1),
            }
        }
    }

    /// A bare identifier: a local variable, or a call with an implicit
    /// receiver.
    fn bare_identifier(&mut self, node: Node<'_>, ctx: &Ctx) {
        let name = self.t(node);
        if self.locals.is_local(name, node.start_byte() as u32) || is_keyword_like(name) {
            return;
        }
        self.push_ref(RefKind::Call, name, None, ctx.parent, span_of(node));
    }

    /// `Foo` / `A::B` / `::A`: a reference to the constant (`kind`), with
    /// the qualifier as receiver (`::` for a top-level `::A`).
    pub(crate) fn constant_ref(&mut self, node: Node<'_>, ctx: &Ctx, kind: RefKind) {
        let Some(path) = const_path(node, self.src) else {
            // `obj::Const` / `self::X`: the qualifier is an expression.
            if let Some(s) = node.child_by_field_name("scope") {
                self.expr(s, ctx, 1);
            }
            return;
        };
        let (recv, name) = match path.rsplit_once("::") {
            Some(("", n)) => (Some("::".to_string()), n.to_string()),
            Some((q, n)) => (Some(q.to_string()), n.to_string()),
            None => (None, path.clone()),
        };
        self.push_ref(kind, &name, recv, ctx.parent, span_of(node));
    }

    /// `recv.m(args) { block }`.
    fn call(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        let Some(method) = node.child_by_field_name("method") else { return self.walk_children(node, ctx, depth) };
        if method.kind() == "super" {
            // `super(a)`: the same method of the next ancestor.
            self.super_call(method, ctx);
            return self.call_parts(node, ctx, depth, false);
        }
        let name = self.t(method).to_string();
        let receiver = node.child_by_field_name("receiver");
        if self.env_call(node, receiver, &name, ctx) {
            return self.call_parts(node, ctx, depth, false);
        }
        if receiver.is_none() && (name == "raise" || name == "fail") {
            self.raise_ref(node, ctx);
        }
        let recv = receiver.map(|r| self.receiver_of(r, ctx, 0));
        let span = span_of(method);
        self.push_ref(RefKind::Call, &name, recv.clone(), ctx.parent, span);
        // `send(:m)` / `public_send(:m)`: also a call of `m`.
        if SEND_LIKE.contains(&name.as_str())
            && let Some(sym) = first_arg(node).and_then(|a| symbol_name(a, self.src))
        {
            self.push_ref(RefKind::Call, &sym, recv.clone(), ctx.parent, span);
        }
        let visit_receiver = receiver.is_some_and(|r| const_path(r, self.src).is_none());
        if let Some(r) = receiver.filter(|r| const_path(*r, self.src).is_some()) {
            self.constant_ref(r, ctx, RefKind::Read);
        }
        self.call_parts(node, ctx, depth, visit_receiver);
    }

    /// The receiver (when `visit_receiver`), arguments and block of a call.
    fn call_parts(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize, visit_receiver: bool) {
        if visit_receiver && let Some(r) = node.child_by_field_name("receiver") {
            self.expr(r, ctx, depth + 1);
        }
        if let Some(a) = node.child_by_field_name("arguments") {
            self.walk_children(a, ctx, depth + 1);
        }
        if let Some(b) = node.child_by_field_name("block") {
            if let Some(body) = b.child_by_field_name("body") {
                self.statements(body, ctx, depth + 1);
            }
            if let Some(params) = b.child_by_field_name("parameters") {
                self.walk_parameter_defaults(params, ctx, depth);
            }
        }
    }

    fn walk_parameter_defaults(&mut self, params: Node<'_>, ctx: &Ctx, depth: usize) {
        let mut cursor = params.walk();
        let children: Vec<Node<'_>> = params.named_children(&mut cursor).collect();
        for p in children {
            if let Some(v) = p.child_by_field_name("value") {
                self.expr(v, ctx, depth + 1);
            }
        }
    }

    /// `ENV.fetch("X")`, `ENV.key?("X")`: an env var read. `true` when it
    /// is one.
    fn env_call(&mut self, node: Node<'_>, receiver: Option<Node<'_>>, name: &str, ctx: &Ctx) -> bool {
        let Some(r) = receiver else { return false };
        if !is_env(r, self.src) || !ENV_READS.contains(&name) {
            return false;
        }
        let Some(var) = first_arg(node).and_then(|a| literal_string(a, self.src)) else { return false };
        self.push_ref(RefKind::EnvVar, &var, None, ctx.parent, span_of(node));
        true
    }

    /// `ENV["X"]`; any other `a[b]` is visited.
    fn element_reference(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        if let Some(obj) = node.child_by_field_name("object")
            && is_env(obj, self.src)
            && let Some(var) = node.named_child(1).and_then(|a| literal_string(a, self.src))
        {
            self.push_ref(RefKind::EnvVar, &var, None, ctx.parent, span_of(node));
            return;
        }
        self.walk_children(node, ctx, depth);
    }

    /// `super` / `super(…)`: the same method of the next ancestor.
    fn super_call(&mut self, node: Node<'_>, ctx: &Ctx) {
        let Some(m) = ctx.method else { return };
        let name = self.defs[m as usize].name.clone();
        self.push_ref(RefKind::Call, &name, Some("$super".into()), ctx.parent, span_of(node));
    }

    /// `x = v`: the value; `self.x = v` / `obj.x = v` write `x`.
    fn assignment(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        if let Some(left) = node.child_by_field_name("left") {
            match left.kind() {
                "call" => {
                    if let Some(m) = left.child_by_field_name("method") {
                        let recv = left.child_by_field_name("receiver").map(|r| self.receiver_of(r, ctx, 0));
                        let name = self.t(m).to_string();
                        self.push_ref(RefKind::Write, &name, recv, ctx.parent, span_of(m));
                        if let Some(r) = left.child_by_field_name("receiver") {
                            self.expr(r, ctx, depth + 1);
                        }
                    }
                }
                "element_reference" => self.walk_children(left, ctx, depth),
                "scope_resolution" | "constant" => self.constant_ref(left, ctx, RefKind::Write),
                _ => {}
            }
        }
        if let Some(r) = node.child_by_field_name("right") {
            self.expr(r, ctx, depth + 1);
        }
    }

    /// `rescue Foo, Bar => e`: the exception classes are type references.
    fn rescue(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        if let Some(ex) = node.child_by_field_name("exceptions") {
            let mut cursor = ex.walk();
            let items: Vec<Node<'_>> = ex.named_children(&mut cursor).collect();
            for e in items {
                if const_path(e, self.src).is_some() {
                    self.constant_ref(e, ctx, RefKind::Type);
                } else {
                    self.expr(e, ctx, depth + 1);
                }
            }
        }
        if let Some(b) = node.child_by_field_name("body") {
            self.statements(b, ctx, depth + 1);
        }
    }

    /// `raise Foo::Error` / `raise Foo::Error.new(…)`: a throw of the class.
    fn raise_ref(&mut self, node: Node<'_>, ctx: &Ctx) {
        let Some(a) = first_arg(node) else { return };
        let target = match a.kind() {
            "call" if a.child_by_field_name("method").is_some_and(|m| self.t(m) == "new") => {
                a.child_by_field_name("receiver")
            }
            _ => Some(a),
        };
        if let Some(t) = target.filter(|t| const_path(*t, self.src).is_some()) {
            self.constant_ref(t, ctx, RefKind::Throw);
        }
    }

    /// `&blk`, `&method(:m)` (a call of `m`), `&:sym` (not followed).
    fn block_argument(&mut self, node: Node<'_>, ctx: &Ctx, depth: usize) {
        let Some(inner) = node.named_child(0) else { return };
        if inner.kind() == "call"
            && inner.child_by_field_name("receiver").is_none()
            && inner.child_by_field_name("method").is_some_and(|m| self.t(m) == "method")
            && let Some(sym) = first_arg(inner).and_then(|a| symbol_name(a, self.src))
        {
            self.push_ref(RefKind::Call, &sym, None, ctx.parent, span_of(inner));
            return;
        }
        self.expr(inner, ctx, depth + 1);
    }
}

/// The first argument of a call.
pub(crate) fn first_arg(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("arguments").and_then(|a| a.named_child(0))
}

fn is_env(node: Node<'_>, src: &[u8]) -> bool {
    matches!(const_path(node, src).as_deref(), Some("ENV" | "::ENV"))
}

/// Identifiers that look like calls but are keywords of the grammar's
/// leaf forms (`__method__` and friends are calls, kept).
fn is_keyword_like(name: &str) -> bool {
    matches!(name, "binding" | "block_given?" | "__method__" | "__dir__")
}
