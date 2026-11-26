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

/// Every child of `node` (named or not) with its field name.
pub(crate) fn children_with_fields<'t>(node: Node<'t>) -> Vec<(Node<'t>, Option<&'t str>)> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            out.push((cursor.node(), cursor.field_name()));
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    out
}

/// Removes generic arguments (balanced `<…>`), a `global::` alias and
/// whitespace from a type text: `Dictionary<K, V>` → `Dictionary`,
/// `global::System.IO.Stream` → `System.IO.Stream`.
pub(crate) fn strip_type_text(t: &str) -> String {
    let t = t.trim();
    let t = t.strip_prefix("global::").unwrap_or(t);
    let mut out = String::with_capacity(t.len());
    let mut depth = 0i32;
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out.replace("::", ".")
}

/// C# keyword types (`predefined_type`), `void` and `var`.
pub(crate) fn is_predefined(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "byte"
            | "sbyte"
            | "char"
            | "decimal"
            | "double"
            | "float"
            | "int"
            | "uint"
            | "long"
            | "ulong"
            | "short"
            | "ushort"
            | "nint"
            | "nuint"
            | "object"
            | "string"
            | "void"
            | "var"
            | "dynamic"
    )
}

/// The class a type node names, generic arguments removed (`Foo`,
/// `Outer.Inner`, `System.IO.Stream`); `string` / `object` for those
/// keywords (the resolver treats them as builtins); `None` for value
/// keywords, `void`, `var`, arrays, tuples, pointers and function pointers.
/// A nullable reference or value type (`Foo?`) is its type.
pub(crate) fn class_of_type(ty: Node<'_>, source: &[u8]) -> Option<String> {
    match ty.kind() {
        "identifier" | "qualified_name" | "generic_name" | "alias_qualified_name" => {
            let t = strip_type_text(text(ty, source));
            (!t.is_empty() && !is_predefined(&t) && t != "dynamic").then_some(t)
        }
        "predefined_type" => {
            let t = text(ty, source);
            matches!(t, "string" | "object").then(|| t.to_string())
        }
        "nullable_type" | "ref_type" | "scoped_type" => class_of_type(ty.child_by_field_name("type")?, source),
        _ => None,
    }
}

/// Normalized parameter type for an overload suffix: generic arguments,
/// namespace qualifiers, nullable marks and parameter modifiers removed,
/// array ranks kept (`List<string>` → `List`, `System.IO.Stream` →
/// `Stream`, `int[]` → `int[]`, `string?` → `string`).
pub(crate) fn suffix_type(type_text: &str) -> String {
    let t = strip_type_text(type_text);
    if t.starts_with('(') {
        return t;
    }
    let (base, dims) = match t.find('[') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t.as_str(), ""),
    };
    let base = base.trim_end_matches('?');
    let base = base.rsplit('.').next().unwrap_or(base);
    let dims: String = dims.chars().filter(|c| *c != '?').collect();
    format!("{base}{dims}")
}

/// Whether a `parameter_list` child is the `params` modifier keyword.
fn is_params_keyword(n: Node<'_>) -> bool {
    !n.is_named() && n.kind() == "params"
}

/// Parameter types of a `parameter_list` (or `bracketed_parameter_list`)
/// for an overload suffix, joined with `,` (`string,int[]`). A `params T[]`
/// array is `T...` (variadic, as the resolver reads suffixes). The grammar
/// puts a `params` parameter's type and name directly in the list.
pub(crate) fn parameter_types(params: Node<'_>, source: &[u8]) -> String {
    let mut out = Vec::new();
    let mut variadic = false;
    for (c, field) in children_with_fields(params) {
        if is_params_keyword(c) {
            variadic = true;
            continue;
        }
        match (c.kind(), field) {
            ("parameter", _) => {
                let ty = c.child_by_field_name("type").map_or("", |t| text(t, source));
                let params_mod =
                    named_children(c).iter().any(|m| m.kind() == "modifier" && text(*m, source) == "params");
                out.push(variadic_type(&suffix_type(ty), params_mod));
            }
            (_, Some("type")) => out.push(variadic_type(&suffix_type(text(c, source)), variadic)),
            _ => {}
        }
    }
    out.join(",")
}

/// `T[]` as the variadic `T...` of a `params` parameter.
fn variadic_type(t: &str, variadic: bool) -> String {
    if !variadic {
        return t.to_string();
    }
    match t.strip_suffix("[]") {
        Some(elem) => format!("{elem}..."),
        None => format!("{t}..."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_texts() {
        assert_eq!(strip_type_text("Dictionary<K, List<V>>"), "Dictionary");
        assert_eq!(strip_type_text("global::System.IO.Stream"), "System.IO.Stream");
        assert_eq!(suffix_type("System.Collections.Generic.List<string>"), "List");
        assert_eq!(suffix_type("int [] []"), "int[][]");
        assert_eq!(suffix_type("string?"), "string");
        assert_eq!(suffix_type("Foo?[]"), "Foo[]");
        assert_eq!(suffix_type("(int a, string b)"), "(inta,stringb)");
        assert!(is_predefined("int") && is_predefined("var") && !is_predefined("Int32"));
    }
}
