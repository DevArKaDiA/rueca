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
        let name_len = if bytes[i..].len() >= 4 && bytes[i..i + 4].eq_ignore_ascii_case(b"href") {
            4
        } else if bytes[i..].len() >= 3 && bytes[i..i + 3].eq_ignore_ascii_case(b"src") {
            3
        } else {
            i += 1;
            continue;
        };
        // Attribute names start after whitespace (`<img src=…`), never
        // inside another name (`data-src`, `srcset`).
        let boundary_before = i > 0 && bytes[i - 1].is_ascii_whitespace();
        let mut j = i + name_len;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if !boundary_before || j >= bytes.len() || bytes[j] != b'=' {
            i += name_len;
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let (start, end) = match bytes.get(j) {
            Some(&q @ (b'"' | b'\'')) => {
                let start = j + 1;
                match bytes[start..].iter().position(|&b| b == q) {
                    Some(len) => (start, start + len),
                    None => break,
                }
            }
            Some(_) => {
                let len =
                    bytes[j..].iter().position(|&b| b.is_ascii_whitespace() || b == b'>').unwrap_or(bytes.len() - j);
                (j, j + len)
            }
            None => break,
        };
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
