//! Docker compose: services → `Resource`; `environment` entries → `EnvVar`
//! providers; `env_file` → imports; `build` → `Image` + imports of the
//! context folder and Dockerfile; `depends_on` → references to services;
//! `extends` → bases; `${VAR}` interpolations anywhere → env var reads.

use std::collections::HashSet;

use rueca_model::Label;

use super::{provider, strings};
use crate::emit::{Ctx, Def, Out};
use crate::keypath;
use crate::text::{self, MASK};
use crate::tree::{Node, Tree, Value};

/// Root `services:` whose entries have `image` or `build`.
pub(crate) fn is_compose_shape(doc: &Node) -> bool {
    let Some(services) = doc.get("services").and_then(Node::as_map) else { return false };
    !services.is_empty()
        && services.iter().all(|s| s.value.as_map().is_some())
        && services.iter().any(|s| s.value.get("image").is_some() || s.value.get("build").is_some())
}

fn join_path(dir: &str, rel: &str) -> String {
    let dir = dir.trim_end_matches('/');
    let rel = rel.trim_start_matches("./");
    if dir.is_empty() || dir == "." { format!("./{rel}") } else { format!("{dir}/{rel}") }
}

pub(crate) fn apply(out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    let mut handled: HashSet<usize> = HashSet::new();
    for doc in &tree.docs {
        let Some(services) = doc.get("services").and_then(Node::as_map) else { continue };
        for svc in services {
            let Some(sid) = ctx.def_of(&svc.value) else { continue };
            out.relabel(sid, Label::Resource);
            let image = svc.value.get_str("image");
            let build = svc.value.get("build");
            let sig = match (image, build) {
                (Some(i), _) => format!("service {}: image {}", svc.key, text::preview(i)),
                (None, Some(b)) => match b.as_str().or_else(|| b.get_str("context")) {
                    Some(c) => format!("service {}: build {}", svc.key, text::preview(c)),
                    None => format!("service {}: build", svc.key),
                },
                _ => format!("service {}", svc.key),
            };
            out.get(sid).signature = Some(sig);
            if let Some(b) = build {
                if let Some(bid) = ctx.def_of(b) {
                    out.relabel(bid, Label::Image);
                }
                let context = b.as_str().or_else(|| b.get_str("context")).unwrap_or(".");
                if !context.contains("://") && !context.starts_with("git@") {
                    // The context folder (not the compose file's own one)
                    // and the Dockerfile in it.
                    if !matches!(context.trim_end_matches('/'), "." | "") {
                        out.import(context, &b.span);
                    }
                    let df = b.get("dockerfile").and_then(Node::as_str).unwrap_or("Dockerfile");
                    out.import(&join_path(context, df), &b.span);
                }
            }
            if let Some(env) = svc.value.get("environment") {
                environment(out, ctx, env, &mut handled);
            }
            if let Some(ef) = svc.value.get("env_file") {
                let files: Vec<&Node> = match &ef.value {
                    Value::Seq(items) => items.iter().map(|i| i.get("path").unwrap_or(i)).collect(),
                    Value::Map(_) => vec![ef.get("path").unwrap_or(ef)],
                    _ => vec![ef],
                };
                for f in files {
                    if let Some(p) = f.as_str() {
                        out.import(p, &f.span);
                    }
                }
            }
            if let Some(dep) = svc.value.get("depends_on") {
                let names: Vec<(&str, std::ops::Range<usize>)> = match &dep.value {
                    Value::Seq(items) => items.iter().filter_map(|i| Some((i.as_str()?, i.span.clone()))).collect(),
                    Value::Map(m) => m.iter().map(|e| (e.key.as_str(), e.key_span.clone())).collect(),
                    _ => Vec::new(),
                };
                for (n, span) in names {
                    let target = keypath::join("services", &keypath::key_segment(n));
                    out.reference(rueca_model::RefKind::DataRef, &target, None, Some(sid), &span);
                }
            }
            if let Some(ext) = svc.value.get("extends") {
                let (service, file) = match &ext.value {
                    Value::Scalar(s) => (Some(s.as_str()), None),
                    Value::Map(_) => (ext.get_str("service"), ext.get_str("file")),
                    _ => (None, None),
                };
                if let Some(s) = service {
                    let target = keypath::join("services", &keypath::key_segment(s));
                    let base = match file {
                        Some(f) => {
                            out.import(f, &ext.span);
                            format!("{f}#{target}")
                        }
                        None => target,
                    };
                    out.get(sid).bases.push(base);
                }
            }
        }
    }
    // `${VAR}` interpolation reads, from the definition holding the value.
    for site in &ctx.scalars {
        if !handled.contains(&site.node.addr()) {
            out.interpolation_reads(&site.node.span, site.enclosing);
        }
    }
}

/// `environment:` as a map (`KEY: value`) or a list (`- KEY=value`).
fn environment(out: &mut Out<'_>, ctx: &Ctx<'_>, env: &Node, handled: &mut HashSet<usize>) {
    match &env.value {
        Value::Map(entries) => {
            for e in entries {
                if let Some(id) = ctx.def_of(&e.value) {
                    provider(out, id, &e.key, ": ", !e.value.is_null());
                    out.interpolation_reads(&e.value.span, Some(id));
                    handled.insert(e.value.addr());
                }
            }
        }
        Value::Seq(_) => {
            let Some(parent) = ctx.def_of(env) else { return };
            let parent_qn = out.get(parent).qualified_name.clone();
            for item in strings(env) {
                let Some(s) = item.as_str() else { continue };
                let (name, value) = match s.split_once('=') {
                    Some((n, v)) => (n.trim(), Some(v)),
                    None => (s.trim(), None),
                };
                if !text::is_env_name(name) {
                    continue;
                }
                let raw = out.slice(&item.span);
                let off = raw.find(name).unwrap_or(0);
                let name_span = item.span.start + off..item.span.start + off + name.len();
                let sig =
                    if value.is_some_and(|v| !v.is_empty()) { format!("{name}={MASK}") } else { name.to_string() };
                let id = out.def(Def {
                    label: Label::EnvVar,
                    name: name.to_string(),
                    qn: keypath::join(&parent_qn, &keypath::id_segment(name)),
                    parent: Some(parent),
                    span: item.span.clone(),
                    name_span,
                    signature: Some(sig),
                    doc: out.doc(item.span.start, Some(item.span.end)),
                });
                if let Some(id) = id {
                    out.interpolation_reads(&item.span, Some(id));
                }
                handled.insert(item.addr());
            }
        }
        _ => {}
    }
}
