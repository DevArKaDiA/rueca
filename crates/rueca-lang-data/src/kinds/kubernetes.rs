//! Kubernetes manifests, per document: `apiVersion` + `kind` +
//! `metadata.name` → a `Resource` named `Kind/name` at the root of the
//! document's key paths; list elements with a `name` are keyed by it
//! (`containers["web"]`); container `env` entries → `EnvVar` providers;
//! `configMapKeyRef` / `secretKeyRef` / `envFrom` → references to the
//! ConfigMap or Secret (in any manifest of the snapshot); Secret values are
//! never put in signatures.

use rueca_extract::DATA_REF_ANY;
use rueca_model::{Label, LocalDefId, RefKind};

use super::provider;
use crate::emit::{Ctx, DocIdentity, Out};
use crate::keypath;
use crate::text::MASK;
use crate::tree::{self, Node, Tree, Value};

fn scalar<'n>(doc: &'n Node, key: &str) -> Option<&'n str> {
    doc.get_str(key).map(str::trim).filter(|s| !s.is_empty())
}

/// `Kind/name` of a manifest document.
pub(crate) fn identity(doc: &Node) -> Option<DocIdentity> {
    let api = scalar(doc, "apiVersion")?;
    let kind = scalar(doc, "kind")?;
    let name = scalar(doc.get("metadata")?, "name")?;
    let ident = format!("{kind}/{name}");
    Some(DocIdentity {
        segment: keypath::key_segment(&ident),
        label: Label::Resource,
        name: ident,
        signature: format!("{kind} {name} ({api})"),
    })
}

/// List elements that carry a `name` are keyed by it.
pub(crate) fn element_identity(_list_key: &str, elem: &Node) -> Option<String> {
    scalar(elem, "name").map(str::to_string)
}

/// Key path of `Kind/name` (as `identity` formats it) plus `rest`.
fn target(kind: &str, name: &str, rest: &[&str]) -> String {
    let mut p = keypath::key_segment(&format!("{kind}/{name}"));
    for r in rest {
        p = keypath::join(&p, &keypath::key_segment(r));
    }
    p
}

pub(crate) fn apply(out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    for doc in &tree.docs {
        let Some(kind) = identity(doc).and(scalar(doc, "kind")) else { continue };
        if matches!(kind, "Secret") {
            mask_secret_values(out, ctx, doc);
        }
        tree::walk(doc, |node| {
            let Some(entries) = node.as_map() else { return };
            let owner = ctx.def_of(node);
            for e in entries {
                match e.key.as_str() {
                    "env" => env_list(out, ctx, &e.value),
                    "envFrom" => env_from(out, owner, &e.value),
                    _ => {}
                }
            }
        });
    }
}

/// Secret values (`data`, `stringData`) never reach signatures.
fn mask_secret_values(out: &mut Out<'_>, ctx: &Ctx<'_>, doc: &Node) {
    for key in ["data", "stringData"] {
        for e in doc.get(key).and_then(Node::as_map).unwrap_or(&[]) {
            if let Some(id) = ctx.def_of(&e.value) {
                out.get(id).signature = Some(format!("{}: {MASK}", e.key));
            }
        }
    }
}

/// `envFrom: [{configMapRef | secretRef: {name}}]` → references to the
/// whole ConfigMap or Secret.
fn env_from(out: &mut Out<'_>, owner: Option<LocalDefId>, env_from: &Node) {
    for item in env_from.as_seq().unwrap_or(&[]) {
        for (key, k) in [("configMapRef", "ConfigMap"), ("secretRef", "Secret")] {
            if let Some(name) = item.get(key).and_then(|r| scalar(r, "name")) {
                let t = target(k, name, &[]);
                out.reference(RefKind::DataRef, &t, Some(DATA_REF_ANY), owner, &item.span);
            }
        }
    }
}

/// `env: [{name, value | valueFrom}]` under a container.
fn env_list(out: &mut Out<'_>, ctx: &Ctx<'_>, env: &Node) {
    let Some(items) = env.as_seq() else { return };
    for item in items {
        let Some(name) = scalar(item, "name") else { continue };
        let Some(id) = ctx.def_of(item) else { continue };
        let has_value = item.get("value").is_some_and(|v| !v.is_null()) || item.get("valueFrom").is_some();
        provider(out, id, name, ": ", has_value);
        let Some(from) = item.get("valueFrom") else { continue };
        value_from(out, id, from);
        if let Some(value) = item.get("value").filter(|v| matches!(v.value, Value::Scalar(_))) {
            dependent_reads(out, id, value);
        }
    }
}

/// `valueFrom: {configMapKeyRef | secretKeyRef: {name, key}}` → a reference
/// to the key in the ConfigMap or Secret.
fn value_from(out: &mut Out<'_>, id: LocalDefId, from: &Node) {
    for (key, kind) in [("configMapKeyRef", "ConfigMap"), ("secretKeyRef", "Secret")] {
        let Some(r) = from.get(key) else { continue };
        let (Some(n), Some(k)) = (scalar(r, "name"), scalar(r, "key")) else { continue };
        // Secrets may set the key under `data` or `stringData`.
        let maps: &[&str] = if kind == "Secret" { &["data", "stringData"] } else { &["data"] };
        for m in maps {
            let t = target(kind, n, &[m, k]);
            out.reference(RefKind::DataRef, &t, Some(DATA_REF_ANY), Some(id), &r.span);
        }
    }
}

/// `$(OTHER)` dependent variables (Kubernetes' own syntax) in a `value`.
fn dependent_reads(out: &mut Out<'_>, id: LocalDefId, value: &Node) {
    let raw = out.slice(&value.span);
    let base = value.span.start;
    let mut rest = raw;
    let mut off = 0;
    while let Some(i) = rest.find("$(") {
        let after = &rest[i + 2..];
        let Some(close) = after.find(')') else { break };
        let var = &after[..close];
        if crate::text::is_env_name(var) {
            let s = base + off + i;
            out.reference(RefKind::EnvVar, var, None, Some(id), &(s..s + close + 3));
        }
        off += i + 2 + close + 1;
        rest = &after[close + 1..];
    }
}
