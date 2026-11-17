//! gRPC (grpcio): stub calls (consumers) and servicer implementations
//! (providers), keyed `Service/Method` (the proto package is not in code).
//! A holder named `…stub` that the file does not bind (a stub passed in
//! as a parameter, `self._stub = stub`) calls `*/Method`
//! (`rueca_model::contract::ANY_SERVICE`): the method of any service.

use rueca_extract::tree_sitter::Node;
use rueca_extract::{first_line, text_of};
use rueca_model::contract::{ANY_SERVICE, rpc_key};
use rueca_model::{ContractKind, ContractSide};

use super::{Ctx, last_name, root_name};

const FRAMEWORK: &str = "grpc-python";

impl Ctx<'_, '_> {
    /// The service of a stub class referenced in `node` (a call's callee or
    /// an annotation, generics included): `<mod>.XStub` with `<mod>`
    /// imported, or an imported `XStub`; never a class of this file.
    fn stub_service(&self, node: Node<'_>) -> Option<String> {
        let mut stack = vec![(node, 0usize)];
        while let Some((n, depth)) = stack.pop() {
            if let Some(svc) = self.stub_class(n) {
                return Some(svc);
            }
            if depth < 8 && n.kind() != "attribute" {
                let mut c = n.walk();
                stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
            }
        }
        None
    }

    fn stub_class(&self, n: Node<'_>) -> Option<String> {
        if !matches!(n.kind(), "identifier" | "attribute") {
            return None;
        }
        let name = last_name(n, self.src)?;
        let svc =
            name.strip_suffix("Stub").filter(|s| !s.is_empty() && s.starts_with(|c: char| c.is_ascii_uppercase()))?;
        let root = root_name(n, self.src)?;
        let imported = self.imports.contains_key(&root);
        (imported && !self.local_classes.contains(&name)).then(|| svc.to_string())
    }

    fn bind_stub(&mut self, holder: &str, svc: String) {
        let holder = holder.trim_start_matches('_').to_string();
        self.stubs
            .entry(holder)
            .and_modify(|v| {
                if v.as_deref() != Some(svc.as_str()) {
                    *v = None
                }
            })
            .or_insert(Some(svc));
    }
}

/// `x = mod.XStub(channel)`, `self._stub = XStub(…)`, `x: mod.XStub = …`.
pub(super) fn scan_assignment(cx: &mut Ctx<'_, '_>, n: Node<'_>) {
    let Some(left) = n.child_by_field_name("left") else { return };
    let from_value = n
        .child_by_field_name("right")
        .filter(|r| r.kind() == "call")
        .and_then(|r| r.child_by_field_name("function"))
        .and_then(|f| cx.stub_service(f));
    let svc = from_value.or_else(|| n.child_by_field_name("type").and_then(|t| cx.stub_service(t)));
    let Some(name) = last_name(left, cx.src) else { return };
    match svc {
        Some(svc) => cx.bind_stub(&name, svc),
        None if built_from_local_class(cx, n) => {
            cx.stubs.insert(name.trim_start_matches('_').to_string(), None);
        }
        None => {}
    }
}

/// `x = Fake…()` with a class of this file: the holder is no stub.
fn built_from_local_class(cx: &Ctx<'_, '_>, n: Node<'_>) -> bool {
    let callee =
        n.child_by_field_name("right").filter(|r| r.kind() == "call").and_then(|r| r.child_by_field_name("function"));
    callee.and_then(|f| last_name(f, cx.src)).is_some_and(|c| cx.local_classes.contains(&c))
}

/// `def grpc_points(self) -> grpc.PointsStub` (a property returning a stub).
pub(super) fn scan_return(cx: &mut Ctx<'_, '_>, n: Node<'_>) {
    let svc = n.child_by_field_name("return_type").and_then(|t| cx.stub_service(t));
    if let (Some(svc), Some(name)) = (svc, n.child_by_field_name("name")) {
        let name = cx.text(name);
        cx.bind_stub(&name, svc);
    }
}

/// `def f(stub: api_v1.DaprStub)`.
pub(super) fn scan_parameter(cx: &mut Ctx<'_, '_>, n: Node<'_>) {
    let name = n.child_by_field_name("name").or_else(|| n.named_child(0)).filter(|c| c.kind() == "identifier");
    let svc = n.child_by_field_name("type").and_then(|t| cx.stub_service(t));
    if let (Some(svc), Some(name)) = (svc, name) {
        let name = cx.text(name);
        cx.bind_stub(&name, svc);
    }
}

/// `<holder>.Method` where the holder name is bound to a stub: a call of
/// (or a reference to) the RPC `Service/Method`.
pub(super) fn stub_call<'t>(cx: &mut Ctx<'t, '_>, node: Node<'t>) {
    let (Some(obj), Some(attr)) = (node.child_by_field_name("object"), node.child_by_field_name("attribute")) else {
        return;
    };
    let method = cx.text(attr);
    if !method.starts_with(|c: char| c.is_ascii_uppercase()) {
        return;
    }
    let Some(holder) = last_name(obj, cx.src) else { return };
    let Some(svc) = holder_service(cx, &holder) else { return };
    if !cx.rpc_seen.insert((node.start_position().row as u32, method.clone())) {
        return;
    }
    let site = node
        .parent()
        .filter(|p| p.kind() == "call" && p.child_by_field_name("function").is_some_and(|f| f.id() == node.id()))
        .unwrap_or(node);
    let owner = cx.enclosing(site);
    cx.push(ContractSide::Consumes, ContractKind::Rpc, rpc_key("", &svc, &method), site, owner, FRAMEWORK);
}

/// The service of a stub holder: bound in the file, else [`ANY_SERVICE`]
/// for an unbound name ending in `stub`; `None` for anything else (a name
/// bound to a class that is not a stub).
fn holder_service(cx: &Ctx<'_, '_>, holder: &str) -> Option<String> {
    let name = holder.trim_start_matches('_');
    match cx.stubs.get(name) {
        Some(bound) => bound.clone(),
        None => name.to_ascii_lowercase().ends_with("stub").then(|| ANY_SERVICE.to_string()),
    }
}

/// `class Impl(pb2_grpc.GreeterServicer)`: every public PascalCase method
/// provides `Greeter/<Method>`.
pub(super) fn servicer(cx: &mut Ctx<'_, '_>, class: Node<'_>) {
    let Some(svc) = servicer_base(cx, class) else { return };
    let Some(body) = class.child_by_field_name("body") else { return };
    let mut c = body.walk();
    let members: Vec<Node<'_>> = body.named_children(&mut c).collect();
    for m in members {
        let func = if m.kind() == "decorated_definition" { m.child_by_field_name("definition") } else { Some(m) };
        let Some(func) = func.filter(|f| f.kind() == "function_definition") else { continue };
        let Some(name) = func.child_by_field_name("name") else { continue };
        let method = cx.text(name);
        if !method.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        let owner = cx.def_named_at(name);
        let raw = first_line(&text_of(func, cx.src), 200);
        cx.push_raw(ContractSide::Provides, ContractKind::Rpc, rpc_key("", &svc, &method), func, owner, FRAMEWORK, raw);
    }
}

/// The service of a `…XServicer` base class not defined in this file.
fn servicer_base(cx: &Ctx<'_, '_>, class: Node<'_>) -> Option<String> {
    let bases = class.child_by_field_name("superclasses")?;
    let mut c = bases.walk();
    bases.named_children(&mut c).find_map(|b| {
        let name = last_name(b, cx.src)?;
        let svc = name.strip_suffix("Servicer").filter(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))?;
        (!cx.local_classes.contains(&name)).then(|| svc.to_string())
    })
}
