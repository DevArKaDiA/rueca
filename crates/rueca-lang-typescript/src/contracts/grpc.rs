//! gRPC (`@grpc/grpc-js` generated code): client calls (consumers) and
//! `server.addService` implementations (providers), keyed
//! `Service/Method` with the proto (PascalCase) method name.

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::rpc_key;
use rueca_model::{ContractKind, ContractSide};

use super::{Ctx, arguments, callee, last_name};

const FRAMEWORK: &str = "grpc-node";

/// Modules whose import marks a file as gRPC code.
const GRPC_MODULES: &[&str] = &["@grpc/grpc-js", "grpc"];

/// Methods of a generated client that are not RPCs.
const CLIENT_METHODS: &[&str] = &[
    "close",
    "getChannel",
    "waitForReady",
    "makeUnaryRequest",
    "makeClientStreamRequest",
    "makeServerStreamRequest",
    "makeBidiStreamRequest",
    "then",
    "catch",
    "on",
    "bind",
    "call",
    "apply",
];

/// The proto name of a JS method name (`sayHello` → `SayHello`).
fn proto_name(method: &str) -> String {
    let mut chars = method.chars();
    chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// The service of `new XClient(…)` when the class is generated gRPC code:
/// imported from a module whose specifier mentions `grpc` or ends in
/// `_pb`.
fn client_service(cx: &Ctx<'_, '_>, value: Node<'_>) -> Option<String> {
    if value.kind() != "new_expression" {
        return None;
    }
    let ctor = value.child_by_field_name("constructor")?;
    let class = last_name(ctor, cx.src)?;
    let svc = class.strip_suffix("Client").filter(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))?;
    let root = super::root_name(ctor, cx.src)?;
    let module = cx.imports.get(&root)?;
    (module.contains("grpc") || module.ends_with("_pb")).then(|| svc.to_string())
}

/// `const c = new GreeterClient(…)`, `this.c = new …`, `c = new …` fields.
pub(super) fn scan_binding(cx: &mut Ctx<'_, '_>, n: Node<'_>) {
    let (holder, value) = match n.kind() {
        "assignment_expression" => (n.child_by_field_name("left"), n.child_by_field_name("right")),
        "public_field_definition" => (n.child_by_field_name("name"), n.child_by_field_name("value")),
        _ => (n.child_by_field_name("name"), n.child_by_field_name("value")),
    };
    let (Some(holder), Some(value)) = (holder, value) else { return };
    let (Some(svc), Some(name)) = (client_service(cx, value), last_name(holder, cx.src)) else { return };
    cx.clients
        .entry(name)
        .and_modify(|v| {
            if v.as_deref() != Some(svc.as_str()) {
                *v = None
            }
        })
        .or_insert(Some(svc));
}

/// `client.sayHello(req, cb)` on a holder bound to a generated client.
/// Whether `call` was one.
pub(super) fn client_call(cx: &mut Ctx<'_, '_>, call: Node<'_>) -> bool {
    if cx.clients.is_empty() {
        return false;
    }
    let Some((Some(recv), method)) = callee(call, cx.src) else { return false };
    let Some(Some(svc)) = last_name(recv, cx.src).and_then(|h| cx.clients.get(&h).cloned()) else { return false };
    if CLIENT_METHODS.contains(&method.as_str()) || !method.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return false;
    }
    let owner = cx.enclosing(call);
    cx.push(ContractSide::Consumes, ContractKind::Rpc, rpc_key("", &svc, &proto_name(&method)), call, owner, FRAMEWORK);
    true
}

/// `server.addService(GreeterService, { sayHello: impl })` /
/// `server.addService(proto.Greeter.service, { … })`. Whether `call` was one.
pub(super) fn add_service(cx: &mut Ctx<'_, '_>, call: Node<'_>) -> bool {
    let Some((Some(_), name)) = callee(call, cx.src) else { return false };
    if name != "addService" || !cx.imports_any(GRPC_MODULES) {
        return false;
    }
    let args = arguments(call);
    let (Some(def), Some(impls)) = (args.first(), args.get(1).filter(|a| a.kind() == "object")) else { return false };
    let Some(svc) = service_of(cx, *def) else { return false };
    let mut c = impls.walk();
    let members: Vec<Node<'_>> = impls.named_children(&mut c).collect();
    for m in members {
        let (key, value) = match m.kind() {
            "pair" => (m.child_by_field_name("key"), m.child_by_field_name("value")),
            "method_definition" => (m.child_by_field_name("name"), None),
            "shorthand_property_identifier" => (Some(m), Some(m)),
            _ => continue,
        };
        let Some(key) = key else { continue };
        let method = proto_name(cx.text(key).trim_matches(['"', '\'']));
        let owner = match m.kind() {
            "method_definition" => cx.def_named_at(key),
            _ => cx.handler_owner(value, call),
        };
        cx.push(ContractSide::Provides, ContractKind::Rpc, rpc_key("", &svc, &method), m, owner, FRAMEWORK);
    }
    true
}

/// `GreeterService` → `Greeter`; `pkg.Greeter.service` → `Greeter`.
fn service_of(cx: &Ctx<'_, '_>, n: Node<'_>) -> Option<String> {
    let name = last_name(n, cx.src)?;
    if name == "service" && n.kind() == "member_expression" {
        return last_name(n.child_by_field_name("object")?, cx.src);
    }
    name.strip_suffix("Service").filter(|s| !s.is_empty()).map(str::to_string)
}
