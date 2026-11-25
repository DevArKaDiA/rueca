//! Declared types: Sorbet `sig`s, RBS inline comments and `T.let`.
//!
//! Ruby has no declared types, but typed Ruby codebases write them in two
//! cheap-to-read forms, which Sorbet (and so the bench's gold) uses:
//!
//! - a Sorbet signature before a method: `sig { params(a: Foo, b:
//!   T.nilable(Bar)).returns(Baz) }` (also `sig do … end`, `sig(:final)`);
//! - an RBS inline comment before a method: `#: (Foo a, ?Bar b) -> Baz`
//!   (Sorbet's `--enable-experimental-rbs-comments`), and after an
//!   assignment: `@x = build #: Foo`.
//!
//! This module reads them as text and normalizes each type to a constant
//! path the resolver can look up (`T.nilable(X)` / `X?` → `X`), or to
//! nothing when the type has no single project class (unions, generics,
//! `T.untyped`, `void`, booleans). `self`, `instance`, `T.self_type` and
//! `T.attached_class` stand for the enclosing class ([`SELF_TYPE`]).

use std::collections::HashMap;

/// A normalized type standing for "the enclosing class" (`self`,
/// `instance`, `T.attached_class`); the extractor replaces it with the
/// enclosing namespace.
pub(crate) const SELF_TYPE: &str = "$self";

/// Types of a method's parameters (by name) and of its return value.
#[derive(Debug, Default, Clone)]
pub(crate) struct MethodTypes {
    pub(crate) params: HashMap<String, String>,
    /// Positional parameter types of an RBS signature without names, in order.
    pub(crate) positional: Vec<Option<String>>,
    pub(crate) returns: Option<String>,
}

/// Normalizes one written type to a constant path, `None` when it does not
/// name one class.
pub(crate) fn normalize(ty: &str) -> Option<String> {
    let t = ty.trim();
    if let Some(inner) = t.strip_prefix("T.nilable(").and_then(|r| r.strip_suffix(')')) {
        return normalize(inner);
    }
    let t = t.trim_end_matches('?').trim();
    match t {
        "self" | "instance" | "T.self_type" | "T.attached_class" | "attached_class" => {
            return Some(SELF_TYPE.to_string());
        }
        "" | "void" | "untyped" | "T.untyped" | "bool" | "boolish" | "T::Boolean" | "nil" | "NilClass" | "top"
        | "bot" | "T.noreturn" | "Object" | "BasicObject" => return None,
        _ => {}
    }
    let path = t.strip_prefix("::").unwrap_or(t);
    let ok = !path.is_empty()
        && path.split("::").all(|s| {
            s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    ok.then(|| t.to_string())
}

/// Splits `s` at top-level commas (outside `()`, `[]`, `{}`, `<>`).
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

/// The text between the `(` at byte `open` and its matching `)`.
fn balanced(s: &str, open: usize) -> Option<&str> {
    let mut depth = 0i32;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Reads a Sorbet `sig { … }` block's text.
pub(crate) fn parse_sig(text: &str) -> MethodTypes {
    let mut out = MethodTypes::default();
    if let Some(k) = find_word(text, "params(")
        && let Some(inner) = balanced(text, k + "params".len())
    {
        for part in split_top(inner) {
            if let Some((name, ty)) = part.split_once(':')
                && let Some(t) = normalize(ty)
            {
                out.params.insert(name.trim().to_string(), t);
            }
        }
    }
    if let Some(k) = find_word(text, "returns(")
        && let Some(inner) = balanced(text, k + "returns".len())
    {
        out.returns = normalize(inner);
    }
    out
}

/// Byte offset of `word` in `text` not preceded by an identifier character.
fn find_word(text: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(k) = text[from..].find(word) {
        let at = from + k;
        let prev = text[..at].chars().next_back();
        if !prev.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Some(at);
        }
        from = at + word.len();
    }
    None
}

/// Reads an RBS method type: `(Foo a, ?Bar b, *Baz, key: Qux) -> Ret`,
/// optionally after type parameters (`[T] (…) -> …`) and with a block
/// (`(…) { (X) -> void } -> Ret`).
pub(crate) fn parse_rbs_method(text: &str) -> Option<MethodTypes> {
    let t = text.trim();
    let t = if t.starts_with('[') { t.split_once(']').map_or(t, |(_, r)| r.trim_start()) } else { t };
    let mut out = MethodTypes::default();
    let rest = if t.starts_with('(') {
        let inner = balanced(t, 0)?;
        for part in split_top(inner) {
            rbs_param(part, &mut out);
        }
        &t[inner.len() + 2..]
    } else {
        t
    };
    let rest = rest.trim_start();
    let rest = if rest.starts_with('{') { skip_braces(rest)? } else { rest };
    let ret = rest.trim_start().strip_prefix("->")?;
    out.returns = normalize(ret);
    Some(out)
}

/// The text after a balanced `{ … }` at the start of `s`.
fn skip_braces(s: &str) -> Option<&str> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[i + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// One RBS parameter: `Type name`, `?Type name`, `*Type name`, `**Type
/// name`, `key: Type`, `?key: Type`.
fn rbs_param(part: &str, out: &mut MethodTypes) {
    let p = part.trim().trim_start_matches('?');
    if let Some((key, ty)) = p.split_once(": ")
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        if let Some(t) = normalize(ty) {
            out.params.insert(key.to_string(), t);
        }
        return;
    }
    if p.starts_with('*') {
        return;
    }
    // `Type name`: the name is the last word when it is lower-case.
    let (ty, name) = match p.rsplit_once(' ') {
        Some((ty, name)) if name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') => (ty, Some(name)),
        _ => (p, None),
    };
    let t = normalize(ty);
    if let (Some(n), Some(t)) = (name, t.clone()) {
        out.params.insert(n.to_string(), t);
    }
    out.positional.push(t);
}

/// The type of an RBS inline type assertion comment (`#: Foo`, `#: Foo?`).
pub(crate) fn rbs_comment_type(comment: &str) -> Option<String> {
    let t = comment.trim().strip_prefix("#:")?.trim();
    if t.starts_with('(') || t.starts_with("->") || t.contains("->") {
        return None;
    }
    normalize(t)
}

/// The type written in `T.let(expr, Type)` / `T.cast(expr, Type)`.
pub(crate) fn t_let_type(args: &str) -> Option<String> {
    let parts = split_top(args);
    if parts.len() != 2 {
        return None;
    }
    normalize(parts[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_types() {
        assert_eq!(normalize("Foo::Bar").as_deref(), Some("Foo::Bar"));
        assert_eq!(normalize("T.nilable(::Foo)").as_deref(), Some("::Foo"));
        assert_eq!(normalize("Foo?").as_deref(), Some("Foo"));
        assert_eq!(normalize("T.attached_class").as_deref(), Some(SELF_TYPE));
        for none in ["T.untyped", "void", "T::Array[Foo]", "Foo | Bar", "T.any(A, B)", "String?  | nil", "bool"] {
            assert_eq!(normalize(none), None, "{none}");
        }
    }

    #[test]
    fn reads_sigs() {
        let s = parse_sig("sig { params(a: Foo, b: T.nilable(Bar::Baz), c: T::Array[X]).returns(Qux) }");
        assert_eq!(s.params.get("a").map(String::as_str), Some("Foo"));
        assert_eq!(s.params.get("b").map(String::as_str), Some("Bar::Baz"));
        assert!(!s.params.contains_key("c"));
        assert_eq!(s.returns.as_deref(), Some("Qux"));
        assert_eq!(parse_sig("sig { void }").returns, None);
    }

    #[test]
    fn reads_rbs_method_types() {
        let m =
            parse_rbs_method("(RubyDocument document, ?GlobalState global_state, Integer, key: Dispatcher) -> Hover?")
                .unwrap();
        assert_eq!(m.params.get("document").map(String::as_str), Some("RubyDocument"));
        assert_eq!(m.params.get("global_state").map(String::as_str), Some("GlobalState"));
        assert_eq!(m.params.get("key").map(String::as_str), Some("Dispatcher"));
        assert_eq!(m.positional.len(), 3);
        assert_eq!(m.returns.as_deref(), Some("Hover"));
        assert_eq!(
            parse_rbs_method("-> Interface::HoverOptions").unwrap().returns.as_deref(),
            Some("Interface::HoverOptions")
        );
        assert_eq!(parse_rbs_method("[T] (T x) { (T) -> void } -> self").unwrap().returns.as_deref(), Some(SELF_TYPE));
        assert_eq!(rbs_comment_type("#: Prism::Node?").as_deref(), Some("Prism::Node"));
        assert_eq!(rbs_comment_type("#: as !nil"), None);
        assert_eq!(t_let_type("nil, T.nilable(Foo)").as_deref(), Some("Foo"));
    }
}
