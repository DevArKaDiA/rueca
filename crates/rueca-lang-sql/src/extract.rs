//! One file → [`FileFacts`]: dialect, masking, statements, then per
//! statement the parsed AST ([`crate::ast`]) or the token scanner
//! ([`crate::scan`]).

use std::time::Instant;

use rueca_extract::{Budget, ExtractError};
use rueca_model::{FileFacts, Import, Label, LocalDefId, RefKind};

use crate::ast::Ctx;
use crate::dialect::{self, Dialect};
use crate::facts::{Builder, Name, NewDef};
use crate::names::Part;
use crate::prep::{self, JinjaKind, Prepared};
use crate::read::Reader;
use crate::split::{self, Stmt};

/// Statement counts of one file (for the dialect coverage table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub dialect: Dialect,
    pub statements: u32,
    /// Statements with at least one parse error.
    pub unparsed: u32,
    /// Parse errors: statements, or statements of a routine body, that
    /// nothing parsed (read by the token scanner).
    pub errors: u32,
    /// Statements read by the pack's own readers rather than sqlparser.
    pub own: u32,
}

/// Statement counts of the SQL file at `path` (no facts kept).
pub fn stats(path: &str, source: &[u8]) -> Stats {
    let (stats, _) = run(path, source, false);
    stats
}

/// The statements (or statements of a routine body) of a file nothing
/// parses, as the parser sees them, with the error of the file's own
/// dialect (for diagnosing coverage).
#[doc(hidden)]
pub fn unparsed_statements(path: &str, source: &[u8]) -> Vec<(String, String)> {
    run(path, source, true).1
}

fn run(path: &str, source: &[u8], failures: bool) -> (Stats, Vec<(String, String)>) {
    let text = String::from_utf8_lossy(source);
    let mut pass = Pass::new(path, &text);
    let mut b = Builder::new(&text, pass.dialect, false);
    let model = pass.definitions(&mut b, path);
    let mut r = Reader::new(&mut b, &pass.masked, &pass.prepared.alone);
    if failures {
        r.failures = Some(Vec::new());
    }
    let (mut statements, mut unparsed) = (0u32, 0u32);
    for st in &pass.stmts {
        let before = r.errors;
        if statement(&mut r, st, model, &pass.prepared.relations) {
            statements += 1;
            unparsed += u32::from(r.errors > before);
        }
    }
    let stats = Stats { dialect: pass.dialect, statements, unparsed, errors: r.errors, own: r.own };
    (stats, r.failures.take().unwrap_or_default())
}

/// The masked text of a file and its statements.
struct Pass {
    dialect: Dialect,
    prepared: Prepared,
    masked: String,
    stmts: Vec<Stmt>,
}

impl Pass {
    fn new(path: &str, text: &str) -> Pass {
        let (dialect, _) = dialect::detect(path, text);
        let mut prepared = prep::prepare(text, dialect);
        let mut masked = std::mem::take(&mut prepared.text);
        let mut stmts = split::split(&mut masked, dialect);
        // A dbt macros file: only its macro blocks are code.
        if !prepared.macros.is_empty() && path.split('/').any(|s| s == "macros") {
            stmts.clear();
        }
        Pass { dialect, prepared, masked, stmts }
    }

    /// The definitions and references masking found: the dbt model,
    /// macros and snapshots, includes, Jinja calls.
    fn definitions(&mut self, b: &mut Builder<'_>, path: &str) -> Option<LocalDefId> {
        let model = dbt_model(b, path, &self.prepared);
        jinja_blocks(b, &self.prepared);
        includes(b, &self.prepared);
        model
    }
}

/// Extracts the facts of the SQL file at `path`.
pub fn extract(path: &str, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    let deadline = Instant::now() + budget.max_parse_time;
    let text = String::from_utf8_lossy(source);
    let invalid_utf8 = u32::from(matches!(text, std::borrow::Cow::Owned(_)));
    let mut pass = Pass::new(path, &text);
    let mut b = Builder::new(&text, pass.dialect, rueca_model::is_test_path(path));
    let model = pass.definitions(&mut b, path);
    let mut r = Reader::new(&mut b, &pass.masked, &pass.prepared.alone);
    for st in &pass.stmts {
        if Instant::now() > deadline {
            return Err(ExtractError::Timeout);
        }
        statement(&mut r, st, model, &pass.prepared.relations);
    }
    let errors = r.errors;
    jinja_refs(&mut b, &pass.prepared);
    let mut facts = b.facts;
    facts.parse_errors = errors + invalid_utf8;
    facts.line_count = rueca_extract::line_count(source);
    Ok(facts)
}

/// A dbt model (or analysis, singular test): a file with `ref` / `source`
/// / `config` calls, or with Jinja under a `models` directory; not under
/// `macros`. One `Struct` named after the file, spanning it.
fn dbt_model(b: &mut Builder<'_>, path: &str, p: &Prepared) -> Option<LocalDefId> {
    let mut segs: Vec<&str> = path.split('/').collect();
    let file = segs.pop()?;
    if segs.contains(&"macros") || !p.snapshots.is_empty() {
        return None;
    }
    if !(p.dbt_model_markers || (p.jinja && segs.contains(&"models"))) {
        return None;
    }
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let def = NewDef {
        label: Label::Struct,
        name: stem.to_lowercase(),
        qualified_name: stem.to_lowercase(),
        parent: None,
        range: (0, b.source.len()),
        name_range: (0, 0),
        signature: Some(format!("dbt model {stem} -- {}", b.dialect.as_str())),
        doc: None,
    };
    Some(b.add_def(def))
}

/// `{% macro %}` blocks (`Function`) and `{% snapshot %}` blocks (`Struct`).
fn jinja_blocks(b: &mut Builder<'_>, p: &Prepared) {
    for m in &p.macros {
        let def = NewDef {
            label: Label::Function,
            name: m.name.clone(),
            qualified_name: m.name.clone(),
            parent: None,
            range: (m.start, m.end),
            name_range: (m.name_start, m.name_start + m.name.len()),
            signature: Some(m.signature.clone()),
            doc: None,
        };
        b.add_def(def);
    }
    for s in &p.snapshots {
        let def = NewDef {
            label: Label::Struct,
            name: s.name.to_lowercase(),
            qualified_name: s.name.to_lowercase(),
            parent: None,
            range: (s.start, s.end),
            name_range: (s.name_start, s.name_start + s.name.len()),
            signature: Some(format!("{{% snapshot {} %}} -- {}", s.name, b.dialect.as_str())),
            doc: None,
        };
        b.add_def(def);
    }
}

/// `\i file` / `:r file` / `.read file` / `SOURCE file` as imports.
fn includes(b: &mut Builder<'_>, p: &Prepared) {
    for inc in &p.includes {
        let span = b.span((inc.start, inc.end));
        b.facts.imports.push(Import {
            module: inc.path.clone(),
            relative_level: 0,
            names: Vec::new(),
            module_alias: None,
            type_only: false,
            reexport: false,
            span,
        });
    }
}

/// Calls, relations and variables found in Jinja, enclosed by the
/// innermost macro / model containing them.
fn jinja_refs(b: &mut Builder<'_>, p: &Prepared) {
    for r in &p.refs {
        let enclosing = b.enclosing_of((r.start, r.end));
        let range = (r.start, r.end);
        match r.kind {
            JinjaKind::Relation => {
                let name = r.name.to_lowercase();
                b.add_ref(RefKind::Read, name, r.receiver.as_ref().map(|s| s.to_lowercase()), enclosing, range)
            }
            JinjaKind::Call => b.add_ref(RefKind::Call, r.name.clone(), r.receiver.clone(), enclosing, range),
            JinjaKind::EnvVar => b.add_ref(RefKind::EnvVar, r.name.clone(), None, enclosing, range),
            JinjaKind::Var => b.add_ref(RefKind::ConfigKey, format!("vars.{}", r.name), None, enclosing, range),
        }
    }
}

/// One statement: a named query annotation, then the readers of
/// [`crate::read`]. `false` for a statement that is only template output.
fn statement(r: &mut Reader<'_, '_>, st: &Stmt, model: Option<LocalDefId>, relations: &[(usize, usize)]) -> bool {
    if is_template_output(r, st, relations) {
        template_relations(r, st, relations, model);
        return false;
    }
    let doc = r.b.doc(st.leading);
    let named = named_query(r.b, st);
    let enclosing = named.or_else(|| r.b.enclosing_of((st.start, st.end))).or(model);
    let ctx = Ctx { enclosing, range: (st.start, st.end), doc: if named.is_some() { None } else { doc } };
    r.statement(&ctx);
    true
}

/// The relations a template-only statement names (`{{ ref('a') }}` alone):
/// read by the model.
fn template_relations(r: &mut Reader<'_, '_>, st: &Stmt, relations: &[(usize, usize)], model: Option<LocalDefId>) {
    for &(s, _) in relations.iter().filter(|(s, _)| *s >= st.start && *s < st.end) {
        let word = r.masked[s..st.end].split_whitespace().next().unwrap_or("");
        let mut name = Name { parts: Vec::new(), ranges: Vec::new() };
        let mut at = s;
        for part in word.split('.') {
            name.parts.push(Part { text: part.to_string(), quoted: false });
            name.ranges.push((at, at + part.len()));
            at += part.len() + 1;
        }
        let enclosing = r.b.enclosing_of((st.start, st.end)).or(model);
        r.b.relation_ref(RefKind::Read, &name, enclosing);
    }
}

/// A statement that is only Jinja output — placeholders, or one `ref` /
/// `source` (a model whose body is one macro call or one relation):
/// nothing to parse.
fn is_template_output(r: &Reader<'_, '_>, st: &Stmt, relations: &[(usize, usize)]) -> bool {
    let text = &r.masked[st.start..st.end];
    if text.bytes().all(|b| b == b'_' || b.is_ascii_whitespace()) {
        return true;
    }
    // every word is a `ref` / `source` tag or a placeholder
    let mut at = st.start;
    text.split_inclusive(char::is_whitespace).all(|piece| {
        let start = at;
        at += piece.len();
        let word = piece.trim_end();
        word.is_empty()
            || word.bytes().all(|b| b == b'_')
            || relations.iter().any(|&(s, e)| s == start && start + word.len() <= e)
    })
}

/// `-- name: GetAuthor :one` (sqlc, yesql, aiosql, HugSQL `-- :name`)
/// right before a statement: a `Function` for the query.
fn named_query(b: &mut Builder<'_>, st: &Stmt) -> Option<LocalDefId> {
    let (ls, le) = st.leading?;
    let comment = b.source.get(ls..le)?;
    let (name, at) = annotation(comment)?;
    let name_start = ls + at;
    let line_end = comment[at..].find('\n').map_or(le, |i| name_start + i);
    let def = NewDef {
        label: Label::Function,
        name: name.clone(),
        qualified_name: name,
        parent: None,
        range: (ls, st.end),
        name_range: (name_start, name_start + comment[at..].split_whitespace().next().unwrap_or("").len()),
        signature: Some(b.signature((ls, line_end), false).trim_start_matches("--").trim().to_string()),
        doc: None,
    };
    Some(b.add_def(def))
}

/// The query name of a `name:` annotation and its offset in `comment`.
fn annotation(comment: &str) -> Option<(String, usize)> {
    let mut off = 0;
    for line in comment.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        let key = lower.find("name:").map(|i| i + 5).or_else(|| lower.find(":name ").map(|i| i + 6));
        if let Some(k) = key {
            let rest = &line[k..];
            let trimmed = rest.trim_start();
            let name: String =
                trimmed.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            // `:one` / `:many` / `*/` or nothing may follow, not `; Type:`
            // (pg_dump's `-- Name: x; Type: TABLE`).
            let after = trimmed[name.len()..].trim();
            let tail_ok = after.is_empty() || after.starts_with(':') || after.starts_with("*/");
            if !name.is_empty() && tail_ok {
                return Some((name.replace('-', "_"), off + k + (rest.len() - trimmed.len())));
            }
        }
        off += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations() {
        assert_eq!(annotation("-- name: GetAuthor :one\n"), Some(("GetAuthor".into(), 9)));
        assert_eq!(annotation("/* name: ListX :many */"), Some(("ListX".into(), 9)));
        assert_eq!(annotation("-- :name get-user :? :1\n"), Some(("get_user".into(), 9)));
        assert_eq!(annotation("-- plain comment\n"), None);
        assert_eq!(annotation("-- Name: actor; Type: TABLE; Schema: public; Owner: postgres\n"), None);
    }
}
