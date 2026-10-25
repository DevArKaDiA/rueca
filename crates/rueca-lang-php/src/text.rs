//! Text helpers: one-line headers without comments, docblock cleaning,
//! truncation.

use rueca_extract::tree_sitter::Node;

/// Maximum length (chars) of signatures, decorators and receivers.
pub(crate) const MAX_ONE_LINE: usize = 200;
/// Maximum length (chars) of docblocks.
pub(crate) const MAX_DOC: usize = 1000;
/// Bytes scanned at most when building a one-line header, so a pathological
/// declaration (a 1 MiB parameter list) stays cheap.
const MAX_SCAN: usize = 8 * 1024;

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

/// Source text of `[start, end)` inside `node` with comment nodes and the
/// `skip` subtrees (attributes) removed, whitespace collapsed to single
/// spaces and bracket padding tidied, so a multi-line header becomes one
/// line: `function f(\n  int $a,\n) : X` → `function f(int $a) : X`.
pub(crate) fn one_line(node: Node<'_>, start: usize, end: usize, skip: &[Node<'_>], source: &[u8]) -> String {
    let end = end.min(source.len()).min(start.saturating_add(MAX_SCAN));
    let start = start.min(end);
    let mut cut: Vec<(usize, usize)> = comment_ranges(node, start, end);
    for s in skip {
        if s.start_byte() < end && s.end_byte() > start {
            cut.push((s.start_byte().max(start), s.end_byte().min(end)));
        }
    }
    cut.sort_unstable();
    let mut bytes = Vec::with_capacity(end - start);
    let mut pos = start;
    for (cs, ce) in cut {
        if cs > pos {
            bytes.extend_from_slice(&source[pos..cs]);
            bytes.push(b' ');
        }
        pos = pos.max(ce);
    }
    if pos < end {
        bytes.extend_from_slice(&source[pos..end]);
    }
    tidy(&String::from_utf8_lossy(&bytes))
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
        if overlaps && n.kind() != "comment" && cursor.goto_first_child() {
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

/// Collapses whitespace runs to one space and tidies `( a, )` → `(a)`.
pub(crate) fn tidy(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            let last = out.chars().next_back();
            if matches!(ch, ')' | ']') && out.ends_with(',') {
                out.pop();
            }
            let skip = matches!(last, Some('(' | '[')) || matches!(ch, ')' | ']' | ',');
            if !skip {
                out.push(' ');
            }
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

/// Cleans a `/** … */` docblock: drops the delimiters and the leading `*`
/// of each line, trims blank leading/trailing lines, truncates to
/// [`MAX_DOC`] chars. `None` for other comments or an empty block.
pub(crate) fn clean_docblock(raw: &str) -> Option<String> {
    let body = raw.strip_prefix("/**")?;
    let body = body.strip_suffix("*/").unwrap_or(body);
    let lines: Vec<&str> = body
        .lines()
        .map(|l| {
            let l = l.trim();
            let l = l.strip_prefix('*').unwrap_or(l);
            l.strip_prefix(' ').unwrap_or(l).trim_end()
        })
        .collect();
    let text = lines.join("\n");
    let text = text.trim();
    if text.is_empty() { None } else { Some(truncate_chars(text, MAX_DOC)) }
}

/// Raw text of the `/** … */` docblock of a declaration: the comment right
/// before it (only whitespace in between) or right after its attributes.
pub(crate) fn docblock_of(decl: Node<'_>, source: &[u8]) -> Option<String> {
    let text = |n: Node<'_>| String::from_utf8_lossy(&source[n.start_byte()..n.end_byte()]).into_owned();
    if let Some(prev) = decl.prev_named_sibling()
        && prev.kind() == "comment"
        && source[prev.end_byte()..decl.start_byte()].iter().all(u8::is_ascii_whitespace)
    {
        let t = text(prev);
        if t.starts_with("/**") {
            return Some(t);
        }
    }
    let mut cursor = decl.walk();
    for child in decl.named_children(&mut cursor) {
        match child.kind() {
            "attribute_list" => {}
            "comment" => {
                let t = text(child);
                return t.starts_with("/**").then_some(t);
            }
            _ => break,
        }
    }
    None
}

/// Docblock pseudo-types that never name a class.
const DOC_PSEUDO_TYPES: &[&str] = &[
    "array",
    "array-key",
    "bool",
    "boolean",
    "callable",
    "callable-string",
    "class-string",
    "double",
    "false",
    "float",
    "int",
    "integer",
    "iterable",
    "list",
    "mixed",
    "never",
    "non-empty-array",
    "non-empty-list",
    "non-empty-string",
    "null",
    "numeric",
    "numeric-string",
    "object",
    "positive-int",
    "resource",
    "scalar",
    "string",
    "true",
    "void",
];

/// The single class named by the type of a `@var` tag (property docblocks,
/// `var == None`) or the `@param` tag of `$var`: `\App\User`, `?User`,
/// `User|null`, `Collection<int, User>` → `Collection`. Unions of several
/// classes, arrays (`User[]`), pseudo-types and shapes give `None`.
pub(crate) fn doc_tag_class(docblock: &str, tag: &str, var: Option<&str>) -> Option<String> {
    for line in docblock.lines() {
        let line = line.trim().trim_start_matches(['/', '*']).trim();
        let Some(rest) = line.strip_prefix(tag) else { continue };
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let (ty, after) = split_type(rest.trim_start());
        if ty.is_empty() {
            continue;
        }
        let name = after.split_whitespace().next();
        match var {
            Some(v) if name.map(|n| n.trim_start_matches('&').trim_start_matches("...")) != Some(v) => continue,
            None if name.is_some_and(|n| n.starts_with('$')) && tag == "@param" => continue,
            _ => {}
        }
        return single_doc_class(ty);
    }
    None
}

/// Splits a docblock type (which may contain spaces inside `<…>`, `{…}`
/// or `(…)`) from the rest of the tag line.
fn split_type(s: &str) -> (&str, &str) {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '{' | '(' | '[' => depth += 1,
            '>' | '}' | ')' | ']' => depth -= 1,
            c if c.is_whitespace() && depth <= 0 => return (&s[..i], &s[i..]),
            _ => {}
        }
    }
    (s, "")
}

fn single_doc_class(ty: &str) -> Option<String> {
    let ty = ty.trim_start_matches('?');
    let ty = match ty.find('<') {
        Some(i) if ty.ends_with('>') => &ty[..i],
        Some(_) => return None,
        None => ty,
    };
    let parts: Vec<&str> = ty.split('|').filter(|p| !p.eq_ignore_ascii_case("null")).collect();
    let [class] = parts.as_slice() else { return None };
    let bare = class.trim_start_matches('\\');
    let valid = !bare.is_empty()
        && bare.split('\\').all(|seg| {
            let mut chars = seg.chars();
            chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
                && chars.all(|c| c == '_' || c.is_alphanumeric())
        });
    if !valid || DOC_PSEUDO_TYPES.binary_search(&bare.to_ascii_lowercase().as_str()).is_ok() {
        return None;
    }
    Some(class.to_string())
}

/// Value of a PHP string literal node's text when it has no interpolation:
/// quotes stripped, `\\` and the quote escape resolved. `None` for
/// heredocs or strings with `$` interpolation.
pub(crate) fn plain_string_value(text: &str) -> Option<String> {
    let quote = text.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let body = text.strip_prefix(quote)?.strip_suffix(quote)?;
    if quote == '"' && body.contains('$') {
        return None;
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if n == '\\' || n == quote => out.push(n),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_collapses() {
        assert_eq!(tidy("function f(\n    int $a,\n    $b,\n): X"), "function f(int $a, $b): X");
        assert_eq!(tidy("foo( a , b )"), "foo(a, b)");
        assert_eq!(tidy("[1, 2]"), "[1, 2]");
    }

    #[test]
    fn docblocks() {
        assert_eq!(
            clean_docblock("/**\n * Handles users.\n *\n * @test\n */").as_deref(),
            Some("Handles users.\n\n@test")
        );
        assert_eq!(clean_docblock("/** One line. */").as_deref(), Some("One line."));
        assert_eq!(clean_docblock("/* not a docblock */"), None);
        assert_eq!(clean_docblock("/** */"), None);
    }

    #[test]
    fn string_values() {
        assert_eq!(plain_string_value("'a\\'b'").as_deref(), Some("a'b"));
        assert_eq!(plain_string_value("\"x.php\"").as_deref(), Some("x.php"));
        assert_eq!(plain_string_value("\"$x.php\""), None);
        assert_eq!(plain_string_value("'C:\\\\dir'").as_deref(), Some("C:\\dir"));
    }

    #[test]
    fn docblock_types() {
        assert!(DOC_PSEUDO_TYPES.windows(2).all(|w| w[0] < w[1]));
        let doc = "/**\n * @param  \\Illuminate\\Container\\Container  $app\n * @param  string|null  $name\n * @param  ?Repo $repo\n * @param  User[]  $users\n * @param  A|B  $ab\n * @param  Collection<int, User>  $items\n * @var Logger\n */";
        assert_eq!(doc_tag_class(doc, "@param", Some("$app")).as_deref(), Some("\\Illuminate\\Container\\Container"));
        assert_eq!(doc_tag_class(doc, "@param", Some("$name")), None);
        assert_eq!(doc_tag_class(doc, "@param", Some("$repo")).as_deref(), Some("Repo"));
        assert_eq!(doc_tag_class(doc, "@param", Some("$users")), None);
        assert_eq!(doc_tag_class(doc, "@param", Some("$ab")), None);
        assert_eq!(doc_tag_class(doc, "@param", Some("$items")).as_deref(), Some("Collection"));
        assert_eq!(doc_tag_class(doc, "@param", Some("$missing")), None);
        assert_eq!(doc_tag_class(doc, "@var", None).as_deref(), Some("Logger"));
        assert_eq!(doc_tag_class("/** @var \\Foo\\Bar|null */", "@var", None).as_deref(), Some("\\Foo\\Bar"));
        assert_eq!(doc_tag_class("/** @var array<string, int> */", "@var", None), None);
        assert_eq!(doc_tag_class("/** @variable X */", "@var", None), None);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("héllo", 2), "hé…");
        assert_eq!(truncate_chars("hi", 2), "hi");
    }
}
