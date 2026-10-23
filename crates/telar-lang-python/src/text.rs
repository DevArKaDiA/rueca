//! Text helpers: whitespace-collapsed node text, docstring cleaning.

use telar_extract::tree_sitter::Node;

/// Maximum length (chars) of signatures and receivers.
pub(crate) const MAX_ONE_LINE: usize = 200;
/// Maximum length (chars) of docstrings.
pub(crate) const MAX_DOC: usize = 1000;

/// Truncates `s` to at most `max` chars, appending `…` when cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => {
            let mut out = s[..idx].to_string();
            out.push('…');
            out
        }
        None => s.to_string(),
    }
}

/// Source text of `[start, end)` within `node`, with comment nodes removed,
/// whitespace runs collapsed to one space and bracket padding tidied, so a
/// multi-line header or receiver becomes one line:
/// `foo(\n  a,\n  b,\n)` → `foo(a, b)`.
pub(crate) fn one_line(node: Node<'_>, start: usize, end: usize, source: &[u8]) -> String {
    let end = end.min(source.len());
    let start = start.min(end);
    let slice = &source[start..end];
    let raw: String = if slice.contains(&b'#') && slice.contains(&b'\n') {
        let comments = comment_ranges(node, start, end);
        let mut bytes = Vec::with_capacity(slice.len());
        let mut pos = start;
        for (cs, ce) in comments {
            if cs > pos {
                bytes.extend_from_slice(&source[pos..cs]);
            }
            pos = pos.max(ce);
        }
        if pos < end {
            bytes.extend_from_slice(&source[pos..end]);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::from_utf8_lossy(slice).into_owned()
    };
    tidy(&raw)
}

/// Byte ranges of comment nodes under `node` that intersect `[start, end)`,
/// in source order. Iterative walk (no recursion).
fn comment_ranges(node: Node<'_>, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    loop {
        let n = cursor.node();
        let overlaps = n.start_byte() < end && n.end_byte() > start;
        if overlaps && n.kind() == "comment" {
            out.push((n.start_byte().max(start), n.end_byte().min(end)));
        }
        if overlaps && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() || cursor.node() == node {
                return out;
            }
        }
    }
}

/// Collapses whitespace and tidies `( a, )` → `(a)`.
pub(crate) fn tidy(raw: &str) -> String {
    // Line continuations (`\` + newline) are whitespace.
    let continued;
    let raw = if raw.contains("\\\n") || raw.contains("\\\r\n") {
        continued = raw.replace("\\\r\n", " ").replace("\\\n", " ");
        continued.as_str()
    } else {
        raw
    };
    let chars: Vec<char> = raw.chars().collect();
    let mut collapsed = String::with_capacity(raw.len());
    let mut pending_space = false;
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !collapsed.is_empty() {
            let last = collapsed.chars().next_back();
            // `a\n  .b` (attribute access split over lines) joins as `a.b`;
            // `[int, ...]` keeps its space.
            let attr_dot = ch == '.' && chars.get(i + 1).is_some_and(|c| c.is_alphabetic() || *c == '_');
            let after_attr_dot = last == Some('.')
                && collapsed.chars().rev().nth(1).is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ')' | ']'));
            let skip = matches!(last, Some('(' | '[' | '{'))
                || matches!(ch, ')' | ']' | '}' | ',')
                || attr_dot
                || after_attr_dot;
            // Multi-line trailing comma: `(\n a,\n)` → `(a)`; `(1,)` is kept.
            if matches!(ch, ')' | ']') && collapsed.ends_with(',') {
                collapsed.pop();
            }
            if !skip {
                collapsed.push(' ');
            }
        }
        pending_space = false;
        collapsed.push(ch);
    }
    collapsed
}

/// The value of a Python string literal node's text: prefix and quotes
/// stripped, no escape processing. `None` for bytes literals.
pub(crate) fn string_literal_value(text: &str) -> Option<&str> {
    let prefix_len = text.find(['"', '\'']).unwrap_or(0);
    let prefix = &text[..prefix_len];
    if prefix.chars().any(|c| !matches!(c, 'r' | 'R' | 'u' | 'U' | 'f' | 'F' | 'b' | 'B' | 't' | 'T')) {
        return None;
    }
    if prefix.contains(['b', 'B']) {
        return None;
    }
    let body = &text[prefix_len..];
    for q in ["\"\"\"", "'''", "\"", "'"] {
        if body.len() >= 2 * q.len() && body.starts_with(q) && body.ends_with(q) {
            return Some(&body[q.len()..body.len() - q.len()]);
        }
    }
    // Unterminated literal (parse error): strip the opening quote only.
    for q in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(rest) = body.strip_prefix(q) {
            return Some(rest);
        }
    }
    None
}

/// `inspect.cleandoc`: expand tabs, strip the first line's leading
/// whitespace, remove the common indentation of the remaining lines, drop
/// leading and trailing blank lines.
pub(crate) fn cleandoc(doc: &str) -> String {
    let expanded = doc.replace('\t', "        ");
    let lines: Vec<&str> = expanded.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let margin = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            out.push(line.trim_start().to_string());
        } else if line.trim().is_empty() {
            out.push(String::new());
        } else {
            // margin <= leading whitespace bytes, all ASCII-or-unicode whitespace;
            // use the char-safe path.
            let lead = line.len() - line.trim_start().len();
            let cut = margin.min(lead);
            let cut = if line.is_char_boundary(cut) { cut } else { lead };
            out.push(line[cut..].trim_end().to_string());
        }
    }
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// True for `UPPER_CASE` names (at least one letter, no lowercase letters).
pub(crate) fn is_upper_case(name: &str) -> bool {
    name.chars().any(|c| c.is_alphabetic()) && !name.chars().any(|c| c.is_lowercase())
}

/// True for a dotted identifier like `Foo` or `pkg.mod.Foo`.
pub(crate) fn is_dotted_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|part| {
            let mut chars = part.chars();
            matches!(chars.next(), Some(c) if c == '_' || c.is_alphabetic())
                && chars.all(|c| c == '_' || c.is_alphanumeric())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_collapses() {
        assert_eq!(tidy("def f(\n    self,\n    a,\n) -> X"), "def f(self, a) -> X");
        assert_eq!(tidy("foo( a , b )"), "foo(a, b)");
        assert_eq!(tidy("a \\\n  .b"), "a.b");
        assert_eq!(tidy("x=(1,), y: Callable[..., T], z: tuple[int, ...]"), "x=(1,), y: Callable[..., T], z: tuple[int, ...]");
    }

    #[test]
    fn literal_values() {
        assert_eq!(string_literal_value("\"\"\"doc\"\"\""), Some("doc"));
        assert_eq!(string_literal_value("r'x'"), Some("x"));
        assert_eq!(string_literal_value("b'x'"), None);
        assert_eq!(string_literal_value("\"\""), Some(""));
    }

    #[test]
    fn cleandoc_dedents() {
        let d = "  Summary.\n\n    Body line.\n      Indented.\n    ";
        assert_eq!(cleandoc(d), "Summary.\n\nBody line.\n  Indented.");
        assert_eq!(cleandoc("\n    Only.\n"), "Only.");
    }

    #[test]
    fn case_detection() {
        assert!(is_upper_case("MAX_SIZE"));
        assert!(is_upper_case("HTTP2"));
        assert!(!is_upper_case("_"));
        assert!(!is_upper_case("Max"));
        assert!(is_dotted_identifier("a.B_c"));
        assert!(!is_dotted_identifier("a..b"));
        assert!(!is_dotted_identifier("List[int]"));
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("héllo", 2), "hé…");
        assert_eq!(truncate_chars("hi", 2), "hi");
    }
}
