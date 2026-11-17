//! Contract hints of a TS / JS file (RFC 0006 phase 4; keys and matching
//! in `rueca_model::contract`). One pass over the syntax tree after the
//! definitions are known, so every hint can name its owner definition.
//!
//! - **HTTP providers** (`routes.rs`), in files that import a server
//!   framework (express, koa-router, fastify, hono, restify, …), or on a
//!   receiver named `app` / `router` / `server` given an inline handler
//!   (a function or a call building one):
//!   `app.get("/x", handler)`, `router.post(…)`, `app.all(…)` (method
//!   `*`), `app.route("/x").get(h).post(h)`, `fastify.route({method, url})`
//!   / hapi `server.route({method, path})`; NestJS `@Controller("x")` +
//!   `@Get(":id")` (composed). Owner: the handler when it is a function the
//!   file defines, else the enclosing definition, else the file.
//! - **HTTP consumers** (`clients.rs`): a call whose callee's last name is
//!   an HTTP verb (`get`, `del`, …), `fetch` (`GET` unless an options
//!   object says `method: "…"`) or `request` (that method, else `*`), with
//!   an argument that renders to a path (`"/…"`, `http(s)://…`,
//!   `` `${base}/…` ``, `"/a/" + id`, a `const` of the enclosing function
//!   bound to one of these) or an options object with such a `url`;
//!   `axios({url, method})`. One hop: when the path interpolates a
//!   parameter of the enclosing function (`` `${host}/api/${endpoint}` ``),
//!   every call of that function in the file passing a string literal there
//!   is a consumer too, at the caller, with the literal in place.
//! - **gRPC** (`grpc.rs`, `@grpc/grpc-js` generated clients): consumers
//!   `client.sayHello(…)` on a name bound to `new GreeterClient(…)` (key
//!   `Greeter/SayHello`: the first letter upper-cased); providers
//!   `server.addService(GreeterService, { sayHello })`.

mod clients;
mod grpc;
mod render;
mod routes;

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_extract::{span_of, text_of};
use rueca_model::contract::truncate_raw;
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, Label, LocalDefId};

/// Contract hints of a parsed file; `defs` are the file's definitions.
pub(crate) fn extract(root: Node<'_>, src: &[u8], defs: &[Definition], max_depth: usize) -> Vec<ContractHint> {
    let mut cx = Ctx::new(root, src, defs, max_depth);
    let mut calls = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            continue;
        }
        match node.kind() {
            "call_expression" => {
                calls.push(node);
                cx.call(node);
            }
            "class_declaration" | "class" | "abstract_class_declaration" => routes::nest_controller(&mut cx, node),
            _ => {}
        }
        let mut c = node.walk();
        stack.extend(node.named_children(&mut c).map(|ch| (ch, depth + 1)));
    }
    clients::hops(&mut cx, &calls);
    cx.hints.sort_by_key(|h| h.span.start_byte);
    cx.hints
}

/// What the file says about names, plus the hints found so far.
pub(super) struct Ctx<'t, 's> {
    src: &'s [u8],
    defs: &'s [Definition],
    max_depth: usize,
    /// Local name → module specifier it was imported / required from.
    imports: HashMap<String, String>,
    /// Every imported / required module specifier.
    modules: HashSet<String>,
    /// Holder name → gRPC service of the generated client it holds; `None`
    /// when bound to several.
    clients: HashMap<String, Option<String>>,
    /// Single-assignment `const` tables of functions, by function node id.
    assigned: HashMap<usize, HashMap<String, Option<Node<'t>>>>,
    /// Consumer templates for the one-hop rule (`clients.rs`).
    templates: Vec<clients::Template>,
    hints: Vec<ContractHint>,
}

impl<'t, 's> Ctx<'t, 's> {
    fn new(root: Node<'t>, src: &'s [u8], defs: &'s [Definition], max_depth: usize) -> Self {
        let mut cx = Ctx {
            src,
            defs,
            max_depth,
            imports: HashMap::new(),
            modules: HashSet::new(),
            clients: HashMap::new(),
            assigned: HashMap::new(),
            templates: Vec::new(),
            hints: Vec::new(),
        };
        cx.scan(root);
        cx
    }

    /// Imports, `require`s and gRPC client bindings.
    fn scan(&mut self, root: Node<'t>) {
        let mut stack = vec![(root, 0usize)];
        let mut bindings = Vec::new();
        while let Some((n, depth)) = stack.pop() {
            match n.kind() {
                "import_statement" => self.scan_import(n),
                "variable_declarator" => {
                    self.scan_require(n);
                    bindings.push(n);
                }
                "assignment_expression" | "public_field_definition" => bindings.push(n),
                _ => {}
            }
            if depth < self.max_depth {
                let mut c = n.walk();
                stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
            }
        }
        for n in bindings {
            grpc::scan_binding(self, n);
        }
    }

    /// `import x, { a as b } from "m"`, `import * as ns from "m"`.
    fn scan_import(&mut self, n: Node<'t>) {
        let Some(module) = n.child_by_field_name("source").map(|s| string_value(s, self.src)) else { return };
        self.modules.insert(module.clone());
        let mut stack = vec![n];
        while let Some(cur) = stack.pop() {
            let local = match cur.kind() {
                "import_specifier" => cur.child_by_field_name("alias").or_else(|| cur.child_by_field_name("name")),
                "identifier" => Some(cur),
                _ => None,
            };
            if let Some(local) = local {
                self.imports.insert(self.text(local), module.clone());
                continue;
            }
            let mut c = cur.walk();
            stack.extend(cur.named_children(&mut c).filter(|ch| ch.kind() != "string"));
        }
    }

    /// `const x = require("m")`, `const { a } = require("m")`.
    fn scan_require(&mut self, n: Node<'t>) {
        let (Some(name), Some(value)) = (n.child_by_field_name("name"), n.child_by_field_name("value")) else {
            return;
        };
        let Some(module) = require_target(value, self.src) else { return };
        self.modules.insert(module.clone());
        let mut stack = vec![name];
        while let Some(cur) = stack.pop() {
            match cur.kind() {
                "identifier" | "shorthand_property_identifier_pattern" => {
                    self.imports.insert(self.text(cur), module.clone());
                }
                _ => {
                    let mut c = cur.walk();
                    stack.extend(cur.named_children(&mut c));
                }
            }
        }
    }

    fn call(&mut self, call: Node<'t>) {
        if routes::server_route(self, call) || grpc::add_service(self, call) || grpc::client_call(self, call) {
            return;
        }
        clients::http_call(self, call);
    }

    // ---------------------------------------------------------------------
    // Shared helpers
    // ---------------------------------------------------------------------

    fn text(&self, n: Node<'_>) -> String {
        text_of(n, self.src).into_owned()
    }

    /// Whether the file imports (or requires) a module whose specifier is
    /// one of `modules`.
    fn imports_any(&self, modules: &[&str]) -> bool {
        self.modules.iter().any(|m| modules.contains(&m.as_str()))
    }

    /// Records a hint whose evidence is `node` (its text, one line).
    fn push(
        &mut self,
        side: ContractSide,
        kind: ContractKind,
        key: String,
        node: Node<'_>,
        owner: Option<LocalDefId>,
        framework: &str,
    ) {
        let raw = text_of(node, self.src).split_whitespace().collect::<Vec<_>>().join(" ");
        self.hints.push(ContractHint {
            side,
            kind,
            key,
            raw: truncate_raw(&raw),
            span: span_of(node),
            owner,
            framework: framework.to_string(),
        });
    }

    /// The definition whose name is the node `name`.
    fn def_named_at(&self, name: Node<'_>) -> Option<LocalDefId> {
        let (s, e) = (name.start_byte() as u32, name.end_byte() as u32);
        self.defs.iter().position(|d| d.name_span.start_byte == s && d.name_span.end_byte == e).map(|i| i as LocalDefId)
    }

    /// The definition whose span is exactly `node` (a function expression
    /// the pack made a definition).
    fn def_spanning(&self, node: Node<'_>) -> Option<LocalDefId> {
        let span = span_of(node);
        self.defs
            .iter()
            .position(|d| {
                is_scope_label(d.label) && d.span.start_byte == span.start_byte && d.span.end_byte == span.end_byte
            })
            .map(|i| i as LocalDefId)
    }

    /// The innermost function, method or class around `node`.
    fn enclosing(&self, node: Node<'_>) -> Option<LocalDefId> {
        let span = span_of(node);
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| is_scope_label(d.label) && d.span.contains(&span))
            .max_by_key(|(_, d)| d.span.start_byte)
            .map(|(i, _)| i as LocalDefId)
    }

    /// The handler a route names: a module-level function of the file
    /// (`handler`), a named function expression, else the enclosing
    /// definition of `site`.
    fn handler_owner(&self, handler: Option<Node<'_>>, site: Node<'_>) -> Option<LocalDefId> {
        let named = handler.and_then(|h| match h.kind() {
            "identifier" | "shorthand_property_identifier" => {
                let name = self.text(h);
                self.defs
                    .iter()
                    .position(|d| d.parent.is_none() && is_scope_label(d.label) && d.qualified_name == name)
                    .map(|i| i as LocalDefId)
            }
            _ => self.def_spanning(h),
        });
        named.or_else(|| self.enclosing(site))
    }
}

fn is_scope_label(label: Label) -> bool {
    matches!(label, Label::Function | Label::Method | Label::Class | Label::Test)
}

/// `require("m")` → `m`.
fn require_target(value: Node<'_>, src: &[u8]) -> Option<String> {
    if value.kind() != "call_expression" {
        return None;
    }
    let func = value.child_by_field_name("function")?;
    if text_of(func, src) != "require" {
        return None;
    }
    let arg = value.child_by_field_name("arguments")?.named_child(0)?;
    (arg.kind() == "string").then(|| string_value(arg, src))
}

/// The text of a string literal without its quotes.
fn string_value(n: Node<'_>, src: &[u8]) -> String {
    let t = text_of(n, src);
    let t = t.as_ref();
    if t.len() >= 2 { t[1..t.len() - 1].to_string() } else { String::new() }
}

/// The literal value of a plain string (no template), if `n` is one.
fn plain_string(n: Node<'_>, src: &[u8]) -> Option<String> {
    (n.kind() == "string").then(|| string_value(n, src))
}

/// Arguments of a call / `new` expression.
fn arguments<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    let Some(args) = call.child_by_field_name("arguments") else { return Vec::new() };
    let mut c = args.walk();
    args.named_children(&mut c).filter(|a| a.kind() != "comment").collect()
}

/// `(receiver, property name)` of a member callee, or `(None, name)` of a
/// bare identifier callee.
fn callee<'t>(call: Node<'t>, src: &[u8]) -> Option<(Option<Node<'t>>, String)> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "member_expression" => {
            let prop = func.child_by_field_name("property")?;
            Some((func.child_by_field_name("object"), text_of(prop, src).into_owned()))
        }
        "identifier" => Some((None, text_of(func, src).into_owned())),
        _ => None,
    }
}

/// The value of property `key` of an object literal (`{ key: v }`, or the
/// shorthand `{ key }`, which is the identifier itself).
fn property<'t>(object: Node<'t>, key: &str, src: &[u8]) -> Option<Node<'t>> {
    if object.kind() != "object" {
        return None;
    }
    let mut c = object.walk();
    object.named_children(&mut c).find_map(|p| match p.kind() {
        "pair" => {
            let k = p.child_by_field_name("key")?;
            let name = text_of(k, src);
            (name.trim_matches(['"', '\'']) == key).then(|| p.child_by_field_name("value")).flatten()
        }
        "shorthand_property_identifier" => (text_of(p, src) == key).then_some(p),
        _ => None,
    })
}

/// Root identifier of a member chain (`a.b.c` → `a`; `this.x` → `this`).
fn root_name(n: Node<'_>, src: &[u8]) -> Option<String> {
    let mut cur = n;
    loop {
        match cur.kind() {
            "member_expression" => cur = cur.child_by_field_name("object")?,
            "call_expression" => cur = cur.child_by_field_name("function")?,
            "identifier" | "this" => return Some(text_of(cur, src).into_owned()),
            _ => return None,
        }
    }
}

/// Last name of an identifier / member chain (`this._client` → `_client`).
fn last_name(n: Node<'_>, src: &[u8]) -> Option<String> {
    match n.kind() {
        "identifier" | "property_identifier" | "private_property_identifier" => Some(text_of(n, src).into_owned()),
        "member_expression" => Some(text_of(n.child_by_field_name("property")?, src).into_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
