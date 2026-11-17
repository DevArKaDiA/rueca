//! HTTP consumers: client calls with a path-like URL argument.

use rueca_extract::text_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::HTTP_METHODS;
use rueca_model::{ContractKind, ContractSide};

use super::render::{Piece, http_key, looks_like_path};
use super::{Ctx, keyword, last_name, positional, root_name};

/// Callee names (leading `_` dropped) that name their HTTP method.
const VERBS: &[(&str, &str)] = &[
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("patch", "PATCH"),
    ("delete", "DELETE"),
    ("head", "HEAD"),
    ("options", "OPTIONS"),
    ("get_api_list", "GET"),
];

/// Keyword arguments that carry the URL.
const URL_KEYWORDS: &[&str] = &["url", "path"];

/// Client libraries a consumer is attributed to, by the module its
/// receiver (or, failing that, the file) imports.
const LIBRARIES: &[&str] = &["httpx", "requests", "aiohttp", "urllib3", "urllib"];

/// The method of a callee name (`_post` → `POST`).
pub(super) fn verb(name: &str) -> Option<&'static str> {
    let bare = name.trim_start_matches('_');
    VERBS.iter().find(|(n, _)| *n == bare).map(|(_, m)| *m)
}

/// The HTTP method a call states in its arguments.
enum Stated<'t> {
    /// A literal method argument or `method=`: the method and the
    /// positional arguments after it.
    Method(String, Vec<Node<'t>>),
    /// `method=` with a value that names no method.
    Unknown,
    None,
}

/// Records the consumer hint of `call`, if it is an HTTP client call.
pub(super) fn http_call<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) {
    let Some(func) = call.child_by_field_name("function") else { return };
    let Some(name) = last_name(func, cx.src) else { return };
    let Some((method, pieces)) = method_and_url(cx, call, &name) else { return };
    let Some(key) = http_key(&method, &pieces) else { return };
    let framework = library(cx, func, &name);
    let owner = cx.enclosing(call);
    cx.push(ContractSide::Consumes, ContractKind::Http, key, call, owner, framework);
}

/// `(method, URL pieces)` of a client call.
fn method_and_url<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>, name: &str) -> Option<(String, Vec<Piece>)> {
    let args = positional(call);
    let keywords: Vec<Node<'t>> = URL_KEYWORDS.iter().filter_map(|k| keyword(call, k, cx.src)).collect();
    match stated_method(cx, call, &args) {
        Stated::Method(m, rest) => first_path(cx, rest.into_iter().chain(keywords)).map(|p| (m, p)),
        Stated::Unknown => first_path(cx, keywords.into_iter()).map(|p| ("*".to_string(), p)),
        Stated::None => {
            let bare = name.trim_start_matches('_');
            let method = match (verb(name), bare) {
                (Some(m), _) => m,
                (None, "urlopen") => "GET",
                (None, "request") => "*",
                _ => return None,
            };
            let candidates: Vec<Node<'t>> = if method == "*" { args } else { args.into_iter().take(1).collect() };
            first_path(cx, candidates.into_iter().chain(keywords)).map(|p| (method.to_string(), p))
        }
    }
}

/// A literal method among the positional arguments (`"POST"`), or
/// `method="PUT"` / `method=self._connection.get`.
fn stated_method<'t>(cx: &Ctx<'t, '_>, call: Node<'t>, args: &[Node<'t>]) -> Stated<'t> {
    for (i, a) in args.iter().enumerate() {
        if let Some(m) = super::render::plain_string(*a, cx.src).map(|s| s.to_ascii_uppercase())
            && HTTP_METHODS.contains(&m.as_str())
        {
            return Stated::Method(m, args[i + 1..].to_vec());
        }
    }
    let Some(value) = keyword(call, "method", cx.src) else { return Stated::None };
    if let Some(m) = super::render::plain_string(value, cx.src) {
        return Stated::Method(m.to_ascii_uppercase(), Vec::new());
    }
    match last_name(value, cx.src).as_deref().and_then(verb) {
        Some(m) => Stated::Method(m.to_string(), Vec::new()),
        None => Stated::Unknown,
    }
}

/// The first candidate that renders to a path-like URL.
fn first_path<'t>(cx: &mut Ctx<'t, '_>, candidates: impl Iterator<Item = Node<'t>>) -> Option<Vec<Piece>> {
    for a in candidates {
        if let Some(p) = cx.render(a, 0)
            && looks_like_path(&p)
        {
            return Some(p);
        }
    }
    None
}

/// The client library of a call: the module its receiver root (or a bare
/// callee) was imported from, else the one the file imports, else
/// `python-http`.
fn library(cx: &Ctx<'_, '_>, func: Node<'_>, name: &str) -> &'static str {
    let root = match func.kind() {
        "attribute" => func.child_by_field_name("object").and_then(|o| root_name(o, cx.src)),
        _ => Some(text_of(func, cx.src).into_owned()),
    };
    let from_root = root.as_deref().and_then(|r| cx.import_root(r));
    if let Some(lib) = LIBRARIES.iter().find(|l| from_root == Some(**l)) {
        return lib;
    }
    if name == "urlopen" {
        return "urllib";
    }
    LIBRARIES.iter().copied().find(|l| cx.imports_root(l)).unwrap_or("python-http")
}
