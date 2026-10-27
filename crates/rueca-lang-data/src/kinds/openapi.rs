//! OpenAPI 3 / Swagger 2 and JSON Schema. `paths.<path>.<method>` → `Route`
//! named `GET /users/{id}` with signature `GET /users/{id} (operationId)`
//! and doc = `summary`, else `description`. For both kinds a definition
//! without a comment gets its schema `description` (or `title`) as doc.
//! `$ref` pointers are handled generically by the emitter.

use rueca_model::Label;

use super::Kind;
use crate::emit::{Ctx, Out};
use crate::text;
use crate::tree::{self, Entry, Node, Tree};

const METHODS: &[&str] = &["get", "put", "post", "delete", "options", "head", "patch", "trace"];

pub(crate) fn is_openapi(doc: &Node) -> bool {
    match (doc.get_str("openapi"), doc.get_str("swagger")) {
        (Some(v), _) => v.trim().starts_with('3'),
        (None, Some(v)) => v.trim().starts_with("2."),
        _ => false,
    }
}

pub(crate) fn apply(kind: Kind, out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    let Some(doc) = tree.docs.first() else { return };
    if kind == Kind::OpenApi {
        for path in doc.get("paths").and_then(Node::as_map).unwrap_or(&[]) {
            for op in path.value.as_map().unwrap_or(&[]) {
                operation(out, ctx, &path.key, op);
            }
        }
    }
    // Schema descriptions as docs.
    tree::walk(doc, |node| schema_doc(out, ctx, node));
}

/// `paths.<path>.<method>` → a `Route` named `GET /path`.
fn operation(out: &mut Out<'_>, ctx: &Ctx<'_>, path: &str, op: &Entry) {
    let method = op.key.to_ascii_lowercase();
    if !METHODS.contains(&method.as_str()) {
        return;
    }
    let Some(id) = ctx.def_of(&op.value) else { return };
    let route = format!("{} {}", method.to_ascii_uppercase(), path);
    out.relabel(id, Label::Route);
    let d = out.get(id);
    d.signature = Some(match op.value.get_str("operationId") {
        Some(o) => format!("{route} ({})", text::preview(o)),
        None => route.clone(),
    });
    d.name = route;
    if let Some(s) = op.value.get_str("summary").or_else(|| op.value.get_str("description")) {
        d.doc = Some(text::truncate(s.trim(), 500));
    }
}

/// A mapping definition without a comment gets its `description` (else
/// `title`) as doc.
fn schema_doc(out: &mut Out<'_>, ctx: &Ctx<'_>, node: &Node) {
    let Some(id) = ctx.def_of(node) else { return };
    if node.as_map().is_none() {
        return;
    }
    let d = out.get(id);
    if d.doc.is_some() {
        return;
    }
    let Some(desc) = node.get_str("description").or_else(|| node.get_str("title")) else { return };
    let desc = desc.trim();
    if !desc.is_empty() {
        d.doc = Some(text::truncate(desc, 500));
    }
}
