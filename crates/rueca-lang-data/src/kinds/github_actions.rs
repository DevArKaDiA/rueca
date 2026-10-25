//! GitHub Actions workflows (`.github/workflows/*.y(a)ml`) and actions
//! (`action.y(a)ml`): jobs are keys with a `runs-on` signature; steps are
//! keyed by `id` when they have one; `needs` → references to jobs; local
//! `uses: ./…` → imports (actions and reusable workflows, from the
//! repository root); `env` maps → `EnvVar` providers; `${{ env.X }}` →
//! env var reads.

use rueca_model::RefKind;

use super::{provider, strings};
use crate::emit::{Ctx, Out};
use crate::keypath;
use crate::text;
use crate::tree::{self, Node, Tree};

/// Steps are keyed by their `id`.
pub(crate) fn element_identity(list_key: &str, elem: &Node) -> Option<String> {
    if list_key != "steps" {
        return None;
    }
    elem.get_str("id").map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub(crate) fn apply(out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    let Some(doc) = tree.docs.first() else { return };
    // Jobs.
    for job in doc.get("jobs").and_then(Node::as_map).unwrap_or(&[]) {
        let Some(id) = ctx.def_of(&job.value) else { continue };
        let detail = match (job.value.get("runs-on"), job.value.get_str("uses")) {
            (Some(r), _) => format!("runs-on {}", crate::emit::value_preview(out, r)),
            (None, Some(u)) => format!("uses {}", text::preview(u)),
            _ => String::new(),
        };
        let d = out.get(id);
        d.signature = Some(if detail.is_empty() { job.key.clone() } else { format!("{}: {detail}", job.key) });
        if d.doc.is_none() {
            d.doc = job.value.get_str("name").map(|n| text::truncate(n, 200));
        }
        if let Some(needs) = job.value.get("needs") {
            for n in strings(needs) {
                if let Some(name) = n.as_str() {
                    let t = keypath::join("jobs", &keypath::key_segment(name));
                    out.reference(RefKind::DataRef, &t, None, Some(id), &n.span);
                }
            }
        }
    }
    // Steps, local `uses`, `env` maps (anywhere: workflow, job, step).
    tree::walk(doc, |node| {
        let Some(entries) = node.as_map() else { return };
        for e in entries {
            match e.key.as_str() {
                "env" => {
                    for v in e.value.as_map().unwrap_or(&[]) {
                        if let Some(id) = ctx.def_of(&v.value) {
                            provider(out, id, &v.key, ": ", !v.value.is_null());
                        }
                    }
                }
                "uses" => {
                    if let Some(u) = e.value.as_str()
                        && u.starts_with("./")
                    {
                        out.import(u.split('@').next().unwrap_or(u), &e.value.span);
                    }
                }
                "steps" => {
                    for step in e.value.as_seq().unwrap_or(&[]) {
                        let Some(sid) = ctx.def_of(step) else { continue };
                        let what = match (step.get_str("uses"), step.get_str("run")) {
                            (Some(u), _) => format!("uses {}", text::preview(u)),
                            (None, Some(r)) => format!("run {}", text::preview(r)),
                            _ => String::new(),
                        };
                        let d = out.get(sid);
                        let label = d.name.clone();
                        let title = step.get_str("name").map(text::preview);
                        d.signature = Some(match (title, what.is_empty()) {
                            (Some(t), false) => format!("{label}: {t} ({what})"),
                            (Some(t), true) => format!("{label}: {t}"),
                            (None, false) => format!("{label}: {what}"),
                            (None, true) => label,
                        });
                    }
                }
                _ => {}
            }
        }
    });
    // `${{ env.NAME }}` reads.
    for site in &ctx.scalars {
        let raw = out.slice(&site.node.span);
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(i) = raw[from..].find("${{") {
            let s = from + i;
            let Some(close) = raw[s..].find("}}") else { break };
            let expr = &raw[s + 3..s + close];
            let mut rest = expr;
            while let Some(j) = rest.find("env.") {
                let before_ok = j == 0 || !rest.as_bytes()[j - 1].is_ascii_alphanumeric();
                let name: String =
                    rest[j + 4..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if before_ok && !name.is_empty() {
                    found.push((name, site.node.span.start + s..site.node.span.start + s + close + 2));
                }
                rest = &rest[j + 4..];
            }
            from = s + close + 2;
        }
        for (name, r) in found {
            out.reference(RefKind::EnvVar, &name, None, site.enclosing, &r);
        }
    }
}
