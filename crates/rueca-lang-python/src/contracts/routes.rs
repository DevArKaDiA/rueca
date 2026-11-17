//! HTTP providers: route decorators and route registration calls.

use rueca_extract::text_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::http_key;
use rueca_model::{ContractKind, ContractSide, LocalDefId};

use super::clients::verb;
use super::render::{plain_string, string_list};
use super::{Ctx, keyword, last_name, positional};

/// Web frameworks a provider is attributed to, by the file's imports, in
/// priority order; `python-web` when none is imported.
const FRAMEWORKS: &[&str] = &["fastapi", "flask", "quart", "sanic", "litestar", "starlette", "aiohttp", "bottle"];

/// aiohttp `router.add_<verb>(path, handler)`.
const AIOHTTP_ADD: &[&str] = &["add_get", "add_post", "add_put", "add_patch", "add_delete", "add_head"];

/// `bp = Blueprint("x", __name__, url_prefix="/api")`,
/// `router = APIRouter(prefix="/api")`: the prefix of the bound name.
pub(super) fn scan_prefix(cx: &mut Ctx<'_, '_>, assignment: Node<'_>) {
    let (Some(left), Some(right)) = (assignment.child_by_field_name("left"), assignment.child_by_field_name("right"))
    else {
        return;
    };
    let keyword_name = match cx.called_class(right).as_deref() {
        Some("Blueprint") => "url_prefix",
        Some("APIRouter") => "prefix",
        _ => return,
    };
    let prefix = keyword(right, keyword_name, cx.src).and_then(|v| plain_string(v, cx.src));
    if let (Some(prefix), "identifier") = (prefix, left.kind()) {
        cx.prefixes.insert(cx.text(left), prefix.trim_end_matches('/').to_string());
    }
}

/// The web framework the file imports.
fn framework(cx: &Ctx<'_, '_>) -> &'static str {
    FRAMEWORKS.iter().copied().find(|f| cx.imports_root(f)).unwrap_or("python-web")
}

/// Route decorators of a decorated function (`@app.route("/x",
/// methods=[…])`, `@router.get("/x")`), one hint per method.
pub(super) fn decorated(cx: &mut Ctx<'_, '_>, node: Node<'_>) {
    let Some(def) = node.child_by_field_name("definition") else { return };
    let owner = def.child_by_field_name("name").and_then(|n| cx.def_named_at(n));
    let mut c = node.walk();
    let decorators: Vec<Node<'_>> = node.named_children(&mut c).filter(|d| d.kind() == "decorator").collect();
    for d in decorators {
        if let Some(call) = d.named_child(0).filter(|c| c.kind() == "call") {
            route_decorator(cx, d, call, owner);
        }
    }
}

fn route_decorator(cx: &mut Ctx<'_, '_>, dec: Node<'_>, call: Node<'_>, owner: Option<LocalDefId>) {
    let Some(func) = call.child_by_field_name("function").filter(|f| f.kind() == "attribute") else { return };
    let (Some(recv), Some(name)) = (func.child_by_field_name("object"), last_name(func, cx.src)) else { return };
    let methods = match name.as_str() {
        "route" | "api_route" => methods_keyword(cx, call),
        _ if !name.starts_with('_') => match verb(&name) {
            Some(m) if name != "get_api_list" => vec![m.to_string()],
            _ => return,
        },
        _ => return,
    };
    let Some(path) = route_path(cx, call, &["rule", "path"]) else { return };
    let prefix = cx.prefixes.get(text_of(recv, cx.src).as_ref()).cloned().unwrap_or_default();
    let full = format!("{prefix}{path}");
    let full = if full.is_empty() { "/".to_string() } else { full };
    if !full.starts_with('/') {
        return;
    }
    let fw = framework(cx);
    for m in methods {
        if let Some(key) = http_key(Some(&m).filter(|m| *m != "*").map(String::as_str), &full) {
            cx.push(ContractSide::Provides, ContractKind::Http, key, dec, owner, fw);
        }
    }
}

/// The literal route path: the first positional argument or one of
/// `keywords`.
fn route_path(cx: &Ctx<'_, '_>, call: Node<'_>, keywords: &[&str]) -> Option<String> {
    let first = positional(call).first().copied();
    first.or_else(|| keywords.iter().find_map(|k| keyword(call, k, cx.src))).and_then(|n| plain_string(n, cx.src))
}

/// `methods=[…]`: the listed methods; `GET` when absent; `*` when not a
/// literal list.
fn methods_keyword(cx: &Ctx<'_, '_>, call: Node<'_>) -> Vec<String> {
    match keyword(call, "methods", cx.src) {
        None => vec!["GET".to_string()],
        Some(v) => {
            let listed = string_list(v, cx.src);
            if listed.is_empty() { vec!["*".to_string()] } else { listed }
        }
    }
}

/// Route registration calls: Django `path()` / `re_path()` / `url()`,
/// aiohttp `router.add_get()` / `add_route()` / `web.get()`, Starlette
/// `Route()`, Flask `add_url_rule()`. Whether `call` was one.
pub(super) fn registration<'t>(cx: &mut Ctx<'t, '_>, call: Node<'t>) -> bool {
    let Some(func) = call.child_by_field_name("function") else { return false };
    let Some(name) = last_name(func, cx.src) else { return false };
    let imported = match func.kind() {
        "identifier" => cx.imports.get(&name).cloned(),
        _ => func.child_by_field_name("object").and_then(|o| cx.imports.get(&cx.text(o)).cloned()),
    }
    .unwrap_or_default();
    if imported.starts_with("django.") {
        return django(cx, call, &name);
    }
    if imported.starts_with("starlette.") && name == "Route" {
        return starlette(cx, call);
    }
    if cx.imports_root("aiohttp") && aiohttp(cx, call, func, &name) {
        return true;
    }
    name == "add_url_rule" && flask_rule(cx, call)
}

/// Records one provider hint per method for a registration call.
fn provide(cx: &mut Ctx<'_, '_>, call: Node<'_>, methods: &[String], path: &str, handler: Option<Node<'_>>, fw: &str) {
    let owner = cx.handler_owner(handler, call);
    for m in methods {
        if let Some(key) = http_key(Some(m.as_str()).filter(|m| *m != "*"), path) {
            cx.push(ContractSide::Provides, ContractKind::Http, key, call, owner, fw);
        }
    }
}

/// Django `path("users/<int:id>/", view)`, `re_path(r"^users/(?P<id>\d+)/$", view)`:
/// method `*`; `include(…)` targets are skipped (their prefix composes
/// across files).
fn django(cx: &mut Ctx<'_, '_>, call: Node<'_>, name: &str) -> bool {
    let args = positional(call);
    let (Some(route), view) = (args.first().and_then(|a| plain_string(*a, cx.src)), args.get(1).copied()) else {
        return false;
    };
    if view.is_none_or(|v| {
        v.kind() == "call"
            && last_name(v.child_by_field_name("function").unwrap_or(v), cx.src).as_deref() == Some("include")
    }) {
        return false;
    }
    let path = match name {
        "path" => Some(route),
        "re_path" | "url" => regex_path(&route),
        _ => return false,
    };
    let Some(path) = path else { return false };
    provide(cx, call, &["*".to_string()], &format!("/{}", path.trim_start_matches('/')), view, "django");
    true
}

/// A Django route regex as a path: anchors dropped, every group a
/// parameter; `None` when other regex syntax remains.
pub(super) fn regex_path(re: &str) -> Option<String> {
    let re = re.strip_prefix('^').unwrap_or(re);
    let re = re.strip_suffix('$').unwrap_or(re);
    let mut out = String::new();
    let mut depth = 0usize;
    for c in re.chars() {
        match c {
            '(' => {
                if depth == 0 {
                    out.push_str("{p}");
                }
                depth += 1;
            }
            ')' => depth = depth.checked_sub(1)?,
            _ if depth > 0 => {}
            '\\' | '[' | ']' | '*' | '+' | '?' | '|' | '{' | '}' => return None,
            _ => out.push(c),
        }
    }
    (depth == 0).then_some(out)
}

/// Starlette `Route("/x", endpoint, methods=[…])` (default `GET`).
fn starlette(cx: &mut Ctx<'_, '_>, call: Node<'_>) -> bool {
    let Some(path) = route_path(cx, call, &["path"]) else { return false };
    let handler = positional(call).get(1).copied().or_else(|| keyword(call, "endpoint", cx.src));
    let methods = methods_keyword(cx, call);
    provide(cx, call, &methods, &path, handler, "starlette");
    true
}

/// aiohttp `router.add_get("/x", h)`, `router.add_route("GET", "/x", h)`,
/// `web.get("/x", h)`, `web.route("GET", "/x", h)`.
fn aiohttp(cx: &mut Ctx<'_, '_>, call: Node<'_>, func: Node<'_>, name: &str) -> bool {
    let args = positional(call);
    let module_level = func
        .child_by_field_name("object")
        .is_some_and(|o| cx.imports.get(&cx.text(o)).is_some_and(|t| t == "aiohttp.web"));
    let (method, rest) = match name {
        n if AIOHTTP_ADD.contains(&n) => (n.trim_start_matches("add_").to_ascii_uppercase(), &args[..]),
        "add_route" | "route" if !args.is_empty() => {
            let Some(m) = plain_string(args[0], cx.src) else { return false };
            (if m == "*" { m } else { m.to_ascii_uppercase() }, &args[1..])
        }
        n if module_level && verb(n).is_some() && n != "get_api_list" => {
            (verb(n).unwrap_or("*").to_string(), &args[..])
        }
        _ => return false,
    };
    let Some(path) = rest.first().and_then(|a| plain_string(*a, cx.src)).filter(|p| p.starts_with('/')) else {
        return false;
    };
    let handler = rest.get(1).copied();
    provide(cx, call, &[method], &path, handler, "aiohttp");
    true
}

/// Flask `app.add_url_rule("/x", "endpoint", view, methods=[…])`.
fn flask_rule(cx: &mut Ctx<'_, '_>, call: Node<'_>) -> bool {
    let Some(path) = route_path(cx, call, &["rule"]).filter(|p| p.starts_with('/')) else { return false };
    let handler = keyword(call, "view_func", cx.src).or_else(|| positional(call).get(2).copied());
    let methods = methods_keyword(cx, call);
    provide(cx, call, &methods, &path, handler, "flask");
    true
}
