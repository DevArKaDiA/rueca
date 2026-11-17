//! The canonical text of a symbol (RFC 0007 §5.2), text builder version
//! [`TEXT_BUILDER_VERSION`], the format of the bench's vector arm
//! (`bench/src/rueca_bench/arms/vector.py`):
//!
//! ```text
//! <language> <label> <qualified name>
//! path: <repository-relative path>
//! <signature>
//! <doc comment>
//! <body head>
//! ```
//!
//! - **Callables** (`Function`, `Method`, `Test`): the signature is the
//!   declaration header scanned from the source, from the span's first line
//!   to the end of the header (Python: the `:` closing it at bracket depth
//!   0; other languages: the first `{` at depth 0), lines trimmed and joined
//!   by one space, at most 12 lines (else the first line; the node's
//!   extracted signature when the span has no text). The doc is the node's
//!   docstring when the extractor found one, else the comment lines directly
//!   above the span (Rust attributes skipped). The body is the rest of the
//!   span after the header and a Python docstring, dedented, without blank
//!   lines and without the closing `}` of brace languages.
//! - **Types** (`Class`, `Struct`, `Interface`, `Trait`, `Enum`): signature
//!   and doc as above, then their members' signatures in source order
//!   instead of the raw body, so a type does not re-embed when a method body
//!   changes.
//! - **Sections** (Markdown): the heading path is the qualified name; the
//!   body is the section's text without blank lines.
//!
//! Empty parts are left out; lines are joined by `\n`; the text is cut to
//! `max_tokens × 4` characters (the model cuts again at `max_tokens` real
//! tokens; a tokenizer-based cut is a follow-up), then right-trimmed.

use rueca_model::{Label, Language, Node};

/// Version of this text builder, keyed in the embed key: changing the text
/// in any way must bump it.
pub const TEXT_BUILDER_VERSION: u32 = 1;

/// Characters per token of the character cut.
pub const CHARS_PER_TOKEN: usize = 4;

/// Header lines scanned for a signature.
const MAX_SIGNATURE_LINES: usize = 12;

/// What kind of unit a label embeds as, `None` for labels without vectors
/// (RFC 0007 §5.1: no fields, variables, constants, type aliases,
/// configuration, contracts or containers in v1).
pub fn unit_kind(label: Label) -> Option<UnitKind> {
    match label {
        Label::Function | Label::Method | Label::Test => Some(UnitKind::Callable),
        Label::Class | Label::Struct | Label::Interface | Label::Trait | Label::Enum => Some(UnitKind::Type),
        Label::Section => Some(UnitKind::Section),
        _ => None,
    }
}

/// How a unit's text is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// Functions, methods, tests: header, doc, body head.
    Callable,
    /// Classes and other types: header, doc, member signatures.
    Type,
    /// Markdown sections: the section text.
    Section,
}

/// The canonical text of `node`, whose file is at `path` with `content`;
/// `members` are a type's member nodes (ignored for other kinds).
pub fn canonical_text(node: &Node, path: &str, content: &[u8], members: &[&Node], max_tokens: usize) -> String {
    let language = node.language.map_or("text", Language::as_str);
    let head = format!("{language} {} {}\npath: {path}", node.label.as_str(), node.qualified_name);
    let parts = match unit_kind(node.label) {
        Some(UnitKind::Section) => vec![head, section_body(node, content)],
        Some(UnitKind::Type) => {
            let (signature, doc, _) = header_parts(node, content);
            vec![head, signature, doc, member_lines(content, members)]
        }
        _ => {
            let (signature, doc, body) = header_parts(node, content);
            vec![head, signature, doc, body]
        }
    };
    let text: Vec<&str> = parts.iter().map(String::as_str).filter(|p| !p.is_empty()).collect();
    cut(&text.join("\n"), max_tokens)
}

/// The first `max_tokens × 4` characters of `text`, right-trimmed.
pub fn cut(text: &str, max_tokens: usize) -> String {
    let max = max_tokens.saturating_mul(CHARS_PER_TOKEN);
    let end = text.char_indices().nth(max).map_or(text.len(), |(i, _)| i);
    text[..end].trim_end().to_string()
}

/// The source lines of a node's span, and the lines of its whole file.
fn span_lines<'c>(node: &Node, content: &'c str) -> (Vec<&'c str>, usize) {
    let Some(span) = node.span else { return (Vec::new(), 0) };
    let (start, end) = (span.start_byte as usize, span.end_byte as usize);
    let text = content.get(start..end.min(content.len())).unwrap_or("");
    (text.lines().collect(), span.start_line as usize)
}

/// (signature, doc, body head) of a callable or type.
fn header_parts(node: &Node, content: &[u8]) -> (String, String, String) {
    let content = String::from_utf8_lossy(content);
    let python = node.language == Some(Language::Python);
    let (lines, first_line) = span_lines(node, &content);
    if lines.is_empty() {
        return (node.signature.clone().unwrap_or_default(), node.doc.clone().unwrap_or_default(), String::new());
    }
    let (signature, last, rest) = signature_of(&lines, python);
    let mut body: Vec<&str> = Vec::new();
    if !rest.is_empty() {
        body.push(rest);
    }
    body.extend(&lines[last + 1..]);
    let mut doc = node.doc.clone().unwrap_or_default().trim().to_string();
    if python && rest.is_empty() {
        let (docstring, n) = python_docstring(&body);
        body.drain(..n);
        if doc.is_empty() {
            doc = docstring;
        }
    }
    if doc.is_empty() {
        let file_lines: Vec<&str> = content.lines().collect();
        doc = doc_above(&file_lines, first_line, python);
    }
    (signature, doc, body_text(&body, python))
}

/// Paren / bracket depth after `line`, and where the header ends on it.
fn scan_header(line: &str, mut depth: usize, python: bool) -> (usize, Option<usize>) {
    for (col, ch) in line.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' if depth == 0 && !python => return (depth, Some(col)),
            _ => {}
        }
    }
    let trimmed = line.trim_end();
    if python && depth == 0 && trimmed.ends_with(':') {
        return (depth, Some(trimmed.len()));
    }
    (depth, None)
}

/// (signature, index of the header's last line, code after the header on
/// that line).
fn signature_of<'a>(lines: &[&'a str], python: bool) -> (String, usize, &'a str) {
    let mut depth = 0;
    let mut parts: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate().take(MAX_SIGNATURE_LINES) {
        let (d, end) = scan_header(line, depth, python);
        depth = d;
        let Some(end) = end else {
            parts.push(line.trim());
            continue;
        };
        parts.push(line[..end].trim());
        let rest = line[end..].trim_start_matches('{').trim();
        let sig: Vec<&str> = parts.into_iter().filter(|p| !p.is_empty()).collect();
        return (sig.join(" "), i, rest);
    }
    (lines[0].trim().to_string(), 0, "")
}

fn is_comment(line: &str, python: bool) -> bool {
    let s = line.trim();
    if python { s.starts_with('#') } else { ["//", "/*", "*", "*/"].iter().any(|m| s.starts_with(m)) }
}

fn is_attribute(line: &str, python: bool) -> bool {
    !python && line.trim().starts_with("#[")
}

/// Comment lines directly above 0-based line `start` (no blank line in
/// between; Rust attributes skipped).
fn doc_above(lines: &[&str], start: usize, python: bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in lines[..start.min(lines.len())].iter().rev() {
        if line.trim().is_empty() {
            break;
        }
        if is_comment(line, python) {
            out.push(line.trim());
        } else if !is_attribute(line, python) {
            break;
        }
    }
    out.reverse();
    out.join("\n")
}

/// (docstring lines trimmed, body lines it spans) when the body opens with
/// a string literal.
fn python_docstring(body: &[&str]) -> (String, usize) {
    let Some(first) = body.first().map(|l| l.trim()) else { return (String::new(), 0) };
    let prefix = first.find(['"', '\'']).filter(|&i| first[..i].chars().all(|c| "rRbBuUfF".contains(c)) && i <= 2);
    let Some(p) = prefix else { return (String::new(), 0) };
    let after = &first[p..];
    let quote = ["\"\"\"", "'''", "\"", "'"].into_iter().find(|q| after.starts_with(q)).unwrap_or("\"");
    if after[quote.len()..].contains(quote) {
        return (first.to_string(), 1);
    }
    for (j, line) in body.iter().enumerate().skip(1) {
        if line.contains(quote) {
            let doc: Vec<&str> = body[..=j].iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
            return (doc.join("\n"), j + 1);
        }
    }
    let doc: Vec<&str> = body.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    (doc.join("\n"), body.len())
}

/// Dedented body without blank lines (and without the closing `}` of
/// brace languages).
fn body_text(body: &[&str], python: bool) -> String {
    let mut lines: Vec<&str> = body.iter().map(|l| l.trim_end()).collect();
    if !python {
        drop_closing(&mut lines);
    }
    let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min();
    let indent = indent.unwrap_or(0);
    let kept: Vec<&str> =
        lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.get(indent..).unwrap_or(l.trim_start())).collect();
    kept.join("\n")
}

/// Removes the final `}` (or `};`) and the blank lines after it.
fn drop_closing(lines: &mut Vec<&str>) {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let Some(last) = lines.pop() else { return };
    let t = last.trim_end();
    let t = t.strip_suffix(';').map_or(t, str::trim_end);
    match t.strip_suffix('}') {
        Some(before) if !before.trim().is_empty() => lines.push(before.trim_end()),
        Some(_) => {}
        None => lines.push(last),
    }
}

/// A type's members, one signature per line in source order.
fn member_lines(content: &[u8], members: &[&Node]) -> String {
    let content = String::from_utf8_lossy(content);
    let mut ms: Vec<&&Node> = members.iter().collect();
    ms.sort_by_key(|m| m.span.map_or(u32::MAX, |s| s.start_byte));
    let lines: Vec<String> = ms
        .iter()
        .filter_map(|m| {
            let sig = m.signature.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let sig = sig.map(str::to_string).or_else(|| {
                let (lines, _) = span_lines(m, &content);
                lines.first().map(|l| l.trim().trim_end_matches('{').trim_end().to_string())
            })?;
            (!sig.is_empty()).then_some(sig)
        })
        .collect();
    lines.join("\n")
}

/// A section's text without blank lines.
fn section_body(node: &Node, content: &[u8]) -> String {
    let content = String::from_utf8_lossy(content);
    let (lines, _) = span_lines(node, &content);
    let kept: Vec<&str> = lines.iter().map(|l| l.trim_end()).filter(|l| !l.trim().is_empty()).collect();
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use rueca_model::Span;

    use super::*;

    fn node(label: Label, qn: &str, lang: Language, src: &str, snippet: &str) -> Node {
        let start = src.find(snippet).expect("snippet in source");
        let end = start + snippet.len();
        let line = |b: usize| src[..b].matches('\n').count() as u32;
        Node {
            label,
            name: qn.rsplit(['.', ':']).next().unwrap_or(qn).into(),
            qualified_name: qn.into(),
            file: Some(0),
            span: Some(Span {
                start_byte: start as u32,
                end_byte: end as u32,
                start_line: line(start),
                start_col: 0,
                end_line: line(end),
                end_col: 0,
            }),
            language: Some(lang),
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        }
    }

    const RUST: &str = "use std::fs;\n\n/// Reads the configuration file.\n/// Missing keys get defaults.\n#[must_use]\npub fn load_settings(\n    path: &Path,\n) -> Settings {\n    let text = fs::read_to_string(path)?;\n\n    toml::from_str(&text)\n}\n";

    #[test]
    fn rust_function_golden() {
        let snippet = "pub fn load_settings(\n    path: &Path,\n) -> Settings {\n    let text = fs::read_to_string(path)?;\n\n    toml::from_str(&text)\n}";
        let n = node(Label::Function, "app::config::load_settings", Language::Rust, RUST, snippet);
        let t = canonical_text(&n, "src/config.rs", RUST.as_bytes(), &[], 512);
        assert_eq!(
            t,
            "rust Function app::config::load_settings\npath: src/config.rs\n\
             pub fn load_settings( path: &Path, ) -> Settings\n\
             /// Reads the configuration file.\n/// Missing keys get defaults.\n\
             let text = fs::read_to_string(path)?;\ntoml::from_str(&text)"
        );
    }

    const PY: &str = "class Loader:\n    \"\"\"Loads things.\"\"\"\n\n    def parse(self, path,\n              strict=False):\n        \"\"\"Parse the\n        file.\"\"\"\n        with open(path) as f:\n            return f.read()\n\n    def close(self): pass\n";

    #[test]
    fn python_method_golden() {
        let snippet = "def parse(self, path,\n              strict=False):\n        \"\"\"Parse the\n        file.\"\"\"\n        with open(path) as f:\n            return f.read()\n";
        let n = node(Label::Method, "pkg.io.Loader.parse", Language::Python, PY, snippet);
        let t = canonical_text(&n, "pkg/io.py", PY.as_bytes(), &[], 512);
        assert_eq!(
            t,
            "python Method pkg.io.Loader.parse\npath: pkg/io.py\n\
             def parse(self, path, strict=False):\n\"\"\"Parse the\nfile.\"\"\"\n\
             with open(path) as f:\n    return f.read()"
        );
        // One-line body after the header; no docstring.
        let close = node(Label::Method, "pkg.io.Loader.close", Language::Python, PY, "def close(self): pass");
        assert_eq!(
            canonical_text(&close, "pkg/io.py", PY.as_bytes(), &[], 512),
            "python Method pkg.io.Loader.close\npath: pkg/io.py\ndef close(self): pass"
        );
    }

    #[test]
    fn type_embeds_member_signatures_not_bodies() {
        let mut ty = node(Label::Class, "pkg.io.Loader", Language::Python, PY, PY.trim_end());
        ty.doc = Some("Loads things.".into());
        let mut parse = node(Label::Method, "pkg.io.Loader.parse", Language::Python, PY, "def parse(self, path,");
        parse.signature = Some("def parse(self, path, strict=False)".into());
        let close = node(Label::Method, "pkg.io.Loader.close", Language::Python, PY, "def close(self): pass");
        let t = canonical_text(&ty, "pkg/io.py", PY.as_bytes(), &[&close, &parse], 512);
        assert_eq!(
            t,
            "python Class pkg.io.Loader\npath: pkg/io.py\nclass Loader:\nLoads things.\n\
             def parse(self, path, strict=False)\ndef close(self): pass"
        );
    }

    #[test]
    fn markdown_section_and_budget_cut() {
        let md = "# Guide\n\n## Install\n\nRun `cargo install rueca`.\n\nThen index.\n";
        let n = node(
            Label::Section,
            "README.md:Guide > Install",
            Language::Markdown,
            md,
            "## Install\n\nRun `cargo install rueca`.\n\nThen index.\n",
        );
        let full = canonical_text(&n, "README.md", md.as_bytes(), &[], 512);
        assert_eq!(
            full,
            "markdown Section README.md:Guide > Install\npath: README.md\n## Install\nRun `cargo install rueca`.\nThen index."
        );
        // 10 tokens = 40 characters, right-trimmed.
        let cut10 = canonical_text(&n, "README.md", md.as_bytes(), &[], 10);
        assert_eq!(cut10, "markdown Section README.md:Guide > Insta");
        assert_eq!(cut("héllo wörld", 1), "héll");
    }

    #[test]
    fn units_exclude_fields_and_config() {
        assert_eq!(unit_kind(Label::Function), Some(UnitKind::Callable));
        assert_eq!(unit_kind(Label::Enum), Some(UnitKind::Type));
        assert_eq!(unit_kind(Label::Section), Some(UnitKind::Section));
        for l in [Label::Field, Label::Variable, Label::Constant, Label::Key, Label::File, Label::Route] {
            assert_eq!(unit_kind(l), None, "{l:?}");
        }
    }

    #[test]
    fn closing_brace_and_semicolon_are_dropped() {
        let mut v = vec!["  a();", "};", ""];
        drop_closing(&mut v);
        assert_eq!(v, vec!["  a();"]);
        let mut w = vec!["x }"];
        drop_closing(&mut w);
        assert_eq!(w, vec!["x"]);
    }
}
