//! Node helpers and type texts: the type a type node names (for `$type:`
//! receivers) and the labels of parameter lists (selector suffixes).

use rueca_extract::tree_sitter::Node;

pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

pub(crate) fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The first named child of `node` with kind `kind`.
pub(crate) fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// The first anonymous (keyword or operator) child of `node` with kind `kind`.
pub(crate) fn keyword_child<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| !c.is_named() && c.kind() == kind)
}

/// Whether `kind` is a type node.
pub(crate) fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "user_type"
            | "type_identifier"
            | "optional_type"
            | "array_type"
            | "dictionary_type"
            | "tuple_type"
            | "function_type"
            | "metatype"
            | "opaque_type"
            | "existential_type"
            | "protocol_composition_type"
            | "type_modifiers"
            | "bracket_qualified_type"
    )
}

/// Removes generic arguments (balanced `<…>`), `?`, `!` and whitespace
/// from a type text: `Foo.Bar<Int>?` → `Foo.Bar`.
pub(crate) fn strip_type_text(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            c if depth == 0 && !c.is_whitespace() && c != '?' && c != '!' => out.push(c),
            _ => {}
        }
    }
    out
}

/// Whether `s` is a dotted path of identifiers (`Foo`, `Foo.Bar`).
pub(crate) fn is_name_path(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_'))
}

/// The nominal type a type node names, generic arguments and optionality
/// removed: `Foo.Bar<Int>?` → `Foo.Bar`, `[Foo]` → `Array`, `[K: V]` →
/// `Dictionary`, `some P` / `any P` → `P`, `inout T` → `T`. `None` for
/// function, tuple and composition types.
pub(crate) fn class_of_type(ty: Node<'_>, source: &[u8]) -> Option<String> {
    let ty = unwrap_type(ty)?;
    match ty.kind() {
        "array_type" => Some("Array".to_string()),
        "dictionary_type" => Some("Dictionary".to_string()),
        "user_type" | "type_identifier" => {
            let s = strip_type_text(text(ty, source));
            is_name_path(&s).then_some(s)
        }
        _ => None,
    }
}

/// The nominal type inside optional, opaque, existential, metatype-free
/// and modifier wrappers.
pub(crate) fn unwrap_type(ty: Node<'_>) -> Option<Node<'_>> {
    let mut t = ty;
    for _ in 0..8 {
        match t.kind() {
            "optional_type" | "opaque_type" | "existential_type" | "type_modifiers" => {
                t = named_children(t).into_iter().rev().find(|c| is_type_node(c.kind()))?;
            }
            _ => return Some(t),
        }
    }
    None
}

/// The type node among the named children of a declaration (parameter,
/// type annotation), if any: the last one (a parameter's names come first).
pub(crate) fn declared_type(node: Node<'_>) -> Option<Node<'_>> {
    let holder = child_of_kind(node, "type_annotation").unwrap_or(node);
    named_children(holder).into_iter().rev().find(|c| is_type_node(c.kind()))
}

/// Whether `name` looks like a type (Swift naming convention: types start
/// upper-case).
pub(crate) fn is_type_like(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

/// The external label of a `parameter` node: its `external_name`, else its
/// name (`_` stays `_`).
pub(crate) fn parameter_label(param: Node<'_>, source: &[u8]) -> String {
    let names: Vec<Node<'_>> = named_children(param).into_iter().filter(|c| c.kind() == "simple_identifier").collect();
    names.first().map(|n| text(*n, source).trim_matches('`').to_string()).unwrap_or_else(|| "_".to_string())
}

/// The internal (bound) name of a `parameter` node.
pub(crate) fn parameter_name<'t>(param: Node<'t>) -> Option<Node<'t>> {
    named_children(param).into_iter().rfind(|c| c.kind() == "simple_identifier")
}

/// The selector suffix of a parameter list: `(at:_:)`, `()` without
/// parameters. `unlabeled` makes a parameter without an external name
/// unlabeled (`subscript(index: Int)` is `subscript(_:)`).
pub(crate) fn selector(decl: Node<'_>, source: &[u8], unlabeled: bool) -> String {
    let mut out = String::from("(");
    for p in named_children(decl).into_iter().filter(|c| c.kind() == "parameter") {
        let has_external = named_children(p).iter().filter(|c| c.kind() == "simple_identifier").count() > 1;
        let label = if unlabeled && !has_external { "_".to_string() } else { parameter_label(p, source) };
        out.push_str(&label);
        out.push(':');
    }
    out.push(')');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripping() {
        assert_eq!(strip_type_text("Foo.Bar<K, V>?"), "Foo.Bar");
        assert_eq!(strip_type_text("Box<Box<Int>>!"), "Box");
        assert!(is_type_like("Foo"));
        assert!(!is_type_like("foo"));
        assert!(is_name_path("A.B"));
        assert!(!is_name_path("a()"));
    }
}
