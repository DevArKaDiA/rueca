//! HTTP consumers: client calls with a path-like URL, and the one-hop rule
//! for wrappers whose URL interpolates a parameter.

use std::collections::HashMap;

use rueca_extract::tree_sitter::Node;
use rueca_model::{ContractKind, ContractSide, LocalDefId};

use super::render::{Piece, enclosing_function, http_key, joined, looks_like_path, params};
use super::{Ctx, arguments, callee, plain_string, property, root_name};

/// Callee names that name their HTTP method.
const VERBS: &[(&str, &str)] = &[
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("patch", "PATCH"),
    ("delete", "DELETE"),
    ("del", "DELETE"),
    ("head", "HEAD"),
    ("options", "OPTIONS"),
];

/// Client libraries a consumer is attributed to, by its callee root or the
/// module that root was imported from.
const LIBRARIES: &[&str] = &["axios", "got", "ky", "superagent", "undici", "node-fetch", "ofetch"];

/// The method of a callee name (`del` → `DELETE`).
pub(super) fn verb(name: &str) -> Option<&'static str> {
    VERBS.iter().find(|(n, _)| *n == name).map(|(_, m)| *m)
}

/// A consumer whose URL interpolates parameters of its function `name`:
/// calls of `name` passing literals there are consumers too.
pub(super) struct Template {
    name: String,
    params: Vec<String>,
    pieces: Vec<Piece>,
    method: String,
    framework: &'static str,
}

/// Records the consumer hint of `call`, if it is an HTTP client call.
pub(super) fn http_call<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) {
    let Some((recv, name)) = callee(call, cx.src) else { return };
    let args = arguments(call);
    let Some(method) = call_method(cx, &name, &args) else { return };
    let Some((pieces, method)) = url_argument(cx, &args, method) else { return };
    let framework = library(cx, recv, &name);
    let Some(key) = http_key(&method, &pieces) else { return };
    let owner = cx.enclosing(call);
    cx.push(ContractSide::Consumes, ContractKind::Http, key, call, owner, framework);
    template(cx, call, pieces, method, framework);
}

/// The method of a client call by its callee: a verb, `fetch` (`GET`
/// unless an options object says otherwise), `request` / `axios(…)`
/// (the options' method, else `*`). `None`: not an HTTP call.
fn call_method(cx: &Ctx<'_, '_>, name: &str, args: &[Node<'_>]) -> Option<String> {
    if let Some(m) = verb(name) {
        return Some(m.to_string());
    }
    let default = match name {
        "fetch" => "GET",
        "request" | "axios" | "got" | "ky" | "ofetch" | "$fetch" => "*",
        _ => return None,
    };
    let stated = args.iter().find_map(|a| property(*a, "method", cx.src)).and_then(|m| plain_string(m, cx.src));
    Some(stated.map_or(default.to_string(), |m| m.to_ascii_uppercase()))
}

/// The first argument that renders to a path, or the `url` / `path` of an
/// options object; an options object's `method` overrides a `*` method.
fn url_argument<'t>(cx: &mut Ctx<'t, '_>, args: &[Node<'t>], method: String) -> Option<(Vec<Piece>, String)> {
    for a in args {
        if let Some(p) = cx.render(*a, 0).filter(|p| looks_like_path(p)) {
            return Some((p, method));
        }
        let url = property(*a, "url", cx.src).or_else(|| property(*a, "path", cx.src));
        if let Some(p) = url.and_then(|u| cx.render(u, 0)).filter(|p| looks_like_path(p)) {
            return Some((p, method));
        }
    }
    None
}

/// The client library of a call: its callee root (`axios.get`, `got(…)`)
/// or the module that root came from; `fetch` for a bare `fetch`; else
/// `js-http`.
fn library(cx: &Ctx<'_, '_>, recv: Option<Node<'_>>, name: &str) -> &'static str {
    let root = recv.and_then(|r| root_name(r, cx.src)).unwrap_or_else(|| name.to_string());
    let module = cx.imports.get(&root).map(String::as_str);
    if let Some(lib) = LIBRARIES.iter().find(|l| **l == root || module == Some(**l)) {
        return lib;
    }
    if name == "fetch" { "fetch" } else { "js-http" }
}

/// Whether a callee receiver comes from an HTTP client library
/// (`axios.get`, `got.post`, `ky.get`).
pub(super) fn is_client_receiver(cx: &Ctx<'_, '_>, recv: Node<'_>) -> bool {
    root_name(recv, cx.src).is_some_and(|root| {
        let module = cx.imports.get(&root).map(String::as_str);
        LIBRARIES.iter().any(|l| *l == root || module == Some(*l))
    })
}

/// Remembers a consumer whose URL interpolates a parameter of its named
/// enclosing function.
fn template(cx: &mut Ctx<'_, '_>, call: Node<'_>, pieces: Vec<Piece>, method: String, framework: &'static str) {
    if !pieces.iter().any(|p| matches!(p, Piece::Hole(Some(_)))) {
        return;
    }
    let Some(func) = enclosing_function(call) else { return };
    let Some(name) = function_name(func, cx.src) else { return };
    let params = params(func, cx.src);
    cx.templates.push(Template { name, params, pieces, method, framework });
}

/// The name a function is called by: its own, or the variable, property or
/// field it is assigned to.
fn function_name(func: Node<'_>, src: &[u8]) -> Option<String> {
    if let Some(n) = func.child_by_field_name("name") {
        return Some(rueca_extract::text_of(n, src).into_owned());
    }
    let parent = func.parent()?;
    let key = match parent.kind() {
        "variable_declarator" | "public_field_definition" => parent.child_by_field_name("name"),
        "pair" => parent.child_by_field_name("key"),
        "assignment_expression" => parent.child_by_field_name("left"),
        _ => None,
    }?;
    super::last_name(key, src).or_else(|| Some(rueca_extract::text_of(key, src).into_owned()))
}

/// The one-hop rule: every call of a template's function passing string
/// literals at its interpolated parameters is a consumer at the caller.
pub(super) fn hops<'t>(cx: &mut Ctx<'t, '_>, calls: &[Node<'t>]) {
    if cx.templates.is_empty() {
        return;
    }
    let mut found: Vec<(Node<'t>, String, &'static str, Option<LocalDefId>)> = Vec::new();
    for call in calls {
        let Some((_, name)) = callee(*call, cx.src) else { continue };
        for t in cx.templates.iter().filter(|t| t.name == name) {
            let subst = literal_args(cx, *call, &t.params);
            if subst.is_empty() {
                continue;
            }
            if let Some(key) = http_key(&t.method, &joined(&t.pieces, &subst)) {
                found.push((*call, key, t.framework, cx.enclosing(*call)));
            }
        }
    }
    for (call, key, framework, owner) in found {
        cx.push(ContractSide::Consumes, ContractKind::Http, key, call, owner, framework);
    }
}

/// Parameter name → string literal passed at its position.
fn literal_args(cx: &Ctx<'_, '_>, call: Node<'_>, params: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (p, a) in params.iter().zip(arguments(call)) {
        if let Some(v) = plain_string(a, cx.src).filter(|_| !p.is_empty()) {
            out.insert(p.clone(), v);
        }
    }
    out
}
