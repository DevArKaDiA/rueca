//! Link destinations: which ones are file links, and how they map to
//! repository paths.

use percent_encoding::percent_decode_str;
use rueca_extract::ProjectFiles;

/// Files tried, in order, when a link points at a directory.
pub const DIRECTORY_INDEXES: &[&str] = &["README.md", "readme.md", "Readme.md", "index.md", "_index.md"];

/// True when `dest` may name a file of the repository: not empty, not a
/// same-document anchor (`#x`), not protocol-relative (`//host/x`) and
/// without a URL scheme (`https:`, `mailto:`, `data:`, …).
pub fn is_local(dest: &str) -> bool {
    let dest = dest.trim();
    if dest.is_empty() || dest.starts_with('#') || dest.starts_with("//") || dest.starts_with('?') {
        return false;
    }
    !has_scheme(dest)
}

/// `scheme:` per RFC 3986: a letter, then letters, digits, `+`, `-`, `.`.
fn has_scheme(dest: &str) -> bool {
    let Some((scheme, _)) = dest.split_once(':') else { return false };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `href` / `src` attribute values of a fragment of raw HTML
/// (`<img src="docs/logo.png">`, `<a href='../api.md'>`), in order.
pub fn html_link_targets(html: &str) -> Vec<&str> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some(name_len) = link_attribute_len(&bytes[i..]) else {
            i += 1;
            continue;
        };
        let Some(equals) = attribute_equals(bytes, i, name_len) else {
            i += name_len;
            continue;
        };
        let Some((start, end)) = attribute_value(bytes, skip_whitespace(bytes, equals + 1)) else { break };
        // Quotes and whitespace are ASCII, so these are char boundaries.
        if let Some(v) = html.get(start..end)
            && !v.is_empty()
        {
            out.push(v);
        }
        i = end.max(i + 1);
    }
    out
}

/// Length of a `href` / `src` name (any case) at the start of `rest`.
fn link_attribute_len(rest: &[u8]) -> Option<usize> {
    if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(b"href") {
        Some(4)
    } else if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case(b"src") {
        Some(3)
    } else {
        None
    }
}

/// Position of the `=` after the attribute name at `bytes[i..i + name_len]`.
/// Attribute names start after whitespace (`<img src=…`), never inside
/// another name (`data-src`, `srcset`).
fn attribute_equals(bytes: &[u8], i: usize, name_len: usize) -> Option<usize> {
    let boundary_before = i > 0 && bytes[i - 1].is_ascii_whitespace();
    let j = skip_whitespace(bytes, i + name_len);
    (boundary_before && bytes.get(j) == Some(&b'=')).then_some(j)
}

fn skip_whitespace(bytes: &[u8], mut j: usize) -> usize {
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    j
}

/// Byte range of the attribute value starting at `j` (quoted or bare);
/// `None` at the end of input or for an unterminated quote.
fn attribute_value(bytes: &[u8], j: usize) -> Option<(usize, usize)> {
    match bytes.get(j) {
        Some(&q @ (b'"' | b'\'')) => {
            let start = j + 1;
            bytes[start..].iter().position(|&b| b == q).map(|len| (start, start + len))
        }
        Some(_) => {
            let len = bytes[j..].iter().position(|&b| b.is_ascii_whitespace() || b == b'>').unwrap_or(bytes.len() - j);
            Some((j, j + len))
        }
        None => None,
    }
}

/// Maps a link destination written in `from_path` to a repository file.
///
/// The destination is stripped of `#fragment` and `?query`,
/// percent-decoded, and joined to the linking file's directory (`/…` is
/// repository-root relative). A bare path (`src/main.py`, no leading `./` or
/// `../`) that does not exist relative to the file is retried from the
/// repository root. Each base path `p` is tried as: `p`; `p.md`; for
/// `x.html` / `x.htm`, `x.md` (rendered-site links); then `p/` +
/// [`DIRECTORY_INDEXES`]. `..` above the root fails.
pub fn resolve(from_path: &str, dest: &str, project: &ProjectFiles) -> Option<String> {
    let dest = dest.trim();
    if !is_local(dest) {
        return None;
    }
    let dest = dest.split(['#', '?']).next().unwrap_or("");
    let decoded = percent_decode_str(dest).decode_utf8().ok()?;
    let dest = decoded.replace('\\', "/");
    if dest.is_empty() {
        return None;
    }
    let from_dir = from_path.rsplit_once('/').map_or("", |(d, _)| d);
    let (base, rel) = match dest.strip_prefix('/') {
        Some(rest) => ("", rest),
        None => (from_dir, dest.as_str()),
    };
    if let Some(p) = normalize(base, rel).and_then(|p| probe(&p, project)) {
        return Some(p);
    }
    let bare = !dest.starts_with('/') && !dest.starts_with("./") && !dest.starts_with("../") && !base.is_empty();
    if bare { normalize("", rel).and_then(|p| probe(&p, project)) } else { None }
}

/// Joins `rel` to `base` and folds `.` / `..` / empty segments.
fn normalize(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

fn probe(path: &str, project: &ProjectFiles) -> Option<String> {
    let hit = |p: String| project.contains(&p).then_some(p);
    if !path.is_empty() {
        if let Some(p) = hit(path.to_string()) {
            return Some(p);
        }
        if let Some(p) = hit(format!("{path}.md")) {
            return Some(p);
        }
        if let Some(stem) = path.strip_suffix(".html").or_else(|| path.strip_suffix(".htm"))
            && let Some(p) = hit(format!("{stem}.md"))
        {
            return Some(p);
        }
    }
    let dir = if path.is_empty() { String::new() } else { format!("{path}/") };
    DIRECTORY_INDEXES.iter().find_map(|index| hit(format!("{dir}{index}")))
}
