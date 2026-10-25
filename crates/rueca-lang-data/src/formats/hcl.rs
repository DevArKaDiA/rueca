//! HCL / Terraform / OpenTofu with `hcl-edit` (MIT OR Apache-2.0; byte
//! spans on every structure). `hcl-edit` has no recursion guard, so the
//! bracket depth is checked before parsing ([`MAX_NESTING`]).
//!
//! | Source | Label | Key path |
//! |---|---|---|
//! | `resource "t" "n"` | `Resource` | `t.n` |
//! | `data "t" "n"` | `Resource` | `data.t.n` |
//! | `module "m"` (+ local `source` → import of the folder) | `Resource` | `module.m` |
//! | `variable "x"` | `Variable` | `var.x` |
//! | `locals { a = … }` | `Variable` | `local.a` |
//! | `output "x"` | `Key` | `output.x` |
//! | `provider "p"`, `terraform`, other blocks | `Key` | ident + labels (`provider.aws`) |
//! | nested blocks | `Key` | parent + ident (`ingress[0]` when repeated) |
//! | attributes | `Key` | parent + name (top level in `.tfvars`) |
//!
//! References: traversals `var.x`, `local.x`, `module.m…`, `data.t.n…` and
//! `t.n…` (a resource type contains `_`) inside any expression become
//! `DataRef`s from the top-level block, resolved in the same directory
//! (the module).

use std::collections::HashMap;
use std::ops::Range;

use hcl_edit::Span;
use hcl_edit::expr::{Expression, Traversal, TraversalOperator};
use hcl_edit::structure::{Attribute, Block, BlockLabel, Body, Structure};
use hcl_edit::visit::{Visit, visit_traversal};
use rueca_extract::ExtractError;
use rueca_model::{Label, LocalDefId, RefKind};

use crate::emit::{Def, Out};
use crate::keypath;
use crate::text::{self, MASK};
use crate::tree::MAX_DEPTH;

/// Bracket nesting refused before parsing (RFC 0003 §4.7).
pub const MAX_NESTING: usize = 128;

/// Deepest `{` / `[` / `(` nesting of `src` (strings and comments included,
/// an upper bound).
fn nesting(src: &str) -> usize {
    let (mut depth, mut max) = (0usize, 0usize);
    for b in src.bytes() {
        match b {
            b'{' | b'[' | b'(' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

pub(crate) fn extract(out: &mut Out<'_>, path: &str) -> Result<(), ExtractError> {
    if nesting(out.src) > MAX_NESTING {
        return Err(ExtractError::Skipped("data-shape".into()));
    }
    let body = hcl_edit::parser::parse_body(out.src).map_err(|e| ExtractError::Parser(e.to_string()))?;
    let tfvars = path.ends_with(".tfvars");
    let mut h = Hcl { out, tfvars };
    h.body(&body, None, "", None, 0);
    Ok(())
}

struct Hcl<'a, 's> {
    out: &'a mut Out<'s>,
    tfvars: bool,
}

fn span_of(s: &impl Span) -> Range<usize> {
    s.span().unwrap_or(0..0)
}

fn label_text(l: &BlockLabel) -> String {
    match l {
        BlockLabel::Ident(i) => i.as_str().to_string(),
        BlockLabel::String(s) => s.as_str().to_string(),
    }
}

fn label_span(l: &BlockLabel) -> Range<usize> {
    match l {
        BlockLabel::Ident(i) => span_of(i),
        BlockLabel::String(s) => span_of(s),
    }
}

/// Traversal references of an expression.
#[derive(Default)]
struct Refs {
    found: Vec<(String, Range<usize>)>,
}

const RESERVED: &[&str] = &["each", "count", "path", "self", "terraform", "var", "local", "module", "data"];

impl Visit for Refs {
    fn visit_traversal(&mut self, t: &Traversal) {
        if let Expression::Variable(root) = &t.expr {
            let attrs: Vec<&str> = t
                .operators
                .iter()
                .map_while(|op| match op.value() {
                    TraversalOperator::GetAttr(a) => Some(a.as_str()),
                    _ => None,
                })
                .collect();
            let root = root.as_str();
            let name = match (root, attrs.as_slice()) {
                ("var" | "local" | "module", [a, ..]) => Some(format!("{root}.{}", keypath::key_segment(a))),
                ("data", [t, n, ..]) => Some(format!("data.{}.{}", keypath::key_segment(t), keypath::key_segment(n))),
                (r, [n, ..]) if r.contains('_') && !RESERVED.contains(&r) => {
                    Some(format!("{}.{}", keypath::key_segment(r), keypath::key_segment(n)))
                }
                _ => None,
            };
            if let Some(n) = name {
                self.found.push((n, span_of(t)));
            }
        }
        visit_traversal(self, t);
    }
}

impl Hcl<'_, '_> {
    fn refs(&mut self, expr: &Expression, enclosing: Option<LocalDefId>) {
        let mut r = Refs::default();
        r.visit_expr(expr);
        for (name, span) in r.found {
            self.out.reference(RefKind::DataRef, &name, None, enclosing, &span);
        }
    }

    fn attr_signature(&self, a: &Attribute) -> String {
        let key = a.key.as_str();
        if text::is_secret_name(key) && !matches!(a.value, Expression::Null(_)) {
            return format!("{key} = {MASK}");
        }
        format!("{key} = {}", text::preview(self.out.slice(&span_of(&a.value))))
    }

    fn attribute(
        &mut self,
        a: &Attribute,
        label: Label,
        qn: String,
        parent: Option<LocalDefId>,
        top: Option<LocalDefId>,
    ) -> Option<LocalDefId> {
        let span = span_of(a);
        let def = Def {
            label,
            name: a.key.as_str().to_string(),
            qn,
            parent,
            span: span.clone(),
            name_span: span_of(&a.key),
            signature: Some(self.attr_signature(a)),
            doc: self.out.doc(span.start, Some(span.end)),
        };
        let id = self.out.def(def)?;
        self.refs(&a.value, top.or(Some(id)));
        Some(id)
    }

    fn body(&mut self, body: &Body, parent: Option<LocalDefId>, qn: &str, top: Option<LocalDefId>, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for s in body.iter() {
            if let Structure::Block(b) = s {
                *counts.entry(b.ident.as_str()).or_default() += 1;
            }
        }
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for s in body.iter() {
            match s {
                Structure::Attribute(a) => {
                    let q = keypath::join(qn, &keypath::key_segment(a.key.as_str()));
                    self.attribute(a, Label::Key, q, parent, top);
                }
                Structure::Block(b) => {
                    let ident = b.ident.as_str();
                    let n = seen.entry(ident).or_default();
                    let index = (counts[ident] > 1).then_some(*n);
                    *n += 1;
                    if depth == 0 && !self.tfvars {
                        self.top_block(b);
                    } else {
                        self.nested_block(b, parent, qn, top, index, depth);
                    }
                }
            }
        }
    }

    fn top_block(&mut self, b: &Block) {
        let ident = b.ident.as_str();
        let labels: Vec<String> = b.labels.iter().map(label_text).collect();
        let seg = |s: &str| keypath::key_segment(s);
        let (label, qn, name) = match (ident, labels.as_slice()) {
            ("resource", [t, n]) => (Label::Resource, format!("{}.{}", seg(t), seg(n)), n.clone()),
            ("data", [t, n]) => (Label::Resource, format!("data.{}.{}", seg(t), seg(n)), n.clone()),
            ("module", [m]) => (Label::Resource, format!("module.{}", seg(m)), m.clone()),
            ("variable", [x]) => (Label::Variable, format!("var.{}", seg(x)), x.clone()),
            ("output", [x]) => (Label::Key, format!("output.{}", seg(x)), x.clone()),
            ("locals", []) => {
                for s in b.body.iter() {
                    match s {
                        Structure::Attribute(a) => {
                            let q = format!("local.{}", seg(a.key.as_str()));
                            self.attribute(a, Label::Variable, q, None, None);
                        }
                        Structure::Block(_) => {}
                    }
                }
                return;
            }
            _ => {
                let mut q = seg(ident);
                for l in &labels {
                    q = keypath::join(&q, &seg(l));
                }
                (Label::Key, q, labels.last().cloned().unwrap_or_else(|| ident.to_string()))
            }
        };
        let span = span_of(b);
        let name_span = b.labels.last().map(label_span).unwrap_or_else(|| span_of(&b.ident));
        let header: Vec<String> = std::iter::once(ident.to_string())
            .chain(b.labels.iter().map(|l| match l {
                BlockLabel::String(s) => keypath::quote(s.as_str()),
                BlockLabel::Ident(i) => i.as_str().to_string(),
            }))
            .collect();
        let mut signature = header.join(" ");
        let attr = |k: &str| b.body.get_attribute(k);
        if ident == "module"
            && let Some(src) = attr("source")
        {
            signature.push_str(&format!(" ({})", text::preview(self.out.slice(&span_of(&src.value)))));
        }
        if ident == "variable"
            && let Some(d) = attr("default")
        {
            signature.push_str(&format!(" = {}", text::preview(self.out.slice(&span_of(&d.value)))));
        }
        let mut doc = self.out.doc(span.start, None);
        if doc.is_none()
            && let Some(Expression::String(s)) = attr("description").map(|a| &a.value)
        {
            doc = Some(text::truncate(s.as_str().trim(), 500));
        }
        let Some(id) = self.out.def(Def {
            label,
            name,
            qn: qn.clone(),
            parent: None,
            span,
            name_span,
            signature: Some(signature),
            doc,
        }) else {
            return;
        };
        if ident == "module"
            && let Some(src) = attr("source")
            && let Expression::String(s) = &src.value
            && (s.starts_with("./") || s.starts_with("../"))
        {
            self.out.import(s.as_str(), &span_of(&src.value));
        }
        // Nested attributes (dynamic blocks and `for_each` included) are
        // walked by `body`, with references attributed to this block.
        self.body(&b.body, Some(id), &qn, Some(id), 1);
    }

    fn nested_block(
        &mut self,
        b: &Block,
        parent: Option<LocalDefId>,
        qn: &str,
        top: Option<LocalDefId>,
        index: Option<usize>,
        depth: usize,
    ) {
        let ident = b.ident.as_str();
        let mut q = keypath::join(qn, &keypath::key_segment(ident));
        for l in &b.labels {
            q = keypath::join(&q, &keypath::key_segment(&label_text(l)));
        }
        if let Some(i) = index {
            q = keypath::join(&q, &keypath::index_segment(i));
        }
        let span = span_of(b);
        let labels: Vec<String> = b.labels.iter().map(|l| keypath::quote(&label_text(l))).collect();
        let sig = if labels.is_empty() {
            format!("{ident} {{…}}")
        } else {
            format!("{ident} {} {{…}}", labels.join(" "))
        };
        let def = Def {
            label: Label::Key,
            name: ident.to_string(),
            qn: q.clone(),
            parent,
            span: span.clone(),
            name_span: span_of(&b.ident),
            signature: Some(sig),
            doc: self.out.doc(span.start, None),
        };
        let Some(id) = self.out.def(def) else { return };
        self.body(&b.body, Some(id), &q, top.or(Some(id)), depth + 1);
    }
}
