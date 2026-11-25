//! One file → [`FileFacts`]: dialect, masking, statements, then per
//! statement the parsed AST ([`crate::ast`]) or the token scanner
//! ([`crate::scan`]).

use std::time::Instant;

use rueca_extract::{Budget, ExtractError};
use rueca_model::{FileFacts, Import, Label, LocalDefId, RefKind};
use sqlparser::parser::Parser;

use crate::ast::{self, Ctx, Pos};
use crate::dialect::{self, Dialect};
use crate::facts::{Builder, NewDef};
use crate::prep::{self, JinjaKind, Prepared};
use crate::scan;
use crate::split::{self, Stmt};
use crate::text::Lines;

/// Statement counts of one file (for the dialect coverage table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub dialect: Dialect,
    pub statements: u32,
    /// Statements no dialect parses (read by the token scanner).
    pub unparsed: u32,
}

/// Statement counts of the SQL file at `path` (no facts).
pub fn stats(path: &str, source: &[u8]) -> Stats {
    let text = String::from_utf8_lossy(source);
    let (dialect, _) = dialect::detect(path, &text);
    let p = prep::prepare(&text);
    let mut masked = p.text;
    let stmts: Vec<Stmt> = split::split(&mut masked, dialect)
        .into_iter()
        .filter(|st| !is_template_output(&masked[st.start..st.end]))
        .collect();
    let unparsed = stmts.iter().filter(|st| !parses(&masked, &p.alone, st, dialect)).count() as u32;
    Stats { dialect, statements: stmts.len() as u32, unparsed }
}

/// Whether some dialect parses the statement (with alone placeholders as
/// queries if needed).
fn parses(masked: &str, alone: &[(usize, usize)], st: &Stmt, dialect: Dialect) -> bool {
    let (c, base) = chunk(masked, st);
    let alt = with_queries(&c, base, alone);
    std::iter::once(c).chain(alt).any(|c| dialect.attempts().into_iter().any(|d| parse(&c, d).is_some()))
}

/// The statements of a file no dialect parses, as the parser sees them,
/// with the error of the file's own dialect (for diagnosing coverage).
#[doc(hidden)]
pub fn unparsed_statements(path: &str, source: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(source);
    let (dialect, _) = dialect::detect(path, &text);
    let p = prep::prepare(&text);
    let mut masked = p.text;
    let stmts = split::split(&mut masked, dialect);
    let mut out = Vec::new();
    for st in stmts.iter().filter(|st| !is_template_output(&masked[st.start..st.end])) {
        let (c, _) = chunk(&masked, st);
        if !parses(&masked, &p.alone, st, dialect) {
            let p = dialect.parser();
            let err = Parser::new(p.as_ref()).try_with_sql(&c).and_then(|mut x| x.parse_statements()).err();
            out.push((c, err.map(|e| e.to_string()).unwrap_or_default()));
        }
    }
    out
}

/// Extracts the facts of the SQL file at `path`.
pub fn extract(path: &str, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    let deadline = Instant::now() + budget.max_parse_time;
    let text = String::from_utf8_lossy(source);
    let invalid_utf8 = u32::from(matches!(text, std::borrow::Cow::Owned(_)));
    let (dialect, _) = dialect::detect(path, &text);
    let mut prepared = prep::prepare(&text);
    let mut b = Builder::new(&text, dialect, rueca_model::is_test_path(path));
    let model = dbt_model(&mut b, path, &prepared);
    jinja_blocks(&mut b, &prepared);
    includes(&mut b, &prepared);
    let mut masked = std::mem::take(&mut prepared.text);
    let stmts = split::split(&mut masked, dialect);
    let mut errors = 0u32;
    for st in &stmts {
        if Instant::now() > deadline {
            return Err(ExtractError::Timeout);
        }
        statement(&mut b, &masked, &prepared.alone, st, model, &mut errors);
    }
    jinja_refs(&mut b, &prepared);
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

/// One statement: a named query annotation, then the first dialect that
/// parses it, else the token scanner (counted as a parse error).
fn statement(
    b: &mut Builder<'_>,
    masked: &str,
    alone: &[(usize, usize)],
    st: &Stmt,
    model: Option<LocalDefId>,
    errors: &mut u32,
) {
    if is_template_output(&masked[st.start..st.end]) {
        return;
    }
    let doc = b.doc(st.leading);
    let named = named_query(b, st);
    let enclosing = named.or_else(|| b.enclosing_of((st.start, st.end))).or(model);
    let ctx = Ctx { enclosing, range: (st.start, st.end), doc: if named.is_some() { None } else { doc.clone() } };
    let (text, base) = chunk(masked, st);
    let alt = with_queries(&text, base, alone);
    for chunk in std::iter::once(text).chain(alt) {
        let Some(parsed) = b.dialect.attempts().into_iter().find_map(|d| parse(&chunk, d)) else { continue };
        let pos = Pos { lines: Lines::new(&chunk), base };
        for s in &parsed {
            for body in ast::statement(b, &pos, s, &ctx) {
                scan::body(b, masked, body.range, body.enclosing);
            }
        }
        return;
    }
    *errors += 1;
    scan::statement(b, masked, ctx.range, ctx.enclosing, ctx.doc);
}

/// The chunk with its alone Jinja placeholders written as a query
/// (`select 1`), when it has any.
fn with_queries(chunk: &str, base: usize, alone: &[(usize, usize)]) -> Option<String> {
    let end = base + chunk.len();
    let mut bytes = chunk.as_bytes().to_vec();
    let mut any = false;
    for &(s, e) in alone.iter().filter(|(s, e)| *s >= base && *e <= end) {
        prep::blank(&mut bytes, s - base, e - base);
        bytes[s - base..s - base + prep::ALONE_QUERY.len()].copy_from_slice(prep::ALONE_QUERY.as_bytes());
        any = true;
    }
    any.then(|| String::from_utf8(bytes).unwrap_or_default())
}

/// A statement that is only Jinja output (a model whose body is one macro
/// call): nothing to parse.
fn is_template_output(text: &str) -> bool {
    text.bytes().all(|b| b == b'_' || b.is_ascii_whitespace())
}

fn parse(chunk: &str, d: Dialect) -> Option<Vec<sqlparser::ast::Statement>> {
    let dialect = d.parser();
    Parser::new(dialect.as_ref()).try_with_sql(chunk).ok()?.parse_statements().ok().filter(|v| !v.is_empty())
}

/// The statement's text from the start of its first line (the part of
/// that line before it blanked), so parser columns are line columns; and
/// the file offset the chunk starts at.
fn chunk(masked: &str, st: &Stmt) -> (String, usize) {
    let line_start = masked[..st.start].rfind('\n').map_or(0, |i| i + 1);
    let mut bytes = masked.as_bytes()[line_start..st.end].to_vec();
    prep::blank(&mut bytes, 0, st.start - line_start);
    (String::from_utf8(bytes).unwrap_or_default(), line_start)
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
