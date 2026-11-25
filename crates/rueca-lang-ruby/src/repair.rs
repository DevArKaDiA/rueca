//! Same-length source repairs for syntax `tree-sitter-ruby` 0.23 does not
//! parse.
//!
//! An endless method whose body is a command call without parentheses
//! (`def reset = raise NotImplementedError, "abstract"`, valid since Ruby
//! 3.1 and common in Sorbet-typed code for abstract methods) is an ERROR
//! that swallows the rest of the class. The repair rewrites such a line to
//! the call with parentheses in the same number of bytes, `def reset
//! =raise(NotImplementedError, "abstract")`: the space after `=` is
//! dropped, the space after the method name becomes `(`, and `)` is
//! appended. Every byte offset outside the rewritten line, and every line
//! and column of the file, stays the same, so spans need no mapping.
//!
//! The repair runs only when the first parse has errors, and its tree is
//! kept only when it has fewer errors.

/// Tokens that end an expression rather than start an argument.
const NOT_ARGUMENT_START: &[u8] = b"=+-*/<>|&?:.[%!^,)}";
const KEYWORDS: &[&str] = &["if", "unless", "and", "or", "rescue", "while", "until", "do", "then", "end", "in"];

/// The rewritten source, or `None` when no line needed a repair.
pub(crate) fn repair(source: &[u8]) -> Option<Vec<u8>> {
    let mut out = source.to_vec();
    let mut changed = false;
    let mut start = 0;
    while start < source.len() {
        let end = source[start..].iter().position(|&b| b == b'\n').map_or(source.len(), |k| start + k);
        if let Some(fixed) = repair_line(&source[start..end]) {
            out[start..end].copy_from_slice(&fixed);
            changed = true;
        }
        start = end + 1;
    }
    changed.then_some(out)
}

/// One line `… def <header> = <name> <args>` → `… def <header> =<name>(<args>)`.
fn repair_line(line: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(line).ok()?;
    let trimmed = text.trim_start();
    let def_at = ["def ", "private def ", "protected def ", "public def "]
        .iter()
        .find(|p| trimmed.starts_with(*p))
        .map(|p| text.len() - trimmed.len() + p.len() - 4)?;
    if text.contains('#') || text.trim_end().ends_with(',') {
        return None;
    }
    let eq = endless_eq(text, def_at)?;
    // `= name args`: the body starts with an identifier and one space.
    let body = eq + 2;
    let name_len = text[body..].bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
    if name_len == 0 || !text.as_bytes()[body].is_ascii_lowercase() {
        return None;
    }
    let name = &text[body..body + name_len];
    let after = body + name_len;
    let bytes = text.as_bytes();
    if KEYWORDS.contains(&name) || bytes.get(after) != Some(&b' ') {
        return None;
    }
    let next = *bytes.get(after + 1)?;
    if next == b' ' || NOT_ARGUMENT_START.contains(&next) || starts_keyword(&text[after + 1..]) {
        return None;
    }
    let content_end = text.trim_end().len();
    let mut fixed = Vec::with_capacity(line.len());
    fixed.extend_from_slice(&bytes[..eq + 1]);
    fixed.extend_from_slice(name.as_bytes());
    fixed.push(b'(');
    fixed.extend_from_slice(&bytes[after + 1..content_end]);
    fixed.push(b')');
    fixed.extend_from_slice(&bytes[content_end..]);
    (fixed.len() == line.len()).then_some(fixed)
}

/// Byte offset of the ` = ` that makes the `def` at `def_at` endless: after
/// the method name and its balanced parameter list.
fn endless_eq(text: &str, def_at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = def_at + 4;
    let mut depth = 0i32;
    while i + 2 < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 && bytes[i + 1] == b'=' && bytes[i + 2] == b' ' => return Some(i + 1),
            b'"' | b'\'' if depth == 0 => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

fn starts_keyword(s: &str) -> bool {
    let word: String = s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    KEYWORDS.contains(&word.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(s: &str) -> Option<String> {
        repair(s.as_bytes()).map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn repairs_endless_command_calls() {
        let src = "class A\n  def reset = raise NotImplementedError, \"x\"\n  def m(a, b:) = raise E\nend\n";
        let got = fix(src).unwrap();
        assert_eq!(got.len(), src.len());
        assert!(got.contains("def reset =raise(NotImplementedError, \"x\")\n"));
        assert!(got.contains("def m(a, b:) =raise(E)\n"));
    }

    #[test]
    fn leaves_other_lines() {
        for s in [
            "def x = 1\n",
            "def x = a + b\n",
            "def x = foo if bar\n",
            "def x = foo(1)\n",
            "x = raise E, 'a'\n",
            "def x = raise E # comment\n",
            "def ==(o) = o\n",
        ] {
            assert_eq!(fix(s), None, "{s}");
        }
    }
}
