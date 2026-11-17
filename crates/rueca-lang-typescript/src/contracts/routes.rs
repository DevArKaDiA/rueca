//! HTTP providers: server route registrations and NestJS controllers.

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::http_key;
use rueca_model::{ContractKind, ContractSide, LocalDefId};

use super::clients::verb;
use super::{Ctx, arguments, callee, plain_string, property};

/// Server frameworks whose import makes `x.get("/path", handler)` a route,
/// with the framework name a hint carries.
const SERVERS: &[(&str, &str)] = &[
    ("express", "express"),
    ("koa-router", "koa"),
    ("@koa/router", "koa"),
    ("fastify", "fastify"),
    ("hono", "hono"),
    ("restify", "restify"),
    ("polka", "polka"),
    ("@hapi/hapi", "hapi"),
    ("elysia", "elysia"),
];

/// Receiver names taken as a server without an import, when a handler
/// function is passed inline.
const SERVER_NAMES: &[&str] = &["app", "router", "server"];

/// NestJS method decorators and their methods (`All` = any).
const NEST_VERBS: &[(&str, &str)] = &[
    ("Get", "GET"),
    ("Post", "POST"),
    ("Put", "PUT"),
    ("Patch", "PATCH"),
    ("Delete", "DELETE"),
    ("Head", "HEAD"),
    ("Options", "OPTIONS"),
    ("All", "*"),
];

/// The server framework the file imports.
fn server(cx: &Ctx<'_, '_>) -> Option<&'static str> {
    SERVERS.iter().find(|(m, _)| cx.modules.contains(*m)).map(|(_, f)| *f)
}

/// Route registrations: `app.get("/x", h)`, `app.all(…)`,
/// `app.route("/x").get(h)`, `fastify.route({method, url, handler})`.
/// Whether `call` was one.
pub(super) fn server_route<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) -> bool {
    let Some((Some(recv), name)) = callee(call, cx.src) else { return false };
    let method = match name.as_str() {
        "all" => "*",
        "route" => return route_object(cx, call),
        n => match verb(n) {
            Some(m) if n != "del" => m,
            _ => return false,
        },
    };
    let args = arguments(call);
    if let Some(path) = chained_route_path(cx, recv) {
        return provide(cx, call, &[method.to_string()], &path, args.first().copied(), server(cx).unwrap_or("express"));
    }
    let Some(path) = args.first().and_then(|a| route_literal(cx, *a)) else { return false };
    let handler = args.last().copied().filter(|h| args.len() > 1 && is_handler(*h));
    if super::clients::is_client_receiver(cx, recv) {
        return false;
    }
    let framework = match (server(cx), handler) {
        (Some(f), Some(_)) => f,
        (None, Some(h)) if is_inline_handler(h) && is_server_name(cx, recv) => "js-web",
        _ => return false,
    };
    provide(cx, call, &[method.to_string()], &path, handler, framework)
}

/// A route path literal: a string or a template without substitutions,
/// starting with `/` (or `*`).
fn route_literal(cx: &Ctx<'_, '_>, n: Node<'_>) -> Option<String> {
    let s = match n.kind() {
        "string" => plain_string(n, cx.src)?,
        "template_string"
            if n.named_child_count() <= 1 && n.named_child(0).is_none_or(|c| c.kind() == "string_fragment") =>
        {
            let t = cx.text(n);
            t.trim_matches('`').to_string()
        }
        _ => return None,
    };
    (s.starts_with('/') || s == "*").then_some(s)
}

/// A route handler argument: a function, a name, a member, middleware.
fn is_handler(n: Node<'_>) -> bool {
    !matches!(n.kind(), "object" | "string" | "template_string" | "number" | "true" | "false" | "null")
}

/// A handler written in place: a function, or a call building one
/// (`serveStatic({…})`).
fn is_inline_handler(n: Node<'_>) -> bool {
    matches!(n.kind(), "arrow_function" | "function_expression" | "function" | "call_expression")
}

fn is_server_name(cx: &Ctx<'_, '_>, recv: Node<'_>) -> bool {
    recv.kind() == "identifier" && SERVER_NAMES.contains(&cx.text(recv).as_str())
}

/// `app.route("/x")` at the root of a `.get(h).post(h)` chain: the path.
fn chained_route_path(cx: &Ctx<'_, '_>, recv: Node<'_>) -> Option<String> {
    let mut cur = recv;
    while cur.kind() == "call_expression" {
        let (inner, name) = callee(cur, cx.src)?;
        if name == "route" && inner.is_some_and(|i| i.kind() != "call_expression") {
            return arguments(cur).first().and_then(|a| route_literal(cx, *a));
        }
        cur = inner?;
    }
    None
}

/// `fastify.route({ method: ["GET", "POST"], url: "/x", handler })`,
/// hapi `server.route({ method, path, handler })`.
fn route_object<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) -> bool {
    let Some(framework) = server(cx) else { return false };
    let Some(obj) = arguments(call).first().copied().filter(|a| a.kind() == "object") else { return false };
    let path =
        property(obj, "url", cx.src).or_else(|| property(obj, "path", cx.src)).and_then(|p| route_literal(cx, p));
    let Some(path) = path else { return false };
    let methods = match property(obj, "method", cx.src) {
        Some(m) if m.kind() == "array" => {
            let mut c = m.walk();
            m.named_children(&mut c).filter_map(|s| plain_string(s, cx.src)).collect()
        }
        Some(m) => plain_string(m, cx.src).into_iter().collect(),
        None => Vec::new(),
    };
    let methods: Vec<String> = if methods.is_empty() { vec!["*".to_string()] } else { methods };
    let handler = property(obj, "handler", cx.src);
    provide(cx, call, &methods, &path, handler, framework)
}

/// One provider hint per method; `true`.
fn provide(
    cx: &mut Ctx<'_, '_>,
    call: Node<'_>,
    methods: &[String],
    path: &str,
    handler: Option<Node<'_>>,
    fw: &str,
) -> bool {
    let owner = cx.handler_owner(handler, call);
    let path = if path == "*" { "/*" } else { path };
    for m in methods {
        if let Some(key) = http_key(Some(m.as_str()).filter(|m| *m != "*"), path) {
            cx.push(ContractSide::Provides, ContractKind::Http, key, call, owner, fw);
        }
    }
    true
}

// -------------------------------------------------------------------------
// NestJS
// -------------------------------------------------------------------------

/// `@Controller("users")` classes: each `@Get(":id")`-style method
/// provides the composed route.
pub(super) fn nest_controller(cx: &mut Ctx<'_, '_>, class: Node<'_>) {
    if !cx.modules.contains("@nestjs/common") {
        return;
    }
    let Some(prefix) = class_decorators(class).into_iter().find_map(|d| controller_prefix(cx, d)) else { return };
    let Some(body) = class.child_by_field_name("body") else { return };
    let mut pending: Vec<Node<'_>> = Vec::new();
    let mut c = body.walk();
    let members: Vec<Node<'_>> = body.named_children(&mut c).collect();
    for m in members {
        match m.kind() {
            "decorator" => pending.push(m),
            "method_definition" => {
                let mut own = pending.clone();
                own.extend(decorators_of(m));
                let owner = m.child_by_field_name("name").and_then(|n| cx.def_named_at(n));
                for d in own {
                    nest_route(cx, d, &prefix, owner);
                }
                pending.clear();
            }
            _ => pending.clear(),
        }
    }
}

/// Decorators of a class: its own and those on an enclosing `export`.
fn class_decorators(class: Node<'_>) -> Vec<Node<'_>> {
    let mut out = decorators_of(class);
    if let Some(p) = class.parent().filter(|p| p.kind() == "export_statement") {
        out.extend(decorators_of(p));
    }
    out
}

fn decorators_of(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c).filter(|d| d.kind() == "decorator").collect()
}

/// The call of a decorator and its callee name (`@Get(":id")` → `Get`).
fn decorator_call<'t>(cx: &Ctx<'_, '_>, d: Node<'t>) -> Option<(Node<'t>, String)> {
    let call = d.named_child(0).filter(|c| c.kind() == "call_expression")?;
    let (recv, name) = callee(call, cx.src)?;
    recv.is_none().then_some((call, name))
}

/// `@Controller("x")` / `@Controller({ path: "x" })` / `@Controller()`.
fn controller_prefix(cx: &Ctx<'_, '_>, d: Node<'_>) -> Option<String> {
    let (call, name) = decorator_call(cx, d)?;
    if name != "Controller" {
        return None;
    }
    let first = arguments(call).first().copied();
    let path = first.and_then(|a| {
        plain_string(a, cx.src).or_else(|| property(a, "path", cx.src).and_then(|p| plain_string(p, cx.src)))
    });
    Some(path.unwrap_or_default())
}

/// `@Get(":id")`, `@Post()`, `@Get(["a", "b"])` on a controller method.
fn nest_route(cx: &mut Ctx<'_, '_>, d: Node<'_>, prefix: &str, owner: Option<LocalDefId>) {
    let Some((call, name)) = decorator_call(cx, d) else { return };
    let Some(method) = NEST_VERBS.iter().find(|(n, _)| *n == name).map(|(_, m)| *m) else { return };
    let paths: Vec<String> = match arguments(call).first() {
        None => vec![String::new()],
        Some(a) if a.kind() == "array" => {
            let mut c = a.walk();
            a.named_children(&mut c).filter_map(|s| plain_string(s, cx.src)).collect()
        }
        Some(a) => plain_string(*a, cx.src).into_iter().collect(),
    };
    for p in paths {
        // Trimmed: a leading `//` would read as a protocol-relative URL.
        let full = format!("/{}/{}", prefix.trim_matches('/'), p.trim_matches('/'));
        if let Some(key) = http_key(Some(method).filter(|m| *m != "*"), &full) {
            cx.push(ContractSide::Provides, ContractKind::Http, key, d, owner, "nestjs");
        }
    }
}
