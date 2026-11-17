//! Contract hints of a Java file (RFC 0006 phase 4; keys and matching in
//! `rueca_model::contract`), read from the syntax tree after the
//! definitions are known, with file-local evidence only (the file names the
//! framework's package):
//!
//! - HTTP routes (`http.rs`): Spring `@GetMapping` / `@RequestMapping`,
//!   JAX-RS `@Path` + `@GET`, Micronaut `@Get`, composed with the class's
//!   own prefix; on a client interface (`@FeignClient`,
//!   `@RegisterRestClient`, Micronaut `@Client`) and for Spring HTTP
//!   interfaces (`@GetExchange`) and Feign `@RequestLine("GET /x")` they
//!   are consumers;
//! - HTTP calls (`http.rs`): `RestTemplate`, `WebClient` / `RestClient`,
//!   `java.net.http.HttpRequest`, OkHttp `Request.Builder`;
//! - gRPC (`grpc.rs`), framework `grpc-java`.

mod grpc;
mod http;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use rueca_extract::span_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::truncate_raw;
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, Label, LocalDefId};

/// Longest annotation / call text read for `raw`, in bytes.
const MAX_RAW_SOURCE: usize = 2000;

/// The contract hints of one file.
pub(crate) fn hints(root: Node<'_>, src: &[u8], defs: &[Definition], max_depth: usize) -> Vec<ContractHint> {
    let source = String::from_utf8_lossy(src);
    let mut file = File { src, defs, max_depth, consts: HashMap::new(), hints: Vec::new() };
    let mut walk =
        Walk { http: http::Http::new(&source), rpc: grpc::Grpc::new(source.contains("io.grpc")), classes: Vec::new() };
    if walk.http.any() || walk.rpc.on {
        file.collect_consts(root, 0);
        walk.rpc.collect_fields(&file, root, 0);
        walk.visit(&mut file, root, 0);
    }
    file.hints.sort_by_key(|h| (h.span.start_byte, h.span.end_byte));
    file.hints
}

/// What the enclosing type declaration says.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClassCtx {
    /// Path prefixes of the class (`@RequestMapping("/api")`, `@Path`).
    prefixes: Vec<String>,
    /// Framework of a client interface / class (`feign`, …): its mappings
    /// are consumers.
    client: Option<&'static str>,
    /// A plain interface: its Spring / JAX-RS mappings are not emitted.
    interface: bool,
    /// The gRPC service a server class implements.
    grpc_service: Option<String>,
}

/// The walk over the file.
struct Walk {
    http: http::Http,
    rpc: grpc::Grpc,
    classes: Vec<ClassCtx>,
}

impl Walk {
    fn visit(&mut self, file: &mut File<'_>, node: Node<'_>, depth: usize) {
        if depth > file.max_depth {
            return;
        }
        let saved = self.rpc.scope;
        let class = matches!(
            node.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration"
        );
        match node.kind() {
            _ if class => {
                let mut ctx = self.http.class_ctx(file, node);
                ctx.grpc_service = self.rpc.server_service(file, node);
                self.classes.push(ctx);
            }
            "method_declaration" => self.method(file, node),
            "constructor_declaration" => self.rpc.scope = node.start_byte(),
            "method_invocation" => {
                self.http.invocation(file, node);
                self.rpc.call(file, node);
            }
            "local_variable_declaration" | "formal_parameter" => self.rpc.bind(file, node),
            _ => {}
        }
        for child in named_children(node) {
            self.visit(file, child, depth + 1);
        }
        if class {
            self.classes.pop();
        }
        self.rpc.scope = saved;
    }

    fn method(&mut self, file: &mut File<'_>, node: Node<'_>) {
        self.rpc.scope = node.start_byte();
        let ctx = self.classes.last().cloned().unwrap_or_default();
        self.http.mappings(file, node, &ctx);
        if let Some(svc) = &ctx.grpc_service {
            self.rpc.provider(file, node, svc);
        }
    }
}

/// UTF-8 text of a node (empty when not valid UTF-8).
fn text<'s>(node: Node<'_>, src: &'s [u8]) -> &'s str {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// Named children, comments left out.
fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|c| !matches!(c.kind(), "line_comment" | "block_comment")).collect()
}

/// The value of a string literal without escapes.
fn string_value(node: Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let inner = named_children(node);
    if inner.iter().any(|c| !matches!(c.kind(), "string_fragment" | "multiline_string_fragment")) {
        return None;
    }
    Some(inner.iter().map(|c| text(*c, src)).collect())
}

/// The last `.` segment of a name, generics dropped (`a.b.C<T>` → `C`).
fn last_segment(name: &str) -> &str {
    let head = name.split('<').next().unwrap_or(name).trim();
    head.rsplit('.').next().unwrap_or(head)
}

/// `sayHello` → `SayHello` (grpc-java's method names back to the proto's).
fn pascal(camel: &str) -> String {
    let mut c = camel.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

/// What the pack knows about one file while emitting hints.
pub(crate) struct File<'s> {
    src: &'s [u8],
    defs: &'s [Definition],
    max_depth: usize,
    /// `static final String X = "…"` fields of the file.
    consts: HashMap<String, String>,
    hints: Vec<ContractHint>,
}

impl<'s> File<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.src)
    }

    /// `static final String` fields initialized with a literal.
    fn collect_consts(&mut self, node: Node<'_>, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        if node.kind() == "field_declaration" && self.text(node).contains("final") {
            let mut cursor = node.walk();
            let decls: Vec<Node<'_>> = node.children_by_field_name("declarator", &mut cursor).collect();
            for d in decls {
                if let (Some(name), Some(v)) = (
                    d.child_by_field_name("name"),
                    d.child_by_field_name("value").and_then(|v| string_value(v, self.src)),
                ) {
                    self.consts.insert(self.text(name).to_string(), v);
                }
            }
        }
        for child in named_children(node) {
            self.collect_consts(child, depth + 1);
        }
    }

    /// A static string: a literal, a constant of the file, `+` of those.
    fn static_string(&self, node: Node<'_>) -> Option<String> {
        match node.kind() {
            "identifier" => self.consts.get(self.text(node)).cloned(),
            "field_access" => self.consts.get(self.text(node.child_by_field_name("field")?)).cloned(),
            "binary_expression" if node.child_by_field_name("operator").is_some_and(|o| self.text(o) == "+") => {
                let l = self.static_string(node.child_by_field_name("left")?)?;
                Some(l + &self.static_string(node.child_by_field_name("right")?)?)
            }
            "parenthesized_expression" => self.static_string(*named_children(node).first()?),
            _ => string_value(node, self.src),
        }
    }

    /// The innermost definition containing `node`.
    fn enclosing(&self, node: Node<'_>) -> Option<LocalDefId> {
        let (start, end) = (node.start_byte() as u32, node.end_byte() as u32);
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.span.start_byte <= start && end <= d.span.end_byte)
            .min_by_key(|(_, d)| d.span.end_byte - d.span.start_byte)
            .map(|(i, _)| i as LocalDefId)
    }

    /// The method definition whose name identifier is `name_node`.
    fn def_at(&self, name_node: Node<'_>) -> Option<LocalDefId> {
        let start = name_node.start_byte() as u32;
        self.defs
            .iter()
            .position(|d| d.name_span.start_byte == start && d.label == Label::Method)
            .map(|i| i as LocalDefId)
    }

    /// The text of `node`, whitespace collapsed, at most `MAX_RAW_CHARS`.
    fn raw(&self, node: Node<'_>) -> String {
        let end = node.end_byte().min(node.start_byte() + MAX_RAW_SOURCE);
        let t = String::from_utf8_lossy(&self.src[node.start_byte()..end]);
        truncate_raw(&t.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    fn push(
        &mut self,
        side: ContractSide,
        kind: ContractKind,
        key: String,
        evidence: Node<'_>,
        owner: Option<LocalDefId>,
        framework: &str,
    ) {
        let hint = ContractHint {
            side,
            kind,
            key,
            raw: self.raw(evidence),
            span: span_of(evidence),
            owner,
            framework: framework.to_string(),
        };
        if !self.hints.contains(&hint) {
            self.hints.push(hint);
        }
    }
}
