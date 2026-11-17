//! HTTP consumers: Guzzle-style clients, Laravel's `Http` facade, curl.

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::HTTP_METHODS;
use rueca_model::{ContractKind, ContractSide};

use super::render::{Piece, http_key, looks_like_path, plain_string};
use super::{Ctx, call_name, positional};

/// Method names (an `Async` suffix dropped) that name their HTTP method.
const VERBS: &[(&str, &str)] = &[
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("patch", "PATCH"),
    ("delete", "DELETE"),
    ("head", "HEAD"),
    ("options", "OPTIONS"),
];

const HTTP_FACADE: &str = "Illuminate\\Support\\Facades\\Http";

/// The method of a method name (`getAsync` → `GET`).
pub(super) fn verb(name: &str) -> Option<&'static str> {
    let bare = name.strip_suffix("Async").unwrap_or(name);
    VERBS.iter().find(|(n, _)| *n == bare).map(|(_, m)| *m)
}

/// Whether a scoped call is on Laravel's `Http` facade.
fn is_http_facade(cx: &Ctx<'_, '_>, call: Node<'_>) -> bool {
    call.child_by_field_name("scope").is_some_and(|s| {
        let fqn = cx.class_fqn(&cx.text(s));
        fqn == "Http" || fqn == HTTP_FACADE
    })
}

/// The framework of a client call: `laravel-http` for a chain rooted at
/// `Http::`, `None` for one rooted at another static call (`Storage::`),
/// else `guzzle` when the file uses Guzzle, else `php-http`.
fn framework(cx: &Ctx<'_, '_>, call: Node<'_>) -> Option<&'static str> {
    let mut cur = call;
    while matches!(cur.kind(), "member_call_expression" | "nullsafe_member_call_expression") {
        match cur.child_by_field_name("object") {
            Some(o) => cur = o,
            None => break,
        }
    }
    if cur.kind() == "scoped_call_expression" {
        return is_http_facade(cx, cur).then_some("laravel-http");
    }
    let guzzle = cx.uses.values().any(|u| u.starts_with("GuzzleHttp\\"));
    Some(if guzzle { "guzzle" } else { "php-http" })
}

/// Records the consumer hint of a method / static call, if it is an HTTP
/// client call.
pub(super) fn http_call<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) {
    let Some(name) = call_name(call, cx.src) else { return };
    let Some(framework) = framework(cx, call) else { return };
    let args = positional(call, cx.src);
    let (method, url) = match verb(&name) {
        Some(m) => (m.to_string(), args.first().copied()),
        None if matches!(name.as_str(), "request" | "requestAsync") => {
            let stated = args.first().and_then(|a| plain_string(*a, cx.src)).map(|m| m.to_ascii_uppercase());
            let method = stated.filter(|m| HTTP_METHODS.contains(&m.as_str())).unwrap_or_else(|| "*".to_string());
            (method, args.get(1).copied())
        }
        None => return,
    };
    let Some(pieces) = url.and_then(|u| path(cx, u)) else { return };
    push(cx, call, &method, &pieces, framework);
}

/// The rendered URL of `n` when it is path-like.
fn path<'t>(cx: &mut Ctx<'t, '_>, n: Node<'t>) -> Option<Vec<Piece>> {
    cx.render(n, 0).filter(|p| looks_like_path(p))
}

fn push(cx: &mut Ctx<'_, '_>, call: Node<'_>, method: &str, pieces: &[Piece], framework: &str) {
    if let Some(key) = http_key(method, pieces) {
        let owner = cx.enclosing(call);
        cx.push(ContractSide::Consumes, ContractKind::Http, key, call, owner, framework);
    }
}

/// `curl_setopt($ch, CURLOPT_URL, '…')`, `curl_init('…')`: method `*`.
pub(super) fn curl<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) {
    let Some(func) = call.child_by_field_name("function") else { return };
    let args = positional(call, cx.src);
    let url = match cx.text(func).trim_start_matches('\\') {
        "curl_init" => args.first().copied(),
        "curl_setopt" if args.get(1).is_some_and(|o| cx.text(*o) == "CURLOPT_URL") => args.get(2).copied(),
        _ => return,
    };
    if let Some(pieces) = url.and_then(|u| path(cx, u)) {
        push(cx, call, "*", &pieces, "curl");
    }
}
