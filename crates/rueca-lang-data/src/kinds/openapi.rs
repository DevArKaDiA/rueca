//! OpenAPI 3 / Swagger 2 and JSON Schema. `paths.<path>.<method>` → `Route`
//! named `GET /users/{id}` with signature `GET /users/{id} (operationId)`
//! and doc = `summary`, else `description`. For both kinds a definition
//! without a comment gets its schema `description` (or `title`) as doc.
//! `$ref` pointers are handled generically by the emitter.

use rueca_model::Label;

use super::Kind;
use crate::emit::{Ctx, Out};
use crate::text;
use crate::tree::{self, Node, Tree};

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
                let method = op.key.to_ascii_lowercase();
                if !METHODS.contains(&method.as_str()) {
                    continue;
                }
                let Some(id) = ctx.def_of(&op.value) else { continue };
                let route = format!("{} {}", method.to_ascii_uppercase(), path.key);
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
        }
    }
    // Schema descriptions as docs.
    tree::walk(doc, |node| {
        let Some(id) = ctx.def_of(node) else { return };
        if node.as_map().is_none() {
            return;
        }
        let d = out.get(id);
        if d.doc.is_none()
            && let Some(desc) = node.get_str("description").or_else(|| node.get_str("title"))
        {
            let desc = desc.trim();
            if !desc.is_empty() {
                d.doc = Some(text::truncate(desc, 500));
            }
        }
    });
}
