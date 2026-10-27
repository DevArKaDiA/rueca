//! Source text helpers shared by every format: byte offset → [`Span`]
//! conversion, comment docs, value previews and the secret rule (RFC 0003
//! §4.6).

use rueca_model::Span;

/// Newline offsets of a source, for byte offset → (line, column) conversion
/// (0-based lines, byte columns: the `Span` convention).
pub(crate) struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub(crate) fn new(src: &str) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(src.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i as u32 + 1));
        LineIndex { starts }
    }

    /// 0-based line of byte offset `at`.
    pub(crate) fn line_of(&self, at: usize) -> usize {
        match self.starts.binary_search(&(at as u32)) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub(crate) fn line_start(&self, line: usize) -> usize {
        self.starts.get(line).copied().unwrap_or(*self.starts.last().unwrap_or(&0)) as usize
    }

    pub(crate) fn line_count(&self) -> usize {
        self.starts.len()
    }

    pub(crate) fn span(&self, start: usize, end: usize) -> Span {
        let end = end.max(start);
        let (sl, el) = (self.line_of(start), self.line_of(end));
        Span {
            start_byte: start as u32,
            end_byte: end as u32,
            start_line: sl as u32,
            start_col: (start - self.line_start(sl)) as u32,
            end_line: el as u32,
            end_col: (end - self.line_start(el)) as u32,
        }
    }
}

/// Text of 0-based line `line` without its line terminator.
pub(crate) fn line_text<'s>(src: &'s str, lines: &LineIndex, line: usize) -> &'s str {
    let start = lines.line_start(line).min(src.len());
    let end = if line + 1 < lines.line_count() { lines.line_start(line + 1) } else { src.len() };
    src.get(start..end.max(start)).unwrap_or("").trim_end_matches(['\n', '\r'])
}

/// Comment syntax of a format, for docs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CommentStyle {
    /// `#` comments (YAML, TOML, `.env`, Dockerfile); trailing ones need
    /// whitespace before `#`.
    Hash,
    /// `#` and `;` full-line comments (INI); no trailing comments.
    Ini,
    /// `#` and `!` full-line comments (`.properties`); no trailing comments.
    Properties,
    /// `//` and `/* */` (JSONC).
    Slash,
    /// `#`, `//` and `/* */` (HCL).
    Hcl,
    /// No comments.
    None,
}

const MAX_DOC: usize = 500;

fn strip_line_comment(style: CommentStyle, t: &str) -> Option<&str> {
    let t = t.trim_start();
    let markers: &[&str] = match style {
        CommentStyle::Hash => &["#"],
        CommentStyle::Ini => &["#", ";"],
        CommentStyle::Properties => &["#", "!"],
        CommentStyle::Slash => &["//"],
        CommentStyle::Hcl => &["#", "//"],
        CommentStyle::None => &[],
    };
    markers.iter().find_map(|m| t.strip_prefix(m)).map(|r| r.trim_start_matches(['#', '/', '!', ';']).trim())
}

/// The comment block immediately above line `line` (no blank line in
/// between), plus a trailing comment on the same line after byte `after`.
pub(crate) fn doc_comment(
    src: &str,
    lines: &LineIndex,
    style: CommentStyle,
    line: usize,
    after: Option<usize>,
) -> Option<String> {
    if style == CommentStyle::None {
        return None;
    }
    let above = comments_above(src, lines, style, line);
    let mut doc = above.join("\n").trim().to_string();
    if let Some(after) = after
        && let Some(c) = trailing_comment(src, lines, style, line, after)
    {
        if !doc.is_empty() {
            doc.push('\n');
        }
        doc.push_str(&c);
    }
    let doc = doc.trim();
    (!doc.is_empty()).then(|| truncate(doc, MAX_DOC))
}

/// The comment lines (line comments, and `/* … */` blocks for C-like
/// styles) directly above `line`, top first, at most about 50.
fn comments_above(src: &str, lines: &LineIndex, style: CommentStyle, line: usize) -> Vec<String> {
    let blocks = matches!(style, CommentStyle::Slash | CommentStyle::Hcl);
    let mut above: Vec<String> = Vec::new();
    let mut l = line;
    let mut in_block = false;
    while l > 0 && above.len() < 50 {
        l -= 1;
        let t = line_text(src, lines, l).trim();
        // `/* … */` blocks ending on the line above.
        if blocks && (in_block || t.ends_with("*/")) {
            in_block = block_comment_line(t, &mut above);
            continue;
        }
        match strip_line_comment(style, t) {
            Some(c) if !t.is_empty() => above.push(c.to_string()),
            _ => break,
        }
    }
    above.reverse();
    above
}

/// A line of a `/* … */` block read bottom-up: its body joins `above`.
/// Whether the block continues above (the line does not open it).
fn block_comment_line(t: &str, above: &mut Vec<String>) -> bool {
    let open = t.starts_with("/*");
    let body = t.trim_start_matches("/*").trim_end_matches("*/").trim().trim_start_matches('*').trim();
    if !body.is_empty() {
        above.push(body.to_string());
    }
    !open
}

/// A trailing comment on `line`, starting after byte `after` and outside
/// quotes.
fn trailing_comment(src: &str, lines: &LineIndex, style: CommentStyle, line: usize, after: usize) -> Option<String> {
    let start = lines.line_start(line);
    let text = line_text(src, lines, line);
    let from = after.saturating_sub(start).min(text.len());
    let rest = text.get(from..)?;
    let bytes = rest.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' && q == b'"' {
                    i += 1;
                } else if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'#' if matches!(style, CommentStyle::Hash | CommentStyle::Hcl)
                    && (i == 0 || bytes[i - 1].is_ascii_whitespace()) =>
                {
                    return Some(rest[i + 1..].trim().to_string()).filter(|s| !s.is_empty());
                }
                b'/' if matches!(style, CommentStyle::Slash | CommentStyle::Hcl) && bytes.get(i + 1) == Some(&b'/') => {
                    return Some(rest[i + 2..].trim().to_string()).filter(|s| !s.is_empty());
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// `text` cut to `max` chars with an ellipsis.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let mut s: String = text.chars().take(max).collect();
        s.push('…');
        s
    } else {
        text.to_string()
    }
}

/// First line of `text`, trimmed, ≤ 80 chars (`…` when cut or when more
/// lines follow).
pub(crate) fn preview(text: &str) -> String {
    let t = text.trim();
    let mut lines = t.lines();
    let first = lines.next().unwrap_or("").trim_end();
    let more = lines.next().is_some();
    let mut s = truncate(first, 80);
    if more && !s.ends_with('…') {
        s.push('…');
    }
    s
}

/// Mask used instead of a value that must not be copied into the index.
pub(crate) const MASK: &str = "•••";

/// Whether a key name looks like it holds a secret (RFC 0003 §4.6:
/// `(?i)pass(word)?|secret|token|api[_-]?key|private[_-]?key|credential`,
/// unanchored).
pub(crate) fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("pass")
        || n.contains("secret")
        || n.contains("token")
        || n.contains("credential")
        || ["apikey", "api_key", "api-key", "privatekey", "private_key", "private-key"].iter().any(|p| n.contains(p))
}

/// Byte offset of char index `ci` in `src` (YAML events count chars).
pub(crate) struct CharOffsets {
    /// `None` for ASCII sources (char index = byte index).
    table: Option<Vec<u32>>,
    len: usize,
}

impl CharOffsets {
    pub(crate) fn new(src: &str) -> CharOffsets {
        let table = (!src.is_ascii()).then(|| {
            let mut t: Vec<u32> = src.char_indices().map(|(b, _)| b as u32).collect();
            t.push(src.len() as u32);
            t
        });
        CharOffsets { table, len: src.len() }
    }

    pub(crate) fn byte(&self, ci: usize) -> usize {
        match &self.table {
            None => ci.min(self.len),
            Some(t) => t.get(ci).copied().map_or(self.len, |b| b as usize),
        }
    }
}

/// Environment variable names read by `${NAME}` / `${NAME:-x}` / `$NAME`
/// interpolations (compose, `.env`, shell-like values), with their byte
/// range inside `text`. `$$` is an escape.
pub(crate) fn interpolations(text: &str) -> Vec<(String, std::ops::Range<usize>)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' {
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'$') {
            i += 2;
            continue;
        }
        let (found, next) = interpolation_at(text, i);
        out.extend(found);
        i = next;
    }
    out
}

/// The `$NAME` / `${NAME…}` interpolation at the `$` at byte `i`, if valid,
/// and the byte where scanning resumes.
fn interpolation_at(text: &str, i: usize) -> (Option<(String, std::ops::Range<usize>)>, usize) {
    let b = text.as_bytes();
    let (start, braced) = if b.get(i + 1) == Some(&b'{') { (i + 2, true) } else { (i + 1, false) };
    let mut j = start;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    let valid = j > start && !b[start].is_ascii_digit();
    let closed = !braced || matches!(b.get(j), Some(b'}' | b':' | b'-' | b'?' | b'+'));
    let found = (valid && closed).then(|| {
        let end = if braced { text[j..].find('}').map_or(j, |k| j + k + 1) } else { j };
        (text[start..j].to_string(), i..end)
    });
    (found, j.max(i + 1))
}

/// Whether `name` is a plausible environment variable name.
pub(crate) fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_docs() {
        let src = "# about a\n# more\na: 1 # trailing\n\nb: 2\n";
        let li = LineIndex::new(src);
        let s = li.span(17, 21);
        assert_eq!((s.start_line, s.start_col, s.end_line, s.end_col), (2, 0, 2, 4));
        assert_eq!(doc_comment(src, &li, CommentStyle::Hash, 2, Some(21)).as_deref(), Some("about a\nmore\ntrailing"));
        assert_eq!(doc_comment(src, &li, CommentStyle::Hash, 4, Some(38)), None);
        let js = "{\n  /* block\n   * doc */\n  \"a\": 1, // t\n}";
        let li = LineIndex::new(js);
        assert_eq!(doc_comment(js, &li, CommentStyle::Slash, 3, Some(31)).as_deref(), Some("block\ndoc\nt"));
    }

    #[test]
    fn secrets_and_interpolation() {
        assert!(is_secret_name("DATABASE_PASSWORD") && is_secret_name("apiKey") && is_secret_name("GITHUB_TOKEN"));
        assert!(!is_secret_name("DATABASE_URL") && !is_secret_name("port"));
        let v = interpolations("postgres://${DB_HOST:-db}:$PORT/x $$HOME ${1x} $");
        let names: Vec<&str> = v.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["DB_HOST", "PORT"]);
        assert_eq!(&"postgres://${DB_HOST:-db}"[v[0].1.clone()], "${DB_HOST:-db}");
        assert_eq!(preview("  a\nb"), "a…");
        let co = CharOffsets::new("é: x");
        assert_eq!(co.byte(1), 2);
    }
}
