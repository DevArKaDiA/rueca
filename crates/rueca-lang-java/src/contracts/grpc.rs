//! grpc-java hints of a Java file (framework `grpc-java`, keys
//! `Service/Method`, the method in the proto's PascalCase); the file must
//! name `io.grpc`.
//!
//! - Providers: the public methods taking a `StreamObserver` of a class
//!   extending `GreeterGrpc.GreeterImplBase` (or implementing
//!   `GreeterGrpc.AsyncService`) → `Greeter/<Method>`.
//! - Consumers: `stub.sayHello(req)` where `stub` is a field, parameter or
//!   local typed `GreeterGrpc.GreeterBlockingStub` / `…Stub` /
//!   `…FutureStub` or built by `GreeterGrpc.newBlockingStub(channel)`
//!   (also through `with…` call options), or a call chained on
//!   `GreeterGrpc.newStub(channel)`.

use std::collections::HashMap;

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::rpc_key;
use rueca_model::{ContractKind, ContractSide};

use super::{File, last_segment, named_children, pascal};

const FRAMEWORK: &str = "grpc-java";

/// Scope of fields.
const GLOBAL: usize = usize::MAX;

/// Stub factories of a generated `XGrpc` class.
const STUB_FACTORIES: &[&str] = &["newBlockingStub", "newStub", "newFutureStub", "newBlockingV2Stub"];

/// Stub type suffixes, longest first.
const STUB_SUFFIXES: &[&str] = &["BlockingV2Stub", "BlockingStub", "FutureStub", "Stub"];

pub(crate) struct Grpc {
    pub(crate) on: bool,
    /// Start byte of the method / constructor being walked ([`GLOBAL`]
    /// outside).
    pub(crate) scope: usize,
    /// (scope, name) → service of the stub it holds.
    stubs: HashMap<(usize, String), String>,
}

impl Grpc {
    pub(crate) fn new(on: bool) -> Grpc {
        Grpc { on, scope: GLOBAL, stubs: HashMap::new() }
    }

    /// Fields typed or initialized as stubs (before the walk: a field may
    /// be declared after the methods using it).
    pub(crate) fn collect_fields(&mut self, file: &File<'_>, node: Node<'_>, depth: usize) {
        if !self.on || depth > file.max_depth {
            return;
        }
        if node.kind() == "field_declaration" {
            self.bind_declarators(file, node, GLOBAL);
        }
        for child in named_children(node) {
            self.collect_fields(file, child, depth + 1);
        }
    }

    /// Locals and parameters typed or initialized as stubs.
    pub(crate) fn bind(&mut self, file: &File<'_>, node: Node<'_>) {
        if !self.on {
            return;
        }
        if node.kind() == "formal_parameter" {
            let svc = node.child_by_field_name("type").and_then(|t| stub_service(file.text(t)));
            if let (Some(svc), Some(name)) = (svc, node.child_by_field_name("name")) {
                self.stubs.insert((self.scope, file.text(name).to_string()), svc);
            }
            return;
        }
        self.bind_declarators(file, node, self.scope);
    }

    fn bind_declarators(&mut self, file: &File<'_>, decl: Node<'_>, scope: usize) {
        let typed = decl.child_by_field_name("type").and_then(|t| stub_service(file.text(t)));
        let mut cursor = decl.walk();
        let declarators: Vec<Node<'_>> = decl.children_by_field_name("declarator", &mut cursor).collect();
        for d in declarators {
            let built = d.child_by_field_name("value").and_then(|v| self.expr_service(file, v, 0));
            if let (Some(svc), Some(name)) = (typed.clone().or(built), d.child_by_field_name("name")) {
                self.stubs.insert((scope, file.text(name).to_string()), svc);
            }
        }
    }

    /// The service of the stub an expression evaluates to.
    fn expr_service(&self, file: &File<'_>, expr: Node<'_>, depth: usize) -> Option<String> {
        if depth > 32 {
            return None;
        }
        match expr.kind() {
            "identifier" => self.lookup(file.text(expr)),
            "field_access" => {
                self.stubs.get(&(GLOBAL, file.text(expr.child_by_field_name("field")?).to_string())).cloned()
            }
            "parenthesized_expression" => self.expr_service(file, *named_children(expr).first()?, depth + 1),
            "method_invocation" => {
                let name = file.text(expr.child_by_field_name("name")?);
                let object = expr.child_by_field_name("object")?;
                if STUB_FACTORIES.contains(&name) {
                    let class = last_segment(file.text(object));
                    return class.strip_suffix("Grpc").filter(|s| !s.is_empty()).map(str::to_string);
                }
                name.starts_with("with").then(|| self.expr_service(file, object, depth + 1)).flatten()
            }
            _ => None,
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        let get = |scope: usize| self.stubs.get(&(scope, name.to_string())).cloned();
        get(self.scope).or_else(|| get(GLOBAL))
    }

    /// A stub call.
    pub(crate) fn call(&self, file: &mut File<'_>, call: Node<'_>) {
        if !self.on {
            return;
        }
        let (Some(name), Some(object)) = (call.child_by_field_name("name"), call.child_by_field_name("object")) else {
            return;
        };
        let method = file.text(name);
        if method.starts_with("with") || matches!(method, "getChannel" | "getCallOptions") {
            return;
        }
        if let Some(svc) = self.expr_service(file, object, 0) {
            let owner = file.enclosing(call);
            file.push(
                ContractSide::Consumes,
                ContractKind::Rpc,
                rpc_key("", &svc, &pascal(method)),
                call,
                owner,
                FRAMEWORK,
            );
        }
    }

    // -- providers -----------------------------------------------------------

    /// The service a class serves: `extends GreeterGrpc.GreeterImplBase`,
    /// `implements GreeterGrpc.AsyncService`.
    pub(crate) fn server_service(&self, file: &File<'_>, class: Node<'_>) -> Option<String> {
        if !self.on {
            return None;
        }
        let base = named_children(class).into_iter().find(|c| c.kind() == "superclass").map(|s| file.text(s));
        let from_base = base
            .and_then(|b| last_segment(b.trim_start_matches("extends").trim()).strip_suffix("ImplBase"))
            .filter(|s| !s.is_empty());
        if let Some(svc) = from_base {
            return Some(svc.to_string());
        }
        let ifaces = class.child_by_field_name("interfaces").map_or("", |i| file.text(i));
        ifaces
            .split([',', ' '])
            .find_map(|t| t.trim().strip_suffix("Grpc.AsyncService"))
            .map(|g| last_segment(g).to_string())
            .filter(|s| !s.is_empty())
    }

    /// A public method taking a `StreamObserver` of a server class.
    pub(crate) fn provider(&self, file: &mut File<'_>, method: Node<'_>, service: &str) {
        let (Some(name), Some(params)) = (method.child_by_field_name("name"), method.child_by_field_name("parameters"))
        else {
            return;
        };
        let public = named_children(method).iter().any(|c| c.kind() == "modifiers" && file.text(*c).contains("public"));
        if public && file.text(params).contains("StreamObserver") {
            let key = rpc_key("", service, &pascal(file.text(name)));
            let owner = file.def_at(name);
            file.push(ContractSide::Provides, ContractKind::Rpc, key, method, owner, FRAMEWORK);
        }
    }
}

/// `Greeter` of `GreeterGrpc.GreeterBlockingStub` / `GreeterStub`.
fn stub_service(ty: &str) -> Option<String> {
    let name = last_segment(ty);
    let svc = STUB_SUFFIXES.iter().find_map(|s| name.strip_suffix(s))?;
    let ok = svc.chars().next().is_some_and(char::is_uppercase) && svc.chars().all(char::is_alphanumeric);
    (ok && svc != "Abstract").then(|| svc.to_string())
}
