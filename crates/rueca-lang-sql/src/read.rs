//! Reading one statement (crate docs, *Statements and recovery*), in
//! order: sqlparser in the file's dialect and then the others; the same
//! after the same-length repairs ([`crate::repair`]); the pack's own
//! readers — statements whose facts are in their head
//! ([`crate::utility`]), T-SQL batches and routines ([`crate::tsql`]),
//! and `;`-terminated procedural code ([`crate::block`]: PL/SQL, MySQL
//! and DB2 routines, BigQuery scripts). What none of them reads is one
//! parse error and goes to the token scanner ([`crate::scan`]).

use rueca_model::LocalDefId;
use sqlparser::ast::{Expr, Statement};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use crate::ast::{self, Ctx, Pos};
use crate::dialect::Dialect;
use crate::facts::Builder;
use crate::prep;
use crate::repair;
use crate::scan;
use crate::text::Lines;
use crate::utility::{self, Own};
use crate::{block, tsql};

/// Whether a statement may define something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A top-level statement: `CREATE` defines.
    Define,
    /// A statement of a routine body: references only.
    Refs,
}

/// Reads statements into a [`Builder`], counting what it could not read.
pub struct Reader<'a, 's> {
    pub b: &'a mut Builder<'s>,
    /// The file's text after masking (same length as the source).
    pub masked: &'a str,
    /// Jinja placeholders that stand alone on their line.
    pub alone: &'a [(usize, usize)],
    /// Statements (or statements of a routine body) nothing parsed.
    pub errors: u32,
    /// Statements read by the pack's own readers rather than sqlparser.
    pub own: u32,
    /// Statements accepted by their head alone ([`crate::utility`]): no
    /// parser validated them, so they count as parse errors of the file
    /// (the strict rate, the one `Facts::parse_errors` carries).
    pub head_only: u32,
    /// With `Some`, the first word of every head-only statement.
    pub heads: Option<Vec<String>>,
    /// With `Some`, the text and parser error of every failure.
    pub failures: Option<Vec<(String, String)>>,
}

impl<'a, 's> Reader<'a, 's> {
    pub fn new(b: &'a mut Builder<'s>, masked: &'a str, alone: &'a [(usize, usize)]) -> Reader<'a, 's> {
        Reader { b, masked, alone, errors: 0, own: 0, head_only: 0, heads: None, failures: None }
    }

    /// One top-level statement at `ctx.range`.
    pub fn statement(&mut self, ctx: &Ctx) {
        if self.sql(ctx.range, ctx.enclosing, Mode::Define, ctx.doc.as_ref()) {
            return;
        }
        let before = self.errors;
        let read = match self.b.dialect {
            Dialect::MsSql => tsql::read(self, ctx),
            _ => block::read(self, ctx),
        };
        if read {
            if self.errors == before {
                self.own += 1;
            }
            return;
        }
        self.leaf_own(ctx.range, ctx.enclosing, Mode::Define, ctx.doc.clone());
    }

    /// A statement sqlparser rejected: one of the pack's own forms, else
    /// a parse error.
    pub fn leaf_own(&mut self, range: (usize, usize), enclosing: Option<LocalDefId>, mode: Mode, doc: Option<String>) {
        let toks = scan::tokens(self.masked, range.0, range.1, self.b.dialect);
        match utility::own(&toks) {
            Own::No => return self.fail(range, enclosing, mode, doc),
            Own::Refs => scan::refs(self.b, self.masked, range, enclosing),
            Own::Head if mode == Mode::Define => {
                scan::statement(self.b, self.masked, range, enclosing, doc);
            }
            Own::Head => scan::refs(self.b, self.masked, range, enclosing),
            Own::Body(s, e) => scan::refs(self.b, self.masked, (s, e), enclosing),
            Own::Routine(s, e) => {
                let def = match mode {
                    Mode::Define => scan::head(self.b, &toks, range, doc).map(|(d, _)| d),
                    Mode::Refs => enclosing,
                };
                let Some(def) = def else { return self.fail(range, enclosing, mode, None) };
                scan::refs(self.b, self.masked, (s, e), Some(def));
            }
        }
        self.own += 1;
        self.head_only += 1;
        if let Some(h) = &mut self.heads {
            let (text, _) = chunk(self.masked, range);
            h.push(text.split_whitespace().take(2).collect::<Vec<_>>().join(" ").to_ascii_uppercase());
        }
    }

    /// A statement: sqlparser, else the pack's own forms, else an error.
    pub fn leaf(&mut self, range: (usize, usize), enclosing: Option<LocalDefId>, mode: Mode, doc: Option<String>) {
        if !self.sql(range, enclosing, mode, doc.as_ref()) {
            self.leaf_own(range, enclosing, mode, doc);
        }
    }

    /// Counts a parse error and reads the range with the token scanner.
    pub fn fail(&mut self, range: (usize, usize), enclosing: Option<LocalDefId>, mode: Mode, doc: Option<String>) {
        self.errors += 1;
        if let Some(f) = &mut self.failures {
            let (text, _) = chunk(self.masked, range);
            let text = repair::repaired(&text, self.b.dialect).unwrap_or(text);
            let d = self.b.dialect.parser();
            let err = Parser::new(d.as_ref()).try_with_sql(&text).and_then(|mut p| p.parse_statements()).err();
            f.push((text, err.map(|e| e.to_string()).unwrap_or_default()));
        }
        match mode {
            Mode::Define => {
                scan::statement(self.b, self.masked, range, enclosing, doc);
            }
            Mode::Refs => scan::refs(self.b, self.masked, range, enclosing),
        }
    }

    /// Parses `range` with sqlparser (as written, then repaired; in the
    /// file's dialect, then the others) and adds its facts.
    pub fn sql(
        &mut self,
        range: (usize, usize),
        enclosing: Option<LocalDefId>,
        mode: Mode,
        doc: Option<&String>,
    ) -> bool {
        self.sql_edit(range, enclosing, mode, doc, &[])
    }

    /// [`Reader::sql`] with the byte ranges `blanks` of the file blanked
    /// first (parts of a procedural statement that are not SQL, like the
    /// variables of `SELECT … INTO a, b`).
    pub fn sql_edit(
        &mut self,
        range: (usize, usize),
        enclosing: Option<LocalDefId>,
        mode: Mode,
        doc: Option<&String>,
        blanks: &[(usize, usize)],
    ) -> bool {
        let (text, base) = chunk(self.masked, range);
        let text = blanked(text, base, blanks);
        let Some((parsed, text)) = self.parse_variants(text, base, parse) else { return false };
        let pos = Pos { lines: Lines::new(&text), base };
        let ctx = Ctx { enclosing, range, doc: doc.cloned() };
        for s in &parsed {
            match mode {
                Mode::Define => {
                    for body in ast::statement(self.b, &pos, s, &ctx) {
                        scan::body(self.b, self.masked, body.range, body.enclosing);
                    }
                }
                Mode::Refs => ast::references(self.b, &pos, s, enclosing),
            }
        }
        true
    }

    /// Parses `range` as one expression (a condition, a value) and adds
    /// its references.
    pub fn expr(&mut self, range: (usize, usize), enclosing: Option<LocalDefId>) -> bool {
        self.expr_edit(range, enclosing, &[])
    }

    /// [`Reader::expr`] with the file ranges `blanks` blanked first.
    pub fn expr_edit(
        &mut self,
        range: (usize, usize),
        enclosing: Option<LocalDefId>,
        blanks: &[(usize, usize)],
    ) -> bool {
        if range.1 <= range.0 {
            return false;
        }
        let (text, base) = chunk(self.masked, range);
        let text = blanked(text, base, blanks);
        let Some((expr, text)) = self.parse_variants(text, base, parse_expr) else { return false };
        let pos = Pos { lines: Lines::new(&text), base };
        ast::expression(self.b, &pos, &expr, enclosing);
        true
    }

    /// The first variant of `text` (as written, alone placeholders as
    /// queries, repaired) some dialect parses, with its result.
    fn parse_variants<T>(
        &self,
        text: String,
        base: usize,
        f: impl Fn(&str, Dialect) -> Option<T>,
    ) -> Option<(T, String)> {
        let alt = with_queries(&text, base, self.alone);
        let dialects = self.b.dialect.attempts();
        for c in std::iter::once(text).chain(alt) {
            if let Some(t) = dialects.iter().find_map(|d| f(&c, *d)) {
                return Some((t, c));
            }
            if let Some(r) = repair::repaired(&c, self.b.dialect)
                && let Some(t) = dialects.iter().find_map(|d| f(&r, *d))
            {
                return Some((t, r));
            }
        }
        None
    }
}

/// `text` (starting at file offset `base`) with the file ranges `blanks`
/// blanked.
fn blanked(text: String, base: usize, blanks: &[(usize, usize)]) -> String {
    if blanks.is_empty() {
        return text;
    }
    let mut bytes = text.into_bytes();
    for &(s, e) in blanks {
        if s >= base && e <= base + bytes.len() {
            prep::blank(&mut bytes, s - base, e - base);
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// The chunk with its alone Jinja placeholders written as a query
/// (`select 1`), when it has any.
fn with_queries(chunk: &str, base: usize, alone: &[(usize, usize)]) -> Option<String> {
    let end = base + chunk.len();
    let mut bytes = chunk.as_bytes().to_vec();
    let mut any = false;
    for &(s, e) in alone.iter().filter(|(s, e)| *s >= base && *e <= end) {
        // Only where a query can stand: first in the statement, after
        // `(`, after a set operator.
        let before = chunk[..s - base].trim_end();
        let word = before.rsplit(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
        let query_position = before.is_empty()
            || before.ends_with('(')
            || ["all", "union", "except", "intersect", "distinct"].iter().any(|w| word.eq_ignore_ascii_case(w));
        if !query_position {
            continue;
        }
        prep::blank(&mut bytes, s - base, e - base);
        bytes[s - base..s - base + prep::ALONE_QUERY.len()].copy_from_slice(prep::ALONE_QUERY.as_bytes());
        any = true;
    }
    any.then(|| String::from_utf8(bytes).unwrap_or_default())
}

fn parse(chunk: &str, d: Dialect) -> Option<Vec<Statement>> {
    let dialect = d.parser();
    Parser::new(dialect.as_ref()).try_with_sql(chunk).ok()?.parse_statements().ok().filter(|v| !v.is_empty())
}

fn parse_expr(chunk: &str, d: Dialect) -> Option<Expr> {
    let dialect = d.parser();
    let mut p = Parser::new(dialect.as_ref()).try_with_sql(chunk).ok()?;
    let e = p.parse_expr().ok()?;
    (p.peek_token().token == Token::EOF).then_some(e)
}

/// The text of `range` from the start of its first line (the part of
/// that line before it blanked), so parser columns are line columns; and
/// the file offset the chunk starts at.
pub fn chunk(masked: &str, range: (usize, usize)) -> (String, usize) {
    let range = (range.0, range.1.max(range.0));
    let line_start = masked[..range.0].rfind('\n').map_or(0, |i| i + 1);
    let mut bytes = masked.as_bytes()[line_start..range.1].to_vec();
    prep::blank(&mut bytes, 0, range.0 - line_start);
    (String::from_utf8(bytes).unwrap_or_default(), line_start)
}
