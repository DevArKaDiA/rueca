//! HTTP route registrations (providers) and HTTP calls (consumers) of a Go
//! file, in one walk that also hands every other method call to the gRPC
//! consumer rules.
//!
//! Providers need the router's import in the file; the path must be
//! static (literals, package-level string constants, `+`), and group /
//! sub-router prefixes are composed when the prefix is built in the same
//! file (`g := r.Group("/v1")`, `r.PathPrefix("/api").Subrouter()`, chi
//! `r.Route("/users", func(r chi.Router) { … })`). The owner is the handler
//! when it is a function or method of this file (the last function value
//! among the arguments, looking inside wrapping calls), else the
//! registering definition.

use std::collections::HashMap;

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{HTTP_METHODS, Part, http_key_from_parts};
use rueca_model::{ContractKind, ContractSide, LocalDefId};

use super::grpc::Bindings;
use super::{File, named_children, string_value};

/// gin / echo route methods (`r.GET("/x", h)`); `Any` is every method.
const UPPER_VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT", "TRACE", "Any"];

/// chi / fiber route methods (`r.Get("/x", h)`), resty request methods;
/// `All` is every method.
const PASCAL_VERBS: &[&str] = &["Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "Connect", "Trace", "All"];

/// The routers a file imports, and the names it binds helper packages to.
struct Frameworks {
    gin: bool,
    echo: bool,
    chi: bool,
    gorilla: bool,
    fiber: bool,
    resty: bool,
    /// Binding name of `net/http`.
    http: Option<String>,
    /// Binding name of `net/http/httptest` (in-process requests: skipped).
    httptest: Option<String>,
    /// Binding name of `fmt`.
    fmt: Option<String>,
}

impl Frameworks {
    fn of(file: &File<'_>) -> Frameworks {
        Frameworks {
            gin: file.imports_path("github.com/gin-gonic/gin"),
            echo: file.imports_path("github.com/labstack/echo"),
            chi: file.imports_path("github.com/go-chi/chi"),
            gorilla: file.imports_path("github.com/gorilla/mux"),
            fiber: file.imports_path("github.com/gofiber/fiber"),
            resty: file.imports_path("github.com/go-resty/resty") || file.imports_path("resty.dev/"),
            http: file.alias("net/http", false).map(str::to_string),
            httptest: file.alias("net/http/httptest", false).map(str::to_string),
            fmt: file.alias("fmt", false).map(str::to_string),
        }
    }

    /// Framework of a `HandleFunc` / `Handle` registration.
    fn mux(&self) -> Option<&'static str> {
        if self.gorilla {
            Some("gorilla")
        } else if self.chi {
            Some("chi")
        } else {
            self.http.as_ref().map(|_| "net/http")
        }
    }

    fn gin_or_echo(&self) -> &'static str {
        if self.gin { "gin" } else { "echo" }
    }

    fn chi_or_fiber(&self) -> &'static str {
        if self.chi { "chi" } else { "fiber" }
    }
}

/// A route registration: its methods (`None` = any), static path, the
/// arguments that may hold the handler, and the framework.
struct Route<'t> {
    methods: Vec<Option<String>>,
    path: String,
    handler_args: Vec<Node<'t>>,
    framework: &'static str,
    /// The call to report (gorilla: the outer `.Methods(…)` call).
    evidence: Node<'t>,
}

/// A piece of a URL expression (owned [`Part`]).
enum Piece {
    Lit(String),
    Hole,
}

/// The walk over a file's calls.
pub(crate) struct Walk<'a, 's> {
    file: &'a mut File<'s>,
    rpc: &'a Bindings,
    fw: Frameworks,
    /// Router variable → path prefix (group, sub-router, chi `Route`).
    prefixes: HashMap<String, String>,
}

impl<'a, 's> Walk<'a, 's> {
    pub(crate) fn new(file: &'a mut File<'s>, rpc: &'a Bindings) -> Walk<'a, 's> {
        let fw = Frameworks::of(file);
        Walk { file, rpc, fw, prefixes: HashMap::new() }
    }

    /// Walks every top-level declaration; prefixes do not cross functions.
    pub(crate) fn run(&mut self, root: Node<'_>) {
        for decl in named_children(root) {
            self.prefixes.clear();
            self.visit(decl, 1);
        }
    }

    fn visit(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.file.max_depth {
            return;
        }
        let scope = match node.kind() {
            "call_expression" => {
                self.call(node);
                self.closure_scope(node)
            }
            "short_var_declaration" | "assignment_statement" => {
                self.record_groups(node);
                None
            }
            _ => None,
        };
        for child in named_children(node) {
            self.visit(child, depth + 1);
        }
        if let Some((name, old)) = scope {
            match old {
                Some(p) => self.prefixes.insert(name, p),
                None => self.prefixes.remove(&name),
            };
        }
    }

    fn text(&self, node: Node<'_>) -> &'s str {
        self.file.text(node)
    }

    // -- prefixes ------------------------------------------------------------

    /// chi `r.Route("/p", func(r chi.Router) { … })` and `r.Group(func(r
    /// chi.Router) { … })`: the closure's router parameter gets the prefix
    /// while its body is walked. Returns the binding to restore.
    fn closure_scope(&mut self, call: Node<'_>) -> Option<(String, Option<String>)> {
        let (recv, m, args) = selector_call(call, self.file.src)?;
        let (prefix, closure) = match (m, args.as_slice()) {
            ("Route", [p, f]) => (format!("{}{}", self.prefix_of(recv), self.path_value(*p)?), *f),
            ("Group", [f]) => (self.prefix_of(recv), *f),
            _ => return None,
        };
        let param = first_param_name(closure, self.file.src)?;
        let old = self.prefixes.insert(param.clone(), prefix);
        Some((param, old))
    }

    /// `g := r.Group("/v1")`, `api := r.PathPrefix("/api").Subrouter()`.
    fn record_groups(&mut self, node: Node<'_>) {
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
            return;
        };
        for (l, r) in named_children(left).into_iter().zip(named_children(right)) {
            if l.kind() == "identifier"
                && let Some(p) = self.group_prefix(r)
            {
                self.prefixes.insert(self.text(l).to_string(), p);
            }
        }
    }

    /// The prefix of a router expression: a recorded variable, a group /
    /// sub-router call, else none.
    fn prefix_of(&self, recv: Node<'_>) -> String {
        match recv.kind() {
            "identifier" => self.prefixes.get(self.text(recv)).cloned().unwrap_or_default(),
            "call_expression" => self.group_prefix(recv).unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// `X.Group("/p")`, `X.PathPrefix("/p")`, `X.PathPrefix("/p").Subrouter()`.
    fn group_prefix(&self, call: Node<'_>) -> Option<String> {
        let (recv, m, args) = selector_call(call, self.file.src)?;
        match m {
            "Group" | "PathPrefix" => Some(format!("{}{}", self.prefix_of(recv), self.path_value(*args.first()?)?)),
            "Subrouter" if recv.kind() == "call_expression" => self.group_prefix(recv),
            _ => None,
        }
    }

    /// A static path: a literal, a package-level string constant, or a `+`
    /// of static pieces.
    fn path_value(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "identifier" => self.file.consts.get(self.text(node)).cloned(),
            "binary_expression" if self.is_plus(node) => {
                let l = self.path_value(node.child_by_field_name("left")?)?;
                Some(l + &self.path_value(node.child_by_field_name("right")?)?)
            }
            "parenthesized_expression" => self.path_value(*named_children(node).first()?),
            _ => string_value(node, self.file.src),
        }
    }

    fn is_plus(&self, node: Node<'_>) -> bool {
        node.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+")
    }

    // -- calls ---------------------------------------------------------------

    fn call(&mut self, call: Node<'_>) {
        let Some((recv, m, args)) = selector_call(call, self.file.src) else { return };
        if let Some(route) = self.route(call, recv, m, &args) {
            self.provide(call, recv, route);
        } else if !self.consume(call, recv, m, &args) {
            self.rpc.consumer(self.file, call, recv, m, &args);
        }
    }

    fn provide(&mut self, call: Node<'_>, recv: Node<'_>, route: Route<'_>) {
        let prefix = self.prefix_of(recv);
        if !route.path.starts_with('/') && (prefix.is_empty() || !route.path.is_empty()) {
            return;
        }
        let path = format!("{prefix}{}", route.path);
        let owner = self.handler_owner(&route.handler_args).or_else(|| self.file.enclosing(call));
        for method in &route.methods {
            let Some(key) = http_key_from_parts(method.as_deref(), &[Part::Lit(&path)]) else { continue };
            self.file.push(ContractSide::Provides, ContractKind::Http, key, route.evidence, owner, route.framework);
        }
    }

    /// The route a call registers, if it is one.
    fn route<'t>(&self, call: Node<'t>, recv: Node<'t>, m: &str, args: &[Node<'t>]) -> Option<Route<'t>> {
        if UPPER_VERBS.contains(&m) && (self.fw.gin || self.fw.echo) {
            let method = (m != "Any").then(|| m.to_string());
            return self.simple(call, vec![method], args, 0, self.fw.gin_or_echo());
        }
        if PASCAL_VERBS.contains(&m)
            && (self.fw.chi || self.fw.fiber)
            && !self.is_http(recv)
            && !self.is_client_call(recv)
        {
            let method = (m != "All").then(|| m.to_string());
            return self.simple(call, vec![method], args, 0, self.fw.chi_or_fiber());
        }
        match m {
            "Handle" | "Add" | "Method" | "MethodFunc" => self
                .method_first(call, m, args)
                .or_else(|| (m == "Handle").then(|| self.mux_route(call, args)).flatten()),
            "Match" if self.fw.gin || self.fw.echo => self.match_route(call, args),
            "HandleFunc" => self.mux_route(call, args),
            _ => None,
        }
    }

    /// `r.GET("/x", h)`: path at `at`, handler after it.
    fn simple<'t>(
        &self,
        call: Node<'t>,
        methods: Vec<Option<String>>,
        args: &[Node<'t>],
        at: usize,
        framework: &'static str,
    ) -> Option<Route<'t>> {
        let path = self.path_value(*args.get(at)?)?;
        Some(Route { methods, path, handler_args: args[at + 1..].to_vec(), framework, evidence: call })
    }

    /// gin `Handle("GET", "/x", h)`, echo `Add`, chi `Method` / `MethodFunc`.
    fn method_first<'t>(&self, call: Node<'t>, m: &str, args: &[Node<'t>]) -> Option<Route<'t>> {
        let framework = match m {
            "Handle" if self.fw.gin => "gin",
            "Add" if self.fw.echo => "echo",
            "Method" | "MethodFunc" if self.fw.chi => "chi",
            _ => return None,
        };
        let method = string_value(*args.first()?, self.file.src).filter(|v| is_http_method(v))?;
        self.simple(call, vec![Some(method)], args, 1, framework)
    }

    /// gin / echo `Match([]string{"GET", "POST"}, "/x", h)`.
    fn match_route<'t>(&self, call: Node<'t>, args: &[Node<'t>]) -> Option<Route<'t>> {
        let methods: Vec<Option<String>> = self.method_list(*args.first()?).into_iter().map(Some).collect();
        if methods.is_empty() {
            return None;
        }
        self.simple(call, methods, args, 1, self.fw.gin_or_echo())
    }

    /// `HandleFunc("/x", h)` / `Handle("GET /x", h)` (net/http, Go 1.22
    /// method patterns), gorilla's `.Methods("GET", "POST")` on it, chi.
    fn mux_route<'t>(&self, call: Node<'t>, args: &[Node<'t>]) -> Option<Route<'t>> {
        let framework = self.fw.mux()?;
        let mut route = self.simple(call, vec![None], args, 0, framework)?;
        if let Some((method, path)) = route.path.split_once(' ')
            && is_http_method(method)
        {
            route.methods = vec![Some(method.to_string())];
            route.path = path.trim_start().to_string();
        }
        if let Some((outer, methods)) = self.gorilla_methods(call) {
            route.methods = methods.into_iter().map(Some).collect();
            route.evidence = outer;
        }
        Some(route)
    }

    /// `X.HandleFunc(…).Methods("GET", http.MethodPost)`: the outer call and
    /// its methods.
    fn gorilla_methods<'t>(&self, call: Node<'t>) -> Option<(Node<'t>, Vec<String>)> {
        let sel = call.parent().filter(|p| p.kind() == "selector_expression")?;
        let field = sel.child_by_field_name("field")?;
        let outer = sel.parent().filter(|p| p.kind() == "call_expression")?;
        if self.text(field) != "Methods" {
            return None;
        }
        let args = named_children(outer.child_by_field_name("arguments")?);
        let methods: Vec<String> = args.iter().filter_map(|a| self.method_of(*a)).collect();
        (!methods.is_empty()).then_some((outer, methods))
    }

    /// `[]string{"GET", http.MethodPost}`.
    fn method_list(&self, node: Node<'_>) -> Vec<String> {
        let Some(body) = node.child_by_field_name("body") else { return Vec::new() };
        let elems = named_children(body).into_iter().flat_map(named_children);
        elems.filter_map(|e| self.method_of(e)).collect()
    }

    /// An HTTP method argument: an upper-case method literal or
    /// `http.MethodPost`.
    fn method_of(&self, node: Node<'_>) -> Option<String> {
        if let Some(v) = string_value(node, self.file.src) {
            return is_http_method(&v).then_some(v);
        }
        let (q, f) = selector_parts(node)?;
        let name = self.text(f).strip_prefix("Method")?;
        let upper = name.to_ascii_uppercase();
        (self.is_http(q) && HTTP_METHODS.contains(&upper.as_str())).then_some(upper)
    }

    // -- handlers ------------------------------------------------------------

    /// The handler among a registration's arguments: the last function or
    /// method of this file named by an argument (looking inside wrapping
    /// calls such as `s.withLogging("/x", s.Handler)`).
    fn handler_owner(&self, args: &[Node<'_>]) -> Option<LocalDefId> {
        args.iter().rev().find_map(|a| self.handler_in(*a, 0))
    }

    fn handler_in(&self, node: Node<'_>, depth: usize) -> Option<LocalDefId> {
        if depth > 8 {
            return None;
        }
        match node.kind() {
            "identifier" => self.file.callable_named(self.text(node)),
            "selector_expression" => self.file.callable_named(self.text(node.child_by_field_name("field")?)),
            "variadic_argument" | "parenthesized_expression" | "unary_expression" => {
                self.handler_in(*named_children(node).last()?, depth + 1)
            }
            "call_expression" => {
                let args = named_children(node.child_by_field_name("arguments")?);
                args.iter().rev().find_map(|a| self.handler_in(*a, depth + 1))
            }
            _ => None,
        }
    }

    // -- consumers -----------------------------------------------------------

    /// Emits a consumer hint when the call is an HTTP request.
    fn consume(&mut self, call: Node<'_>, recv: Node<'_>, m: &str, args: &[Node<'_>]) -> bool {
        let Some((method, url, framework)) = self.request(recv, m, args) else { return false };
        let Some(key) = self.url_key(method.as_deref(), url) else { return false };
        let owner = self.file.enclosing(call);
        self.file.push(ContractSide::Consumes, ContractKind::Http, key, call, owner, framework);
        true
    }

    /// (method, URL argument, framework) of an HTTP request call.
    fn request<'t>(
        &self,
        recv: Node<'t>,
        m: &str,
        args: &[Node<'t>],
    ) -> Option<(Option<String>, Node<'t>, &'static str)> {
        if self.is_http(recv) {
            return self.net_http_call(m, args);
        }
        if self.fw.httptest.as_deref() == Some(self.text(recv)) {
            return None;
        }
        self.client_call(recv, m, args).or_else(|| self.wrapper_call(args))
    }

    /// `http.Get(url)`, `http.Post(url, …)`, `http.NewRequest(m, url, …)`,
    /// `http.NewRequestWithContext(ctx, m, url, …)`.
    fn net_http_call<'t>(&self, m: &str, args: &[Node<'t>]) -> Option<(Option<String>, Node<'t>, &'static str)> {
        let (method, url) = match m {
            "Get" | "Head" => (Some(m.to_ascii_uppercase()), *args.first()?),
            "Post" | "PostForm" => (Some("POST".to_string()), *args.first()?),
            "NewRequest" => (self.method_or_star(*args.first()?), *args.get(1)?),
            "NewRequestWithContext" => (self.method_or_star(*args.get(1)?), *args.get(2)?),
            _ => return None,
        };
        Some((method, url, "net/http"))
    }

    /// A method argument as written: a literal, `http.MethodX`, else `*`.
    fn method_or_star(&self, node: Node<'_>) -> Option<String> {
        string_value(node, self.file.src).or_else(|| self.method_of(node))
    }

    /// `client.Get(url)` on an `http.Client`-like receiver, resty
    /// `client.R().Post("/x")` / `.Execute("GET", "/x")`.
    fn client_call<'t>(
        &self,
        recv: Node<'t>,
        m: &str,
        args: &[Node<'t>],
    ) -> Option<(Option<String>, Node<'t>, &'static str)> {
        let first = *args.first()?;
        if self.fw.resty && self.text(recv).contains("R()") {
            return match m {
                "Execute" => Some((self.method_or_star(first), *args.get(1)?, "resty")),
                _ if PASCAL_VERBS.contains(&m) && m != "All" => Some((Some(m.to_ascii_uppercase()), first, "resty")),
                _ => None,
            };
        }
        let method = match m {
            "Get" | "Head" => m.to_ascii_uppercase(),
            "Post" | "PostForm" => "POST".to_string(),
            _ => return None,
        };
        (self.fw.http.is_some() && self.is_client_call(recv)).then_some((Some(method), first, "net/http"))
    }

    /// An SDK wrapper called with a method and then a URL:
    /// `c.do(ctx, http.MethodPost, "/api/generate", …)`.
    fn wrapper_call<'t>(&self, args: &[Node<'t>]) -> Option<(Option<String>, Node<'t>, &'static str)> {
        let i = args.iter().position(|a| self.method_of(*a).is_some())?;
        let url = *args.get(i + 1)?;
        Some((self.method_of(args[i]), url, "http-client"))
    }

    /// Whether the receiver looks like an HTTP client (`http.DefaultClient`,
    /// a name containing `client`) or a resty request chain.
    fn is_client_call(&self, recv: Node<'_>) -> bool {
        let last = match recv.kind() {
            "selector_expression" => recv.child_by_field_name("field").map_or("", |f| self.text(f)),
            "identifier" => self.text(recv),
            _ => return self.text(recv).contains("R()"),
        };
        last.to_ascii_lowercase().contains("client")
    }

    fn is_http(&self, node: Node<'_>) -> bool {
        node.kind() == "identifier" && self.fw.http.as_deref() == Some(self.text(node))
    }

    /// The canonical key of a URL expression, when a literal piece of it
    /// holds a `/`.
    fn url_key(&self, method: Option<&str>, url: Node<'_>) -> Option<String> {
        let mut pieces = Vec::new();
        self.url_pieces(url, &mut pieces, 0);
        if !has_path_literal(&pieces) {
            return None;
        }
        let parts: Vec<Part<'_>> = pieces
            .iter()
            .map(|p| match p {
                Piece::Lit(s) => Part::Lit(s),
                Piece::Hole => Part::Hole,
            })
            .collect();
        http_key_from_parts(method, &parts)
    }

    /// Literal pieces and holes of a URL expression: literals, constants,
    /// `+`, `fmt.Sprintf("%s/api/%s", …)`.
    fn url_pieces(&self, node: Node<'_>, out: &mut Vec<Piece>, depth: usize) {
        if depth > 16 {
            return out.push(Piece::Hole);
        }
        if let Some(v) = self.path_value(node) {
            return out.push(Piece::Lit(v));
        }
        match node.kind() {
            "binary_expression" if self.is_plus(node) => {
                for side in ["left", "right"] {
                    match node.child_by_field_name(side) {
                        Some(s) => self.url_pieces(s, out, depth + 1),
                        None => out.push(Piece::Hole),
                    }
                }
            }
            "call_expression" => out.push(self.sprintf(node).map_or(Piece::Hole, Piece::Lit)),
            _ => out.push(Piece::Hole),
        }
    }

    /// The format of `fmt.Sprintf("…", …)` (its verbs are holes).
    fn sprintf(&self, call: Node<'_>) -> Option<String> {
        let (recv, m, args) = selector_call(call, self.file.src)?;
        let is_fmt = recv.kind() == "identifier" && self.fw.fmt.as_deref() == Some(self.text(recv));
        (is_fmt && m == "Sprintf").then(|| string_value(*args.first()?, self.file.src)).flatten()
    }
}

/// `recv.M(args)`: the receiver expression, `M` and the arguments.
pub(super) fn selector_call<'t, 's>(call: Node<'t>, src: &'s [u8]) -> Option<(Node<'t>, &'s str, Vec<Node<'t>>)> {
    let func = call.child_by_field_name("function")?;
    let (recv, field) = selector_parts(func)?;
    let args = named_children(call.child_by_field_name("arguments")?);
    Some((recv, super::text(field, src), args))
}

/// Operand and field of a selector expression.
pub(super) fn selector_parts(node: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    if node.kind() != "selector_expression" {
        return None;
    }
    Some((node.child_by_field_name("operand")?, node.child_by_field_name("field")?))
}

/// The name of a function literal's first parameter.
fn first_param_name(closure: Node<'_>, src: &[u8]) -> Option<String> {
    if closure.kind() != "func_literal" {
        return None;
    }
    let params = named_children(closure.child_by_field_name("parameters")?);
    let name = params.first()?.child_by_field_name("name")?;
    Some(super::text(name, src).to_string())
}

/// An upper-case HTTP method name.
fn is_http_method(s: &str) -> bool {
    HTTP_METHODS.contains(&s) || s == "CONNECT"
}

/// Whether a literal piece holds a `/` of the path (not only the
/// `scheme://` of a URL whose host and path are variables).
fn has_path_literal(pieces: &[Piece]) -> bool {
    pieces.iter().any(|p| match p {
        Piece::Lit(s) => s.split_once("://").map_or(s.as_str(), |(_, rest)| rest).contains('/'),
        Piece::Hole => false,
    })
}
