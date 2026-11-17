//! HTTP providers: Laravel route registrations, Symfony route attributes,
//! Slim / Lumen `$app->get(…)`.

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::http_key;
use rueca_model::{ContractKind, ContractSide, LocalDefId};

use super::clients::verb;
use super::render::{array_value, plain_string, string_list};
use super::{Ctx, call_name, named, positional};

/// Variables taken as a Slim / Lumen application or router.
const ROUTER_VARIABLES: &[&str] = &["$app", "$router", "$group", "$r"];

/// Composes a route path from its prefixes (outer first): one leading
/// `/`, no doubled separators.
fn compose(prefixes: &[String], path: &str) -> String {
    let parts: Vec<&str> = prefixes
        .iter()
        .map(String::as_str)
        .chain([path])
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// One provider hint per method with evidence `node`.
fn provide(cx: &mut Ctx<'_, '_>, node: Node<'_>, methods: &[String], path: &str, owner: Option<LocalDefId>, fw: &str) {
    for m in methods {
        if let Some(key) = http_key(Some(m.as_str()).filter(|m| *m != "*"), path) {
            cx.push(ContractSide::Provides, ContractKind::Http, key, node, owner, fw);
        }
    }
}

// -------------------------------------------------------------------------
// Laravel
// -------------------------------------------------------------------------

/// Whether a scoped call's class is Laravel's `Route` facade (imported, or
/// the global alias).
fn is_route_facade(cx: &Ctx<'_, '_>, call: Node<'_>) -> bool {
    call.kind() == "scoped_call_expression"
        && call.child_by_field_name("scope").is_some_and(|s| {
            let fqn = cx.class_fqn(&cx.text(s));
            fqn == "Route" || fqn == "Illuminate\\Support\\Facades\\Route"
        })
}

/// The `Route::…` call at the root of a member-call chain, if any.
fn route_root<'t>(cx: &Ctx<'_, '_>, call: Node<'t>) -> Option<Node<'t>> {
    let mut cur = call;
    loop {
        match cur.kind() {
            "scoped_call_expression" => return is_route_facade(cx, cur).then_some(cur),
            "member_call_expression" | "nullsafe_member_call_expression" => cur = cur.child_by_field_name("object")?,
            _ => return None,
        }
    }
}

/// Route registrations (Laravel, Slim / Lumen). Whether `call` was a call
/// on a router (a route or not), so it is no HTTP client call.
pub(super) fn route_call(cx: &mut Ctx<'_, '_>, call: Node<'_>) -> bool {
    if route_root(cx, call).is_some() {
        laravel(cx, call);
        return true;
    }
    slim(cx, call)
}

/// `Route::get('/x', …)`, `Route::match([…], '/x', …)`, `Route::any(…)`,
/// `Route::resource('photos', …)`, also at the end of a chain.
fn laravel(cx: &mut Ctx<'_, '_>, call: Node<'_>) {
    let Some(name) = call_name(call, cx.src) else { return };
    let args = positional(call, cx.src);
    let (methods, path) = match name.as_str() {
        "any" => (vec!["*".to_string()], args.first()),
        "match" => (args.first().map(|a| string_list(*a, cx.src)).unwrap_or_default(), args.get(1)),
        "resource" | "apiResource" => return resource(cx, call, &name, args.first().copied()),
        n => match verb(n) {
            Some(m) => (vec![m.to_string()], args.first()),
            None => return,
        },
    };
    let Some(path) = path.and_then(|p| plain_string(*p, cx.src)) else { return };
    let full = compose(&laravel_prefixes(cx, call), &path);
    let owner = cx.enclosing(call);
    provide(cx, call, &methods, &full, owner, "laravel");
}

/// `Route::resource('photos', PhotoController::class)`: the seven (five
/// for `apiResource`) resource routes.
fn resource(cx: &mut Ctx<'_, '_>, call: Node<'_>, name: &str, first: Option<Node<'_>>) {
    let Some(base) = first.and_then(|f| plain_string(f, cx.src)).filter(|b| !b.contains('.')) else { return };
    let prefixes = laravel_prefixes(cx, call);
    let mut routes = vec![("GET", ""), ("POST", ""), ("GET", "{}"), ("PUT", "{}"), ("PATCH", "{}"), ("DELETE", "{}")];
    if name == "resource" {
        routes.extend([("GET", "create"), ("GET", "{}/edit")]);
    }
    let owner = cx.enclosing(call);
    for (m, tail) in routes {
        let full = compose(&prefixes, &format!("{base}/{tail}"));
        provide(cx, call, &[m.to_string()], &full, owner, "laravel");
    }
}

/// Route prefixes around `call`, outer first: `prefix('…')` calls in its
/// own chain and in the chains of the `group(…)` calls whose closures hold
/// it, and `Route::group(['prefix' => …], …)`.
fn laravel_prefixes(cx: &Ctx<'_, '_>, call: Node<'_>) -> Vec<String> {
    let mut out = chain_prefixes(cx, call);
    let mut cur = call.parent();
    while let Some(n) = cur {
        if let Some(group) = group_of_closure(cx, n) {
            let mut outer = chain_prefixes(cx, group);
            if call_name(group, cx.src).as_deref() == Some("group") {
                outer.extend(group_array_prefix(cx, group));
            }
            outer.extend(out);
            out = outer;
        }
        cur = n.parent();
    }
    out
}

/// The `group(…)` call `n` (a closure) is an argument of.
fn group_of_closure<'t>(cx: &Ctx<'_, '_>, n: Node<'t>) -> Option<Node<'t>> {
    if !matches!(n.kind(), "anonymous_function" | "arrow_function") {
        return None;
    }
    let call = n.parent()?.parent()?.parent()?;
    let is_group = call_name(call, cx.src).as_deref() == Some("group") && route_root(cx, call).is_some();
    is_group.then_some(call)
}

/// `['prefix' => 'api']` as the first argument of `Route::group`.
fn group_array_prefix(cx: &Ctx<'_, '_>, group: Node<'_>) -> Option<String> {
    let first = *positional(group, cx.src).first()?;
    array_value(first, "prefix", cx.src).and_then(|v| plain_string(v, cx.src))
}

/// `prefix('…')` calls along a member-call chain, outer (root) first.
fn chain_prefixes(cx: &Ctx<'_, '_>, call: Node<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = call.child_by_field_name("object");
    while let Some(n) = cur {
        if call_name(n, cx.src).as_deref() == Some("prefix")
            && let Some(p) = positional(n, cx.src).first().and_then(|a| plain_string(*a, cx.src))
        {
            out.push(p);
        }
        cur = n.child_by_field_name("object");
    }
    out.reverse();
    out
}

// -------------------------------------------------------------------------
// Slim / Lumen
// -------------------------------------------------------------------------

/// `$app->get('/x', handler)` / `$router->post(…)` with a handler argument
/// (a closure, `'Ctl@m'`, `[Ctl::class, 'm']`, `Ctl::class`). Whether it
/// was one.
fn slim(cx: &mut Ctx<'_, '_>, call: Node<'_>) -> bool {
    if call.kind() != "member_call_expression" {
        return false;
    }
    let object = call.child_by_field_name("object").filter(|o| o.kind() == "variable_name");
    if !object.is_some_and(|o| ROUTER_VARIABLES.contains(&cx.text(o).as_str())) {
        return false;
    }
    let Some(name) = call_name(call, cx.src) else { return false };
    let method = match name.as_str() {
        "any" => "*",
        n => match verb(n) {
            Some(m) => m,
            None => return false,
        },
    };
    let args = positional(call, cx.src);
    let path = args.first().and_then(|a| plain_string(*a, cx.src)).filter(|p| p.starts_with('/'));
    let (Some(path), Some(handler)) = (path, args.get(1)) else { return false };
    if !is_handler(cx, *handler) {
        return false;
    }
    let owner = cx.enclosing(call);
    provide(cx, call, &[method.to_string()], &compose(&[], &path), owner, "php-router");
    true
}

fn is_handler(cx: &Ctx<'_, '_>, n: Node<'_>) -> bool {
    match n.kind() {
        "anonymous_function" | "arrow_function" | "class_constant_access_expression" => true,
        "string" | "encapsed_string" => plain_string(n, cx.src).is_some_and(|s| s.contains('@') || s.contains(':')),
        "array_creation_expression" => n
            .named_child(0)
            .and_then(|e| e.named_child(0))
            .is_some_and(|e| e.kind() == "class_constant_access_expression"),
        _ => false,
    }
}

// -------------------------------------------------------------------------
// Symfony
// -------------------------------------------------------------------------

/// The Symfony `Route` attributes of a declaration.
fn route_attributes<'t>(cx: &Ctx<'_, '_>, decl: Node<'t>) -> Vec<Node<'t>> {
    let Some(list) = decl.child_by_field_name("attributes") else { return Vec::new() };
    let mut out = Vec::new();
    let mut stack = vec![list];
    while let Some(n) = stack.pop() {
        if n.kind() == "attribute" {
            let name = n.named_child(0).map(|c| cx.class_fqn(&cx.text(c))).unwrap_or_default();
            if name.starts_with("Symfony\\Component\\Routing\\") && name.ends_with("\\Route") {
                out.push(n);
            }
            continue;
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    out.reverse();
    out
}

/// The path of a `#[Route]` attribute (first argument or `path:`).
fn attribute_path(cx: &Ctx<'_, '_>, attr: Node<'_>) -> Option<String> {
    positional(attr, cx.src)
        .first()
        .copied()
        .or_else(|| named(attr, "path", cx.src))
        .and_then(|p| plain_string(p, cx.src))
}

/// `#[Route('/prefix')] class C { #[Route('/x', methods: ['GET'])] function m() }`.
pub(super) fn symfony_class(cx: &mut Ctx<'_, '_>, class: Node<'_>) {
    if !cx.uses.values().any(|u| u.starts_with("Symfony\\Component\\Routing\\")) {
        return;
    }
    let prefix: Vec<String> =
        route_attributes(cx, class).first().and_then(|a| attribute_path(cx, *a)).into_iter().collect();
    let Some(body) = class.child_by_field_name("body") else { return };
    let mut c = body.walk();
    let methods: Vec<Node<'_>> = body.named_children(&mut c).filter(|m| m.kind() == "method_declaration").collect();
    for m in methods {
        let owner = m.child_by_field_name("name").and_then(|n| cx.def_named_at(n));
        for attr in route_attributes(cx, m) {
            let Some(path) = attribute_path(cx, attr) else { continue };
            let listed = named(attr, "methods", cx.src).map(|v| string_list(v, cx.src)).unwrap_or_default();
            let methods = if listed.is_empty() { vec!["*".to_string()] } else { listed };
            provide(cx, attr, &methods, &compose(&prefix, &path), owner, "symfony");
        }
    }
}
