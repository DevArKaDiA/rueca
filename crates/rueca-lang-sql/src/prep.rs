//! Text the SQL parser cannot read is masked before splitting, keeping
//! every byte offset (crate docs, *dbt and Jinja*, *Client commands*):
//!
//! - dbt / Jinja: `{# … #}` comments are blanked; `{{ ref('m') }}` becomes
//!   the identifier `m` and `{{ source('s', 't') }}` becomes `s.t` (padded
//!   with spaces), `{{ config(…) }}` is blanked and any other `{{ … }}`
//!   becomes a placeholder identifier of underscores as long as the tag; `{% if %}` keeps its first
//!   branch, `{% for %}` its body once, `{% macro %}` / `{% test %}` /
//!   `{% materialization %}` blocks are blanked and reported as macros,
//!   `{% snapshot s %}` keeps its query and is reported; every other tag is
//!   blanked. Calls inside Jinja — `ref` / `source` not substituted, macro
//!   calls, `var('x')`, `env_var('X')` — are reported as [`JinjaRef`]s.
//! - Client commands, whole lines: psql `\…` (`\i` / `\ir` / `\include`
//!   include a file), sqlcmd `:r` / `:setvar` / `:on error`, SQLite `.read`
//!   and other dot commands, MySQL `SOURCE file`.

/// A `{% macro name(args) %}` (also `test`, `materialization`) block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Macro {
    pub name: String,
    pub signature: String,
    pub start: usize,
    pub end: usize,
    pub name_start: usize,
}

/// A `{% snapshot name %}` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub name_start: usize,
}

/// What a Jinja call refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JinjaKind {
    /// `ref('m')` / `source('s', 't')` outside a substitutable position:
    /// a relation (`receiver` = the source name).
    Relation,
    /// A macro call `name(…)` / `pkg.name(…)`.
    Call,
    /// `env_var('X')`.
    EnvVar,
    /// `var('x')`: the dbt project variable, key path `vars.x`.
    Var,
}

/// A reference found in Jinja.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JinjaRef {
    pub kind: JinjaKind,
    pub name: String,
    pub receiver: Option<String>,
    pub start: usize,
    pub end: usize,
}

/// A file included by a client command (`\i x.sql`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub path: String,
    pub start: usize,
    pub end: usize,
}

/// The masked text and what masking found.
#[derive(Debug, Default)]
pub struct Prepared {
    pub text: String,
    pub macros: Vec<Macro>,
    pub snapshots: Vec<Snapshot>,
    pub refs: Vec<JinjaRef>,
    pub includes: Vec<Include>,
    /// The file has Jinja tags (`{{`, `{%`).
    pub jinja: bool,
    /// Placeholders of `{{ … }}` tags alone on their lines (a macro that
    /// may expand to a whole query): byte ranges of the placeholder.
    pub alone: Vec<(usize, usize)>,
    /// The file has a `{{ config(…) }}` call or a `ref` / `source` call
    /// outside macros (a dbt model, analysis or test).
    pub dbt_model_markers: bool,
}

/// What an alone placeholder becomes when a statement does not parse
/// with it as an identifier (a macro that expands to a query).
pub const ALONE_QUERY: &str = "select 1";

/// Jinja names that are not project macros.
const JINJA_BUILTINS: &[&str] = &[
    "ref",
    "source",
    "var",
    "env_var",
    "config",
    "this",
    "adapter",
    "return",
    "log",
    "run_query",
    "is_incremental",
    "range",
    "loop",
    "caller",
    "exceptions",
    "modules",
    "zip",
    "statement",
    "load_result",
    "target",
    "execute",
    "graph",
    "model",
    "builtins",
    "dbt",
    "api",
    "print",
    "tojson",
    "fromjson",
    "toyaml",
    "fromyaml",
    "set",
    "local_md5",
    "invocation_id",
    "run_started_at",
    "flags",
    "dict",
    "list",
    "string",
    "namespace",
    "lower",
    "upper",
    "replace",
    "join",
    "trim",
    "length",
    "format",
    "items",
    "keys",
    "values",
    "append",
    "extend",
    "update",
    "get",
    "split",
    "startswith",
    "endswith",
    "strip",
    "default",
    "unique",
    "select",
    "reject",
    "map",
    "sort",
    "first",
    "last",
    "selectattr",
    "rejectattr",
    "safe",
    "string",
    "int",
    "float",
    "bool",
];

/// Masks `source` (see the module docs).
pub fn prepare(source: &str) -> Prepared {
    let mut p = Prepared { text: String::new(), ..Prepared::default() };
    let mut bytes = source.as_bytes().to_vec();
    client_commands(source, &mut bytes, &mut p);
    if source.contains("{{") || source.contains("{%") || source.contains("{#") {
        p.jinja = true;
        Jinja { src: source, out: &mut bytes, p: &mut p, macro_depth: 0, placeholders: Vec::new() }.run();
    }
    p.text = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    p
}

/// Blanks `bytes[start..end]`, keeping newlines and the byte length (a
/// multi-byte character becomes as many spaces as it has bytes).
pub(crate) fn blank(bytes: &mut [u8], start: usize, end: usize) {
    let end = end.min(bytes.len());
    for b in bytes[start..end].iter_mut().filter(|b| **b != b'\n') {
        *b = b' ';
    }
}

// ---------------------------------------------------------------------------
// Client commands
// ---------------------------------------------------------------------------

fn client_commands(source: &str, bytes: &mut [u8], p: &mut Prepared) {
    let mut pos = 0;
    let mut copy_data = false;
    for line in source.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        if copy_data {
            // psql `COPY … FROM stdin;` rows, up to the `\.` line.
            copy_data = trimmed != "\\.";
            blank(bytes, pos, pos + body.len());
        } else if let Some(path) = command_line(trimmed) {
            if let Some((path, off)) = path {
                let start = pos + (body.len() - trimmed.len()) + off;
                p.includes.push(Include { path, start, end: pos + body.len() });
            }
            blank(bytes, pos, pos + body.len());
        } else {
            copy_data = copies_from_stdin(trimmed);
        }
        pos += line.len();
    }
}

/// `COPY t (a, b) FROM stdin;` (psql inline data follows).
fn copies_from_stdin(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("copy ") && lower.trim_end().trim_end_matches(';').trim_end().ends_with("from stdin")
}

/// `Some` when the line is a client command; `Some(Some((path, offset)))`
/// when it includes a file (offset of the path in the line).
fn command_line(line: &str) -> Option<Option<(String, usize)>> {
    let first = line.split_whitespace().next()?;
    let lower = first.to_ascii_lowercase();
    let include = match lower.as_str() {
        "\\i" | "\\ir" | "\\include" | "\\include_relative" | ":r" | ".read" | "source" | "\\." => true,
        _ if first.starts_with('\\') => false,
        ":setvar" | ":on" | ":connect" | ":out" | ":error" => false,
        _ if first.starts_with('.') && first.len() > 1 && first[1..].chars().all(|c| c.is_ascii_alphabetic()) => false,
        _ => return None,
    };
    if !include {
        return Some(None);
    }
    let rest = line[first.len()..].trim();
    let rest = rest.trim_end_matches(';').trim().trim_matches(['"', '\'']);
    if rest.is_empty() || rest.contains(char::is_whitespace) || (lower == "source" && !rest.contains('.')) {
        return if lower == "source" { None } else { Some(None) };
    }
    let off = line.find(rest).unwrap_or(0);
    Some(Some((rest.to_string(), off)))
}

// ---------------------------------------------------------------------------
// Jinja
// ---------------------------------------------------------------------------

/// One Jinja tag: `{{ … }}`, `{% … %}` or `{# … #}`.
#[derive(Debug, Clone)]
struct Tag {
    start: usize,
    end: usize,
    /// `{`, `%` or `#`.
    kind: u8,
    /// Content without the delimiters and whitespace-control dashes.
    inner_start: usize,
    inner_end: usize,
}

struct Jinja<'a> {
    src: &'a str,
    out: &'a mut Vec<u8>,
    p: &'a mut Prepared,
    macro_depth: u32,
    /// Placeholder ranges written, in order.
    placeholders: Vec<(usize, usize)>,
}

impl Jinja<'_> {
    fn run(&mut self) {
        let tags = tags(self.src);
        let mut i = 0;
        while i < tags.len() {
            i = self.tag(&tags, i);
        }
        self.merge_placeholders();
    }

    /// Placeholders separated only by whitespace (`select {{ a() }} {{ b() }}
    /// from t`: column lists from several macros) become one: the later
    /// ones are blanked.
    fn merge_placeholders(&mut self) {
        let mut prev_end: Option<usize> = None;
        for &(s, e) in &self.placeholders.clone() {
            let glued = prev_end.is_some_and(|pe| pe <= s && self.out[pe..s].iter().all(u8::is_ascii_whitespace));
            if glued {
                blank(self.out, s, e);
                self.p.alone.retain(|a| a.0 != s);
            } else {
                prev_end = Some(e);
            }
        }
    }

    fn inner(&self, t: &Tag) -> &str {
        self.src[t.inner_start..t.inner_end].trim()
    }

    /// The trimmed content of a tag and its byte offset.
    fn inner_at(&self, t: &Tag) -> (String, usize) {
        let raw = &self.src[t.inner_start..t.inner_end];
        (raw.trim().to_string(), t.inner_start + (raw.len() - raw.trim_start().len()))
    }

    /// Handles tag `i`; returns the index of the next tag to handle.
    fn tag(&mut self, tags: &[Tag], i: usize) -> usize {
        let t = tags[i].clone();
        match t.kind {
            b'#' => blank(self.out, t.start, t.end),
            b'{' => self.expression(&t),
            _ => return self.statement(tags, i),
        }
        i + 1
    }

    /// `{{ … }}`.
    fn expression(&mut self, t: &Tag) {
        let (inner, off) = self.inner_at(t);
        blank(self.out, t.start, t.end);
        if call_name(&inner).is_some_and(|n| n == "config") {
            self.p.dbt_model_markers |= self.macro_depth == 0;
            return;
        }
        if self.macro_depth == 0
            && let Some(ident) = relation_identifier(&inner)
        {
            self.p.dbt_model_markers = true;
            self.out[t.start..t.start + ident.len()].copy_from_slice(ident.as_bytes());
            return;
        }
        if self.macro_depth == 0 {
            self.placeholder(t);
        }
        self.calls(&inner, off);
    }

    /// One placeholder identifier over the tag's first line, so text glued
    /// to it stays one identifier (`{{ target.schema }}_dev.t`); a tag
    /// alone on its lines is also recorded in [`Prepared::alone`].
    fn placeholder(&mut self, t: &Tag) {
        let first_end = self.src[t.start..t.end].find('\n').map_or(t.end, |i| t.start + i);
        for b in &mut self.out[t.start..first_end] {
            *b = b'_';
        }
        self.placeholders.push((t.start, first_end));
        let line_start = self.src[..t.start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.src[t.end..].find('\n').map_or(self.src.len(), |i| t.end + i);
        let before = self.src[line_start..t.start].trim().is_empty();
        let after = self.src[t.end..line_end].trim().trim_end_matches(',').is_empty();
        if before && after && first_end - t.start >= ALONE_QUERY.len() {
            self.p.alone.push((t.start, first_end));
        }
    }

    /// `{% … %}`: returns the next tag index.
    fn statement(&mut self, tags: &[Tag], i: usize) -> usize {
        let t = &tags[i];
        let (inner, off) = self.inner_at(t);
        let word = first_word(&inner).to_string();
        match word.as_str() {
            "macro" | "test" | "materialization" => return self.macro_block(tags, i, &word, &inner, off),
            "snapshot" => self.snapshot(tags, i, &inner, off),
            "elif" | "else" if self.macro_depth == 0 => return self.skip_branch(tags, i),
            "set" if !inner.contains('=') => return self.set_block(tags, i),
            _ => {}
        }
        blank(self.out, t.start, t.end);
        self.calls(&inner[word.len()..], off + word.len());
        i + 1
    }

    /// `{% macro name(args) %} … {% endmacro %}`: a macro; the block is
    /// blanked, calls inside it are reported (the macro encloses them).
    fn macro_block(&mut self, tags: &[Tag], i: usize, word: &str, inner: &str, off: usize) -> usize {
        let end_word = format!("end{word}");
        let close = matching(self.src, tags, i, word, &end_word);
        let t = &tags[i];
        let end = close.map_or(self.src.len(), |c| tags[c].end);
        let rest = inner[word.len()..].trim_start();
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            let name_start = off + inner.find(rest).unwrap_or(0);
            let signature = format!("{{% {} %}}", inner.split_whitespace().collect::<Vec<_>>().join(" "));
            self.p.macros.push(Macro { name, signature, start: t.start, end, name_start });
        }
        self.macro_depth += 1;
        let last = close.unwrap_or(tags.len());
        self.calls_in(&tags[(i + 1).min(last)..last]);
        self.macro_depth -= 1;
        blank(self.out, t.start, end);
        last + 1
    }

    /// `{% snapshot name %}`.
    fn snapshot(&mut self, tags: &[Tag], i: usize, inner: &str, off: usize) {
        let close = matching(self.src, tags, i, "snapshot", "endsnapshot");
        let rest = inner["snapshot".len()..].trim_start();
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            let end = close.map_or(self.src.len(), |c| tags[c].end);
            let name_start = off + inner.find(rest).unwrap_or(0);
            self.p.snapshots.push(Snapshot { name, start: tags[i].start, end, name_start });
        }
    }

    /// `{% elif %}` / `{% else %}` of an `if` (or `for … else`): the
    /// branch up to the matching `endif` / `endfor` is blanked; `ref` /
    /// `source` / macro calls in it are still reported.
    fn skip_branch(&mut self, tags: &[Tag], i: usize) -> usize {
        let mut depth = 0i32;
        let mut j = i + 1;
        while j < tags.len() {
            let w = first_word(self.inner(&tags[j]));
            if tags[j].kind == b'%' {
                match w {
                    "if" | "for" => depth += 1,
                    "endif" | "endfor" if depth == 0 => break,
                    "endif" | "endfor" => depth -= 1,
                    _ => {}
                }
            }
            j += 1;
        }
        let end = tags.get(j).map_or(self.src.len(), |t| t.end);
        self.calls_in(&tags[i..j.min(tags.len())]);
        blank(self.out, tags[i].start, end);
        j + 1
    }

    /// `{% set x %} … {% endset %}`: blanked, calls reported.
    fn set_block(&mut self, tags: &[Tag], i: usize) -> usize {
        let close = matching(self.src, tags, i, "set", "endset").unwrap_or(i);
        let end = tags[close].end;
        let body = self.src[tags[i].end..tags[close].start].to_string();
        self.calls(&body, tags[i].end);
        blank(self.out, tags[i].start, end);
        close + 1
    }

    /// Reports the calls in every expression and statement tag of `tags`.
    fn calls_in(&mut self, tags: &[Tag]) {
        for t in tags.iter().filter(|t| t.kind != b'#') {
            let (inner, off) = self.inner_at(t);
            self.calls(&inner, off);
        }
    }

    /// Reports the calls in a Jinja expression starting at byte `off`.
    fn calls(&mut self, expr: &str, off: usize) {
        for (start, end, path, args) in scan_calls(expr) {
            if let Some(r) = jinja_ref(&path, &args, off + start, off + end) {
                if r.kind == JinjaKind::Relation && self.macro_depth == 0 {
                    self.p.dbt_model_markers = true;
                }
                self.p.refs.push(r);
            }
        }
    }
}

/// The reference a call `path(args)` stands for.
fn jinja_ref(path: &str, args: &[String], start: usize, end: usize) -> Option<JinjaRef> {
    let (receiver, name) = match path.rsplit_once('.') {
        Some((r, n)) => (Some(r.to_string()), n),
        None => (None, path),
    };
    let mk = |kind, name: &str, receiver| Some(JinjaRef { kind, name: name.to_string(), receiver, start, end });
    match (receiver.as_deref(), name) {
        (None, "ref") => mk(JinjaKind::Relation, args.last()?, None),
        (None, "source") if args.len() >= 2 => mk(JinjaKind::Relation, &args[1], Some(args[0].clone())),
        (None, "env_var") => mk(JinjaKind::EnvVar, args.first()?, None),
        (None, "var") => mk(JinjaKind::Var, args.first()?, None),
        (Some("adapter" | "exceptions" | "modules" | "this" | "api" | "dbt" | "loop" | "caller"), _) => None,
        (None, n) if JINJA_BUILTINS.contains(&n) => None,
        (Some(r), n) if JINJA_BUILTINS.contains(&n) && !r.contains('_') => None,
        _ => mk(JinjaKind::Call, name, receiver),
    }
}

/// `ref('m')` / `ref('pkg', 'm')` → `m`; `source('s', 't')` → `s.t`;
/// `this` → `this`: the identifier that stands for the whole expression.
fn relation_identifier(inner: &str) -> Option<String> {
    if inner == "this" {
        return Some("this".into());
    }
    let calls = scan_calls(inner);
    let [(start, end, path, args)] = calls.as_slice() else { return None };
    if *start != 0 || *end != inner.len() {
        return None;
    }
    let id = match path.as_str() {
        "ref" => args.last()?.clone(),
        "source" if args.len() >= 2 => format!("{}.{}", args[0], args[1]),
        _ => return None,
    };
    let plain = id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    (plain && !id.is_empty() && id.len() <= inner.len()).then_some(id)
}

/// The name of a call that is the whole expression (`config(…)`).
fn call_name(inner: &str) -> Option<String> {
    let name: String = inner.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    inner[name.len()..].trim_start().starts_with('(').then_some(name)
}

fn first_word(s: &str) -> &str {
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next().unwrap_or("")
}

/// The closing tag (`{% end<word> %}`) of the block opened by tag `i`.
fn matching(src: &str, tags: &[Tag], i: usize, word: &str, end_word: &str) -> Option<usize> {
    let mut depth = 0u32;
    for (j, t) in tags.iter().enumerate().skip(i + 1) {
        if t.kind != b'%' {
            continue;
        }
        let w = first_word(src[t.inner_start..t.inner_end].trim());
        if w == word && word != "set" {
            depth += 1;
        } else if w == end_word {
            if depth == 0 {
                return Some(j);
            }
            depth -= 1;
        }
    }
    None
}

/// Every Jinja tag, in order. `{% raw %}` blocks are left alone.
fn tags(src: &str) -> Vec<Tag> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] != b'{' || !matches!(b[i + 1], b'{' | b'%' | b'#') {
            i += 1;
            continue;
        }
        let Some(tag) = tag_at(b, i) else { break };
        i = tag.end;
        out.push(tag);
    }
    out
}

/// The tag opening at `i` (`{{`, `{%` or `{#`), if it is closed.
fn tag_at(b: &[u8], i: usize) -> Option<Tag> {
    let kind = b[i + 1];
    let close: &[u8] = match kind {
        b'{' => b"}}",
        b'%' => b"%}",
        _ => b"#}",
    };
    let end = i + 2 + find(&b[i + 2..], close)? + 2;
    let dash = |c: Option<&u8>| matches!(c, Some(b'-' | b'+'));
    let inner_start = i + 2 + usize::from(dash(b.get(i + 2)));
    let inner_end = end - 2 - usize::from(end - 2 > inner_start && dash(b.get(end - 3)));
    Some(Tag { start: i, end, kind, inner_start, inner_end })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Calls in a Jinja expression: `(start, end, dotted name, string
/// arguments)`, outermost first, nested ones too (`star(from=ref('x'))`
/// yields `star` and `ref`). Strings are skipped while looking for names.
fn scan_calls(expr: &str) -> Vec<(usize, usize, String, Vec<String>)> {
    let b = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b'\'' | b'"') {
            i = skip_string(b, i);
        } else if starts_name(b, i) {
            let start = i;
            i = name_end(b, i);
            if let Some(call) = call_at(expr, start, i) {
                out.push(call);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn is_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'.'
}

/// A dotted name starts at `i` (a letter or `_` not inside another name).
fn starts_name(b: &[u8], i: usize) -> bool {
    (b[i].is_ascii_alphabetic() || b[i] == b'_') && (i == 0 || !is_name_byte(b[i - 1]))
}

fn name_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_name_byte(b[i]) {
        i += 1;
    }
    i
}

/// The call whose name spans `start..end`, when `(` follows it.
fn call_at(expr: &str, start: usize, end: usize) -> Option<(usize, usize, String, Vec<String>)> {
    let b = expr.as_bytes();
    let open = end + expr[end..].len() - expr[end..].trim_start().len();
    if b.get(open) != Some(&b'(') {
        return None;
    }
    let close = matching_paren(b, open);
    let args = string_args(&expr[open + 1..close.saturating_sub(1).max(open + 1)]);
    Some((start, close, expr[start..end].trim_end_matches('.').to_string(), args))
}

/// Index after the `)` matching the `(` at `open` (or the end).
fn matching_paren(b: &[u8], open: usize) -> usize {
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                i = skip_string(b, i);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

fn skip_string(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() && b[j] != q {
        if b[j] == b'\\' {
            j += 1;
        }
        j += 1;
    }
    (j + 1).min(b.len())
}

/// The top-level string literal arguments of a call (others skipped).
fn string_args(args: &str) -> Vec<String> {
    let b = args.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                let end = skip_string(b, i);
                if depth == 0 {
                    out.push(args[i + 1..end.saturating_sub(1).max(i + 1)].to_string());
                }
                i = end;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_become_identifiers() {
        let src = "{{ config(materialized='table') }}\nselect * from {{ ref('stg_orders') }} o\njoin {{ source('shop', 'raw_customers') }} c on 1=1";
        let p = prepare(src);
        assert_eq!(p.text.len(), src.len());
        assert!(p.text.contains("from stg_orders "));
        assert!(p.text.contains("join shop.raw_customers "));
        assert!(!p.text.contains("config"));
        assert!(p.dbt_model_markers);
    }

    #[test]
    fn branches_macros_and_calls() {
        let src = "{% macro cents(col) %}({{ col }} / 100){% endmacro %}\nselect {{ dbt_utils.star(from=ref('a')) }}, {{ cents('x') }} from t\n{% if var('full') %}where 1=1{% else %}where {{ ref('b') }}.x = 1{% endif %}\n-- {{ env_var('DBT_USER') }}";
        let p = prepare(src);
        assert_eq!(p.macros.len(), 1);
        assert_eq!(p.macros[0].name, "cents");
        assert!(p.text.contains("select ___"));
        assert!(p.text.contains("where 1=1"));
        assert!(!p.text.contains(".x = 1"));
        let names: Vec<(&JinjaKind, &str)> = p.refs.iter().map(|r| (&r.kind, r.name.as_str())).collect();
        assert!(names.contains(&(&JinjaKind::Call, "star")));
        assert!(names.contains(&(&JinjaKind::Relation, "a")));
        assert!(names.contains(&(&JinjaKind::Relation, "b")));
        assert!(names.contains(&(&JinjaKind::Call, "cents")));
        assert!(names.contains(&(&JinjaKind::Var, "full")));
        assert!(names.contains(&(&JinjaKind::EnvVar, "DBT_USER")));
        let r = p.refs.iter().find(|r| r.name == "a").unwrap();
        assert_eq!(&src[r.start..r.end], "ref('a')");
    }

    #[test]
    fn client_commands_masked() {
        let src = "\\set ON_ERROR_STOP on\n\\i schema/tables.sql\n:r .\\procs.sql\n.read seed.sql\nCOPY t (a) FROM stdin;\n1\tx\n\\.\nselect 1;";
        let p = prepare(src);
        let paths: Vec<&str> = p.includes.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["schema/tables.sql", ".\\procs.sql", "seed.sql"]);
        assert_eq!(&src[p.includes[0].start..p.includes[0].end], "schema/tables.sql");
        assert!(p.text.contains("COPY t (a) FROM stdin;"));
        assert!(p.text.trim_end().ends_with("select 1;"));
        assert!(!p.text.contains("1\tx"));
    }
}
