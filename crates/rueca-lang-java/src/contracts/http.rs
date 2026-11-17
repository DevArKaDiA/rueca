//! HTTP routes and calls of a Java file.
//!
//! Routes (annotations; the class's own prefix composed):
//! - Spring (`org.springframework` named in the file): `@GetMapping`,
//!   `@PostMapping`, `@PutMapping`, `@DeleteMapping`, `@PatchMapping`,
//!   `@RequestMapping(value = "/x", method = RequestMethod.GET)` (no
//!   method: any), paths from `value` / `path` (arrays: one hint each),
//!   class prefix from `@RequestMapping`;
//! - JAX-RS (`javax.ws.rs` / `jakarta.ws.rs`): `@GET` / `@POST` / … with
//!   the method's `@Path` (none: the class path), class `@Path` prefix;
//! - Micronaut (`io.micronaut`): `@Get("/x")`, …, class `@Controller("/p")`.
//!
//! Mappings are providers, owned by their method, except in a client type
//! (`@FeignClient(path = "/p")`, MicroProfile `@RegisterRestClient`,
//! Micronaut `@Client`, Spring `@HttpExchange`): consumers. Spring HTTP
//! interface methods (`@GetExchange("/x")`) and Feign `@RequestLine("GET
//! /x")` are always consumers. Mappings on a plain interface (shared by
//! controllers in other files) are not emitted.
//!
//! Calls (the outermost call of a chain is read once; owner: the enclosing
//! definition):
//! - `RestTemplate` (named in the file): `getForObject` / `getForEntity`,
//!   `postForObject` / `postForEntity` / `postForLocation`,
//!   `patchForObject`, `headForHeaders`, `optionsForAllow`, and `put`,
//!   `delete`, `exchange(url, HttpMethod.X, …)` on a receiver named like a
//!   template (`rest`, `template`);
//! - `WebClient` / `RestClient`: `.get().uri("/x")`, `.method(HttpMethod.X).uri(…)`;
//! - `java.net.http`: `HttpRequest.newBuilder(URI.create("…"))` /
//!   `.uri(…)` with `.GET()` / `.POST(…)` / `.method("X", …)` (default GET);
//! - OkHttp (`okhttp3`): `new Request.Builder().url("…")` with `.post(…)`
//!   / … / `.method("X", …)` (default GET).
//!
//! URLs: literals, constants of the file, `+`, `URI.create(…)`,
//! `String.format("%s/x", …)`; `{id}` templates are parameters.

use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{HTTP_METHODS, Part, http_key_from_parts};
use rueca_model::{ContractKind, ContractSide};

use super::{ClassCtx, File, last_segment, named_children, string_value, text};

/// Spring `@<Verb>Mapping`, Spring `@<Verb>Exchange`, Micronaut `@<Verb>`
/// annotation prefixes.
const PASCAL_VERBS: &[&str] = &["Get", "Post", "Put", "Delete", "Patch", "Head", "Options", "Trace"];

/// RestTemplate methods with a fixed HTTP method.
const REST_TEMPLATE: &[(&str, &str)] = &[
    ("getForObject", "GET"),
    ("getForEntity", "GET"),
    ("postForObject", "POST"),
    ("postForEntity", "POST"),
    ("postForLocation", "POST"),
    ("patchForObject", "PATCH"),
    ("headForHeaders", "HEAD"),
    ("optionsForAllow", "OPTIONS"),
    ("put", "PUT"),
    ("delete", "DELETE"),
];

/// A route mapping read from annotations.
struct Mapping<'t> {
    methods: Vec<Option<String>>,
    paths: Vec<String>,
    /// Forced consumer (Spring HTTP interface, Feign `@RequestLine`).
    consumer: Option<&'static str>,
    framework: &'static str,
    anchor: Node<'t>,
}

/// A piece of a URL expression (owned [`Part`]).
enum Piece {
    Lit(String),
    Hole,
}

/// Which frameworks the file names.
pub(crate) struct Http {
    spring: bool,
    jaxrs: bool,
    micronaut: bool,
    feign: bool,
    microprofile: bool,
    rest_template: bool,
    web_client: bool,
    jdk: bool,
    okhttp: bool,
}

impl Http {
    pub(crate) fn new(source: &str) -> Http {
        Http {
            spring: source.contains("org.springframework"),
            jaxrs: source.contains("javax.ws.rs") || source.contains("jakarta.ws.rs"),
            micronaut: source.contains("io.micronaut"),
            feign: source.contains("feign"),
            microprofile: source.contains("org.eclipse.microprofile.rest.client"),
            rest_template: source.contains("RestTemplate"),
            web_client: source.contains("WebClient") || source.contains("RestClient"),
            jdk: source.contains("java.net.http"),
            okhttp: source.contains("okhttp3"),
        }
    }

    pub(crate) fn any(&self) -> bool {
        self.spring || self.jaxrs || self.micronaut || self.feign || self.microprofile || self.jdk || self.okhttp
    }

    // -- type annotations ----------------------------------------------------

    /// Prefixes and client role of a type declaration.
    pub(crate) fn class_ctx(&self, file: &File<'_>, decl: Node<'_>) -> ClassCtx {
        let mut ctx = ClassCtx { interface: decl.kind() == "interface_declaration", ..ClassCtx::default() };
        for a in annotations(decl) {
            let (prefixes, client) = self.class_annotation(file, a);
            ctx.prefixes.extend(prefixes);
            ctx.client = ctx.client.or(client);
        }
        if ctx.prefixes.is_empty() {
            ctx.prefixes.push(String::new());
        }
        ctx
    }

    /// (prefixes, client framework) of one type annotation.
    fn class_annotation(&self, file: &File<'_>, a: Node<'_>) -> (Vec<String>, Option<&'static str>) {
        match annotation_name(a, file.src) {
            "RequestMapping" if self.spring => (values(file, a, &["value", "path"]), None),
            "HttpExchange" if self.spring => (values(file, a, &["value", "url"]), Some("spring")),
            "Path" if self.jaxrs => (values(file, a, &["value"]), None),
            "Controller" if self.micronaut => (values(file, a, &["value"]), None),
            "Client" if self.micronaut => (Vec::new(), Some("micronaut")),
            "FeignClient" if self.feign => (values(file, a, &["path"]), Some("feign")),
            "RegisterRestClient" if self.microprofile => (Vec::new(), Some("microprofile")),
            _ => (Vec::new(), None),
        }
    }

    // -- method annotations --------------------------------------------------

    /// The route hints of a method's annotations.
    pub(crate) fn mappings(&self, file: &mut File<'_>, method: Node<'_>, ctx: &ClassCtx) {
        let Some(name) = method.child_by_field_name("name") else { return };
        let owner = file.def_at(name);
        let anns = annotations(method);
        let mut found: Vec<Mapping<'_>> = anns.iter().filter_map(|a| self.mapping(file, *a)).collect();
        found.extend(self.jaxrs_mapping(file, &anns));
        for m in found {
            let (side, framework) = match (m.consumer, ctx.client) {
                (Some(f), _) | (None, Some(f)) => (ContractSide::Consumes, f),
                (None, None) if ctx.interface => continue,
                (None, None) => (ContractSide::Provides, m.framework),
            };
            for key in keys(&ctx.prefixes, &m.paths, &m.methods) {
                file.push(side, ContractKind::Http, key, m.anchor, owner, framework);
            }
        }
    }

    /// One Spring / Micronaut / Feign mapping annotation.
    fn mapping<'t>(&self, file: &File<'_>, a: Node<'t>) -> Option<Mapping<'t>> {
        let name = annotation_name(a, file.src);
        let paths = |keys: &[&str]| non_empty(values(file, a, keys));
        let verb = |suffix: &str| {
            name.strip_suffix(suffix).filter(|v| PASCAL_VERBS.contains(v)).map(|v| v.to_ascii_uppercase())
        };
        let mapping = |methods, paths, consumer, framework| Mapping { methods, paths, consumer, framework, anchor: a };
        if self.spring {
            if let Some(v) = verb("Mapping") {
                return Some(mapping(vec![Some(v)], paths(&["value", "path"]), None, "spring"));
            }
            if name == "RequestMapping" {
                return Some(mapping(request_methods(file, a), paths(&["value", "path"]), None, "spring"));
            }
            if let Some(v) = verb("Exchange") {
                return Some(mapping(vec![Some(v)], paths(&["value", "url"]), Some("spring"), "spring"));
            }
        }
        if self.feign && name == "RequestLine" {
            let line = values(file, a, &["value"]).into_iter().next()?;
            let (method, path) = line.trim().split_once(' ')?;
            return Some(mapping(
                vec![Some(method.to_string())],
                vec![path.trim().to_string()],
                Some("feign"),
                "feign",
            ));
        }
        let micronaut = self.micronaut && PASCAL_VERBS.contains(&name);
        micronaut.then(|| mapping(vec![Some(name.to_ascii_uppercase())], paths(&["value", "uri"]), None, "micronaut"))
    }

    /// JAX-RS: the method's `@GET` / `@POST` / … with its `@Path`.
    fn jaxrs_mapping<'t>(&self, file: &File<'_>, anns: &[Node<'t>]) -> Option<Mapping<'t>> {
        if !self.jaxrs {
            return None;
        }
        let verbs: Vec<(Node<'t>, &str)> = anns
            .iter()
            .map(|a| (*a, annotation_name(*a, file.src)))
            .filter(|(_, n)| HTTP_METHODS.contains(n))
            .collect();
        let anchor = verbs.first()?.0;
        let path = anns.iter().find(|a| annotation_name(**a, file.src) == "Path");
        let paths = non_empty(path.map(|p| values(file, *p, &["value"])).unwrap_or_default());
        let methods = verbs.iter().map(|(_, n)| Some(n.to_string())).collect();
        Some(Mapping { methods, paths, consumer: None, framework: "jaxrs", anchor })
    }

    // -- calls ---------------------------------------------------------------

    /// HTTP calls of a method-call chain, read at its outermost call.
    pub(crate) fn invocation(&self, file: &mut File<'_>, call: Node<'_>) {
        let is_inner = call
            .parent()
            .filter(|p| p.kind() == "method_invocation")
            .and_then(|p| p.child_by_field_name("object"))
            .is_some_and(|o| o == call);
        if is_inner {
            return;
        }
        let chain = chain_of(call);
        let found = self
            .rest_template_call(file, &chain)
            .or_else(|| self.fluent_call(file, &chain))
            .or_else(|| self.jdk_call(file, &chain))
            .or_else(|| self.okhttp_call(file, &chain));
        if let Some((evidence, method, url, framework)) = found {
            consume(file, evidence, method.as_deref(), url, framework);
        }
    }

    /// `restTemplate.getForObject(url, …)`, `exchange(url, HttpMethod.X, …)`.
    fn rest_template_call<'t>(&self, file: &File<'_>, chain: &[Node<'t>]) -> Option<Call<'t>> {
        if !(self.spring && self.rest_template) {
            return None;
        }
        chain.iter().find_map(|c| {
            let (name, args) = (call_name(*c, file.src), call_args(*c));
            let url = *args.first()?;
            let template = c.child_by_field_name("object").is_some_and(|o| {
                let t = file.text(o).to_ascii_lowercase();
                t.contains("rest") || t.contains("template")
            });
            if name == "exchange" && template {
                return Some((*c, args.get(1).map(|m| last_segment(file.text(*m)).to_string()), url, "resttemplate"));
            }
            let (_, method) = REST_TEMPLATE.iter().find(|(n, _)| *n == name)?;
            let generic = matches!(name, "put" | "delete");
            (!generic || template).then(|| (*c, Some(method.to_string()), url, "resttemplate"))
        })
    }

    /// `webClient.get().uri("/x")`, `restClient.method(HttpMethod.X).uri(…)`.
    fn fluent_call<'t>(&self, file: &File<'_>, chain: &[Node<'t>]) -> Option<Call<'t>> {
        if !(self.spring && self.web_client) {
            return None;
        }
        chain.windows(2).find_map(|w| {
            let (uri, verb) = (w[0], w[1]);
            let url = *call_args(uri).first().filter(|_| call_name(uri, file.src) == "uri")?;
            let method = fluent_method(file, verb)?;
            let framework = if file.text(verb).contains("RestClient") { "restclient" } else { "webclient" };
            Some((uri, method, url, framework))
        })
    }

    /// `HttpRequest.newBuilder(URI.create("…")).POST(…)`.
    fn jdk_call<'t>(&self, file: &File<'_>, chain: &[Node<'t>]) -> Option<Call<'t>> {
        if !self.jdk {
            return None;
        }
        let builder = *chain.iter().find(|c| {
            call_name(**c, file.src) == "newBuilder"
                && c.child_by_field_name("object").is_some_and(|o| file.text(o).ends_with("HttpRequest"))
        })?;
        let url = call_args(builder).first().copied().or_else(|| named_arg(file, chain, "uri"))?;
        let method = builder_method(file, chain, &["GET", "POST", "PUT", "DELETE", "HEAD"]);
        Some((builder, method, url, "java.net.http"))
    }

    /// `new Request.Builder().url("…").post(body)`.
    fn okhttp_call<'t>(&self, file: &File<'_>, chain: &[Node<'t>]) -> Option<Call<'t>> {
        if !self.okhttp {
            return None;
        }
        let base = chain.last()?.child_by_field_name("object")?;
        let builder = base.kind() == "object_creation_expression"
            && base.child_by_field_name("type").is_some_and(|t| file.text(t).ends_with("Builder"));
        if !builder {
            return None;
        }
        let url_call = *chain.iter().find(|c| call_name(**c, file.src) == "url")?;
        let url = *call_args(url_call).first()?;
        let method = builder_method(file, chain, &["get", "post", "put", "delete", "patch", "head"]);
        Some((url_call, method, url, "okhttp"))
    }
}

/// (evidence, method, URL argument, framework) of an HTTP call.
type Call<'t> = (Node<'t>, Option<String>, Node<'t>, &'static str);

/// The method of a builder chain: a call named like a method (`GET()`,
/// `post(body)`), `method("PATCH", …)`, else GET.
fn builder_method(file: &File<'_>, chain: &[Node<'_>], names: &[&str]) -> Option<String> {
    let explicit = chain.iter().find_map(|c| {
        let name = call_name(*c, file.src);
        if names.contains(&name) {
            return Some(name.to_ascii_uppercase());
        }
        (name == "method").then(|| string_value(*call_args(*c).first()?, file.src)).flatten()
    });
    Some(explicit.unwrap_or_else(|| "GET".to_string()))
}

/// The first argument of the chain's call named `name`.
fn named_arg<'t>(file: &File<'_>, chain: &[Node<'t>], name: &str) -> Option<Node<'t>> {
    let c = chain.iter().find(|c| call_name(**c, file.src) == name)?;
    call_args(*c).first().copied()
}

/// The method of `get()` / `method(HttpMethod.POST)` before `.uri(…)`.
fn fluent_method(file: &File<'_>, verb: Node<'_>) -> Option<Option<String>> {
    let name = call_name(verb, file.src);
    let args = call_args(verb);
    let upper = name.to_ascii_uppercase();
    if args.is_empty() && HTTP_METHODS.contains(&upper.as_str()) {
        return Some(Some(upper));
    }
    (name == "method" && args.len() == 1).then(|| Some(last_segment(file.text(args[0])).to_string()))
}

/// The calls of a method chain, outermost first.
fn chain_of(top: Node<'_>) -> Vec<Node<'_>> {
    let mut out = vec![top];
    let mut cur = top;
    while let Some(o) = cur.child_by_field_name("object").filter(|o| o.kind() == "method_invocation") {
        if out.len() > 64 {
            break;
        }
        out.push(o);
        cur = o;
    }
    out
}

fn call_name<'s>(call: Node<'_>, src: &'s [u8]) -> &'s str {
    call.child_by_field_name("name").map_or("", |n| text(n, src))
}

fn call_args(call: Node<'_>) -> Vec<Node<'_>> {
    call.child_by_field_name("arguments").map(named_children).unwrap_or_default()
}

/// A consumer hint for an HTTP call.
fn consume(file: &mut File<'_>, evidence: Node<'_>, method: Option<&str>, url: Node<'_>, framework: &str) {
    let mut pieces = Vec::new();
    url_pieces(file, url, &mut pieces, 0);
    if !has_path_literal(&pieces) {
        return;
    }
    let parts: Vec<Part<'_>> = pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(s) => Part::Lit(s),
            Piece::Hole => Part::Hole,
        })
        .collect();
    if let Some(key) = http_key_from_parts(method, &parts) {
        let owner = file.enclosing(evidence);
        file.push(ContractSide::Consumes, ContractKind::Http, key, evidence, owner, framework);
    }
}

/// Literal pieces and holes of a URL expression.
fn url_pieces(file: &File<'_>, node: Node<'_>, out: &mut Vec<Piece>, depth: usize) {
    if depth > 16 {
        return out.push(Piece::Hole);
    }
    if let Some(v) = file.static_string(node) {
        return out.push(Piece::Lit(v));
    }
    match node.kind() {
        "binary_expression" => {
            for side in ["left", "right"] {
                match node.child_by_field_name(side) {
                    Some(s) => url_pieces(file, s, out, depth + 1),
                    None => out.push(Piece::Hole),
                }
            }
        }
        "method_invocation" => {
            let obj = node.child_by_field_name("object").map_or("", |o| file.text(o));
            let first = call_args(node).first().copied();
            match (obj, call_name(node, file.src), first) {
                ("URI", "create", Some(a)) | ("String", "format", Some(a)) => url_pieces(file, a, out, depth + 1),
                _ => out.push(Piece::Hole),
            }
        }
        _ => out.push(Piece::Hole),
    }
}

/// The annotations of a declaration (its `modifiers`).
fn annotations(decl: Node<'_>) -> Vec<Node<'_>> {
    let Some(mods) = named_children(decl).into_iter().find(|c| c.kind() == "modifiers") else { return Vec::new() };
    named_children(mods).into_iter().filter(|a| matches!(a.kind(), "annotation" | "marker_annotation")).collect()
}

/// `GetMapping` of `@GetMapping(…)` / `@org.x.GetMapping`.
fn annotation_name<'s>(a: Node<'_>, src: &'s [u8]) -> &'s str {
    a.child_by_field_name("name").map_or("", |n| last_segment(text(n, src)))
}

/// The static strings of an annotation's elements named in `keys`
/// (`value` also takes the positional argument; arrays give each value).
fn values(file: &File<'_>, a: Node<'_>, keys: &[&str]) -> Vec<String> {
    let Some(args) = a.child_by_field_name("arguments") else { return Vec::new() };
    let mut out = Vec::new();
    for c in named_children(args) {
        let value = match c.kind() {
            "element_value_pair" => c
                .child_by_field_name("key")
                .filter(|k| keys.contains(&file.text(*k)))
                .and_then(|_| c.child_by_field_name("value")),
            _ => keys.contains(&"value").then_some(c),
        };
        if let Some(v) = value {
            out.extend(strings(file, v));
        }
    }
    out
}

/// A static string or an array of them.
fn strings(file: &File<'_>, v: Node<'_>) -> Vec<String> {
    if v.kind() == "element_value_array_initializer" {
        return named_children(v).into_iter().filter_map(|e| file.static_string(e)).collect();
    }
    file.static_string(v).into_iter().collect()
}

/// `method = RequestMethod.GET` / `{RequestMethod.GET, RequestMethod.POST}`
/// of `@RequestMapping` (none: any method).
fn request_methods(file: &File<'_>, a: Node<'_>) -> Vec<Option<String>> {
    let Some(args) = a.child_by_field_name("arguments") else { return vec![None] };
    let pair = named_children(args).into_iter().find(|c| {
        c.kind() == "element_value_pair" && c.child_by_field_name("key").is_some_and(|k| file.text(k) == "method")
    });
    let Some(value) = pair.and_then(|p| p.child_by_field_name("value")) else { return vec![None] };
    let items = if value.kind() == "element_value_array_initializer" { named_children(value) } else { vec![value] };
    let methods: Vec<Option<String>> = items.iter().map(|m| Some(last_segment(file.text(*m)).to_string())).collect();
    if methods.is_empty() { vec![None] } else { methods }
}

/// `[""]` for no paths.
fn non_empty(paths: Vec<String>) -> Vec<String> {
    if paths.is_empty() { vec![String::new()] } else { paths }
}

/// Keys of every prefix × path × method.
fn keys(prefixes: &[String], paths: &[String], methods: &[Option<String>]) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in prefixes {
        for path in paths {
            let full = format!("{}/{}", prefix.trim_end_matches('/'), path.trim_start_matches('/'));
            for m in methods {
                out.extend(http_key_from_parts(m.as_deref(), &[Part::Lit(&full)]));
            }
        }
    }
    out
}

/// Whether a literal piece holds a `/` of the path (not only the
/// `scheme://` of a URL whose host and path are variables).
fn has_path_literal(pieces: &[Piece]) -> bool {
    pieces.iter().any(|p| match p {
        Piece::Lit(s) => s.split_once("://").map_or(s.as_str(), |(_, rest)| rest).contains('/'),
        Piece::Hole => false,
    })
}
