//! The key-path grammar of RFC 0003 §4.3: a definition's local qualified
//! name in a data file.
//!
//! | Element | Syntax | Example |
//! |---|---|---|
//! | mapping key | bare segment, joined with `.` | `database.host` |
//! | key that needs quoting | `"…"` with JSON string escapes, when the key is empty, contains `.`, `[`, `]`, `"`, `\`, whitespace or control characters, or starts with `{` | `paths."/v1/users.{format}".get` |
//! | array element (mappings only) | `[N]`, 0-based | `jobs.test.steps[2].run` |
//! | identity-keyed list element | `["name"]` | `containers["web"].env["DATABASE_URL"]` |
//! | document of a multi-document stream | `{N}` at the root | `{1}.spec.replicas` |
//! | kind-named document | the kind identity | `Deployment/web.spec.replicas` |
//!
//! [`format()`] and [`parse()`] round-trip (tested).

/// One segment of a key path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// A mapping key (bare or quoted when formatted).
    Key(String),
    /// `[N]`: the N-th element of an array (0-based).
    Index(usize),
    /// `["id"]`: a list element keyed by its identity field.
    Id(String),
    /// `{N}`: the N-th document of a multi-document stream.
    Doc(usize),
    /// A kind identity written verbatim (`Deployment/web`); only at the root.
    Raw(String),
}

/// Whether a mapping key must be quoted.
pub fn needs_quotes(key: &str) -> bool {
    key.is_empty()
        || key.starts_with('{')
        || key.chars().any(|c| matches!(c, '.' | '[' | ']' | '"' | '\\') || c.is_whitespace() || c.is_control())
}

/// JSON string literal of `s` (with quotes).
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A mapping key as a key-path segment (bare or quoted).
pub fn key_segment(key: &str) -> String {
    if needs_quotes(key) { quote(key) } else { key.to_string() }
}

/// Appends a formatted segment to a formatted parent path: `.` before keys,
/// nothing before `[…]`.
pub fn join(parent: &str, seg: &str) -> String {
    if parent.is_empty() {
        seg.to_string()
    } else if seg.starts_with('[') {
        format!("{parent}{seg}")
    } else {
        format!("{parent}.{seg}")
    }
}

/// `[N]`.
pub fn index_segment(i: usize) -> String {
    format!("[{i}]")
}

/// `["id"]`.
pub fn id_segment(id: &str) -> String {
    format!("[{}]", quote(id))
}

/// Formats a key path.
pub fn format(segments: &[Segment]) -> String {
    let mut out = String::new();
    for s in segments {
        let seg = match s {
            Segment::Key(k) => key_segment(k),
            Segment::Index(i) => index_segment(*i),
            Segment::Id(id) => id_segment(id),
            Segment::Doc(i) => format!("{{{i}}}"),
            Segment::Raw(r) => r.clone(),
        };
        out = join(&out, &seg);
    }
    out
}

/// Parses a key path written by [`format()`]. `Raw` segments cannot be told
/// apart from keys and parse as [`Segment::Key`]. `None` on malformed input.
pub fn parse(path: &str) -> Option<Vec<Segment>> {
    let mut out = Vec::new();
    let mut chars = path.char_indices().peekable();
    let mut expect_segment = true;
    while let Some(&(i, c)) = chars.peek() {
        if c == '.' && !expect_segment {
            chars.next();
            expect_segment = true;
            continue;
        }
        let seg = match c {
            '[' => bracket_segment(&mut chars)?,
            '{' if out.is_empty() && i == 0 => doc_segment(&mut chars)?,
            '"' if expect_segment => Segment::Key(read_quoted(&mut chars)?.0),
            _ if expect_segment => bare_key(&mut chars)?,
            _ => return None,
        };
        out.push(seg);
        expect_segment = false;
    }
    (!expect_segment || out.is_empty()).then_some(out)
}

type Chars<'a> = std::iter::Peekable<std::str::CharIndices<'a>>;

/// Characters up to (not including) the first one matching `stop`.
fn take_until(chars: &mut Chars<'_>, stop: impl Fn(char) -> bool) -> String {
    let mut s = String::new();
    while let Some(&(_, d)) = chars.peek() {
        if stop(d) {
            break;
        }
        s.push(d);
        chars.next();
    }
    s
}

/// `["id"]` or `[N]`.
fn bracket_segment(chars: &mut Chars<'_>) -> Option<Segment> {
    chars.next();
    let seg = if chars.peek().map(|p| p.1) == Some('"') {
        Segment::Id(read_quoted(chars)?.0)
    } else {
        Segment::Index(take_until(chars, |d| d == ']').parse().ok()?)
    };
    (chars.next()?.1 == ']').then_some(seg)
}

/// `{N}` (a document index, only at the start).
fn doc_segment(chars: &mut Chars<'_>) -> Option<Segment> {
    chars.next();
    let mut n = String::new();
    for (_, d) in chars.by_ref() {
        if d == '}' {
            break;
        }
        n.push(d);
    }
    Some(Segment::Doc(n.parse().ok()?))
}

/// An unquoted key, up to the next `.` or `[`; none when empty.
fn bare_key(chars: &mut Chars<'_>) -> Option<Segment> {
    let k = take_until(chars, |d| d == '.' || d == '[');
    (!k.is_empty()).then_some(Segment::Key(k))
}

fn read_quoted(chars: &mut Chars<'_>) -> Option<(String, ())> {
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut s = String::new();
    loop {
        let (_, c) = chars.next()?;
        match c {
            '"' => return Some((s, ())),
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => s.push('\n'),
                    'r' => s.push('\r'),
                    't' => s.push('\t'),
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'u' => {
                        let hex: String = (0..4).filter_map(|_| chars.next().map(|p| p.1)).collect();
                        s.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    other => s.push(other),
                }
            }
            c => s.push(c),
        }
    }
}

/// Decodes one JSON Pointer reference token (RFC 6901: `~1` → `/`, `~0` →
/// `~`) after percent-decoding it (URI fragments).
pub fn pointer_token(token: &str) -> String {
    percent_decode(token).replace("~1", "/").replace("~0", "~")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use Segment::*;

    #[test]
    fn quoting() {
        assert_eq!(key_segment("host"), "host");
        assert_eq!(key_segment("/users/{id}"), "/users/{id}");
        assert_eq!(key_segment("@types/node"), "@types/node");
        assert_eq!(key_segment("$defs"), "$defs");
        assert_eq!(key_segment("a.b"), "\"a.b\"");
        assert_eq!(key_segment(""), "\"\"");
        assert_eq!(key_segment("{x}"), "\"{x}\"");
        assert_eq!(key_segment("a b"), "\"a b\"");
        assert_eq!(key_segment("App\\"), "\"App\\\\\"");
        assert_eq!(key_segment("tab\t"), "\"tab\\t\"");
    }

    #[test]
    fn round_trip() {
        let cases: Vec<Vec<Segment>> = vec![
            vec![Key("database".into()), Key("host".into())],
            vec![Key("paths".into()), Key("/v1/users.{format}".into()), Key("get".into())],
            vec![Key("jobs".into()), Key("test".into()), Key("steps".into()), Index(2), Key("run".into())],
            vec![Key("containers".into()), Id("web".into()), Key("env".into()), Id("DATABASE_URL".into())],
            vec![Doc(1), Key("spec".into()), Key("replicas".into())],
            vec![Key("a.b".into()), Key("c".into())],
            vec![Key("".into()), Key("x\"y\\z\n".into()), Index(0), Index(3)],
            vec![Key("dependencies".into()), Key("@types/node".into())],
            vec![Key("ünï".into()), Id("a\u{1}b".into())],
        ];
        for segs in cases {
            let s = format(&segs);
            assert_eq!(parse(&s).as_ref(), Some(&segs), "{s}");
        }
        assert_eq!(format(&[Key("paths".into()), Key("/v1/users.{format}".into())]), "paths.\"/v1/users.{format}\"");
        assert_eq!(format(&[Key("steps".into()), Index(2), Key("run".into())]), "steps[2].run");
        assert_eq!(format(&[Key("env".into()), Id("X".into())]), "env[\"X\"]");
        assert_eq!(format(&[Doc(1), Key("a".into())]), "{1}.a");
        assert_eq!(parse("a..b"), None);
        assert_eq!(parse("a[x]"), None);
    }

    #[test]
    fn pointers() {
        assert_eq!(pointer_token("~1users~1{id}"), "/users/{id}");
        assert_eq!(pointer_token("a~0b"), "a~b");
        assert_eq!(pointer_token("%7Bid%7D"), "{id}");
    }
}
