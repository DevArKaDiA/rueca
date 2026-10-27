//! Package manifests: `package.json` (entry points and workspaces →
//! imports), `tsconfig` / `jsconfig` (`extends`, `references` → imports) and
//! `pyproject.toml` (script entry points → references to the Python
//! definition by global qualified name).

use crate::emit::{Ctx, Out, qn_ref};
use crate::tree::{Node, Tree, Value};

use super::strings;

fn is_local_path(s: &str) -> bool {
    !s.is_empty() && !s.contains("://") && !s.contains('*') && !s.starts_with('#')
}

/// Imports `n` when it is a string naming a local path.
fn import_local_str(out: &mut Out<'_>, n: &Node) {
    if let Some(s) = n.as_str().filter(|s| is_local_path(s)) {
        out.import(s, &n.span);
    }
}

/// `main`, `module`, `types`, `typings`, `browser` (string), `bin`
/// (string or map), `exports` (every string leaf, conditions included) and
/// `workspaces` without globs → imports.
pub(crate) fn package_json(out: &mut Out<'_>, tree: &Tree) {
    let Some(doc) = tree.docs.first() else { return };
    for key in ["main", "module", "types", "typings", "browser"] {
        if let Some(n) = doc.get(key) {
            import_local_str(out, n);
        }
    }
    if let Some(bin) = doc.get("bin") {
        let nodes: Vec<&Node> = match &bin.value {
            Value::Map(m) => m.iter().map(|e| &e.value).collect(),
            _ => vec![bin],
        };
        for n in nodes {
            import_local_str(out, n);
        }
    }
    if let Some(exports) = doc.get("exports") {
        export_imports(out, exports);
    }
    let ws = doc.get("workspaces").map(|w| w.get("packages").unwrap_or(w));
    for n in ws.map(strings).unwrap_or_default() {
        import_local_str(out, n);
    }
}

/// Every `./…` string leaf of `exports` (conditions included), in order.
fn export_imports(out: &mut Out<'_>, exports: &Node) {
    let mut stack = vec![exports];
    while let Some(n) = stack.pop() {
        match &n.value {
            Value::Scalar(s) if s.starts_with("./") && is_local_path(s) => out.import(s, &n.span),
            Value::Map(m) => stack.extend(m.iter().rev().map(|e| &e.value)),
            Value::Seq(items) => stack.extend(items.iter().rev()),
            _ => {}
        }
    }
}

/// `extends` (string or array) and `references[].path` → imports. Package
/// specifiers (`@tsconfig/node20/tsconfig.json`) resolve to nothing.
pub(crate) fn tsconfig(out: &mut Out<'_>, tree: &Tree) {
    let Some(doc) = tree.docs.first() else { return };
    for n in doc.get("extends").map(strings).unwrap_or_default() {
        if let Some(s) = n.as_str().filter(|s| is_local_path(s)) {
            out.import(s, &n.span);
        }
    }
    for r in doc.get("references").and_then(Node::as_seq).unwrap_or(&[]) {
        if let Some(p) = r.get("path")
            && let Some(s) = p.as_str().filter(|s| is_local_path(s))
        {
            out.import(s, &p.span);
        }
    }
}

/// `[project.scripts]`, `[project.gui-scripts]`, `[project.entry-points.*]`
/// and `[tool.poetry.scripts]`: `name = "pkg.mod:func"` → a `DataRef` to the
/// Python definition `pkg.mod.func` (exact global qualified name).
pub(crate) fn pyproject(out: &mut Out<'_>, tree: &Tree, ctx: &Ctx<'_>) {
    let Some(doc) = tree.docs.first() else { return };
    let project = doc.get("project");
    let mut tables: Vec<&Node> = Vec::new();
    if let Some(p) = project {
        tables.extend(p.get("scripts"));
        tables.extend(p.get("gui-scripts"));
        for g in p.get("entry-points").and_then(Node::as_map).unwrap_or(&[]) {
            tables.push(&g.value);
        }
    }
    if let Some(s) = doc.get("tool").and_then(|t| t.get("poetry")).and_then(|p| p.get("scripts")) {
        tables.push(s);
    }
    for t in tables {
        for e in t.as_map().unwrap_or(&[]) {
            let Some(spec) = e.value.as_str().or_else(|| e.value.get_str("reference")) else { continue };
            let spec = spec.split('[').next().unwrap_or(spec).trim();
            let Some((module, attr)) = spec.split_once(':') else { continue };
            let (module, attr) = (module.trim(), attr.trim());
            let ok = |s: &str| !s.is_empty() && s.split('.').all(|p| !p.is_empty() && !p.contains(char::is_whitespace));
            if ok(module) && ok(attr) {
                qn_ref(out, &format!("{module}.{attr}"), ctx.def_of(&e.value), &e.value.span);
            }
        }
    }
}
