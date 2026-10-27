//! Small line-oriented parsers built here, because no maintained crate
//! reports positions (RFC 0003 §8): `.env`, INI and Java `.properties`.
//! Each produces a data tree; none of them can fail.

use std::ops::Range;

use crate::tree::{Entry, Node, Tree, Value};

/// Logical lines of `src`: (byte start, text without the terminator).
fn physical_lines(src: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut pos = 0;
    src.split_inclusive('\n').map(move |l| {
        let start = pos;
        pos += l.len();
        (start, l.trim_end_matches(['\n', '\r']))
    })
}

fn trimmed_range(line_start: usize, line: &str, from: usize, to: usize) -> Range<usize> {
    let part = &line[from..to];
    let lead = part.len() - part.trim_start().len();
    let trail = part.len() - part.trim_end().len();
    line_start + from + lead..(line_start + to - trail).max(line_start + from + lead)
}

/// `.env`: `NAME=value` lines, optional `export`, `#` comments, single or
/// double quotes (double-quoted values may span lines), `#` after
/// whitespace starts a trailing comment in unquoted values.
pub(crate) fn dotenv(src: &str) -> Tree {
    let mut entries = Vec::new();
    let mut lines = physical_lines(src).peekable();
    while let Some((start, line)) = lines.next() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = line.len() - t.len();
        let body = t.strip_prefix("export ").map(str::trim_start).unwrap_or(t);
        let body_off = indent + (t.len() - body.len());
        let Some(eq) = body.find('=') else { continue };
        let key = body[..eq].trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let key_span = start + body_off..start + body_off + key.len();
        let vstart_rel = body_off + eq + 1;
        let rest = &line[vstart_rel..];
        let lead = rest.len() - rest.trim_start().len();
        let vstart = vstart_rel + lead;
        let (text, vend_abs) = dotenv_value(line, start, vstart, &mut lines);
        let value = Node::new(Value::Scalar(text), start + vstart..vend_abs);
        entries.push(Entry { key: key.to_string(), key_span, value, split: false });
    }
    tree(src, entries)
}

/// Text and absolute end of the `.env` value starting at `vstart` in `line`
/// (which starts at `start`); a multi-line double-quoted value consumes the
/// following `lines`.
fn dotenv_value<'s>(
    line: &str,
    start: usize,
    vstart: usize,
    lines: &mut impl Iterator<Item = (usize, &'s str)>,
) -> (String, usize) {
    let raw = &line[vstart..];
    let Some(q @ (b'"' | b'\'')) = raw.as_bytes().first() else {
        let cut = raw.find(" #").or_else(|| raw.find("\t#")).unwrap_or(raw.len());
        let v = raw[..cut].trim_end();
        return (v.to_string(), start + vstart + v.len());
    };
    let q = *q as char;
    match raw[1..].find(q) {
        Some(i) => (raw[1..1 + i].to_string(), start + vstart + i + 2),
        None if q == '"' => multiline_value(raw[1..].to_string(), start + line.len(), lines),
        None => (raw[1..].to_string(), start + line.len()),
    }
}

/// The rest of a double-quoted value opened by `text`: following lines up
/// to the closing quote (or the end of the file).
fn multiline_value<'s>(
    mut text: String,
    mut end: usize,
    lines: &mut impl Iterator<Item = (usize, &'s str)>,
) -> (String, usize) {
    for (s2, l2) in lines.by_ref() {
        text.push('\n');
        if let Some(i) = l2.find('"') {
            text.push_str(&l2[..i]);
            end = s2 + i + 1;
            break;
        }
        text.push_str(l2);
        end = s2 + l2.len();
    }
    (text, end)
}

fn tree(src: &str, entries: Vec<Entry>) -> Tree {
    let end = src.trim_end().len();
    Tree { docs: vec![Node::new(Value::Map(entries), 0..end)], errors: 0 }
}

/// An INI section being read: name, header span, entries.
type Section = (String, Range<usize>, Vec<Entry>);

/// Top-level entries and the open section of an INI file.
#[derive(Default)]
struct Ini {
    root: Vec<Entry>,
    section: Option<Section>,
}

impl Ini {
    /// Closes the open section into a top-level entry.
    fn flush(&mut self) {
        if let Some((name, hs, entries)) = self.section.take() {
            let end = entries.last().map_or(hs.end, |e| e.value.span.end.max(e.key_span.end));
            let value = Node::new(Value::Map(entries), hs.start..end);
            self.root.push(Entry { key: name, key_span: hs, value, split: false });
        }
    }

    /// A `[section]` header (`t`, at `indent` in the line at `start`).
    fn header(&mut self, start: usize, indent: usize, t: &str) {
        let Some(close) = t.rfind(']') else { return };
        self.flush();
        let name = t[1..close].trim().to_string();
        let s = start + indent + 1;
        self.section = Some((name, s..s + t[1..close].len(), Vec::new()));
    }

    /// Entries of the open section, else the top level.
    fn target(&mut self) -> &mut Vec<Entry> {
        match &mut self.section {
            Some((_, _, e)) => e,
            None => &mut self.root,
        }
    }
}

/// INI (`php.ini`, `setup.cfg`, `tox.ini`): `[section]` headers, `key = value`
/// or `key: value`, `#` / `;` comment lines, indented continuation lines
/// (Python `configparser`). Keys before the first section are top level.
pub(crate) fn ini(src: &str) -> Tree {
    let mut ini = Ini::default();
    for (start, line) in physical_lines(src) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if t.starts_with('[') {
            ini.header(start, indent, t);
            continue;
        }
        let target = ini.target();
        // Continuation of the previous value.
        if indent > 0
            && let Some(last) = target.last_mut()
        {
            last.value.span.end = start + line.trim_end().len();
            if let Value::Scalar(s) = &mut last.value.value {
                s.push('\n');
                s.push_str(t);
            }
            continue;
        }
        if let Some(entry) = ini_entry(src, start, line, indent) {
            target.push(entry);
        }
    }
    ini.flush();
    tree(src, ini.root)
}

/// `key = value` / `key: value` / `key` (the line at `start`); none for an
/// empty key.
fn ini_entry(src: &str, start: usize, line: &str, indent: usize) -> Option<Entry> {
    let (key_end, vstart) = match line.find(['=', ':']) {
        Some(i) => (i, i + 1),
        None => (line.len(), line.len()),
    };
    let key_span = trimmed_range(start, line, indent, key_end);
    let key = &src[key_span.clone()];
    if key.is_empty() {
        return None;
    }
    let vspan = trimmed_range(start, line, vstart, line.len());
    let value = Node::new(Value::Scalar(src[vspan.clone()].to_string()), vspan);
    Some(Entry { key: key.to_string(), key_span, value, split: false })
}

/// Java `.properties`: `key=value`, `key: value` or `key value`; `#` / `!`
/// comment lines; a line ending in an odd number of backslashes continues;
/// `\` escapes in keys. Keys are dotted paths (`server.port`), split into
/// segments by the emitter.
pub(crate) fn properties(src: &str) -> Tree {
    let mut entries = Vec::new();
    let mut lines = physical_lines(src);
    while let Some((start, line)) = lines.next() {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
            continue;
        }
        let indent = line.len() - t.len();
        let (key, i) = properties_key(t);
        if key.is_empty() {
            continue;
        }
        let key_span = start + indent..start + indent + i;
        let mut rest = t[i..].trim_start_matches([' ', '\t']);
        if let Some(r) = rest.strip_prefix(['=', ':']) {
            rest = r.trim_start_matches([' ', '\t']);
        }
        let vstart = start + line.len() - rest.len();
        let (text, vend) = properties_value(rest, start + line.len(), &mut lines);
        let value = Node::new(Value::Scalar(text), vstart..vend.max(vstart));
        entries.push(Entry { key, key_span, value, split: true });
    }
    tree(src, entries)
}

/// The unescaped key of a `.properties` line and the byte length it spans:
/// up to the first unescaped separator.
fn properties_key(t: &str) -> (String, usize) {
    let b = t.as_bytes();
    let mut i = 0;
    let mut key = String::new();
    while i < b.len() {
        if matches!(b[i], b'=' | b':' | b' ' | b'\t') {
            break;
        }
        if b[i] == b'\\' && i + 1 < b.len() {
            let c = t[i + 1..].chars().next().unwrap_or(' ');
            key.push(unescape_key_char(c));
            i += 1 + c.len_utf8();
        } else {
            let c = t[i..].chars().next().unwrap_or(' ');
            key.push(c);
            i += c.len_utf8();
        }
    }
    (key, i)
}

/// The character a `\c` escape in a key stands for.
fn unescape_key_char(c: char) -> char {
    match c {
        't' => '\t',
        'n' => '\n',
        other => other,
    }
}

/// Whether a `.properties` line continues: it ends in an odd number of
/// backslashes.
fn continues(s: &str) -> bool {
    s.bytes().rev().take_while(|&c| c == b'\\').count() % 2 == 1
}

/// Text and end of a `.properties` value starting with `rest` (whose line
/// ends at `line_end`), continuation lines joined.
fn properties_value<'s>(
    rest: &str,
    line_end: usize,
    lines: &mut impl Iterator<Item = (usize, &'s str)>,
) -> (String, usize) {
    let mut text = rest.to_string();
    let mut vend = line_end;
    let mut cont = continues(rest);
    while cont {
        text.pop();
        let Some((s2, l2)) = lines.next() else { break };
        let l2t = l2.trim_start();
        text.push_str(l2t);
        vend = s2 + l2.len();
        cont = continues(l2t);
    }
    (text, vend)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(t: &Tree) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for e in t.docs[0].as_map().unwrap() {
            match &e.value.value {
                Value::Map(m) => {
                    for c in m {
                        out.push((format!("{}.{}", e.key, c.key), c.value.as_str().unwrap_or("").to_string()))
                    }
                }
                _ => out.push((e.key.clone(), e.value.as_str().unwrap_or("").to_string())),
            }
        }
        out
    }

    #[test]
    fn dotenv_lines() {
        let src = "# c\nexport A=1\nB = \"two # x\"\nC='3' # t\nD=4 # comment\nE=\"multi\nline\"\nnot a line\n=x\n";
        let t = dotenv(src);
        let k = keys(&t);
        assert_eq!(
            k,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "two # x".into()),
                ("C".into(), "3".into()),
                ("D".into(), "4".into()),
                ("E".into(), "multi\nline".into())
            ]
        );
        let e = &t.docs[0].as_map().unwrap()[4];
        assert_eq!(&src[e.value.span.clone()], "\"multi\nline\"");
        assert_eq!(&src[t.docs[0].as_map().unwrap()[0].key_span.clone()], "A");
    }

    #[test]
    fn ini_sections() {
        let src = "top = 1\n[database]\nhost = db ; not a comment\nport: 5432\n  continued\n; c\n[a.b]\nk=v\n";
        let t = ini(src);
        assert_eq!(
            keys(&t),
            vec![
                ("top".into(), "1".into()),
                ("database.host".into(), "db ; not a comment".into()),
                ("database.port".into(), "5432\ncontinued".into()),
                ("a.b.k".into(), "v".into())
            ]
        );
    }

    #[test]
    fn properties_lines() {
        let src = "# c\nserver.port=8080\nspring.datasource.url : jdbc:x\nkey\\ with\\:colon value\nlong = a\\\n    b\nempty\n";
        let t = properties(src);
        assert_eq!(
            keys(&t),
            vec![
                ("server.port".into(), "8080".into()),
                ("spring.datasource.url".into(), "jdbc:x".into()),
                ("key with:colon".into(), "value".into()),
                ("long".into(), "ab".into()),
                ("empty".into(), "".into())
            ]
        );
    }
}
