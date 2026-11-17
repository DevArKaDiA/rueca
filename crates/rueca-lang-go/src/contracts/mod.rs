//! Contract hints of a Go file (RFC 0006 phase 4; keys and matching in
//! `rueca_model::contract`), read from the syntax tree after the
//! definitions are known, with file-local evidence only (the imports of
//! the router, client or proto package):
//!
//! - HTTP providers (`http.rs`): route registrations of `net/http`, gin,
//!   echo, chi, gorilla/mux and fiber, with group / sub-router prefixes
//!   built in the same file composed;
//! - HTTP consumers (`http.rs`): `net/http` calls, resty, and SDK wrappers
//!   called with a method and a path (`c.do(ctx, http.MethodPost,
//!   "/api/generate", …)`);
//! - gRPC providers and consumers (`grpc.rs`), framework `grpc-go`.
//!
//! Generated files (`// Code generated … DO NOT EDIT.`) have no hints: the
//! `.proto` is the specification.

mod grpc;
mod http;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use rueca_extract::span_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::truncate_raw;
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, Import, Label, LocalDefId};

/// Longest registration / call text read for `raw`, in bytes.
const MAX_RAW_SOURCE: usize = 2000;

/// The contract hints of one file.
pub(crate) fn hints(
    root: Node<'_>,
    src: &[u8],
    defs: &[Definition],
    imports: &[Import],
    max_depth: usize,
) -> Vec<ContractHint> {
    if is_generated(root, src) {
        return Vec::new();
    }
    let mut file = File::new(src, defs, imports, max_depth);
    file.collect_consts(root);
    let rpc = grpc::Bindings::collect(&file, root);
    rpc.providers(&mut file, root);
    http::Walk::new(&mut file, &rpc).run(root);
    file.hints
}

/// `// Code generated … DO NOT EDIT.` before the `package` clause.
fn is_generated(root: Node<'_>, src: &[u8]) -> bool {
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .take_while(|c| c.kind() == "comment")
        .any(|c| text(c, src).starts_with("// Code generated") && text(c, src).contains("DO NOT EDIT"))
}

/// UTF-8 text of a node (empty when not valid UTF-8).
fn text<'s>(node: Node<'_>, src: &'s [u8]) -> &'s str {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// Named children, comments left out.
fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|c| c.kind() != "comment").collect()
}

/// The value of a string literal without escapes or interpolation.
fn string_value(node: Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "interpreted_string_literal" => {
            let inner = named_children(node);
            if inner.iter().any(|c| c.kind() != "interpreted_string_literal_content") {
                return None;
            }
            Some(inner.iter().map(|c| text(*c, src)).collect())
        }
        "raw_string_literal" => Some(named_children(node).iter().map(|c| text(*c, src)).collect()),
        _ => None,
    }
}

/// What the pack knows about one file while emitting hints.
pub(crate) struct File<'s> {
    src: &'s [u8],
    defs: &'s [Definition],
    /// Import binding name → import path.
    imports: HashMap<String, String>,
    /// Package-level string constants (`const prefix = "/api"`).
    consts: HashMap<String, String>,
    max_depth: usize,
    hints: Vec<ContractHint>,
}

impl<'s> File<'s> {
    fn new(src: &'s [u8], defs: &'s [Definition], imports: &[Import], max_depth: usize) -> File<'s> {
        let imports =
            imports.iter().filter_map(|i| i.module_alias.as_ref().map(|a| (a.clone(), i.module.clone()))).collect();
        File { src, defs, imports, consts: HashMap::new(), max_depth, hints: Vec::new() }
    }

    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.src)
    }

    /// Package-level `const X = "…"` with one literal value.
    fn collect_consts(&mut self, root: Node<'_>) {
        for decl in named_children(root).into_iter().filter(|d| d.kind() == "const_declaration") {
            for spec in named_children(decl).into_iter().filter(|s| s.kind() == "const_spec") {
                self.const_spec(spec);
            }
        }
    }

    fn const_spec(&mut self, spec: Node<'_>) {
        let (Some(name), Some(value)) = (spec.child_by_field_name("name"), spec.child_by_field_name("value")) else {
            return;
        };
        let values = named_children(value);
        if let [only] = values.as_slice()
            && let Some(v) = string_value(*only, self.src)
        {
            self.consts.insert(self.text(name).to_string(), v);
        }
    }

    /// Binding name of the first import whose path is `path` or, with
    /// `prefix`, starts with `path` (`github.com/labstack/echo/v4`).
    fn alias(&self, path: &str, prefix: bool) -> Option<&str> {
        let hit = |p: &String| if prefix { p.starts_with(path) } else { p == path };
        let mut found: Vec<&str> = self.imports.iter().filter(|(_, p)| hit(p)).map(|(a, _)| a.as_str()).collect();
        found.sort_unstable();
        found.first().copied()
    }

    fn imports_path(&self, path: &str) -> bool {
        self.alias(path, true).is_some()
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
