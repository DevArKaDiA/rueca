//! gRPC hints of a Go file (framework `grpc-go`; keys `Service/Method`,
//! or `*/Method` when only the method is known, `rueca_model::contract::ANY_SERVICE`).
//!
//! Providers are exported methods declared in the file:
//! - on a type the file declares as a server: it embeds
//!   `pb.Unimplemented<S>Server` / `pb.Unsafe<S>Server` or the server
//!   interface `pb.<S>Server` (a delegating wrapper), or the file
//!   registers it with `pb.Register<S>Server(s, x)` where `x` is `&T{}`,
//!   the receiver of the enclosing method `func (x *T)`, or a variable of
//!   the enclosing function assigned `&T{…}` → `S/Method`;
//! - taking a stream `pb.<S>_<Method>Server` → `S/Method`;
//! - shaped like a unary server method, `(ctx context.Context, in
//!   *pb.<Method>Request) (*pb.<Resp>, error)` (a trailing `Alpha1` /
//!   `Beta2` / `V2` of the method may be absent from the request type or
//!   follow `Request`: `DeleteFileRequestAlpha1`) → `*/Method`.
//!
//! A server type declared in the file (embedding or registration) also
//! gets a service marker `S/*` (`rueca_model::contract::ANY_METHOD`) owned
//! by the type: the resolver expands it to the type's handler-shaped
//! methods in every file of the package (methods split over files).
//!
//! `pb` is a proto package: an import whose binding name ends in `pb` or
//! whose path has a `proto` / `protos` / `protocol` / `*pb` segment
//! (`google.golang.org/protobuf` well-known types excluded).
//!
//! Consumers are calls `x.Method(…)` of an exported method where `x` is
//! bound in the file to a `<S>Client` (a variable, parameter or struct
//! field typed `pb.<S>Client`, or built by `pb.New<S>Client(conn)`), or the
//! call is chained on `pb.New<S>Client(conn)` → `S/Method`; or, in a file
//! importing a proto package, the first argument is a context (`ctx`,
//! `context.…`) and `x`'s last name is a gRPC client's (`protoClient`,
//! `grpcClient`: `client` with `proto` / `grpc` / `pb` / `rpc`) or an
//! argument is the method's request (`&pb.<Method>Request{…}`) →
//! `*/Method`. Stream types (`Greeter_ChatClient`) are not clients.

use std::collections::{HashMap, HashSet};

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{ANY_METHOD, ANY_SERVICE, rpc_key};
use rueca_model::{ContractKind, ContractSide};

use super::http::{selector_call, selector_parts};
use super::{File, named_children, text};

const FRAMEWORK: &str = "grpc-go";

/// Scope of struct fields and package-level variables.
const GLOBAL: usize = usize::MAX;

/// What the file says about gRPC servers and clients.
pub(crate) struct Bindings {
    /// Server type → service.
    servers: HashMap<String, String>,
    /// Every (server type, service) the file declares, with the evidence
    /// node's byte range, in order.
    served: Vec<(String, String, (usize, usize))>,
    /// Names typed or built as `<S>Client` → `S`, by scope: struct fields
    /// and package-level variables in [`GLOBAL`], parameters and locals in
    /// their top-level declaration (its start byte).
    clients: HashMap<(usize, String), String>,
    /// Binding names of proto packages.
    proto: HashSet<String>,
}

impl Bindings {
    pub(crate) fn collect(file: &File<'_>, root: Node<'_>) -> Bindings {
        let proto = file.imports.iter().filter(|(a, p)| is_proto_import(a, p)).map(|(a, _)| a.clone()).collect();
        let mut b = Bindings { servers: HashMap::new(), served: Vec::new(), clients: HashMap::new(), proto };
        b.visit(file, root, 0);
        b
    }

    fn visit(&mut self, file: &File<'_>, node: Node<'_>, depth: usize) {
        if depth > file.max_depth {
            return;
        }
        match node.kind() {
            "type_spec" => self.type_spec(file, node),
            "field_declaration" | "parameter_declaration" | "var_spec" => self.typed_names(file, node),
            "short_var_declaration" | "assignment_statement" => self.constructed(file, node),
            "call_expression" => self.registration(file, node),
            _ => {}
        }
        for child in named_children(node) {
            self.visit(file, child, depth + 1);
        }
    }

    /// `type T struct { pb.UnimplementedGreeterServer }`.
    fn type_spec(&mut self, file: &File<'_>, spec: Node<'_>) {
        let (Some(name), Some(ty)) = (spec.child_by_field_name("name"), spec.child_by_field_name("type")) else {
            return;
        };
        if ty.kind() != "struct_type" {
            return;
        }
        let fields = named_children(ty).into_iter().flat_map(named_children);
        let embedded = fields.filter(|f| f.kind() == "field_declaration" && f.child_by_field_name("name").is_none());
        for f in embedded {
            let Some(t) = f.child_by_field_name("type") else { continue };
            if let Some(svc) = server_service(type_name(t, file.src)).or_else(|| self.server_interface(file, t)) {
                self.serve(file.text(name), svc, (spec.start_byte(), spec.end_byte()));
            }
        }
    }

    /// `S` of an embedded server interface `pb.<S>Server` (a wrapper that
    /// delegates to the server it embeds).
    fn server_interface<'s>(&self, file: &File<'s>, ty: Node<'_>) -> Option<&'s str> {
        let ty = if ty.kind() == "pointer_type" { *named_children(ty).first()? } else { ty };
        if ty.kind() != "qualified_type" || !self.proto.contains(file.text(ty.child_by_field_name("package")?)) {
            return None;
        }
        file.text(ty.child_by_field_name("name")?).strip_suffix("Server").filter(|s| is_service_name(s))
    }

    /// Fields, parameters and variables typed `pb.<S>Client`.
    fn typed_names(&mut self, file: &File<'_>, node: Node<'_>) {
        let Some(svc) = node.child_by_field_name("type").and_then(|t| self.client_type(file, t)) else { return };
        let scope = match node.kind() {
            "field_declaration" => GLOBAL,
            "var_spec" if top_node(node).kind() == "var_declaration" => GLOBAL,
            _ => top_of(node),
        };
        let mut cursor = node.walk();
        for name in node.children_by_field_name("name", &mut cursor) {
            self.clients.insert((scope, file.text(name).to_string()), svc.clone());
        }
    }

    /// `x := pb.NewGreeterClient(conn)`.
    fn constructed(&mut self, file: &File<'_>, node: Node<'_>) {
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
            return;
        };
        for (l, r) in named_children(left).into_iter().zip(named_children(right)) {
            if l.kind() == "identifier"
                && let Some(svc) = self.constructor_service(file, r)
            {
                self.clients.insert((top_of(node), file.text(l).to_string()), svc);
            }
        }
    }

    /// `pb.RegisterGreeterServer(s, &server{})`: `server` serves `Greeter`.
    fn registration(&mut self, file: &File<'_>, call: Node<'_>) {
        let Some((qualifier, name)) = call.child_by_field_name("function").and_then(|f| qualified_name(f, file.src))
        else {
            return;
        };
        let Some(svc) = name.strip_prefix("Register").and_then(|n| n.strip_suffix("Server")) else { return };
        if !is_service_name(svc) || !self.proto.contains(qualifier) {
            return;
        }
        let args = call.child_by_field_name("arguments").map(named_children).unwrap_or_default();
        let ty = args.get(1).and_then(|a| constructed_type(*a, file.src).or_else(|| variable_type(*a, file.src)));
        if let Some(t) = ty {
            self.serve(t, svc, (call.start_byte(), call.end_byte()));
        }
    }

    fn serve(&mut self, ty: &str, svc: &str, at: (usize, usize)) {
        self.servers.insert(ty.to_string(), svc.to_string());
        self.served.push((ty.to_string(), svc.to_string(), at));
    }

    /// `S` of a type `pb.<S>Client` / `*pb.<S>Client`.
    fn client_type(&self, file: &File<'_>, ty: Node<'_>) -> Option<String> {
        let ty = if ty.kind() == "pointer_type" { *named_children(ty).first()? } else { ty };
        if ty.kind() != "qualified_type" {
            return None;
        }
        let qualifier = file.text(ty.child_by_field_name("package")?);
        let svc = file.text(ty.child_by_field_name("name")?).strip_suffix("Client").filter(|s| is_service_name(s))?;
        self.proto.contains(qualifier).then(|| svc.to_string())
    }

    /// `S` of `pb.New<S>Client(…)`.
    fn constructor_service(&self, file: &File<'_>, call: Node<'_>) -> Option<String> {
        if call.kind() != "call_expression" {
            return None;
        }
        let (qualifier, name) = qualified_name(call.child_by_field_name("function")?, file.src)?;
        let svc = name.strip_prefix("New")?.strip_suffix("Client").filter(|s| is_service_name(s))?;
        self.proto.contains(qualifier).then(|| svc.to_string())
    }

    // -- providers -----------------------------------------------------------

    /// Provider hints of the file's methods.
    pub(crate) fn providers(&self, file: &mut File<'_>, root: Node<'_>) {
        for decl in named_children(root).into_iter().filter(|d| d.kind() == "method_declaration") {
            let Some(name) = decl.child_by_field_name("name") else { continue };
            let method = file.text(name).to_string();
            if !crate::extract::is_exported(&method) {
                continue;
            }
            if let Some(key) = self.provider_key(file, decl, &method) {
                let owner = file.def_at(name);
                file.push(ContractSide::Provides, ContractKind::Rpc, key, decl, owner, FRAMEWORK);
            }
        }
        self.service_markers(file, root);
    }

    /// One `S/*` marker per (server type of this file, service), on the
    /// type's definition; the evidence is the embedding type or the
    /// registration call.
    fn service_markers(&self, file: &mut File<'_>, root: Node<'_>) {
        for (ty, svc, (start, end)) in &self.served {
            let (Some(owner), Some(evidence)) = (file.type_named(ty), root.descendant_for_byte_range(*start, *end))
            else {
                continue;
            };
            file.push(
                ContractSide::Provides,
                ContractKind::Rpc,
                rpc_key("", svc, ANY_METHOD),
                evidence,
                Some(owner),
                FRAMEWORK,
            );
        }
    }

    fn provider_key(&self, file: &File<'_>, decl: Node<'_>, method: &str) -> Option<String> {
        let params = decl.child_by_field_name("parameters")?;
        let types = param_types(params, file.src);
        if types.iter().any(|t| t.contains("CallOption")) {
            return None;
        }
        let recv = decl.child_by_field_name("receiver").and_then(|r| receiver_type(r, file.src));
        if let Some(svc) = recv.and_then(|r| self.servers.get(r)) {
            return handler_params(&types, method).then(|| rpc_key("", svc, method));
        }
        if let Some(svc) = types.iter().find_map(|t| self.stream_service(t, method)) {
            return Some(rpc_key("", svc, method));
        }
        let result = decl.child_by_field_name("result").map_or("", |r| text(r, file.src));
        self.unary_shaped(&types, result, method).then(|| rpc_key("", ANY_SERVICE, method))
    }

    /// `S` of a stream parameter type `pb.<S>_<Method>Server`.
    fn stream_service<'t>(&self, ty: &'t str, method: &str) -> Option<&'t str> {
        let (qualifier, name) = ty.split_once('.')?;
        let svc = name.strip_suffix("Server")?.strip_suffix(method)?.strip_suffix('_')?;
        (!svc.is_empty() && self.proto.contains(qualifier)).then_some(svc)
    }

    /// `(ctx context.Context, in *pb.<Method>Request) (*pb.X, error)`.
    fn unary_shaped(&self, types: &[String], result: &str, method: &str) -> bool {
        let [ctx, req] = types else { return false };
        let Some((qualifier, name)) = req.strip_prefix('*').and_then(|r| r.split_once('.')) else { return false };
        let proto_result = result.contains("error") && self.proto.iter().any(|p| result.contains(&format!("*{p}.")));
        let empty_result = result.contains("emptypb.Empty") || result.contains("empty.Empty");
        ctx == "context.Context"
            && self.proto.contains(qualifier)
            && request_matches(name, method)
            && (proto_result || empty_result)
    }

    // -- consumers -----------------------------------------------------------

    /// Emits a consumer hint when `recv.m(args)` is a gRPC client call.
    pub(crate) fn consumer(&self, file: &mut File<'_>, call: Node<'_>, recv: Node<'_>, m: &str, args: &[Node<'_>]) {
        if !crate::extract::is_exported(m) {
            return;
        }
        let svc = self.receiver_service(file, recv).or_else(|| self.loose_client(file, recv, m, args));
        if let Some(svc) = svc {
            let owner = file.enclosing(call);
            file.push(ContractSide::Consumes, ContractKind::Rpc, rpc_key("", &svc, m), call, owner, FRAMEWORK);
        }
    }

    /// The service of a receiver bound to a `<S>Client` in this file.
    fn receiver_service(&self, file: &File<'_>, recv: Node<'_>) -> Option<String> {
        let get = |scope: usize, name: &str| self.clients.get(&(scope, name.to_string())).cloned();
        match recv.kind() {
            "identifier" => get(top_of(recv), file.text(recv)).or_else(|| get(GLOBAL, file.text(recv))),
            "selector_expression" => get(GLOBAL, file.text(recv.child_by_field_name("field")?)),
            "call_expression" => self.constructor_service(file, recv),
            _ => None,
        }
    }

    /// `c.protoClient.M(ctx, …)` with the field typed in another file: the
    /// file imports a proto package, the first argument is a context, and
    /// the last name is a gRPC client's (`protoClient`, `grpcClient`) or an
    /// argument is the method's request (`&pb.<M>Request{…}`).
    fn loose_client(&self, file: &File<'_>, recv: Node<'_>, m: &str, args: &[Node<'_>]) -> Option<String> {
        let last = match recv.kind() {
            "identifier" if !file.imports.contains_key(file.text(recv)) => file.text(recv),
            "selector_expression" => file.text(recv.child_by_field_name("field")?),
            _ => return None,
        };
        let ctx = file.text(*args.first()?).to_ascii_lowercase();
        let ctx_like = ctx.contains("ctx") || ctx.starts_with("context.");
        let last = last.to_ascii_lowercase();
        let rpc_name = ["proto", "grpc", "pb", "rpc"].iter().any(|w| last.contains(w));
        let request =
            self.proto.iter().any(|p| args.iter().any(|a| file.text(*a).contains(&format!("{p}.{m}Request"))));
        (!self.proto.is_empty() && ctx_like && last.contains("client") && (rpc_name || request))
            .then(|| ANY_SERVICE.to_string())
    }
}

/// The top-level declaration holding `node`.
fn top_node(node: Node<'_>) -> Node<'_> {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if p.kind() == "source_file" {
            break;
        }
        cur = p;
    }
    cur
}

/// Start byte of the top-level declaration holding `node`.
fn top_of(node: Node<'_>) -> usize {
    top_node(node).start_byte()
}

/// Whether an import is a proto package (see the module docs).
fn is_proto_import(alias: &str, path: &str) -> bool {
    if path.starts_with("google.golang.org/protobuf") || path.starts_with("github.com/golang/protobuf") {
        return false;
    }
    let proto_segment = path
        .split('/')
        .any(|s| matches!(s, "proto" | "protos" | "protocol" | "protobuf") || (s.len() > 2 && s.ends_with("pb")));
    (alias.len() > 2 && alias.ends_with("pb")) || proto_segment
}

/// A service name: starts upper-case, no `_` (stream types such as
/// `Greeter_ChatClient` are not services).
fn is_service_name(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase) && !s.contains('_')
}

/// Parameters of a handler of a server type: a context and a pointer
/// (`(ctx context.Context, in *pb.Req)`), or a stream (`pb.<S>_<Method>Server`,
/// `grpc.BidiStreamingServer[…]`) — as the resolver's service markers
/// require.
fn handler_params(types: &[String], method: &str) -> bool {
    let unary = types.iter().any(|t| t == "context.Context") && types.iter().any(|t| t.starts_with('*'));
    let suffix = format!("_{method}Server");
    unary || types.iter().any(|t| t.ends_with(&suffix) || t.contains("StreamingServer["))
}

/// `S` of `Unimplemented<S>Server` / `Unsafe<S>Server`.
fn server_service(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Unimplemented").or_else(|| name.strip_prefix("Unsafe"))?;
    rest.strip_suffix("Server").filter(|s| is_service_name(s))
}

/// The type name of a field type without pointer or package (`pb.X` → `X`).
fn type_name<'s>(ty: Node<'_>, src: &'s [u8]) -> &'s str {
    let t = text(ty, src).trim_start_matches('*');
    t.rsplit_once('.').map_or(t, |(_, n)| n)
}

/// (qualifier, name) of `pb.Name` or (`""`, name) of `Name`.
fn qualified_name<'s>(func: Node<'_>, src: &'s [u8]) -> Option<(&'s str, &'s str)> {
    match func.kind() {
        "identifier" => Some(("", text(func, src))),
        _ => {
            let (q, f) = selector_parts(func)?;
            (q.kind() == "identifier").then(|| (text(q, src), text(f, src)))
        }
    }
}

/// The type of identifier `arg`: the receiver of the enclosing method
/// (`func (s *T) …`), else a variable of the enclosing function assigned
/// `&T{…}` / `T{…}` / `new(T)` (`s := &T{}`).
fn variable_type<'s>(arg: Node<'_>, src: &'s [u8]) -> Option<&'s str> {
    if arg.kind() != "identifier" {
        return None;
    }
    let name = text(arg, src);
    let func = top_node(arg);
    let receiver = func.child_by_field_name("receiver").and_then(|r| named_children(r).first().copied());
    if let Some(p) = receiver.filter(|p| p.child_by_field_name("name").is_some_and(|n| text(n, src) == name)) {
        return receiver_type(p.parent()?, src);
    }
    assigned_type(func.child_by_field_name("body")?, name, src, 0)
}

/// The type constructed into `name` by an assignment inside `node`.
fn assigned_type<'s>(node: Node<'_>, name: &str, src: &'s [u8], depth: usize) -> Option<&'s str> {
    if depth > 64 {
        return None;
    }
    if matches!(node.kind(), "short_var_declaration" | "assignment_statement") {
        let (left, right) = (node.child_by_field_name("left")?, node.child_by_field_name("right")?);
        let pairs = named_children(left).into_iter().zip(named_children(right));
        return pairs.filter(|(l, _)| text(*l, src) == name).find_map(|(_, r)| constructed_type(r, src));
    }
    named_children(node).into_iter().find_map(|c| assigned_type(c, name, src, depth + 1))
}

/// The type built by `&T{}`, `T{}` or `new(T)`.
fn constructed_type<'s>(node: Node<'_>, src: &'s [u8]) -> Option<&'s str> {
    match node.kind() {
        "unary_expression" => constructed_type(*named_children(node).last()?, src),
        "composite_literal" => Some(type_name(node.child_by_field_name("type")?, src)),
        "call_expression" if selector_call(node, src).is_none() => {
            let f = node.child_by_field_name("function")?;
            let arg = *named_children(node.child_by_field_name("arguments")?).first()?;
            (text(f, src) == "new").then(|| type_name(arg, src))
        }
        _ => None,
    }
}

/// The receiver type name of `func (s *T[K]) …`.
fn receiver_type<'s>(list: Node<'_>, src: &'s [u8]) -> Option<&'s str> {
    let param = *named_children(list).first()?;
    let t = text(param.child_by_field_name("type")?, src).trim_start_matches('*');
    Some(t.split('[').next().unwrap_or(t).trim())
}

/// Parameter types as written, one per parameter name.
fn param_types(params: Node<'_>, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for p in named_children(params) {
        let Some(ty) = p.child_by_field_name("type") else { continue };
        let t: String = text(ty, src).split_whitespace().collect();
        let mut cursor = p.walk();
        let names = p.children_by_field_name("name", &mut cursor).count().max(1);
        out.extend(std::iter::repeat_n(t, names));
    }
    out
}

/// Whether a request type names the method: `<Method>Request`, or the
/// method without a trailing version, the version after `Request` or
/// dropped (`GetConfigurationAlpha1` → `GetConfigurationRequest`,
/// `DeleteBinaryFileAlpha1` → `DeleteBinaryFileRequestAlpha1`).
fn request_matches(request: &str, method: &str) -> bool {
    let base = strip_version(method);
    let version = &method[base.len()..];
    let Some((name, rest)) = request.rsplit_once("Request") else { return false };
    (name == method && rest.is_empty()) || (name == base && (rest.is_empty() || rest == version))
}

/// `XAlpha1` / `XBeta2` / `XV2` → `X`.
fn strip_version(method: &str) -> &str {
    let digits = method.trim_end_matches(|c: char| c.is_ascii_digit());
    ["Alpha", "Beta", "V"].iter().find_map(|s| digits.strip_suffix(s)).unwrap_or(method)
}
