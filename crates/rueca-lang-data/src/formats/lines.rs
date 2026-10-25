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
        let raw = &line[vstart..];
        let (text, vend_abs) = match raw.as_bytes().first() {
            Some(q @ (b'"' | b'\'')) => {
                let q = *q as char;
                match raw[1..].find(q) {
                    Some(i) => (raw[1..1 + i].to_string(), start + vstart + i + 2),
                    None if q == '"' => {
                        // Multi-line double-quoted value.
                        let mut text = raw[1..].to_string();
                        let mut end = start + line.len();
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
                    None => (raw[1..].to_string(), start + line.len()),
                }
            }
            _ => {
                let cut = raw.find(" #").or_else(|| raw.find("\t#")).unwrap_or(raw.len());
                let v = raw[..cut].trim_end();
                (v.to_string(), start + vstart + v.len())
            }
        };
        let value = Node::new(Value::Scalar(text), start + vstart..vend_abs);
        entries.push(Entry { key: key.to_string(), key_span, value, split: false });
    }
    tree(src, entries)
}

fn tree(src: &str, entries: Vec<Entry>) -> Tree {
    let end = src.trim_end().len();
    Tree { docs: vec![Node::new(Value::Map(entries), 0..end)], errors: 0 }
}

/// INI (`php.ini`, `setup.cfg`, `tox.ini`): `[section]` headers, `key = value`
/// or `key: value`, `#` / `;` comment lines, indented continuation lines
/// (Python `configparser`). Keys before the first section are top level.
pub(crate) fn ini(src: &str) -> Tree {
    let mut root: Vec<Entry> = Vec::new();
    // (name, header span, entries)
    let mut section: Option<(String, Range<usize>, Vec<Entry>)> = None;
    let flush = |root: &mut Vec<Entry>, section: &mut Option<(String, Range<usize>, Vec<Entry>)>| {
        if let Some((name, hs, entries)) = section.take() {
            let end = entries.last().map_or(hs.end, |e| e.value.span.end.max(e.key_span.end));
            let value = Node::new(Value::Map(entries), hs.start..end);
            root.push(Entry { key: name, key_span: hs, value, split: false });
        }
    };
    for (start, line) in physical_lines(src) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if t.starts_with('[') {
            if let Some(close) = t.rfind(']') {
                flush(&mut root, &mut section);
                let name = t[1..close].trim().to_string();
                let s = start + indent + 1;
                section = Some((name.clone(), s..s + t[1..close].len(), Vec::new()));
            }
            continue;
        }
        let target = match &mut section {
            Some((_, _, e)) => e,
            None => &mut root,
        };
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
        let sep = line.find(['=', ':']);
        let (key_end, vstart) = match sep {
            Some(i) => (i, i + 1),
            None => (line.len(), line.len()),
        };
        let key_span = trimmed_range(start, line, indent, key_end);
        let key = &src[key_span.clone()];
        if key.is_empty() {
            continue;
        }
        let vspan = trimmed_range(start, line, vstart, line.len());
        let value = Node::new(Value::Scalar(src[vspan.clone()].to_string()), vspan);
        target.push(Entry { key: key.to_string(), key_span, value, split: false });
    }
    flush(&mut root, &mut section);
    tree(src, root)
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
        // Key: up to the first unescaped separator.
        let b = t.as_bytes();
        let mut i = 0;
        let mut key = String::new();
        while i < b.len() {
            match b[i] {
                b'\\' if i + 1 < b.len() => {
                    let c = t[i + 1..].chars().next().unwrap_or(' ');
                    key.push(match c {
                        't' => '\t',
                        'n' => '\n',
                        other => other,
                    });
                    i += 1 + c.len_utf8();
                    continue;
                }
                b'=' | b':' | b' ' | b'\t' => break,
                _ => {
                    let c = t[i..].chars().next().unwrap_or(' ');
                    key.push(c);
                    i += c.len_utf8();
                    continue;
                }
            }
        }
        if key.is_empty() {
            continue;
        }
        let key_span = start + indent..start + indent + i;
        let mut rest = t[i..].trim_start_matches([' ', '\t']);
        if let Some(r) = rest.strip_prefix(['=', ':']) {
            rest = r.trim_start_matches([' ', '\t']);
        }
        let vstart = start + line.len() - rest.len();
        let mut text = rest.to_string();
        let mut vend = start + line.len();
        let continues = |s: &str| s.bytes().rev().take_while(|&c| c == b'\\').count() % 2 == 1;
        let mut cont = continues(rest);
        while cont {
            text.pop();
            match lines.next() {
                Some((s2, l2)) => {
                    let l2t = l2.trim_start();
                    text.push_str(l2t);
                    vend = s2 + l2.len();
                    cont = continues(l2t);
                }
                None => break,
            }
        }
        let value = Node::new(Value::Scalar(text), vstart..vend.max(vstart));
        entries.push(Entry { key, key_span, value, split: true });
    }
    tree(src, entries)
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
