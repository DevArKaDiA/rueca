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

/// Removes generic arguments (balanced `<…>`), annotations (`@A`,
/// `@A(…)`) and whitespace from a type text: `Map.Entry<K, V>` →
/// `Map.Entry`, `@NonNull List<String>` → `List`.
pub(crate) fn strip_type_text(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut depth = 0i32;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '@' if depth == 0 => {
                // Skip the annotation name and its arguments.
                while chars.peek().is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '$') {
                    chars.next();
                }
                if chars.peek() == Some(&'(') {
                    let mut paren = 0i32;
                    for c in chars.by_ref() {
                        match c {
                            '(' => paren += 1,
                            ')' => {
                                paren -= 1;
                                if paren == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

/// Java primitive type names and `void` / `var`.
pub(crate) fn is_primitive(name: &str) -> bool {
    matches!(name, "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double" | "void" | "var")
}

/// The class a type node names, generic arguments removed (`Foo`,
/// `Map.Entry`, `com.x.Foo`); `None` for primitives, `void`, `var` and
/// arrays.
pub(crate) fn class_of_type(ty: Node<'_>, source: &[u8]) -> Option<String> {
    match ty.kind() {
        "type_identifier" => {
            let t = text(ty, source);
            (!is_primitive(t) && !t.is_empty()).then(|| t.to_string())
        }
        "scoped_type_identifier" | "generic_type" => {
            let t = strip_type_text(text(ty, source));
            (!t.is_empty() && !t.contains('[')).then_some(t)
        }
        "annotated_type" => {
            let inner =
                named_children(ty).into_iter().find(|c| !matches!(c.kind(), "annotation" | "marker_annotation"))?;
            class_of_type(inner, source)
        }
        _ => None,
    }
}

/// Normalized parameter type for an overload suffix: generic arguments,
/// annotations and package qualifiers removed, array dimensions kept
/// (`List<String>` → `List`, `java.util.Map.Entry<K, V>` → `Entry`,
/// `int[]` → `int[]`, `String...` → `String...`).
pub(crate) fn suffix_type(type_text: &str) -> String {
    let t = strip_type_text(type_text);
    let (base, dims) = match t.find('[') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t.as_str(), ""),
    };
    let base = base.rsplit('.').next().unwrap_or(base);
    format!("{base}{dims}")
}

/// Parameter types of a `formal_parameters` node for an overload suffix,
/// joined with `,` (`String,int[]`, `Object...`). The receiver parameter
/// (`Foo this`) is not a parameter.
pub(crate) fn parameter_types(params: Node<'_>, source: &[u8]) -> String {
    let mut out = Vec::new();
    for p in named_children(params) {
        match p.kind() {
            "formal_parameter" => {
                let ty = p.child_by_field_name("type").map_or("", |t| text(t, source));
                let mut s = suffix_type(ty);
                if let Some(d) = p.child_by_field_name("dimensions") {
                    s.push_str(&strip_type_text(text(d, source)));
                }
                out.push(s);
            }
            "spread_parameter" => {
                let ty = named_children(p)
                    .into_iter()
                    .find(|c| {
                        !matches!(c.kind(), "modifiers" | "variable_declarator" | "annotation" | "marker_annotation")
                    })
                    .map_or("", |t| text(t, source));
                out.push(format!("{}...", suffix_type(ty)));
            }
            _ => {}
        }
    }
    out.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_texts() {
        assert_eq!(strip_type_text("Map.Entry<K, V>"), "Map.Entry");
        assert_eq!(strip_type_text("@NonNull List<Map<String, int[]>>"), "List");
        assert_eq!(strip_type_text("java.util. List < String >"), "java.util.List");
        assert_eq!(strip_type_text("@A(x = \"y\") Foo"), "Foo");
        assert_eq!(suffix_type("java.util.Map.Entry<K, V>"), "Entry");
        assert_eq!(suffix_type("int [] []"), "int[][]");
        assert_eq!(suffix_type("List<String>[]"), "List[]");
        assert!(is_primitive("int") && is_primitive("var") && !is_primitive("Integer"));
    }
}
