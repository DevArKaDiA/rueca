//! Contract hints of a Rust file (RFC 0006 phase 4; keys and matching in
//! `rueca_model::contract`), read from the syntax tree after the
//! definitions are known, with file-local evidence only (the file names the
//! framework: `actix_web`, `axum`, `rocket`, `reqwest`, `tonic`):
//!
//! - HTTP providers (`http.rs`): actix-web / rocket route attributes
//!   (`#[get("/x")]` on a function), axum `.route("/x", get(h).post(g))`,
//!   actix `.route("/x", web::get().to(h))` and `web::resource("/x")`, with
//!   `web::scope("/p")` and axum `.nest("/p", Router::new()…)` prefixes
//!   built in the same expression or file composed;
//! - HTTP consumers (`http.rs`): reqwest calls;
//! - gRPC providers and consumers (`grpc.rs`), framework `tonic`.
//!
//! Macro arguments are not looked into (route registrations inside a macro
//! are missed).

mod grpc;
mod http;
#[cfg(test)]
mod tests;

use rueca_extract::span_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::truncate_raw;
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, Label, LocalDefId, Span};

/// Longest registration / call text read for `raw`, in bytes.
const MAX_RAW_SOURCE: usize = 2000;

/// The contract hints of one file.
pub(crate) fn hints(root: Node<'_>, src: &[u8], defs: &[Definition], max_depth: usize) -> Vec<ContractHint> {
    let mut file = File { src, defs, max_depth, hints: Vec::new() };
    let source = String::from_utf8_lossy(src);
    let mentions = Mentions {
        actix: source.contains("actix_web"),
        axum: source.contains("axum"),
        rocket: source.contains("rocket"),
        reqwest: source.contains("reqwest"),
        tonic: source.contains("tonic"),
    };
    if mentions.any() {
        let mut walk =
            Walk { http: http::Http::new(mentions, root, src), rpc: grpc::Tonic::new(mentions.tonic, root, src) };
        walk.visit(&mut file, root, 0);
    }
    file.hints.sort_by_key(|h| (h.span.start_byte, h.span.end_byte));
    file.hints
}

/// Which frameworks the file's text names.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mentions {
    actix: bool,
    axum: bool,
    rocket: bool,
    reqwest: bool,
    tonic: bool,
}

impl Mentions {
    fn any(self) -> bool {
        self.actix || self.axum || self.rocket || self.reqwest || self.tonic
    }
}

/// The walk over the file, dispatching nodes to the HTTP and gRPC rules.
struct Walk {
    http: http::Http,
    rpc: grpc::Tonic,
}

impl Walk {
    fn visit(&mut self, file: &mut File<'_>, node: Node<'_>, depth: usize) {
        if depth > file.max_depth {
            return;
        }
        let saved = self.rpc.scope;
        match node.kind() {
            "function_item" => {
                self.http.attribute_routes(file, node);
                self.rpc.enter_function(file, node);
            }
            "impl_item" => self.rpc.server_impl(file, node),
            "call_expression" => {
                self.http.call(file, node);
                self.rpc.call(file, node);
            }
            "let_declaration" => self.rpc.let_binding(file, node),
            "field_declaration" => self.rpc.field(file, node),
            _ => {}
        }
        for child in named_children(node) {
            self.visit(file, child, depth + 1);
        }
        self.rpc.scope = saved;
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

/// The value of a plain or raw string literal.
fn string_value(node: Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "string_literal" | "raw_string_literal" => {
            let inner = named_children(node);
            if inner.iter().any(|c| c.kind() == "escape_sequence") {
                return None;
            }
            Some(inner.iter().map(|c| text(*c, src)).collect())
        }
        _ => None,
    }
}

/// The last `::` segment of a path (`web::get` → `get`).
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path).trim()
}

/// What the pack knows about one file while emitting hints.
pub(crate) struct File<'s> {
    src: &'s [u8],
    defs: &'s [Definition],
    max_depth: usize,
    hints: Vec<ContractHint>,
}

impl<'s> File<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.src)
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

    /// The function or method of this file named `name` (the first one).
    fn callable_named(&self, name: &str) -> Option<LocalDefId> {
        self.defs
            .iter()
            .position(|d| d.name == name && matches!(d.label, Label::Function | Label::Method))
            .map(|i| i as LocalDefId)
    }

    /// The definition whose name identifier is `name_node`.
    fn def_at(&self, name_node: Node<'_>) -> Option<LocalDefId> {
        let start = name_node.start_byte() as u32;
        self.defs.iter().position(|d| d.name_span.start_byte == start).map(|i| i as LocalDefId)
    }

    /// The text from `start` to `end`, whitespace collapsed, at most
    /// `MAX_RAW_CHARS`.
    fn raw(&self, start: Node<'_>, end: Node<'_>) -> String {
        let to = end.end_byte().min(start.start_byte() + MAX_RAW_SOURCE).max(start.start_byte());
        let t = String::from_utf8_lossy(&self.src[start.start_byte()..to]);
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
        self.push_range(side, kind, key, (evidence, evidence), owner, framework);
    }

    /// A hint whose evidence runs from the start of `range.0` to the end of
    /// `range.1` (a call in a method chain: from its method name).
    fn push_range(
        &mut self,
        side: ContractSide,
        kind: ContractKind,
        key: String,
        range: (Node<'_>, Node<'_>),
        owner: Option<LocalDefId>,
        framework: &str,
    ) {
        let (start, end) = (span_of(range.0), span_of(range.1));
        let span = Span { end_byte: end.end_byte, end_line: end.end_line, end_col: end.end_col, ..start };
        let raw = self.raw(range.0, range.1);
        let hint = ContractHint { side, kind, key, raw, span, owner, framework: framework.to_string() };
        if !self.hints.contains(&hint) {
            self.hints.push(hint);
        }
    }
}
