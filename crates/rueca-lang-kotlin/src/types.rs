//! Type texts: the class a type node names (for `$type:` receivers) and the
//! normalized parameter types of overload suffixes.

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

/// Whether `node` has an anonymous (keyword) child `keyword`.
pub(crate) fn has_keyword(node: Node<'_>, keyword: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| !c.is_named() && c.kind() == keyword)
}

/// Whether `kind` is a type node (`user_type`, `nullable_type`, …).
pub(crate) fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "user_type"
            | "nullable_type"
            | "not_nullable_type"
            | "function_type"
            | "parenthesized_type"
            | "parenthesized_user_type"
            | "type_modifiers"
    )
}

/// Removes generic arguments (balanced `<…>`), annotations, `?` and
/// whitespace from a type text: `Map.Entry<K, V>?` → `Map.Entry`.
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

/// The class a type node names, generic arguments and nullability removed
/// (`List<Foo>?` → `List`, `a.b.Foo` → `a.b.Foo`); `None` for function
/// types and anything that is not a plain (possibly qualified) name.
pub(crate) fn class_of_type(ty: Node<'_>, source: &[u8]) -> Option<String> {
    let ty = unwrap_type(ty)?;
    if ty.kind() != "user_type" {
        return None;
    }
    let s = strip_type_text(text(ty, source));
    let ok = !s.is_empty() && s.split('.').all(|seg| seg.chars().all(|c| c.is_alphanumeric() || c == '_'));
    ok.then_some(s)
}

/// The `user_type` / `function_type` inside nullable, parenthesized and
/// annotated type wrappers.
pub(crate) fn unwrap_type(ty: Node<'_>) -> Option<Node<'_>> {
    let mut t = ty;
    for _ in 0..8 {
        match t.kind() {
            "nullable_type" | "parenthesized_type" | "parenthesized_user_type" | "not_nullable_type" => {
                t = named_children(t).into_iter().find(|c| is_type_node(c.kind()))?;
            }
            "type_modifiers" => return None,
            _ => return Some(t),
        }
    }
    None
}

/// A type in overload-suffix form: generic arguments, nullability and
/// package qualifiers removed (`kotlin.collections.List<Foo>?` → `List`);
/// function types become `Function`.
pub(crate) fn suffix_type(ty: Node<'_>, source: &[u8]) -> String {
    let Some(t) = unwrap_type(ty) else { return "?".to_string() };
    if t.kind() == "function_type" {
        return "Function".to_string();
    }
    let s = strip_type_text(text(t, source));
    s.rsplit('.').next().unwrap_or(&s).to_string()
}

/// The type node among the named children of a declaration (parameter,
/// variable declaration, `catch`), if any.
pub(crate) fn declared_type(node: Node<'_>) -> Option<Node<'_>> {
    named_children(node).into_iter().find(|c| is_type_node(c.kind()))
}

/// Whether `name` looks like a type (Kotlin naming convention: classes
/// start upper-case).
pub(crate) fn is_type_like(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripping() {
        assert_eq!(strip_type_text("Map.Entry<K, V>?"), "Map.Entry");
        assert_eq!(strip_type_text("List<List<Int>>"), "List");
        assert!(is_type_like("Foo"));
        assert!(!is_type_like("foo"));
    }
}
