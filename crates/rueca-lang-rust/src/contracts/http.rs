//! HTTP routes (providers) and reqwest calls (consumers) of a Rust file.
//!
//! - actix-web / rocket: `#[get("/x")]`, `#[route("/x", method = "GET",
//!   method = "POST")]` (also path-qualified, `#[actix_web::get(…)]`) on a
//!   function: that function handles it; prefixed by the
//!   `web::scope("/p")` whose `.service(f)` registers the function in this
//!   file.
//! - actix-web: `.route("/x", web::get().to(h))` (on an `App`, a scope or a
//!   `ServiceConfig`), `web::resource("/x").route(web::post().to(h))`,
//!   `web::resource("/x").to(h)` (any method), prefixed by enclosing
//!   scopes of the same expression.
//! - axum: `.route("/x", get(h).post(g))` (`routing::get`, `any` = every
//!   method), prefixed by `.nest("/p", Router::new()…)` when the nested
//!   router is built in the same expression (a router built elsewhere and
//!   nested by name is a recorded gap).
//! - reqwest: `reqwest::get(url)`, `client.get(url)` / `post` / `put` /
//!   `patch` / `delete` / `head` and `client.request(Method::POST, url)` on
//!   a receiver whose text contains `client`; URLs from literals,
//!   `format!` (its `{}` are parameters; a leading one is the base URL,
//!   dropped) and `+`.
//!
//! The owner of a route is its handler when the handler is a function of
//! this file (by name), else the registering definition.

use std::collections::HashMap;

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{HTTP_METHODS, Part, http_key_from_parts};
use rueca_model::{ContractKind, ContractSide, LocalDefId};

use super::{File, Mentions, last_segment, named_children, string_value, text};

/// Lower-case route verbs of actix / rocket attributes, axum method
/// routers, actix `web::get()` and reqwest methods.
const VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options", "trace"];

/// A method router's (method, handler, framework); `None` = any method.
type Pair<'t> = (Option<String>, Node<'t>, &'static str);

/// A piece of a URL expression (owned [`Part`]).
enum Piece {
    Lit(String),
    Hole,
}

pub(crate) struct Http {
    m: Mentions,
    /// actix functions registered with `.service(f)` inside a
    /// `web::scope("/p")` of this file → the scope's full prefix.
    scoped: HashMap<String, String>,
}

impl Http {
    pub(crate) fn new(m: Mentions, root: Node<'_>, src: &[u8]) -> Http {
        let mut h = Http { m, scoped: HashMap::new() };
        if m.actix {
            h.collect_scopes(root, src, 0);
        }
        h
    }

    /// `.service(f)` calls under a `web::scope("/p")`.
    fn collect_scopes(&mut self, node: Node<'_>, src: &[u8], depth: usize) {
        if depth > 512 {
            return;
        }
        if let Some((_, "service", args)) = method_call(node, src) {
            let prefix = prefix_of_chain(node, src, 0);
            for a in args.iter().filter(|a| matches!(a.kind(), "identifier" | "scoped_identifier")) {
                if !prefix.is_empty() {
                    self.scoped.insert(last_segment(text(*a, src)).to_string(), prefix.clone());
                }
            }
        }
        for child in named_children(node) {
            self.collect_scopes(child, src, depth + 1);
        }
    }

    // -- attribute routes ----------------------------------------------------

    /// `#[get("/x")]` and friends on a function item.
    pub(crate) fn attribute_routes(&self, file: &mut File<'_>, func: Node<'_>) {
        if !(self.m.actix || self.m.rocket) {
            return;
        }
        let Some(name) = func.child_by_field_name("name") else { return };
        let framework = if self.m.actix { "actix" } else { "rocket" };
        let prefix = self.scoped.get(file.text(name)).cloned().unwrap_or_default();
        let owner = file.def_at(name);
        for attr in attributes(func) {
            let Some((methods, path)) = route_attribute(attr, file.src) else { continue };
            let path = format!("{prefix}{path}");
            for method in methods {
                if let Some(key) = http_key_from_parts(method.as_deref(), &[Part::Lit(&path)]) {
                    file.push(ContractSide::Provides, ContractKind::Http, key, attr, owner, framework);
                }
            }
        }
    }

    // -- calls ---------------------------------------------------------------

    pub(crate) fn call(&self, file: &mut File<'_>, call: Node<'_>) {
        if let Some((recv, name, args)) = method_call(call, file.src) {
            self.method(file, call, recv, name, &args);
        } else if let Some((path, args)) = path_call(call, file.src)
            && self.m.reqwest
            && path.contains("reqwest")
            && last_segment(path) == "get"
            && let [url] = args.as_slice()
        {
            consume(file, call, Some("GET"), *url);
        }
    }

    fn method(&self, file: &mut File<'_>, call: Node<'_>, recv: Node<'_>, name: &str, args: &[Node<'_>]) {
        match (name, args) {
            ("route", [path, router]) if self.m.axum || self.m.actix => {
                let Some(path) = string_value(*path, file.src) else { return };
                let pairs = method_pairs(*router, file.src, 0);
                self.routes(file, (method_name(call).unwrap_or(call), call), &path, pairs);
            }
            ("route" | "to", [arg]) if self.m.actix => {
                let base = chain_base(recv);
                let Some(path) = resource_path(base, file.src) else { return };
                let pairs = if name == "to" { vec![(None, *arg, "actix")] } else { method_pairs(*arg, file.src, 0) };
                self.routes(file, (base, call), &path, pairs);
            }
            (verb, [url]) if self.m.reqwest && VERBS.contains(&verb) && is_client(recv, file.src) => {
                consume(file, call, Some(verb), *url);
            }
            ("request", [method, url]) if self.m.reqwest && is_client(recv, file.src) => {
                let m = last_segment(file.text(*method)).to_ascii_uppercase();
                let m = HTTP_METHODS.contains(&m.as_str()).then_some(m);
                consume(file, call, m.as_deref(), *url);
            }
            _ => {}
        }
    }

    /// Provider hints of a route's method router pairs; the evidence runs
    /// from `start` (the method name, or `web::resource(…)`) to the call's
    /// end.
    fn routes(&self, file: &mut File<'_>, (start, call): (Node<'_>, Node<'_>), path: &str, pairs: Vec<Pair<'_>>) {
        let path = format!("{}{path}", prefix_of_chain(call, file.src, 0));
        for (method, handler, framework) in pairs {
            let owner = handler_owner(file, handler).or_else(|| file.enclosing(call));
            if let Some(key) = http_key_from_parts(method.as_deref(), &[Part::Lit(&path)]) {
                file.push_range(ContractSide::Provides, ContractKind::Http, key, (start, call), owner, framework);
            }
        }
    }
}

/// The attribute items directly above an item.
fn attributes(item: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cur = item.prev_sibling();
    while let Some(c) = cur {
        match c.kind() {
            "attribute_item" => out.push(c),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        cur = c.prev_sibling();
    }
    out.reverse();
    out
}

/// (methods, path) of `#[get("/x")]` / `#[route("/x", method = "GET")]`.
fn route_attribute(item: Node<'_>, src: &[u8]) -> Option<(Vec<Option<String>>, String)> {
    let attr = *named_children(item).first()?;
    let name = last_segment(text(*named_children(attr).first()?, src));
    let tokens = named_children(attr.child_by_field_name("arguments")?);
    let path = tokens.iter().find_map(|t| string_value(*t, src))?;
    let methods = if VERBS.contains(&name) {
        vec![Some(name.to_ascii_uppercase())]
    } else if name == "route" {
        attribute_methods(&tokens, src)
    } else {
        return None;
    };
    (!methods.is_empty() && path.starts_with('/')).then_some((methods, path))
}

/// The `method = "GET"` values of a `#[route(…)]` attribute.
fn attribute_methods(tokens: &[Node<'_>], src: &[u8]) -> Vec<Option<String>> {
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if text(*t, src) == "method"
            && let Some(v) = tokens.get(i + 1).and_then(|n| string_value(*n, src))
        {
            out.push(Some(v.to_ascii_uppercase()));
        }
    }
    out
}

/// The (method, handler, framework) pairs of a method router:
/// `get(h).post(g)` (axum), `web::get().to(h)` (actix).
fn method_pairs<'t>(expr: Node<'t>, src: &[u8], depth: usize) -> Vec<Pair<'t>> {
    if depth > 32 || expr.kind() != "call_expression" {
        return Vec::new();
    }
    if let Some((recv, name, args)) = method_call(expr, src) {
        return match (name, args.as_slice()) {
            ("to", [h]) => actix_method(recv, src).map(|m| vec![(m, *h, "actix")]).unwrap_or_default(),
            (verb, [h]) if VERBS.contains(&verb) || verb == "any" => {
                let mut pairs = method_pairs(recv, src, depth + 1);
                pairs.push((axum_method(verb), *h, "axum"));
                pairs
            }
            _ => method_pairs(recv, src, depth + 1),
        };
    }
    match path_call(expr, src) {
        Some((path, args)) => match (last_segment(path), args.as_slice()) {
            (verb, [h]) if VERBS.contains(&verb) || verb == "any" => vec![(axum_method(verb), *h, "axum")],
            _ => Vec::new(),
        },
        None => Vec::new(),
    }
}

fn axum_method(verb: &str) -> Option<String> {
    (verb != "any").then(|| verb.to_ascii_uppercase())
}

/// The method of actix's `web::get()` (`Some(None)` for `web::route()`).
fn actix_method(recv: Node<'_>, src: &[u8]) -> Option<Option<String>> {
    let base = chain_base(recv);
    let (path, args) = path_call(base, src)?;
    match last_segment(path) {
        _ if !args.is_empty() => None,
        "route" => Some(None),
        verb if VERBS.contains(&verb) => Some(Some(verb.to_ascii_uppercase())),
        _ => None,
    }
}

/// The path of `web::resource("/x")`.
fn resource_path(base: Node<'_>, src: &[u8]) -> Option<String> {
    let (path, args) = path_call(base, src)?;
    (last_segment(path) == "resource").then(|| string_value(*args.first()?, src)).flatten()
}

/// The handler function of this file named by a route argument.
fn handler_owner(file: &File<'_>, handler: Node<'_>) -> Option<LocalDefId> {
    match handler.kind() {
        "identifier" | "scoped_identifier" => file.callable_named(last_segment(file.text(handler))),
        "generic_function" => handler_owner(file, handler.child_by_field_name("function")?),
        _ => None,
    }
}

// -- chains -----------------------------------------------------------------

/// `recv.name(args)`.
pub(super) fn method_call<'t, 's>(call: Node<'t>, src: &'s [u8]) -> Option<(Node<'t>, &'s str, Vec<Node<'t>>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let func = call.child_by_field_name("function")?;
    let func = if func.kind() == "generic_function" { func.child_by_field_name("function")? } else { func };
    if func.kind() != "field_expression" {
        return None;
    }
    let name = text(func.child_by_field_name("field")?, src);
    Some((func.child_by_field_name("value")?, name, named_children(call.child_by_field_name("arguments")?)))
}

/// The method name node of `recv.name(args)`.
fn method_name(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("function")?.child_by_field_name("field")
}

/// `path::to::f(args)`: the path as written and the arguments.
pub(super) fn path_call<'t, 's>(call: Node<'t>, src: &'s [u8]) -> Option<(&'s str, Vec<Node<'t>>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let func = call.child_by_field_name("function")?;
    let func = if func.kind() == "generic_function" { func.child_by_field_name("function")? } else { func };
    if !matches!(func.kind(), "identifier" | "scoped_identifier") {
        return None;
    }
    Some((text(func, src), named_children(call.child_by_field_name("arguments")?)))
}

/// The first call of a method chain (`web::scope("/a")` of
/// `web::scope("/a").service(f).route(…)`).
fn chain_base(mut node: Node<'_>) -> Node<'_> {
    for _ in 0..256 {
        let next = node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "field_expression")
            .and_then(|f| f.child_by_field_name("value"));
        match next {
            Some(n) if node.kind() == "call_expression" => node = n,
            _ => break,
        }
    }
    node
}

/// The last call of the method chain `node` is part of.
fn chain_top(mut node: Node<'_>) -> Node<'_> {
    for _ in 0..256 {
        let up = node
            .parent()
            .filter(|p| p.kind() == "field_expression")
            .and_then(|p| p.parent())
            .filter(|g| g.kind() == "call_expression");
        match up {
            Some(g) => node = g,
            None => break,
        }
    }
    node
}

/// The path prefix of a router chain: its own `web::scope("/p")` base, and
/// the scopes / axum `.nest("/p", …)` it is an argument of.
fn prefix_of_chain(node: Node<'_>, src: &[u8], depth: usize) -> String {
    if depth > 16 {
        return String::new();
    }
    let own = scope_path(chain_base(node), src).unwrap_or_default();
    let top = chain_top(node);
    let outer = top
        .parent()
        .filter(|p| p.kind() == "arguments")
        .and_then(|a| a.parent())
        .and_then(|c| outer_prefix(c, top, src, depth))
        .unwrap_or_default();
    format!("{outer}{own}")
}

/// The prefix a container call gives the router chain `arg` passed to it.
fn outer_prefix(container: Node<'_>, arg: Node<'_>, src: &[u8], depth: usize) -> Option<String> {
    let (_, name, args) = method_call(container, src)?;
    match (name, args.as_slice()) {
        ("nest", [p, r]) if *r == arg => Some(string_value(*p, src)? + &prefix_of_chain(container, src, depth + 1)),
        ("service", _) => Some(prefix_of_chain(container, src, depth + 1)),
        _ => None,
    }
}

/// The path of `web::scope("/p")`.
fn scope_path(base: Node<'_>, src: &[u8]) -> Option<String> {
    let (path, args) = path_call(base, src)?;
    (last_segment(path) == "scope").then(|| string_value(*args.first()?, src)).flatten()
}

// -- consumers --------------------------------------------------------------

/// Whether a receiver looks like an HTTP client (`client`, `self.http_client`,
/// `reqwest::Client::new()`).
fn is_client(recv: Node<'_>, src: &[u8]) -> bool {
    text(recv, src).to_ascii_lowercase().contains("client")
}

/// A consumer hint for a call with a URL argument.
fn consume(file: &mut File<'_>, call: Node<'_>, method: Option<&str>, url: Node<'_>) {
    let mut pieces = Vec::new();
    url_pieces(url, file.src, &mut pieces, 0);
    if !has_path_literal(&pieces) {
        return;
    }
    let parts: Vec<Part<'_>> = pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(s) => Part::Lit(s),
            Piece::Hole => Part::Hole,
        })
        .collect();
    if let Some(key) = http_key_from_parts(method, &parts) {
        let owner = file.enclosing(call);
        file.push(ContractSide::Consumes, ContractKind::Http, key, call, owner, "reqwest");
    }
}

/// Literal pieces and holes of a URL expression.
fn url_pieces(node: Node<'_>, src: &[u8], out: &mut Vec<Piece>, depth: usize) {
    if depth > 16 {
        return out.push(Piece::Hole);
    }
    if let Some(v) = string_value(node, src).or_else(|| format_string(node, src)) {
        return out.push(Piece::Lit(v));
    }
    match node.kind() {
        "reference_expression" | "parenthesized_expression" => match named_children(node).last() {
            Some(inner) => url_pieces(*inner, src, out, depth + 1),
            None => out.push(Piece::Hole),
        },
        "binary_expression" => {
            for side in ["left", "right"] {
                match node.child_by_field_name(side) {
                    Some(s) => url_pieces(s, src, out, depth + 1),
                    None => out.push(Piece::Hole),
                }
            }
        }
        "call_expression" => match method_call(node, src) {
            Some((recv, "as_str" | "to_string" | "to_owned" | "as_ref" | "clone" | "into", _)) => {
                url_pieces(recv, src, out, depth + 1)
            }
            _ => out.push(Piece::Hole),
        },
        _ => out.push(Piece::Hole),
    }
}

/// The format string of `format!("…", …)` / `concat!`.
fn format_string(node: Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "macro_invocation" {
        return None;
    }
    let name = last_segment(text(node.child_by_field_name("macro")?, src));
    if !matches!(name, "format" | "concat") {
        return None;
    }
    let tokens = named_children(node).into_iter().find(|c| c.kind() == "token_tree")?;
    named_children(tokens).first().and_then(|t| string_value(*t, src))
}

/// Whether a literal piece holds a `/` of the path (not only the
/// `scheme://` of a URL whose host and path are variables).
fn has_path_literal(pieces: &[Piece]) -> bool {
    pieces.iter().any(|p| match p {
        Piece::Lit(s) => s.split_once("://").map_or(s.as_str(), |(_, rest)| rest).contains('/'),
        Piece::Hole => false,
    })
}
