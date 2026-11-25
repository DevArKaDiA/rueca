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

use crate::dialect::Dialect;

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
    /// `{{ ref(…) }}` / `{{ source(…) }}` tags written as identifiers:
    /// byte ranges of the tags.
    pub relations: Vec<(usize, usize)>,
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
pub fn prepare(source: &str, dialect: Dialect) -> Prepared {
    let mut p = Prepared { text: String::new(), ..Prepared::default() };
    let mut bytes = source.as_bytes().to_vec();
    if source.starts_with('\u{feff}') {
        blank(&mut bytes, 0, 3);
    }
    client_commands(source, &mut bytes, &mut p, dialect);
    substitutions(&mut bytes, dialect);
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

fn client_commands(source: &str, bytes: &mut [u8], p: &mut Prepared, dialect: Dialect) {
    let mut c = Client { bytes, p, pos: 0, copy_data: false, unit: false, stmt: false, comment: false };
    for line in source.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        if !(dialect == Dialect::Oracle && c.oracle_line(body, trimmed)) {
            c.line(body, trimmed);
        }
        c.pos += line.len();
    }
}

/// Line-by-line state of [`client_commands`].
struct Client<'a> {
    bytes: &'a mut [u8],
    p: &'a mut Prepared,
    /// Byte offset of the current line.
    pos: usize,
    /// Inside the rows of a psql `COPY … FROM stdin`.
    copy_data: bool,
    // SQL*Plus commands are lines of their own between statements; inside
    // a statement or a PL/SQL unit (which runs to its `/` line) the same
    // words are code.
    unit: bool,
    stmt: bool,
    comment: bool,
}

impl Client<'_> {
    /// Blanks a command line; a file it includes is an import.
    fn command(&mut self, body: &str, trimmed: &str, path: Option<(String, usize)>) {
        if let Some((path, off)) = path {
            let start = self.pos + (body.len() - trimmed.len()) + off;
            self.p.includes.push(Include { path, start, end: self.pos + body.len() });
        }
        blank(self.bytes, self.pos, self.pos + body.len());
    }

    /// An Oracle script line: `true` when it is consumed here (a block
    /// comment between statements, a SQL*Plus command).
    fn oracle_line(&mut self, body: &str, trimmed: &str) -> bool {
        let between = !self.unit && !self.stmt;
        if self.comment || trimmed.starts_with("/*") {
            // A block comment between statements is not code.
            let from = if self.comment { 0 } else { 2 };
            self.comment = !trimmed[from.min(trimmed.len())..].contains("*/");
            if between {
                return true;
            }
        }
        if between && let Some(path) = sqlplus_line(trimmed) {
            self.command(body, trimmed, path);
            return true;
        }
        self.track(trimmed);
        false
    }

    /// Whether the lines after this one are inside a statement or a
    /// PL/SQL unit.
    fn track(&mut self, trimmed: &str) {
        let code = trimmed.split("--").next().unwrap_or("").trim_end();
        if code == "/" {
            (self.unit, self.stmt) = (false, false);
            return;
        }
        if code.is_empty() || self.unit {
            return;
        }
        if !self.stmt && plsql_unit(code) {
            self.unit = true;
        } else {
            self.stmt = !code.ends_with(';');
        }
    }

    /// A line of any dialect: `COPY` data, a client command, or code.
    fn line(&mut self, body: &str, trimmed: &str) {
        if self.copy_data {
            // psql `COPY … FROM stdin;` rows, up to the `\.` line.
            self.copy_data = trimmed != "\\.";
            blank(self.bytes, self.pos, self.pos + body.len());
            return;
        }
        let Some(path) = command_line(trimmed) else {
            self.copy_data = copies_from_stdin(trimmed);
            return;
        };
        self.command(body, trimmed, path);
        // psql `\gset` / `\gexec` / `\g` / `\crosstabview` / `\watch`
        // run the statement before them: they end it.
        if ["\\g", "\\crosstabview", "\\watch"].iter().any(|c| trimmed.starts_with(c)) {
            self.bytes[self.pos + (body.len() - trimmed.len())] = b';';
        }
    }
}

/// Whether a line opens a PL/SQL unit, which runs to its `/` line.
fn plsql_unit(code: &str) -> bool {
    let lower = code.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    matches!(first, "declare" | "begin")
        || (first == "create"
            && ["procedure", "function", "package", "trigger", "type"]
                .iter()
                .any(|w| lower.split_whitespace().take(6).any(|x| x == *w)))
}

/// SQL*Plus commands (Oracle scripts): `Some` when the line is one,
/// `Some(Some((path, offset)))` for `@file` / `@@file` / `START file`.
fn sqlplus_line(line: &str) -> Option<Option<(String, usize)>> {
    const COMMANDS: &[&str] = &[
        "prompt",
        "pro",
        "spool",
        "whenever",
        "define",
        "undefine",
        "accept",
        "column",
        "col",
        "show",
        "exit",
        "quit",
        "set",
        "variable",
        "var",
        "print",
        "rem",
        "remark",
        "pause",
        "clear",
        "ttitle",
        "btitle",
        "break",
        "compute",
        "host",
        "timing",
        "conn",
        "connect",
        "disconnect",
        "exec",
        "execute",
        "start",
    ];
    if line.starts_with("$ ") || line.starts_with("! ") {
        return Some(None);
    }
    if let Some(rest) = line.strip_prefix('@') {
        let rest = rest.trim_start_matches('@').trim().trim_end_matches(';');
        let path = rest.split_whitespace().next()?;
        return Some(Some((path.to_string(), line.find(path).unwrap_or(0))));
    }
    let first = line.split_whitespace().next()?;
    let lower = first.trim_end_matches(';').to_ascii_lowercase();
    if !COMMANDS.contains(&lower.as_str()) {
        return None;
    }
    let rest = line[first.len()..].trim();
    match lower.as_str() {
        // SQL statements that share a first word with a command.
        "set" => {
            let w = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            (!matches!(w.as_str(), "transaction" | "role" | "constraint" | "constraints")).then_some(None)
        }
        "start" => {
            let path = rest.trim_end_matches(';').split_whitespace().next()?;
            (!path.eq_ignore_ascii_case("with")).then(|| Some((path.to_string(), line.find(path).unwrap_or(0))))
        }
        "connect" | "conn" => (!rest.to_ascii_lowercase().starts_with("by")).then_some(None),
        "show" | "exit" | "quit" | "print" | "break" | "column" | "col" | "var" | "variable" | "define" | "clear"
        | "compute" | "host" | "timing" | "pause" | "accept" | "undefine" | "disconnect" | "ttitle" | "btitle" => {
            (!rest.contains(":=") && !rest.starts_with('(')).then_some(None)
        }
        _ => Some(None),
    }
}

/// Client-side substitutions, rewritten in place (same length) so the
/// statement around them parses:
///
/// - sqlcmd `$(Name)` → the identifier `__Name_`;
/// - PostgreSQL extension scripts `@extschema@` → `__extschema` (a schema
///   placeholder, dropped from names like a Jinja one);
/// - psql `:'name'` / `:"name"` → the quoted value without the colon, and
///   a statement ended by `\gset` / `\gexec` / `\g` → `;`;
/// - Oracle trigger rows `:new.` / `:old.` → `new.` / `old.`.
fn substitutions(bytes: &mut [u8], dialect: Dialect) {
    let mut i = 0;
    while i < bytes.len() {
        i = substitute(bytes, i, dialect) + 1;
    }
}

/// The substitution at byte `i`, if any; the index of its last byte (`i`
/// when there is none).
fn substitute(bytes: &mut [u8], i: usize, dialect: Dialect) -> usize {
    let oracle = dialect == Dialect::Oracle;
    match bytes[i] {
        b'$' if bytes.get(i + 1) == Some(&b'(') => sqlcmd_variable(bytes, i),
        b'$' if oracle => conditional_compilation(bytes, i),
        b'@' if bytes[i..].starts_with(b"@extschema@") => {
            bytes[i] = b'_';
            bytes[i + 1..i + 11].copy_from_slice(b"_extschema");
            i + 10
        }
        b':' if psql_interpolation(bytes, i, dialect) || (oracle && trigger_row(bytes, i)) => {
            bytes[i] = b' ';
            i
        }
        b'&' if oracle => substitution_variable(bytes, i),
        b'\\' if psql_terminator(bytes, i, dialect) => {
            let end = (i..bytes.len()).find(|&j| bytes[j] == b'\n').unwrap_or(bytes.len());
            blank(bytes, i, end);
            bytes[i] = b';';
            end
        }
        _ => i,
    }
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// sqlcmd `$(Name)` → `__Name_`.
fn sqlcmd_variable(bytes: &mut [u8], i: usize) -> usize {
    let n = bytes.len();
    let end = (i + 2..n).find(|&j| !is_name_char(bytes[j])).unwrap_or(n);
    if end <= i + 2 || bytes.get(end) != Some(&b')') {
        return i;
    }
    bytes[i] = b'_';
    bytes[i + 1] = b'_';
    bytes[end] = b'_';
    end
}

/// psql `:'name'` / `:"name"` at the colon `i`: not `::`, and not a path
/// into semi-structured data (Snowflake `value:"key"`).
fn psql_interpolation(bytes: &[u8], i: usize, dialect: Dialect) -> bool {
    matches!(dialect, Dialect::Postgres | Dialect::Generic | Dialect::Redshift)
        && matches!(bytes.get(i + 1), Some(b'\'' | b'"'))
        && i > 0
        && !matches!(bytes[i - 1], b':' | b'_' | b')' | b']' | b'"')
        && !bytes[i - 1].is_ascii_alphanumeric()
        && psql_variable(bytes, i + 1)
}

/// Oracle trigger rows `:new.` / `:old.` at the colon `i`.
fn trigger_row(bytes: &[u8], i: usize) -> bool {
    i > 0
        && !bytes[i - 1].is_ascii_alphanumeric()
        && bytes.get(i + 4) == Some(&b'.')
        && (bytes[i + 1..i + 4].eq_ignore_ascii_case(b"new") || bytes[i + 1..i + 4].eq_ignore_ascii_case(b"old"))
}

/// psql `\gset` / `\gexec` / `\g` after a statement on its line.
fn psql_terminator(bytes: &[u8], i: usize, dialect: Dialect) -> bool {
    matches!(dialect, Dialect::Postgres | Dialect::Generic)
        && bytes.get(i + 1) == Some(&b'g')
        && i > 0
        && bytes[i - 1].is_ascii_whitespace()
}

/// SQL*Plus substitution variable `&name` / `&&name.` → an identifier.
fn substitution_variable(bytes: &mut [u8], i: usize) -> usize {
    let n = bytes.len();
    let from = if bytes.get(i + 1) == Some(&b'&') { i + 2 } else { i + 1 };
    let end = (from..n).find(|&j| !is_name_char(bytes[j])).unwrap_or(n);
    if end <= from {
        return i;
    }
    blank_with(bytes, i, from, b'_');
    if bytes.get(end) == Some(&b'.') {
        bytes[end] = b'_';
    }
    end
}

/// Oracle conditional compilation: `$if … $then`, `$elsif … $then`,
/// `$else`, `$end` are blanked (every branch is read); `$$name` inquiry
/// directives become identifiers.
fn conditional_compilation(bytes: &mut [u8], i: usize) -> usize {
    let n = bytes.len();
    let end = (i + 1..n).find(|&j| !(is_name_char(bytes[j]) || (j == i + 1 && bytes[j] == b'$'))).unwrap_or(n);
    let word = bytes[i + 1..end].to_ascii_lowercase();
    // Blanks through the directive `closer` that ends this one.
    let through = |bytes: &mut [u8], closer: &[u8]| {
        let len = closer.len();
        let stop = (end..n.saturating_sub(len - 1))
            .find(|&j| bytes[j..j + len].eq_ignore_ascii_case(closer))
            .map_or(end, |j| j + len);
        blank(bytes, i, stop);
        stop.max(i + 1) - 1
    };
    match word.as_slice() {
        b"if" | b"elsif" => through(bytes, b"$then"),
        b"error" => through(bytes, b"$end"),
        b"else" | b"end" | b"then" => {
            blank(bytes, i, end);
            i
        }
        w if w.first() == Some(&b'$') && w.len() > 1 => {
            blank_with(bytes, i, i + 2, b'_');
            i
        }
        _ => i,
    }
}

fn blank_with(bytes: &mut [u8], from: usize, to: usize, byte: u8) {
    for b in &mut bytes[from..to] {
        *b = byte;
    }
}

/// `'name'` / `"name"` at `i` with a variable-like name inside.
fn psql_variable(bytes: &[u8], i: usize) -> bool {
    let q = bytes[i];
    let end =
        (i + 1..bytes.len()).find(|&j| !(bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')).unwrap_or(bytes.len());
    end > i + 1 && bytes.get(end) == Some(&q)
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
        // mysql client: show warnings after every statement, or not
        "warnings" | "nowarning" if line.trim().trim_end_matches(';') == first => false,
        ":setvar" | ":on" | ":connect" | ":out" | ":error" | ":reset" | ":ed" | ":list" | ":help" | ":xml"
        | ":perftrace" | ":serverlist" | ":listvar" | "!!" => false,
        _ if lower.starts_with(":exit") || lower.starts_with(":quit") => false,
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
            self.p.relations.push((t.start, t.end));
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
        let line_start = self.src[..t.start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.src[t.end..].find('\n').map_or(self.src.len(), |i| t.end + i);
        let before = self.src[line_start..t.start].trim().is_empty();
        let after = self.src[t.end..line_end].trim().trim_end_matches(',').is_empty();
        let alone = before && after;
        // A tag alone on its lines whose first line is too short for the
        // query form (`{{` then the call on the next lines): the
        // placeholder goes on its first line that is long enough.
        let (mut s, mut e) = (t.start, first_end);
        if alone && e - s < ALONE_QUERY.len() {
            let mut at = t.start;
            for line in self.src[t.start..t.end].split_inclusive('\n') {
                let body = line.trim_end_matches(['\n', '\r']);
                let indent = body.len() - body.trim_start().len();
                if body.len() - indent >= ALONE_QUERY.len() {
                    (s, e) = (at + indent, at + body.len());
                    break;
                }
                at += line.len();
            }
        }
        for b in &mut self.out[s..e] {
            *b = b'_';
        }
        self.placeholders.push((s, e));
        if alone && e - s >= ALONE_QUERY.len() {
            self.p.alone.push((s, e));
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
            "if" if self.macro_depth == 0 && (inner.contains("loop.last") || inner.contains("loop.first")) => {
                return self.separator(tags, i);
            }
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

    /// `{% if not loop.last %} union all {% endif %}`: what separates the
    /// iterations of a loop; the loop body is read once, so the separator
    /// is blanked (calls reported).
    fn separator(&mut self, tags: &[Tag], i: usize) -> usize {
        let Some(close) = matching(self.src, tags, i, "if", "endif") else {
            blank(self.out, tags[i].start, tags[i].end);
            return i + 1;
        };
        self.calls_in(&tags[i..close]);
        blank(self.out, tags[i].start, tags[close].end);
        close + 1
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
    let end = i + 2 + find_close(&b[i + 2..], close, kind != b'#')? + 2;
    let dash = |c: Option<&u8>| matches!(c, Some(b'-' | b'+'));
    let inner_start = i + 2 + usize::from(dash(b.get(i + 2)));
    let inner_end = end - 2 - usize::from(end - 2 > inner_start && dash(b.get(end - 3)));
    Some(Tag { start: i, end, kind, inner_start, inner_end })
}

/// The closing delimiter of a tag; in `{{ … }}` and `{% … %}` one inside a
/// string literal (`post_hook="{{ m() }}"`) does not close the tag. A
/// string that never ends falls back to the first delimiter.
fn find_close(hay: &[u8], close: &[u8], strings: bool) -> Option<usize> {
    let plain = find(hay, close)?;
    if !strings {
        return Some(plain);
    }
    let mut i = 0;
    while i + 1 < hay.len() {
        match hay[i] {
            b'\'' | b'"' => {
                let q = hay[i];
                match (i + 1..hay.len()).find(|&j| hay[j] == q && hay[j - 1] != b'\\') {
                    Some(j)
                        if !hay[i..j].contains(&b'\n')
                            || hay[i + 1..j].windows(2).any(|w| w == b"{{" || w == b"{%") =>
                    {
                        i = j
                    }
                    _ => {}
                }
            }
            _ if hay[i..].starts_with(close) => return Some(i),
            _ => {}
        }
        i += 1;
    }
    Some(plain)
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
        let p = prepare(src, Dialect::Generic);
        assert_eq!(p.text.len(), src.len());
        assert!(p.text.contains("from stg_orders "));
        assert!(p.text.contains("join shop.raw_customers "));
        assert!(!p.text.contains("config"));
        assert!(p.dbt_model_markers);
    }

    #[test]
    fn branches_macros_and_calls() {
        let src = "{% macro cents(col) %}({{ col }} / 100){% endmacro %}\nselect {{ dbt_utils.star(from=ref('a')) }}, {{ cents('x') }} from t\n{% if var('full') %}where 1=1{% else %}where {{ ref('b') }}.x = 1{% endif %}\n-- {{ env_var('DBT_USER') }}";
        let p = prepare(src, Dialect::Generic);
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
        let p = prepare(src, Dialect::Generic);
        let paths: Vec<&str> = p.includes.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["schema/tables.sql", ".\\procs.sql", "seed.sql"]);
        assert_eq!(&src[p.includes[0].start..p.includes[0].end], "schema/tables.sql");
        assert!(p.text.contains("COPY t (a) FROM stdin;"));
        assert!(p.text.trim_end().ends_with("select 1;"));
        assert!(!p.text.contains("1\tx"));
    }
}
