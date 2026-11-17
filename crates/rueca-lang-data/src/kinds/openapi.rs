//! OpenAPI 3 / Swagger 2 and JSON Schema. `paths.<path>.<method>` → `Route`
//! named `GET /users/{id}` with signature `GET /users/{id} (operationId)`
//! and doc = `summary`, else `description`. Every `Route` also provides an
//! HTTP contract (RFC 0006 phase 4): a `Provides` hint, framework `openapi`,
//! key `GET /users/{}` (`rueca_model::contract::http_key`), owner = the
//! `Route`, raw = its signature, span = the operation's. For both kinds a definition
//! without a comment gets its schema `description` (or `title`) as doc.
//! `$ref` pointers are handled generically by the emitter. An operation the
//! key cap left out (a large spec whose schemas come first) still becomes a
//! `Route` (routes are not keys), child of its path's key when that exists.
//! A spec larger than the data-file budget is recognized by a root
//! `openapi: 3…` / `swagger: "2…"` key anywhere in the file
//! ([`declares_version`]): JSON emitters that sort keys write `swagger`
//! after `paths`.

use rueca_model::contract::{http_key, truncate_raw};
use rueca_model::{ContractHint, ContractKind, ContractSide, Label, LocalDefId};

use super::Kind;
use crate::emit::{self, Ctx, Out};
use crate::keypath;
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
                operation(out, ctx, path, op);
            }
        }
    }
    // Schema descriptions as docs.
    tree::walk(doc, |node| schema_doc(out, ctx, node));
}

/// `paths.<path>.<method>` → a `Route` named `GET /path`.
fn operation(out: &mut Out<'_>, ctx: &Ctx<'_>, path_entry: &Entry, op: &Entry) {
    let method = op.key.to_ascii_lowercase();
    if !METHODS.contains(&method.as_str()) {
        return;
    }
    let Some(id) = ctx.def_of(&op.value).or_else(|| capped_route(out, ctx, path_entry, op)) else { return };
    let path = path_entry.key.as_str();
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
    let (span, raw) = (d.span, d.signature.clone().unwrap_or_default());
    if let Some(key) = http_key(Some(&method), path) {
        out.facts.contracts.push(ContractHint {
            side: ContractSide::Provides,
            kind: ContractKind::Http,
            key,
            raw: truncate_raw(&raw),
            span,
            owner: Some(id),
            framework: "openapi".into(),
        });
    }
}

/// A new `Route` definition for an operation the key cap left out.
fn capped_route(out: &mut Out<'_>, ctx: &Ctx<'_>, path: &Entry, op: &Entry) -> Option<LocalDefId> {
    let paths = keypath::join("", &keypath::key_segment("paths"));
    let qn = keypath::join(&keypath::join(&paths, &keypath::key_segment(&path.key)), &keypath::key_segment(&op.key));
    let mut d = emit::key_def(out, op, op.key.clone(), qn, ctx.def_of(&path.value));
    d.label = Label::Route;
    out.def(d)
}

/// Whether `src` has a root `openapi: 3…` or `swagger: "2…"` key: JSON
/// `"openapi": "3.1.0"` anywhere, YAML `swagger: "2.0"` at a line start.
pub(crate) fn declares_version(src: &str) -> bool {
    ["openapi", "swagger"].iter().any(|k| src.match_indices(k).any(|(at, _)| version_follows(src, at, k.len())))
}

/// Whether the word at `src[at..at + len]` is a key whose value starts
/// like an OpenAPI 3 / Swagger 2 version.
fn version_follows(src: &str, at: usize, len: usize) -> bool {
    let (before, after) = (&src[..at], &src[at + len..]);
    let rest = if before.ends_with('"') && after.starts_with('"') {
        &after[1..]
    } else if before.is_empty() || before.ends_with('\n') {
        after
    } else {
        return false;
    };
    let Some(value) = rest.trim_start_matches([' ', '\t']).strip_prefix(':') else { return false };
    let v = value.trim_start_matches([' ', '\t', '"', '\'']);
    v.starts_with('3') || v.starts_with("2.")
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
