//! tonic gRPC hints of a Rust file (framework `tonic`, keys
//! `Service/Method`); the file must name `tonic`.
//!
//! - Providers: the methods of `impl <Service> for <Type>` (the trait
//!   tonic generates for a service) taking a `Request<…>` and returning a
//!   `Response<…>` / `Status` result. tonic names them in snake_case
//!   (`async fn upsert`); the key carries the proto name (`Points/Upsert`).
//! - Consumers: `x.method(…)` where `x` is bound in the same function to a
//!   tonic client: `let x = PointsClient::new(channel)` (or `connect`,
//!   `with_origin`, `with_interceptor`, then builder calls), a parameter /
//!   `let` typed `PointsClient<…>`, the closure parameter of a call to a
//!   function of this file taking `impl Fn(PointsClient<…>)`; or
//!   `self.field` with a struct field typed `PointsClient<…>`; or a call
//!   chained on `PointsClient::new(…)` (through builder and `clone` calls,
//!   which are not calls of the service).

use std::collections::HashMap;

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::rpc_key;
use rueca_model::{ContractKind, ContractSide};

use super::http::{method_call, path_call};
use super::{File, last_segment, named_children, text};

const FRAMEWORK: &str = "tonic";

/// Scope of struct fields.
pub(crate) const GLOBAL: usize = usize::MAX;

/// Constructors of a tonic client (`PointsClient::new(channel)`).
const CONSTRUCTORS: &[&str] = &["new", "connect", "with_origin", "with_interceptor"];

/// Methods of a tonic client that are not rpcs.
const NOT_RPCS: &[&str] = &[
    "new",
    "connect",
    "with_origin",
    "with_interceptor",
    "max_decoding_message_size",
    "max_encoding_message_size",
    "send_compressed",
    "accept_compressed",
    "clone",
    "ready",
    "into_inner",
    "as_mut",
    "as_ref",
];

pub(crate) struct Tonic {
    on: bool,
    /// Start byte of the function being walked ([`GLOBAL`] outside).
    pub(crate) scope: usize,
    /// (scope, name) → service of the client it holds.
    clients: HashMap<(usize, String), String>,
    /// Functions of this file taking `impl Fn(<S>Client<…>)` → `S`.
    client_fns: HashMap<String, String>,
}

impl Tonic {
    pub(crate) fn new(on: bool, root: Node<'_>, src: &[u8]) -> Tonic {
        let mut t = Tonic { on, scope: GLOBAL, clients: HashMap::new(), client_fns: HashMap::new() };
        if on {
            t.collect_client_fns(root, src, 0);
        }
        t
    }

    /// Functions with an `impl Fn(<S>Client<…>)` parameter.
    fn collect_client_fns(&mut self, node: Node<'_>, src: &[u8], depth: usize) {
        if depth > 512 {
            return;
        }
        if node.kind() == "function_item"
            && let (Some(name), Some(params)) =
                (node.child_by_field_name("name"), node.child_by_field_name("parameters"))
            && let Some(svc) = closure_client(text(params, src))
        {
            self.client_fns.insert(text(name, src).to_string(), svc);
        }
        for child in named_children(node) {
            self.collect_client_fns(child, src, depth + 1);
        }
    }

    /// Enters a function: its typed client parameters are bound.
    pub(crate) fn enter_function(&mut self, file: &File<'_>, func: Node<'_>) {
        if !self.on {
            return;
        }
        self.scope = func.start_byte();
        let params = func.child_by_field_name("parameters").map(named_children).unwrap_or_default();
        for p in params.into_iter().filter(|p| p.kind() == "parameter") {
            self.bind_typed(file, p);
        }
    }

    /// `name: PointsClient<…>` (parameter, closure parameter).
    fn bind_typed(&mut self, file: &File<'_>, param: Node<'_>) {
        let (Some(pat), Some(ty)) = (param.child_by_field_name("pattern"), param.child_by_field_name("type")) else {
            return;
        };
        if let (Some(name), Some(svc)) = (pattern_name(pat, file.src), type_service(file.text(ty))) {
            self.clients.insert((self.scope, name), svc);
        }
    }

    /// `let client = PointsClient::new(channel);`, `let c: PointsClient<_> = …`.
    pub(crate) fn let_binding(&mut self, file: &File<'_>, decl: Node<'_>) {
        if !self.on {
            return;
        }
        let Some(name) = decl.child_by_field_name("pattern").and_then(|p| pattern_name(p, file.src)) else { return };
        let typed = decl.child_by_field_name("type").and_then(|t| type_service(file.text(t)));
        let built = decl.child_by_field_name("value").and_then(|v| self.expr_service(file, v, 0));
        if let Some(svc) = typed.or(built) {
            self.clients.insert((self.scope, name), svc);
        }
    }

    /// A struct field typed `PointsClient<…>`.
    pub(crate) fn field(&mut self, file: &File<'_>, decl: Node<'_>) {
        if !self.on {
            return;
        }
        let (Some(name), Some(ty)) = (decl.child_by_field_name("name"), decl.child_by_field_name("type")) else {
            return;
        };
        if let Some(svc) = type_service(file.text(ty)) {
            self.clients.insert((GLOBAL, file.text(name).to_string()), svc);
        }
    }

    /// The service of the client an expression evaluates to.
    fn expr_service(&self, file: &File<'_>, expr: Node<'_>, depth: usize) -> Option<String> {
        if depth > 32 {
            return None;
        }
        match expr.kind() {
            "try_expression" | "await_expression" | "parenthesized_expression" | "reference_expression" => {
                self.expr_service(file, *named_children(expr).first()?, depth + 1)
            }
            "identifier" => self.lookup(file.text(expr)),
            "field_expression" => self.field_service(file, expr),
            "call_expression" => match method_call(expr, file.src) {
                Some((recv, name, _)) if NOT_RPCS.contains(&name) => self.expr_service(file, recv, depth + 1),
                Some(_) => None,
                None => constructor_service(path_call(expr, file.src)?.0),
            },
            _ => None,
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        let get = |scope: usize| self.clients.get(&(scope, name.to_string())).cloned();
        get(self.scope).or_else(|| get(GLOBAL))
    }

    /// `self.client` with a field typed as a client.
    fn field_service(&self, file: &File<'_>, expr: Node<'_>) -> Option<String> {
        let field = file.text(expr.child_by_field_name("field")?);
        self.clients.get(&(GLOBAL, field.to_string())).cloned()
    }

    // -- calls ---------------------------------------------------------------

    /// A client call, or a call passing closures to an `impl Fn(<S>Client)`
    /// function of this file (their first parameter is bound).
    pub(crate) fn call(&mut self, file: &mut File<'_>, call: Node<'_>) {
        if !self.on {
            return;
        }
        self.bind_closures(file, call);
        let Some((recv, name, _)) = method_call(call, file.src) else { return };
        if NOT_RPCS.contains(&name) || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return;
        }
        if let Some(svc) = self.expr_service(file, recv, 0) {
            let owner = file.enclosing(call);
            file.push(
                ContractSide::Consumes,
                ContractKind::Rpc,
                rpc_key("", &svc, &pascal(name)),
                call,
                owner,
                FRAMEWORK,
            );
        }
    }

    fn bind_closures(&mut self, file: &File<'_>, call: Node<'_>) {
        let (name, args) = match (method_call(call, file.src), path_call(call, file.src)) {
            (Some((_, name, args)), _) => (name, args),
            (None, Some((path, args))) => (last_segment(path), args),
            _ => return,
        };
        let Some(svc) = self.client_fns.get(name).cloned() else { return };
        for closure in args.iter().filter(|a| a.kind() == "closure_expression") {
            let params = closure.child_by_field_name("parameters").map(named_children).unwrap_or_default();
            let first = params.first().map(|p| p.child_by_field_name("pattern").unwrap_or(*p));
            if let Some(name) = first.and_then(|p| pattern_name(p, file.src)) {
                self.clients.insert((self.scope, name), svc.clone());
            }
        }
    }

    // -- providers -----------------------------------------------------------

    /// `impl Points for PointsService { async fn upsert(&self, request:
    /// Request<UpsertPoints>) -> Result<Response<…>, Status> }`.
    pub(crate) fn server_impl(&self, file: &mut File<'_>, imp: Node<'_>) {
        if !self.on {
            return;
        }
        let Some(service) = imp.child_by_field_name("trait").map(|t| trait_name(file.text(t))) else { return };
        let body = imp.child_by_field_name("body").map(named_children).unwrap_or_default();
        for func in body.into_iter().filter(|f| f.kind() == "function_item") {
            let Some(name) = func.child_by_field_name("name") else { continue };
            if is_rpc_signature(file, func) {
                let key = rpc_key("", &service, &pascal(file.text(name)));
                let owner = file.def_at(name);
                file.push(ContractSide::Provides, ContractKind::Rpc, key, func, owner, FRAMEWORK);
            }
        }
    }
}

/// Takes a `Request<…>` and returns a `Response<…>` (in a `Result`).
fn is_rpc_signature(file: &File<'_>, func: Node<'_>) -> bool {
    let params = func.child_by_field_name("parameters").map_or("", |p| file.text(p));
    let ret = func.child_by_field_name("return_type").map_or("", |r| file.text(r));
    params.contains("Request<") && ret.contains("Response<")
}

/// `Points` of `Points`, `qdrant::points_server::Points`, `Points<T>`.
fn trait_name(t: &str) -> String {
    last_segment(t.split('<').next().unwrap_or(t)).to_string()
}

/// The bound name of a simple pattern (`client`, `mut client`).
fn pattern_name(pat: Node<'_>, src: &[u8]) -> Option<String> {
    let t = text(pat, src).trim();
    let t = t.strip_prefix("mut ").unwrap_or(t).trim();
    t.chars().all(|c| c.is_alphanumeric() || c == '_').then(|| t.to_string()).filter(|t| !t.is_empty())
}

/// `S` of a type `SClient<…>` / `path::SClient<…>` (`&mut` allowed).
fn type_service(ty: &str) -> Option<String> {
    let t = ty.trim_start_matches('&').trim_start_matches("mut ").trim();
    let head = t.split('<').next().unwrap_or(t);
    client_service(last_segment(head))
}

/// `S` of `<S>Client`: `S` starts upper-case and is alphanumeric.
fn client_service(name: &str) -> Option<String> {
    let svc = name.strip_suffix("Client")?;
    let ok = svc.chars().next().is_some_and(char::is_uppercase) && svc.chars().all(char::is_alphanumeric);
    ok.then(|| svc.to_string())
}

/// `S` of a constructor path `…::SClient::new`.
fn constructor_service(path: &str) -> Option<String> {
    let (head, ctor) = path.rsplit_once("::")?;
    if !CONSTRUCTORS.contains(&ctor.trim()) {
        return None;
    }
    client_service(last_segment(head.split('<').next().unwrap_or(head)))
}

/// `S` of a parameter list holding `impl Fn(SClient<…>)` (also `FnOnce`,
/// `FnMut`).
fn closure_client(params: &str) -> Option<String> {
    let at = ["Fn(", "FnOnce(", "FnMut("].iter().find_map(|f| params.find(f).map(|i| i + f.len()))?;
    let arg = params[at..].split(['<', ',', ')']).next()?;
    client_service(last_segment(arg.trim()))
}

/// `query_batch` → `QueryBatch` (tonic's method names back to the proto's).
pub(crate) fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}
