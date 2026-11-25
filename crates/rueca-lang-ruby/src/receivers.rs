//! Receivers of calls (crate docs, *Receivers*).
//!
//! A receiver is written for the resolver as:
//!
//! | `x` in `x.m` | Receiver |
//! |---|---|
//! | `self` | `self` |
//! | `self.class` | `self.class` (the class: its singleton methods) |
//! | a constant path `Foo::Bar`, `::Foo` | as written (singleton methods; `new` → `initialize`) |
//! | a local or instance variable every assignment of which builds the same class (`x = Foo.new`), a parameter typed by a `sig` / RBS comment, `T.let(v, Foo)`, `x = v #: Foo` | `$type:Foo` |
//! | the result of a call (`a.b.m`, `helper.m`, a variable assigned from one) | `$ret:<receiver of the call>#<name>` (empty receiver = implicit `self`); the resolver types it from the callee's declared return type (`#: -> T` in its signature) |
//! | anything else | `?` |

use rueca_extract::tree_sitter::Node;

use crate::extract::{Ctx, Extractor, absolute_type, const_path, statement_node};
use crate::locals::DefKind;
use crate::refs::first_arg;
use crate::types;

/// What `self` is inside `n` when `n` decides it: `Some(true)` for the
/// class itself (`def self.x`, `class << self`, a class body), `Some(false)`
/// for an instance (`def x`).
fn self_kind(n: Node<'_>) -> Option<bool> {
    match n.kind() {
        "singleton_method" => Some(n.child_by_field_name("object").is_some_and(|o| o.kind() == "self")),
        "method" => Some(false),
        "singleton_class" => Some(n.child_by_field_name("value").is_some_and(|v| v.kind() == "self")),
        // Class-body code runs with `self` = the class.
        "class" | "module" => Some(true),
        _ => None,
    }
}

/// Every node of a tree in document order.
fn preorder(root: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        out.push(n);
        let mut c = n.walk();
        let children: Vec<Node<'_>> = n.children(&mut c).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// Unknown receiver.
pub(crate) const UNKNOWN: &str = "?";
/// Nested `$ret:` receivers written at most.
const MAX_RET: usize = 3;

fn root_of(node: Node<'_>) -> Node<'_> {
    let mut n = node;
    while let Some(p) = n.parent() {
        n = p;
    }
    n
}

fn ret_depth(s: &str) -> usize {
    s.matches("$ret:").count()
}

impl Extractor<'_> {
    /// The receiver text of `node` in receiver position (module docs).
    pub(crate) fn receiver_of(&self, node: Node<'_>, ctx: &Ctx, depth: usize) -> String {
        self.value_of(node, ctx.ns.as_deref(), ctx.singleton, depth).unwrap_or_else(|| UNKNOWN.to_string())
    }

    /// What `node` evaluates to, as a receiver; `None` when unknown.
    fn value_of(&self, node: Node<'_>, ns: Option<&str>, singleton: bool, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match node.kind() {
            "self" => Some("self".into()),
            "constant" | "scope_resolution" => const_path(node, self.src),
            "identifier" => self.identifier_value(node),
            "instance_variable" => {
                self.ivars.get(&(ns.unwrap_or("").to_string(), singleton, self.t(node).to_string())).cloned().flatten()
            }
            "parenthesized_statements" if node.named_child_count() == 1 => {
                self.value_of(node.named_child(0)?, ns, singleton, depth + 1)
            }
            "call" => self.call_value(node, ns, singleton, depth),
            _ => None,
        }
    }

    /// A local's consensus value, or the result of the implicit-self call
    /// the identifier is.
    fn identifier_value(&self, node: Node<'_>) -> Option<String> {
        let name = self.t(node);
        let pos = node.start_byte() as u32;
        if !self.locals.is_local(name, pos) {
            return Some(format!("$ret:#{name}"));
        }
        let ids = self.locals.same_table(name, pos);
        let root = root_of(node);
        let mut value: Option<String> = None;
        for i in ids {
            let v = self.binding_value(root, i)?;
            match &value {
                Some(prev) if *prev != v => return None,
                _ => value = Some(v),
            }
        }
        value
    }

    /// `Foo.new` → `$type:Foo`; `self.class`; `T.let(v, Foo)`; else the
    /// call's result (`$ret:`).
    fn call_value(&self, node: Node<'_>, ns: Option<&str>, singleton: bool, depth: usize) -> Option<String> {
        let m = self.t(node.child_by_field_name("method")?);
        let recv = node.child_by_field_name("receiver");
        if let Some(r) = recv {
            let path = const_path(r, self.src);
            match (path.as_deref(), m) {
                (Some(p), "new") => return Some(format!("$type:{p}")),
                (Some("T" | "::T"), "let" | "cast") => {
                    let args = node.child_by_field_name("arguments")?;
                    let text = self.t(args);
                    let inner = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')).unwrap_or(text);
                    return types::t_let_type(inner).map(|t| format!("$type:{}", absolute_type(&t, ns)));
                }
                (Some("T" | "::T"), "must" | "unsafe" | "must_because") => {
                    return self.value_of(first_arg(node)?, ns, singleton, depth + 1);
                }
                _ => {}
            }
            if r.kind() == "self" && m == "class" {
                return Some("self.class".into());
            }
        }
        let inner = match recv {
            Some(r) => self.value_of(r, ns, singleton, depth + 1)?,
            None => String::new(),
        };
        (ret_depth(&inner) < MAX_RET).then(|| format!("$ret:{inner}#{m}"))
    }

    /// The value a local binding holds (memoized): a parameter's declared
    /// type, an assignment's value (or its `#: T` comment).
    fn binding_value(&self, root: Node<'_>, i: usize) -> Option<String> {
        if let Some(v) = self.binding_memo.borrow().get(&i) {
            return v.clone();
        }
        if !self.in_progress.borrow_mut().insert(i) {
            return None;
        }
        let b = &self.locals.bindings[i];
        let v = match b.kind {
            DefKind::Parameter => self.param_types.get(&b.pos).map(|t| format!("$type:{t}")),
            DefKind::Var => self.assigned_value(root, b.pos),
        };
        self.in_progress.borrow_mut().remove(&i);
        self.binding_memo.borrow_mut().insert(i, v.clone());
        v
    }

    /// The value of the assignment whose target starts at byte `pos`.
    fn assigned_value(&self, root: Node<'_>, pos: u32) -> Option<String> {
        let ident = root.descendant_for_byte_range(pos as usize, pos as usize + 1)?;
        let assign = ident.parent().filter(|p| {
            matches!(p.kind(), "assignment" | "operator_assignment")
                && p.child_by_field_name("left").is_some_and(|l| l.id() == ident.id())
        })?;
        let (ns, singleton) = self.site_of(assign);
        if let Some(t) = self.trailing_type(assign) {
            return Some(format!("$type:{}", absolute_type(&t, ns.as_deref())));
        }
        self.value_of(assign.child_by_field_name("right")?, ns.as_deref(), singleton, 1)
    }

    /// The type of a `#: T` comment right after `node` on its last line.
    fn trailing_type(&self, node: Node<'_>) -> Option<String> {
        let next = statement_node(node).next_sibling().or_else(|| node.next_sibling())?;
        (next.kind() == "comment" && next.start_position().row == node.end_position().row)
            .then(|| types::rbs_comment_type(self.t(next)))
            .flatten()
    }

    /// The namespace and `self` kind at `node` (from its ancestors).
    pub(crate) fn site_of(&self, node: Node<'_>) -> (Option<String>, bool) {
        let mut parts: Vec<String> = Vec::new();
        let mut singleton: Option<bool> = None;
        let mut cur = node.parent();
        while let Some(n) = cur {
            if singleton.is_none() {
                singleton = self_kind(n);
            }
            if matches!(n.kind(), "class" | "module")
                && let Some(p) = n.child_by_field_name("name").and_then(|x| const_path(x, self.src))
            {
                parts.push(p.trim_start_matches("::").to_string());
                if p.starts_with("::") {
                    break;
                }
            }
            cur = n.parent();
        }
        parts.reverse();
        let ns = (!parts.is_empty()).then(|| parts.join("::"));
        (ns, singleton.unwrap_or(false))
    }

    /// Types parameters of every method and instance variables of every
    /// class before references are written (a use may precede the
    /// assignment that types it).
    pub(crate) fn prepass_ivars(&mut self, root: Node<'_>) {
        let mut assigns: Vec<Node<'_>> = Vec::new();
        for n in preorder(root) {
            match n.kind() {
                "method" | "singleton_method" => self.prepass_method(n),
                "assignment" | "operator_assignment"
                    if n.child_by_field_name("left").is_some_and(|l| l.kind() == "instance_variable") =>
                {
                    assigns.push(n);
                }
                _ => {}
            }
        }
        for a in assigns {
            self.prepass_ivar(a);
        }
    }

    fn prepass_method(&mut self, n: Node<'_>) {
        let (ns, _) = self.site_of(n);
        if let Some(mt) = self.method_types(n) {
            self.type_parameters(n, &mt, ns.as_deref());
        }
    }

    fn prepass_ivar(&mut self, a: Node<'_>) {
        let Some(left) = a.child_by_field_name("left") else { return };
        let (ns, singleton) = self.site_of(a);
        let key = (ns.clone().unwrap_or_default(), singleton, self.t(left).to_string());
        let value = match self.trailing_type(a) {
            Some(t) => Some(format!("$type:{}", absolute_type(&t, ns.as_deref()))),
            None => a.child_by_field_name("right").and_then(|r| self.value_of(r, ns.as_deref(), singleton, 1)),
        };
        // `@x ||= nil`, `@x = nil`: no information either way.
        if a.child_by_field_name("right").is_some_and(|r| r.kind() == "nil") {
            return;
        }
        let merged = match self.ivars.get(&key) {
            None => value,
            Some(prev) if *prev == value => value,
            Some(_) => None,
        };
        self.ivars.insert(key, merged);
    }
}
