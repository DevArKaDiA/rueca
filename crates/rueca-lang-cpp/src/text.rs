//! Text helpers: node text, signatures, doc comments and the normalized
//! parameter lists used as overload suffixes.

use std::borrow::Cow;

use rueca_extract::tree_sitter::{Node, TreeCursor};

/// Longest signature kept, in characters.
pub(crate) const MAX_SIGNATURE: usize = 200;
/// Longest doc comment kept, in characters.
pub(crate) const MAX_DOC: usize = 1000;

/// UTF-8 text of a node (lossy).
pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> Cow<'s, str> {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()])
}

/// Whitespace runs collapsed to one space, trimmed.
pub(crate) fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncates to `max` characters with an ellipsis.
pub(crate) fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// A type or parameter text with whitespace collapsed and no spaces around
/// `*`, `&`, `,`, brackets and angle brackets where C++ does not need them:
/// `const std::string &` → `const std::string&`, `int (*) (int)` →
/// `int (*)(int)`.
pub(crate) fn tidy(s: &str) -> String {
    let c = collapse(s);
    let chars: Vec<char> = c.chars().collect();
    let mut out = String::with_capacity(c.len());
    for (i, &ch) in chars.iter().enumerate() {
        if ch == ' ' {
            let prev = out.chars().last().unwrap_or(' ');
            let next = chars.get(i + 1).copied().unwrap_or(' ');
            if matches!(prev, '(' | '<' | '[' | ',') || matches!(next, '*' | '&' | ',' | ')' | '>' | '[' | ']' | '(') {
                if prev == ',' {
                    out.push(' ');
                }
                continue;
            }
        }
        if ch == ',' {
            out.push(',');
            out.push(' ');
            continue;
        }
        out.push(ch);
    }
    collapse(&out.replace(" ,", ","))
}

/// Text of `node` without the byte ranges in `exclude` (sorted, inside the
/// node).
pub(crate) fn text_without(node: Node<'_>, source: &[u8], exclude: &[(usize, usize)]) -> String {
    let (start, end) = (node.start_byte(), node.end_byte());
    let mut out = Vec::with_capacity(end - start);
    let mut at = start;
    for &(a, b) in exclude {
        let (a, b) = (a.clamp(at, end), b.clamp(at, end));
        out.extend_from_slice(&source[at..a]);
        out.push(b' ');
        at = b;
    }
    out.extend_from_slice(&source[at..end]);
    String::from_utf8_lossy(&out).into_owned()
}

/// Comment byte ranges inside `node` that start before `limit` (iterative:
/// deeply nested code cannot overflow the stack).
fn comment_ranges(node: Node<'_>, limit: usize, out: &mut Vec<(usize, usize)>) {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let n = cursor.node();
        let descend = n.start_byte() < limit && n.child_count() > 0;
        if n.kind() == "comment" && n.start_byte() < limit {
            out.push((n.start_byte(), n.end_byte()));
        }
        if descend && n.kind() != "comment" && cursor.goto_first_child() {
            continue;
        }
        if !next_before(&mut cursor, node, limit) {
            return;
        }
    }
}

/// Moves `cursor` to the next sibling of its node, or of its nearest
/// ancestor below `root`, when the node it leaves starts before `limit`;
/// `false` when there is none.
fn next_before(cursor: &mut TreeCursor<'_>, root: Node<'_>, limit: usize) -> bool {
    loop {
        if cursor.node().start_byte() < limit && cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() || cursor.node().id() == root.id() {
            return false;
        }
    }
}

/// Moves `cursor` to the next sibling of its node or of the nearest
/// ancestor that has one (the next node in document order after the
/// node's subtree); `false` past the last node.
pub(crate) fn next_in_preorder(cursor: &mut TreeCursor<'_>) -> bool {
    loop {
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
    }
}

/// One-line signature: the source from `node`'s start to `until`'s start
/// (or `node`'s end), comments removed, whitespace collapsed, trailing `;`
/// / `{` / `=` dropped, at most [`MAX_SIGNATURE`] characters.
pub(crate) fn signature(node: Node<'_>, until: Option<Node<'_>>, source: &[u8]) -> Option<String> {
    let end = until.map_or(node.end_byte(), |u| u.start_byte()).max(node.start_byte());
    let mut comments = Vec::new();
    comment_ranges(node, end, &mut comments);
    comments.sort_unstable();
    let mut out = Vec::new();
    let mut at = node.start_byte();
    for (a, b) in comments {
        if a < at {
            continue;
        }
        out.extend_from_slice(&source[at..a]);
        out.push(b' ');
        at = b.min(end);
    }
    if at < end {
        out.extend_from_slice(&source[at..end]);
    }
    let s = collapse(&String::from_utf8_lossy(&out));
    let s = s.trim_end_matches([';', '{', ' ', '=']).trim_end().to_string();
    (!s.is_empty()).then(|| truncate(s, MAX_SIGNATURE))
}

/// The doc comment of a definition: the comments right before `node` (no
/// blank line in between, not trailing code on their own line), `///`,
/// `//!`, `//`, `/** */`, `/*! */` and `/* */` markers and leading `*`
/// stripped, at most [`MAX_DOC`] characters.
pub(crate) fn doc_of(node: Node<'_>, source: &[u8]) -> Option<String> {
    let comments = doc_comments(node, source);
    if comments.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for c in comments.into_iter().rev() {
        comment_lines(&text(c, source), &mut lines);
    }
    let doc = lines.join("\n").trim().to_string();
    (!doc.is_empty()).then(|| truncate(doc, MAX_DOC))
}

/// The comments right before `node`, nearest first: no blank line in
/// between, not a member doc (`///<`) and not trailing code.
fn doc_comments<'t>(node: Node<'t>, source: &[u8]) -> Vec<Node<'t>> {
    let mut comments = Vec::new();
    let mut next_row = node.start_position().row;
    let mut cur = node;
    while let Some(p) = cur.prev_sibling() {
        if p.kind() != "comment" || p.end_position().row + 1 < next_row {
            break;
        }
        let t = text(p, source);
        if t.starts_with("///<") || t.starts_with("//!<") || t.starts_with("/**<") || t.starts_with("/*!<") {
            break;
        }
        if trails_code(p) {
            break;
        }
        comments.push(p);
        next_row = p.start_position().row;
        cur = p;
    }
    comments
}

/// Whether comment `p` trails the code of its line, which it belongs to
/// (a preprocessor line ends after its newline: column 0 of the next).
fn trails_code(p: Node<'_>) -> bool {
    let Some(pp) = p.prev_sibling().filter(|pp| pp.kind() != "comment") else { return false };
    let end = pp.end_position();
    let end_row = if end.column == 0 && end.row > pp.start_position().row { end.row - 1 } else { end.row };
    end_row == p.start_position().row
}

/// Appends the lines of comment `t`, markers and leading `*` stripped.
fn comment_lines(t: &str, lines: &mut Vec<String>) {
    if let Some(rest) = t.strip_prefix("//") {
        let rest = rest.strip_prefix(['/', '!']).unwrap_or(rest);
        lines.push(rest.strip_prefix(' ').unwrap_or(rest).trim_end().to_string());
        return;
    }
    let body = t.strip_prefix("/*").unwrap_or(t);
    let body = body.strip_suffix("*/").unwrap_or(body);
    let body = body.strip_prefix(['*', '!']).unwrap_or(body);
    for line in body.lines() {
        let l = line.trim();
        let l = l.strip_prefix('*').unwrap_or(l);
        lines.push(l.strip_prefix(' ').unwrap_or(l).trim_end().to_string());
    }
}

/// The identifier a declarator binds (`*name`, `&name`, `name[4]`,
/// `(*name)(int)`, `name = 1`, `...name`), descending through declarator
/// wrappers. Also `field_identifier`, `type_identifier` (typedefs) and
/// `qualified_identifier` (`int Foo::count = 0;`).
pub(crate) fn declarator_ident(mut node: Node<'_>) -> Option<Node<'_>> {
    for _ in 0..32 {
        match node.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "qualified_identifier"
            | "operator_name"
            | "destructor_name" => return Some(node),
            "pointer_declarator"
            | "array_declarator"
            | "init_declarator"
            | "attributed_declarator"
            | "function_declarator" => node = node.child_by_field_name("declarator")?,
            "reference_declarator" | "parenthesized_declarator" | "variadic_declarator" => {
                let mut cursor = node.walk();
                node = node.named_children(&mut cursor).find(|c| c.kind() != "comment")?;
            }
            _ => return None,
        }
    }
    None
}

/// Normalized parameter types of a `parameter_list`, for overload suffixes:
/// names, default values, attributes and comments removed, whitespace
/// tidied, `(void)` as empty: `(const std::string& name, int n = 0)` →
/// `const std::string&, int`.
pub(crate) fn parameter_types(params: Node<'_>, source: &[u8]) -> String {
    let mut out = Vec::new();
    let mut cursor = params.walk();
    for p in params.children(&mut cursor) {
        match p.kind() {
            // C++ writes a C variadic as an anonymous `...` token.
            "..." => {
                out.push("...".to_string());
                continue;
            }
            _ if !p.is_named() => continue,
            "comment" => continue,
            "variadic_parameter" => {
                out.push("...".to_string());
                continue;
            }
            _ => {}
        }
        let mut exclude = Vec::new();
        comment_ranges(p, p.end_byte(), &mut exclude);
        let mut inner = p.walk();
        for c in p.children(&mut inner) {
            if c.kind() == "attribute_declaration" {
                exclude.push((c.start_byte(), c.end_byte()));
            }
            if c.kind() == "=" {
                exclude.push((c.start_byte(), p.end_byte()));
            }
        }
        if let Some(d) = p.child_by_field_name("declarator")
            && let Some(id) = declarator_ident(d)
            && id.kind() == "identifier"
        {
            exclude.push((id.start_byte(), id.end_byte()));
        }
        exclude.sort_unstable();
        out.push(tidy(&text_without(p, source, &exclude)));
    }
    if out.len() == 1 && out[0] == "void" {
        return String::new();
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_types() {
        assert_eq!(tidy("const std::string &"), "const std::string&");
        assert_eq!(tidy("char * const"), "char* const");
        assert_eq!(tidy("std::map< int ,  std::vector<int> >"), "std::map<int, std::vector<int>>");
        assert_eq!(tidy("int ( * ) ( int )"), "int(*)(int)");
        assert_eq!(tidy("unsigned   long"), "unsigned long");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abc".into(), 5), "abc");
        assert_eq!(truncate("abcdef".into(), 3), "abc…");
    }
}
