//! Attribute macros as decorators.
//!
//! An attribute whose path is not one of the compiler's (`#[tokio::main]`,
//! `#[tracing::instrument]`, `#[async_trait]`, `#[rstest]`) is a procedural
//! macro applied to the item, as a Python decorator or a Java annotation
//! is: a `Decorator` reference from the item (the resolver's `DECORATES`
//! edge goes from the macro's function to the item), and its text in the
//! item's `decorators`. Not decorators: built-in attributes (`#[derive]`,
//! `#[cfg]`, `#[test]`, `#[inline]`, `#[doc]`, …), tool attributes
//! (`#[rustfmt::skip]`, `#[clippy::…]`, `#[diagnostic::…]`), and on an item
//! with a `#[derive]` every single-segment attribute (`#[serde(…)]`,
//! `#[error(…)]`: helper attributes of the derives, not macros).

use rueca_extract::tree_sitter::Node;
use rueca_model::{LocalDefId, RefKind};

use crate::extract::{Container, Cx, Extractor, named_children, one_line};
use crate::types::path_segments;

/// Attributes of the language (the Rust Reference, *Built-in attributes*,
/// plus the standard library's internal ones): sorted.
const BUILTIN_ATTRIBUTES: &[&str] = &[
    "allow",
    "allow_internal_unstable",
    "automatically_derived",
    "bench",
    "cfg",
    "cfg_attr",
    "cfg_eval",
    "cold",
    "collapse_debuginfo",
    "const_trait",
    "coverage",
    "crate_name",
    "crate_type",
    "debugger_visualizer",
    "default",
    "deny",
    "deprecated",
    "derive",
    "doc",
    "expect",
    "export_name",
    "feature",
    "forbid",
    "global_allocator",
    "ignore",
    "inline",
    "instruction_set",
    "link",
    "link_name",
    "link_ordinal",
    "link_section",
    "macro_export",
    "macro_use",
    "must_use",
    "naked",
    "no_builtins",
    "no_implicit_prelude",
    "no_link",
    "no_main",
    "no_mangle",
    "no_std",
    "non_exhaustive",
    "optimize",
    "panic_handler",
    "path",
    "proc_macro",
    "proc_macro_attribute",
    "proc_macro_derive",
    "recursion_limit",
    "register_tool",
    "repr",
    "should_panic",
    "stable",
    "target_feature",
    "test",
    "test_runner",
    "thread_local",
    "track_caller",
    "type_length_limit",
    "unsafe",
    "unstable",
    "used",
    "warn",
    "windows_subsystem",
];

/// Namespaces of tool attributes (`#[rustfmt::skip]`).
const TOOL_NAMESPACES: &[&str] = &["clippy", "diagnostic", "miri", "rust_analyzer", "rustdoc", "rustfmt"];

/// Whether an attribute path (segments) names a procedural macro.
fn is_macro_attribute(segs: &[String], derives: bool) -> bool {
    match segs {
        [] => false,
        [one] => !derives && BUILTIN_ATTRIBUTES.binary_search(&one.as_str()).is_err() && !one.starts_with("rustc_"),
        [first, ..] => !TOOL_NAMESPACES.contains(&first.as_str()),
    }
}

/// The `attribute` nodes of the `#[…]` items directly above `node`.
fn outer_attributes(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cur = node.prev_sibling();
    while let Some(c) = cur {
        match c.kind() {
            "attribute_item" => out.extend(named_children(c).into_iter().find(|a| a.kind() == "attribute")),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        cur = c.prev_sibling();
    }
    out.reverse();
    out
}

impl Extractor<'_> {
    /// The attribute macros of item `node` (definition `id`, declared in
    /// `c`): its `decorators` and a `Decorator` reference each.
    pub(crate) fn decorate(&mut self, node: Node<'_>, id: LocalDefId, c: &Container) {
        let cx = Cx::main(self.src);
        let attrs: Vec<(Node<'_>, Node<'_>, Vec<String>)> = outer_attributes(node)
            .into_iter()
            .filter_map(|a| {
                let path = named_children(a).into_iter().next()?;
                Some((a, path, path_segments(cx, path)?))
            })
            .collect();
        let derives = attrs.iter().any(|(_, _, s)| s.len() == 1 && s[0] == "derive");
        let macros: Vec<_> = attrs.into_iter().filter(|(_, _, s)| is_macro_attribute(s, derives)).collect();
        if macros.is_empty() {
            return;
        }
        self.defs[id as usize].decorators = macros.iter().map(|(a, _, _)| one_line(cx.text(*a), 200)).collect();
        self.within(Some(id), c.clone(), |ex| {
            for (a, path, segs) in &macros {
                let Some((name, qualifier)) = segs.split_last() else { continue };
                let receiver = if qualifier.is_empty() { None } else { ex.path_receiver(cx, qualifier, *path) };
                let span = ex.span(cx, *a);
                ex.push_ref(RefKind::Decorator, name, receiver, span);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_attributes_are_sorted() {
        assert!(BUILTIN_ATTRIBUTES.windows(2).all(|w| w[0] < w[1]));
    }
}
