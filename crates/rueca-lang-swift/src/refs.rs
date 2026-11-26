//! Expressions: calls with their receivers and argument labels, type
//! references, and environment-variable reads (RFC 0003 §7.2). See the
//! crate docs for the receiver forms.

use rueca_extract::tree_sitter::Node;
use rueca_extract::{ARG_TYPES_SEPARATOR, MEMBER_RECEIVER_PREFIX, SELF_RECEIVER, span_of, type_receiver};
use rueca_model::RefKind;

use crate::extract::Extractor;
use crate::locals::{call_class, cast_type, name_path, operand_callee, strip_expr};
use crate::types::{child_of_kind, class_of_type, declared_type, is_type_like, is_type_node, named_children, text};

/// Prefix of a call-result receiver (`$ret:<receiver>#<name>`): the value
/// a call returns, typed by the resolver from the callee's declared return
/// type (the form of the Rust pack).
pub const RET_RECEIVER_PREFIX: &str = "$ret:";

/// Receiver of an implicit member call (`.f(…)`) whose type the file
/// does not show.
pub const IMPLICIT_MEMBER_RECEIVER: &str = ".";

/// How many initializers deep a local's receiver is typed.
const MAX_RECEIVER_DEPTH: usize = 2;

/// Longest receiver text kept as written.
const MAX_RECEIVER: usize = 200;

/// Wildcard label of a trailing closure (it may fill any closure
/// parameter, labeled or not).
pub const TRAILING_LABEL: &str = "?";

/// Whether a call suffix's arguments are a subscript (`a[i]`) rather than
/// a call (`f(x)`).
fn is_subscript(suffix: Node<'_>) -> bool {
    child_of_kind(suffix, "value_arguments").and_then(|a| a.child(0)).is_some_and(|t| t.kind() == "[")
}

/// The string literal's text when `node` is a plain literal (`"X"`).
fn literal_text<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if node.kind() != "line_string_literal" {
        return None;
    }
    let kids = named_children(node);
    match kids.as_slice() {
        [t] if t.kind() == "line_str_text" => Some(text(*t, source)),
        _ => None,
    }
}

/// The single argument value of a call suffix, if it has exactly one.
fn single_argument(suffix: Node<'_>) -> Option<Node<'_>> {
    let args = child_of_kind(suffix, "value_arguments")?;
    let vals = named_children(args);
    match vals.as_slice() {
        [v] if v.kind() == "value_argument" => v.child_by_field_name("value"),
        _ => None,
    }
}

impl Extractor<'_> {
    /// Expressions and statements: calls, casts, type references; anything
    /// else is walked for nested ones.
    pub(crate) fn visit_expression(&mut self, node: Node<'_>, depth: usize) {
        match node.kind() {
            "call_expression" => self.call(node, depth),
            "constructor_expression" => self.construction(node, depth),
            "as_expression" | "check_expression" => {
                for c in named_children(node) {
                    if is_type_node(c.kind()) { self.type_ref(c) } else { self.visit(c, depth + 1) }
                }
            }
            "lambda_parameter" | "parameter" => {
                if let Some(t) = declared_type(node) {
                    self.type_ref(t);
                }
            }
            "comment" | "multiline_comment" => {}
            _ => self.visit_children(node, depth),
        }
    }

    // -- types -----------------------------------------------------------------

    /// `Type` references of a type node: each nominal type it names
    /// (`[Foo: Bar]` → `Foo`, `Bar`; `a.B<C>` → `B` with receiver `a`, `C`),
    /// generic parameters in scope skipped.
    pub(crate) fn type_ref(&mut self, node: Node<'_>) {
        if node.kind() != "user_type" {
            for c in named_children(node) {
                self.type_ref(c);
            }
            return;
        }
        let segs: Vec<String> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "type_identifier")
            .map(|c| self.text(c).trim_matches('`').to_string())
            .collect();
        if let Some((last, quals)) = segs.split_last()
            && !(quals.is_empty() && self.type_params.contains(last))
        {
            let receiver = (!quals.is_empty()).then(|| quals.join("."));
            self.push_ref(RefKind::Type, last, receiver, span_of(node), None);
        }
        for args in named_children(node).into_iter().filter(|c| c.kind() == "type_arguments") {
            self.type_ref(args);
        }
    }

    // -- calls -------------------------------------------------------------------

    /// Argument labels and arity of a call suffix: one label per argument
    /// (`_` unlabeled), then one per trailing closure ([`TRAILING_LABEL`]
    /// for the first, its label for the others).
    fn call_labels(&self, suffix: Node<'_>) -> Vec<String> {
        let mut labels = Vec::new();
        let mut pending_label: Option<String> = None;
        for c in named_children(suffix) {
            match c.kind() {
                "value_arguments" => {
                    for a in named_children(c).into_iter().filter(|a| a.kind() == "value_argument") {
                        let label = a.child_by_field_name("name").map(|l| self.text(l).trim_matches('`').to_string());
                        labels.push(label.unwrap_or_else(|| "_".to_string()));
                    }
                }
                "simple_identifier" => pending_label = Some(self.text(c).to_string()),
                "lambda_literal" => labels.push(pending_label.take().unwrap_or_else(|| TRAILING_LABEL.to_string())),
                _ => {}
            }
        }
        labels
    }

    /// A call name carrying its argument labels: `draw$args:at,_`.
    fn labeled_name(name: &str, labels: &[String]) -> String {
        if labels.is_empty() { name.to_string() } else { format!("{name}{ARG_TYPES_SEPARATOR}{}", labels.join(",")) }
    }

    /// Walks the arguments and trailing closures of a call suffix.
    fn visit_suffix(&mut self, suffix: Node<'_>, depth: usize) {
        for c in named_children(suffix) {
            if c.kind() != "simple_identifier" {
                self.visit(c, depth + 1);
            }
        }
    }

    /// `f(…)`, `x.f(…)`, `T(…)`, `.f(…)`; subscripts `a[i]` are not calls.
    fn call(&mut self, node: Node<'_>, depth: usize) {
        let (Some(callee), Some(suffix)) = (node.named_child(0), child_of_kind(node, "call_suffix")) else {
            return self.visit_children(node, depth);
        };
        if is_subscript(suffix) {
            self.subscript(callee, suffix, depth);
            return;
        }
        let labels = self.call_labels(suffix);
        let callee = self.real_callee(callee, depth);
        match callee.kind() {
            "simple_identifier" => self.bare_call(callee, suffix, &labels),
            "navigation_expression" => self.member_call(callee, &labels, depth),
            "prefix_expression" => self.implicit_member_call(node, callee, &labels),
            _ => self.visit(callee, depth + 1),
        }
        self.visit_suffix(suffix, depth);
    }

    /// The callee the call applies to. The grammar attaches a call suffix
    /// to a whole binary or prefix-operator expression (`a + b.f(1)` is
    /// parsed as `(a + b.f)(1)`, `!x.isEmpty()` as `(!x.isEmpty)()`): the
    /// callee is the right-most operand; the rest is walked as expressions.
    fn real_callee<'t>(&mut self, callee: Node<'t>, depth: usize) -> Node<'t> {
        let mut cur = callee;
        for _ in 0..64 {
            if let Some(rhs) = cur.child_by_field_name("rhs") {
                for c in named_children(cur).into_iter().filter(|c| *c != rhs) {
                    self.visit(c, depth + 1);
                }
                cur = rhs;
            } else if cur.kind() == "prefix_expression" && !self.text(cur).starts_with('.') {
                match cur.child_by_field_name("target") {
                    Some(t) => cur = t,
                    None => return cur,
                }
            } else {
                return cur;
            }
        }
        cur
    }

    /// `name(…)`: a local closure is no reference; `getenv("X")` reads `X`.
    fn bare_call(&mut self, callee: Node<'_>, suffix: Node<'_>, labels: &[String]) {
        let name = self.text(callee).trim_matches('`');
        if self.locals.is_local(name, callee.start_byte() as u32) {
            return;
        }
        if name == "getenv"
            && let Some(var) = single_argument(suffix).and_then(|v| literal_text(v, self.source))
        {
            self.push_ref(RefKind::EnvVar, var, None, span_of(callee), None);
        }
        let n = Self::labeled_name(name, labels);
        self.push_ref(RefKind::Call, &n, None, span_of(callee), Some(labels.len() as u32));
    }

    /// `x.f(…)`: the receiver typed when the file shows its type.
    fn member_call(&mut self, callee: Node<'_>, labels: &[String], depth: usize) {
        let Some(target) = callee.child_by_field_name("target") else { return self.visit(callee, depth + 1) };
        let Some(id) = callee
            .child_by_field_name("suffix")
            .and_then(|s| s.child_by_field_name("suffix"))
            .filter(|s| s.kind() == "simple_identifier")
        else {
            return self.visit(callee, depth + 1);
        };
        let name = self.text(id).trim_matches('`').to_string();
        let receiver = self.receiver_of(target);
        let n = Self::labeled_name(&name, labels);
        self.push_ref(RefKind::Call, &n, receiver, span_of(id), Some(labels.len() as u32));
        self.visit_receiver(target, depth);
    }

    /// Walks a receiver expression for nested calls (not its plain names).
    fn visit_receiver(&mut self, target: Node<'_>, depth: usize) {
        match target.kind() {
            "simple_identifier" | "self_expression" | "super_expression" => {}
            "navigation_expression" => {
                if let Some(t) = target.child_by_field_name("target") {
                    self.visit_receiver(t, depth + 1);
                }
            }
            _ => self.visit(target, depth + 1),
        }
    }

    /// `.f(…)` (an implicit member): typed by its context (see
    /// [`Extractor::contextual_type`]), else with the receiver `.`.
    fn implicit_member_call(&mut self, call: Node<'_>, callee: Node<'_>, labels: &[String]) {
        let Some(id) = callee.child_by_field_name("target").filter(|t| t.kind() == "simple_identifier") else {
            return;
        };
        // Without a contextual type the receiver is the static member of
        // some type: only the unique-method rung may resolve it.
        let receiver = match self.contextual_type(call) {
            Some(ty) => type_receiver(&ty),
            None => IMPLICIT_MEMBER_RECEIVER.to_string(),
        };
        let name = self.text(id).trim_matches('`').to_string();
        let n = Self::labeled_name(&name, labels);
        self.push_ref(RefKind::Call, &n, Some(receiver), span_of(id), Some(labels.len() as u32));
    }

    /// The type the context gives `expr`: the declared type of the property
    /// it initializes (`let p: Point = .init(…)`), or the declared return
    /// type of the enclosing function or computed property when `expr` is
    /// what it returns (`return .div(…)`, a single-expression body); never
    /// inside a closure.
    fn contextual_type(&self, expr: Node<'_>) -> Option<String> {
        let parent = expr.parent()?;
        match parent.kind() {
            "property_declaration" => {
                let ann = named_children(parent)
                    .into_iter()
                    .filter(|c| c.end_byte() <= expr.start_byte())
                    .rev()
                    .find(|c| c.kind() == "type_annotation")?;
                declared_type(ann).and_then(|t| class_of_type(t, self.source))
            }
            "control_transfer_statement" if self.text(parent).starts_with("return") => self.returned_type(parent),
            "statements" if parent.named_child_count() == 1 => self.returned_type(parent),
            _ => None,
        }
    }

    /// The return type of the callable or computed property whose body holds
    /// `node`, unless a closure is in between.
    fn returned_type(&self, node: Node<'_>) -> Option<String> {
        let mut cur = node.parent();
        while let Some(n) = cur {
            match n.kind() {
                "lambda_literal" => return None,
                "function_body" | "computed_property" | "computed_getter" => {
                    return self.return_types.last().cloned().flatten();
                }
                _ => cur = n.parent(),
            }
        }
        None
    }

    /// `Box<Int>(…)`: a construction of the type.
    fn construction(&mut self, node: Node<'_>, depth: usize) {
        let ty = node.child_by_field_name("constructed_type");
        let suffix = child_of_kind(node, "constructor_suffix");
        let labels = suffix.map(|s| self.call_labels(s)).unwrap_or_default();
        if let Some(path) = ty.and_then(|t| name_path(t, self.source)) {
            let (recv, name) = match path.rsplit_once('.') {
                Some((r, n)) => (Some(r.to_string()), n.to_string()),
                None => (None, path.clone()),
            };
            let n = Self::labeled_name(&name, &labels);
            self.push_ref(RefKind::Call, &n, recv, span_of(ty.unwrap_or(node)), Some(labels.len() as u32));
        }
        if let Some(t) = ty {
            for a in named_children(t).into_iter().filter(|c| c.kind() == "type_arguments") {
                self.type_ref(a);
            }
        }
        if let Some(s) = suffix {
            self.visit_suffix(s, depth);
        }
    }

    /// `a[i]`: `ProcessInfo.processInfo.environment["X"]` reads `X`; the
    /// receiver and the index are walked.
    fn subscript(&mut self, callee: Node<'_>, suffix: Node<'_>, depth: usize) {
        let env = name_path(callee, self.source).is_some_and(|p| p.ends_with("processInfo.environment"));
        if env && let Some(var) = single_argument(suffix).and_then(|v| literal_text(v, self.source)) {
            self.push_ref(RefKind::EnvVar, var, None, span_of(callee), None);
        }
        self.visit_receiver(callee, depth);
        self.visit_suffix(suffix, depth);
    }

    // -- receivers ---------------------------------------------------------------

    /// The receiver text of `x.f()` (see the crate docs).
    pub(crate) fn receiver_of(&self, target: Node<'_>) -> Option<String> {
        let t = strip_expr(target);
        match t.kind() {
            "self_expression" => Some(SELF_RECEIVER.to_string()),
            "super_expression" => Some("super".to_string()),
            "simple_identifier" => Some(self.identifier_receiver(t)),
            "navigation_expression" => Some(self.path_receiver(t)),
            "call_expression" => self.call_result_receiver(t),
            "constructor_expression" => t
                .child_by_field_name("constructed_type")
                .and_then(|c| name_path(c, self.source))
                .map(|p| type_receiver(&p)),
            "as_expression" => cast_type(t, self.source).map(|c| type_receiver(&c)),
            "line_string_literal" | "multi_line_string_literal" => Some(type_receiver("String")),
            "array_literal" => Some(type_receiver("Array")),
            _ => Some(self.raw_receiver(t)),
        }
    }

    /// A receiver as written, bounded.
    fn raw_receiver(&self, t: Node<'_>) -> String {
        let s = self.text(t);
        s.chars().take(MAX_RECEIVER).collect()
    }

    /// A typed receiver; a generic parameter in scope stands for its first
    /// constraint (`T: Shape` → `$type:Shape`), unconstrained for nothing.
    fn typed(&self, class: &str) -> Option<String> {
        match self.type_params.iter().rposition(|p| p == class) {
            Some(i) => self.type_bounds[i].as_deref().map(type_receiver),
            None => Some(type_receiver(class)),
        }
    }

    /// `x.f()`: a local or parameter (typed when the file shows it), a
    /// property of the enclosing type (`$type:` when declared in the file,
    /// else `$member:x`), `Self`, or a name as written (a type, a global).
    fn identifier_receiver(&self, id: Node<'_>) -> String {
        let name = self.text(id).trim_matches('`');
        let pos = id.start_byte() as u32;
        if name == "Self" {
            return SELF_RECEIVER.to_string();
        }
        if self.locals.is_local(name, pos) {
            return self.local_receiver(name, pos).unwrap_or_else(|| name.to_string());
        }
        if !is_type_like(name)
            && let Some(r) = self.property_receiver(&[name])
        {
            return r;
        }
        name.to_string()
    }

    /// The receiver of a local: its type, else the result of the call that
    /// initializes it.
    fn local_receiver(&self, name: &str, pos: u32) -> Option<String> {
        if let Some(c) = self.locals.class_of(name, pos) {
            return self.typed(&c);
        }
        let (s, e) = self.locals.lookup(name, pos)?.init?;
        let init = self.node_at(s, e)?;
        // `let x = x.f()` names the outer `x` in its initializer, which the
        // table cannot tell from the new one: bounded.
        let depth = self.recv_depth.get();
        if depth >= MAX_RECEIVER_DEPTH || strip_expr(init).kind() != "call_expression" {
            return None;
        }
        self.recv_depth.set(depth + 1);
        let out = self.call_result_receiver(strip_expr(init));
        self.recv_depth.set(depth);
        out
    }

    /// The node spanning exactly `[s, e)` in this file.
    fn node_at(&self, s: usize, e: usize) -> Option<Node<'_>> {
        let root = self.root?;
        root.descendant_for_byte_range(s, e).filter(|n| n.start_byte() == s && n.end_byte() == e)
    }

    /// `a.b.f()` / `self.a.f()`: a member chain of the enclosing type, a
    /// type path (`Foo.Bar.f()`), a static property typed in the file
    /// (`Foo.shared.f()`), else the path as written.
    fn path_receiver(&self, nav: Node<'_>) -> String {
        let Some(path) = name_path(nav, self.source) else { return self.raw_receiver(nav) };
        let segs: Vec<&str> = path.split('.').collect();
        if segs[0] == "self" {
            return self.property_receiver(&segs[1..]).unwrap_or_else(|| self.raw_receiver(nav));
        }
        if segs.len() == 2
            && is_type_like(segs[0])
            && !is_type_like(segs[1])
            && let Some(Some(c)) = self.props.get(segs[0]).and_then(|m| m.get(segs[1]))
            && let Some(r) = self.typed(c)
        {
            return r;
        }
        let first = nav_head(nav);
        if let Some(h) = first
            && h.kind() == "simple_identifier"
            && !is_type_like(segs[0])
            && !self.locals.is_local(segs[0], h.start_byte() as u32)
            && let Some(r) = self.property_receiver(&segs)
        {
            return r;
        }
        path
    }

    /// A chain of properties of the enclosing type: `$type:T` when the
    /// file declares the first property's type and the chain has one
    /// segment, else `$member:a.b`. `None` outside a type.
    fn property_receiver(&self, chain: &[&str]) -> Option<String> {
        let owner = self.containers.iter().rev().find_map(|c| c.type_name.clone())?;
        if chain.is_empty() {
            return Some(SELF_RECEIVER.to_string());
        }
        let declared = self.props.get(&owner).and_then(|m| m.get(chain[0]));
        match declared {
            Some(Some(c)) if chain.len() == 1 => self.typed(c),
            _ if chain.iter().all(|s| !s.is_empty()) => Some(format!("{MEMBER_RECEIVER_PREFIX}{}", chain.join("."))),
            _ => None,
        }
    }

    /// `f().g()`: a construction is its type; another call is a
    /// `$ret:<receiver>#<name>` receiver.
    fn call_result_receiver(&self, call: Node<'_>) -> Option<String> {
        if let Some(c) = call_class(call, self.source) {
            return self.typed(&c).or(Some(c));
        }
        let callee = operand_callee(call.named_child(0)?, self.source);
        let (recv, name) = match callee.kind() {
            "simple_identifier" => {
                let name = self.text(callee).trim_matches('`');
                if self.locals.is_local(name, callee.start_byte() as u32) {
                    return Some(self.raw_receiver(call));
                }
                (String::new(), name.to_string())
            }
            "navigation_expression" => {
                let target = callee.child_by_field_name("target")?;
                let id = callee.child_by_field_name("suffix")?.child_by_field_name("suffix")?;
                // One call deep (a chain `a.f().g().h()` would recurse per
                // link): a construction as the target is its type.
                let t = strip_expr(target);
                let recv = match t.kind() {
                    "call_expression" => match call_class(t, self.source) {
                        Some(c) => self.typed(&c)?,
                        None => return Some(self.raw_receiver(call)),
                    },
                    "constructor_expression" => self.receiver_of(t)?,
                    _ => self.receiver_of(target)?,
                };
                if recv.contains('#') || recv.contains('(') {
                    return Some(self.raw_receiver(call));
                }
                (recv, self.text(id).trim_matches('`').to_string())
            }
            _ => return Some(self.raw_receiver(call)),
        };
        Some(format!("{RET_RECEIVER_PREFIX}{recv}#{name}"))
    }
}

/// The innermost target of a navigation chain (`a` of `a.b.c`).
fn nav_head(nav: Node<'_>) -> Option<Node<'_>> {
    let mut cur = nav;
    for _ in 0..32 {
        match cur.child_by_field_name("target") {
            Some(t) if t.kind() == "navigation_expression" => cur = t,
            Some(t) => return Some(t),
            None => return None,
        }
    }
    None
}
