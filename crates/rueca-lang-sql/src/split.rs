//! Statement splitting (crate docs, *Statements and recovery*): one bad
//! statement must not lose the file, so the text is cut into statements
//! first and each is parsed on its own.
//!
//! 1. Batches: T-SQL `GO` lines and MySQL `DELIMITER` regions cut the text
//!    (the lines and the custom delimiters are blanked, same length).
//! 2. Inside a batch, top-level `;` ends a statement, except inside
//!    `BEGIN … END` / `CASE … END` (procedural bodies). A batch that starts
//!    with T-SQL `CREATE PROC` / `FUNCTION` / `TRIGGER` / `VIEW` is one
//!    statement (it runs to the end of the batch), and so is every
//!    statement of a `DELIMITER` region.
//!
//! Tokens come from sqlparser's tokenizer for the file's dialect (strings,
//! comments, dollar quotes and brackets are never cut); a batch it cannot
//! tokenize is one statement.

use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace};

use crate::dialect::Dialect;
use crate::text::Lines;

/// One statement: its byte range in the text, and the comments right
/// before it (for docs and `-- name:` annotations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stmt {
    pub start: usize,
    pub end: usize,
    /// Byte range of the comment lines that end on the line before the
    /// statement (or on its first line before it).
    pub leading: Option<(usize, usize)>,
}

/// A region of the text cut by batch separators.
#[derive(Debug, Clone, Copy)]
struct Batch {
    start: usize,
    end: usize,
    /// `false` inside a `DELIMITER` region: the batch is one statement.
    split_semicolons: bool,
}

/// Splits `text` (blanking `GO` / `DELIMITER` lines in place) into
/// statements.
pub fn split(text: &mut String, dialect: Dialect) -> Vec<Stmt> {
    let batches = batches(text, dialect);
    let mut out = Vec::new();
    for b in batches {
        split_batch(text, dialect, b, &mut out);
    }
    out
}

/// Cuts the text at `GO` lines and `DELIMITER` regions, blanking them.
fn batches(text: &mut String, dialect: Dialect) -> Vec<Batch> {
    // Safety of the byte edits: only ASCII bytes are replaced by spaces.
    let mut bytes = std::mem::take(text).into_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut delimiter: Option<Vec<u8>> = None;
    let mut pos = 0usize;
    while pos < bytes.len() {
        let line_end = bytes[pos..].iter().position(|b| *b == b'\n').map_or(bytes.len(), |i| pos + i);
        let line = &bytes[pos..line_end];
        if let Some(d) = delimiter_line(line).or_else(|| db2_terminator(line, dialect)) {
            push_batch(&mut out, start, pos, delimiter.is_none());
            blank(&mut bytes[pos..line_end]);
            delimiter = (d != b";").then_some(d);
            start = line_end;
        } else if is_go_line(line) || (dialect == Dialect::Oracle && is_slash_line(line)) {
            push_batch(&mut out, start, pos, delimiter.is_none());
            blank(&mut bytes[pos..line_end]);
            start = line_end;
        } else if let Some(d) = &delimiter {
            cut_custom(&mut bytes, pos, line_end, d, &mut start, &mut out);
        }
        pos = line_end + 1;
    }
    push_batch(&mut out, start, bytes.len(), delimiter.is_none());
    *text = String::from_utf8(bytes).unwrap_or_default();
    out
}

/// Inside a `DELIMITER` region: every occurrence of the delimiter on the
/// line ends a statement (one batch each).
fn cut_custom(bytes: &mut [u8], pos: usize, line_end: usize, d: &[u8], start: &mut usize, out: &mut Vec<Batch>) {
    let mut i = pos;
    while i + d.len() <= line_end {
        if &bytes[i..i + d.len()] == d {
            push_batch(out, *start, i, false);
            blank(&mut bytes[i..i + d.len()]);
            *start = i + d.len();
            i += d.len();
        } else {
            i += 1;
        }
    }
}

fn push_batch(out: &mut Vec<Batch>, start: usize, end: usize, split_semicolons: bool) {
    if end > start {
        out.push(Batch { start, end, split_semicolons });
    }
}

fn blank(bytes: &mut [u8]) {
    for b in bytes.iter_mut().filter(|b| **b != b'\n') {
        *b = b' ';
    }
}

/// `DELIMITER $$` (MySQL client): the new delimiter.
fn delimiter_line(line: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(line).ok()?.trim();
    let (word, rest) = text.split_once(char::is_whitespace)?;
    if !word.eq_ignore_ascii_case("delimiter") {
        return None;
    }
    let d = rest.trim();
    (!d.is_empty() && !d.contains(char::is_whitespace)).then(|| d.as_bytes().to_vec())
}

/// `--#SET TERMINATOR @` (DB2 command line processor): the new statement
/// terminator.
fn db2_terminator(line: &[u8], dialect: Dialect) -> Option<Vec<u8>> {
    if !matches!(dialect, Dialect::Db2 | Dialect::Generic) {
        return None;
    }
    let text = std::str::from_utf8(line).ok()?.trim();
    let rest = text.strip_prefix("--#")?.trim_start();
    let (set, rest) = rest.split_once(char::is_whitespace)?;
    let (word, d) = rest.trim().split_once(char::is_whitespace)?;
    let d = d.trim();
    (set.eq_ignore_ascii_case("set") && word.eq_ignore_ascii_case("terminator") && !d.is_empty() && !d.contains(' '))
        .then(|| d.as_bytes().to_vec())
}

/// SQL*Plus: `/` alone on its line runs the buffer (ends a PL/SQL unit).
fn is_slash_line(line: &[u8]) -> bool {
    std::str::from_utf8(line).is_ok_and(|t| t.trim() == "/")
}

/// A T-SQL batch separator: `GO` alone on its line (optionally a count).
fn is_go_line(line: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(line) else { return false };
    let mut words = text.split_whitespace();
    let first = words.next().is_some_and(|w| w.eq_ignore_ascii_case("go"));
    let rest: Vec<&str> = words.collect();
    first && (rest.is_empty() || (rest.len() == 1 && rest[0].chars().all(|c| c.is_ascii_digit())))
}

/// Splits one batch into statements.
fn split_batch(text: &str, dialect: Dialect, b: Batch, out: &mut Vec<Stmt>) {
    let slice = &text[b.start..b.end];
    let Some(tokens) = tokenize(slice, dialect) else {
        push_stmt(text, b.start, b.end, None, out);
        return;
    };
    let lines = Lines::new(slice);
    let spans: Vec<(usize, usize)> = tokens.iter().map(|t| token_range(&lines, t)).collect();
    let whole = !b.split_semicolons
        || (dialect == Dialect::MsSql && starts_routine(&tokens))
        || (dialect == Dialect::Oracle && starts_plsql_unit(&tokens));
    let scripted = matches!(
        dialect,
        Dialect::BigQuery | Dialect::MySql | Dialect::Oracle | Dialect::Db2 | Dialect::Generic | Dialect::Snowflake
    );
    let mut cutter = Cutter {
        tokens: &tokens,
        spans: &spans,
        depth: 0,
        control: 0,
        scripted,
        oracle: dialect == Dialect::Oracle,
        rest: false,
        base: b.start,
        cur: None,
        leading: None,
        newlines: 0,
    };
    for i in 0..tokens.len() {
        cutter.step(i, whole, text, out);
    }
    cutter.flush(text, out);
}

/// Tokens of `slice` in `dialect`, else in the generic dialect.
pub(crate) fn tokenize(slice: &str, dialect: Dialect) -> Option<Vec<TokenWithSpan>> {
    for d in [dialect, Dialect::Generic] {
        let parser = d.parser();
        if let Ok(t) = Tokenizer::new(parser.as_ref(), slice).tokenize_with_location() {
            return Some(t);
        }
    }
    None
}

/// The byte range of a token in the text its locations refer to.
pub(crate) fn token_range(lines: &Lines<'_>, t: &TokenWithSpan) -> (usize, usize) {
    let s = lines.byte_at(t.span.start.line, t.span.start.column);
    let e = lines.byte_at(t.span.end.line, t.span.end.column);
    (s, e.max(s))
}

/// Walks the tokens of one batch, cutting statements.
struct Cutter<'t> {
    tokens: &'t [TokenWithSpan],
    spans: &'t [(usize, usize)],
    depth: u32,
    /// Open control statements of a script (`IF … END IF`, `FOR … END
    /// FOR`, `WHILE`, `LOOP`, `REPEAT`) at the top level.
    control: u32,
    /// Whether the dialect has such statements outside routines.
    scripted: bool,
    oracle: bool,
    /// Inside an Oracle PL/SQL unit that started after other statements
    /// of its buffer: it runs to the end of the buffer.
    rest: bool,
    base: usize,
    /// Current statement: (first byte, last byte) relative to the batch.
    cur: Option<(usize, usize)>,
    /// Comment block since the last statement, relative to the batch.
    leading: Option<(usize, usize)>,
    /// Newlines since the last comment of `leading`.
    newlines: u32,
}

impl Cutter<'_> {
    fn step(&mut self, i: usize, whole: bool, text: &str, out: &mut Vec<Stmt>) {
        let tok = &self.tokens[i].token;
        let (s, e) = self.spans[i];
        if let Token::Whitespace(w) = tok {
            self.whitespace(w, s, e);
            return;
        }
        if self.oracle && self.cur.is_none() && !self.rest && starts_plsql_unit(&self.tokens[i..]) {
            self.rest = !matches!(tok, Token::Word(w) if w.keyword == Keyword::BEGIN);
        }
        if matches!(tok, Token::SemiColon) && self.depth == 0 && self.control == 0 && !whole && !self.rest {
            self.flush(text, out);
            return;
        }
        self.track_depth(i);
        self.cur = Some(self.cur.map_or((s, e), |(a, _)| (a, e)));
    }

    /// Comments before a statement form its leading block; a blank line
    /// (two newlines after a comment) detaches the block.
    fn whitespace(&mut self, w: &Whitespace, s: usize, e: usize) {
        if self.cur.is_some() {
            return;
        }
        match w {
            Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_) => {
                self.leading = Some(self.leading.map_or((s, e), |(a, _)| (a, e)));
                self.newlines = 0;
            }
            Whitespace::Newline if self.leading.is_some() => {
                self.newlines += 1;
                if self.newlines >= 2 {
                    self.leading = None;
                }
            }
            _ => {}
        }
    }

    /// `BEGIN` (not a transaction) and `CASE` open a block; `END` closes
    /// one unless it ends an `IF` / `LOOP` / `WHILE` / `REPEAT` / `FOR`.
    fn track_depth(&mut self, i: usize) {
        let Token::Word(w) = &self.tokens[i].token else { return };
        if w.quote_style.is_some() {
            return;
        }
        let next = self.next_word(i);
        let next = next.as_deref();
        let prev = self.prev_word(i);
        if self.scripted && self.depth == 0 {
            self.track_control(i, &w.value.to_ascii_uppercase(), &prev, next);
        }
        match w.keyword {
            Keyword::BEGIN if !begins_transaction(next) => self.depth += 1,
            Keyword::CASE if prev != "END" => self.depth += 1,
            Keyword::END if !matches!(next, Some("IF" | "LOOP" | "WHILE" | "REPEAT" | "FOR")) => {
                self.depth = self.depth.saturating_sub(1);
            }
            _ => {}
        }
    }

    /// Script control statements outside `BEGIN … END`: one opens at the
    /// start of a statement (or, inside another, after `;` / `THEN` /
    /// `ELSE` / `DO` / `LOOP`) when its `THEN` / `DO` / `LOOP` comes
    /// before the next `;`, and closes at its `END IF` / `END FOR` / ….
    fn track_control(&mut self, i: usize, k: &str, prev: &str, next: Option<&str>) {
        if k == "END" && matches!(next, Some("IF" | "LOOP" | "WHILE" | "REPEAT" | "FOR")) {
            self.control = self.control.saturating_sub(1);
            return;
        }
        let at_start = if self.control == 0 {
            self.cur.is_none()
        } else {
            matches!(prev, ";" | "THEN" | "ELSE" | "DO" | "LOOP")
        };
        if !at_start {
            return;
        }
        let opens = match k {
            "IF" => self.before_semicolon(i, &["THEN"]),
            "FOR" | "WHILE" => self.before_semicolon(i, &["DO", "LOOP"]),
            "LOOP" | "REPEAT" => true,
            _ => false,
        };
        if opens {
            self.control += 1;
        }
    }

    /// Whether one of `words` comes before the next `;` after token `i`.
    fn before_semicolon(&self, i: usize, words: &[&str]) -> bool {
        for t in &self.tokens[i + 1..] {
            match &t.token {
                Token::SemiColon => return false,
                Token::Word(w) if w.quote_style.is_none() && words.iter().any(|x| w.value.eq_ignore_ascii_case(x)) => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// The previous significant token: an unquoted word in upper case,
    /// `";"` for a semicolon, `""` otherwise.
    fn prev_word(&self, i: usize) -> String {
        self.tokens[..i].iter().rev().find(|t| !matches!(t.token, Token::Whitespace(_))).map_or(String::new(), |t| {
            match &t.token {
                Token::Word(w) if w.quote_style.is_none() => w.value.to_ascii_uppercase(),
                Token::SemiColon => ";".to_string(),
                _ => String::new(),
            }
        })
    }

    /// The next significant token in upper case when it is an unquoted
    /// word, `Some("")` for another token, `None` at the end.
    fn next_word(&self, i: usize) -> Option<String> {
        self.tokens[i + 1..].iter().find(|t| !matches!(t.token, Token::Whitespace(_))).map(|t| match &t.token {
            Token::Word(w) if w.quote_style.is_none() => w.value.to_ascii_uppercase(),
            _ => String::new(),
        })
    }

    fn flush(&mut self, text: &str, out: &mut Vec<Stmt>) {
        if let Some((s, e)) = self.cur.take() {
            let leading = self.leading.take().map(|(a, b)| (a + self.base, b + self.base));
            push_stmt(text, s + self.base, e + self.base, leading, out);
        }
        self.leading = None;
    }
}

/// `BEGIN` that starts a transaction rather than a block (`BEGIN;`,
/// `BEGIN TRANSACTION`, SQLite `BEGIN IMMEDIATE`, …).
fn begins_transaction(next: Option<&str>) -> bool {
    matches!(
        next,
        None | Some(
            "" | "TRANSACTION" | "TRAN" | "WORK" | "ISOLATION" | "DEFERRED" | "IMMEDIATE" | "EXCLUSIVE" | "DISTRIBUTED"
        )
    )
}

/// A T-SQL batch whose first statement creates a routine or view.
fn starts_routine(tokens: &[TokenWithSpan]) -> bool {
    let words: Vec<Keyword> = tokens
        .iter()
        .filter_map(|t| match &t.token {
            Token::Word(w) => Some(w.keyword),
            Token::Whitespace(_) => None,
            _ => Some(Keyword::NoKeyword),
        })
        .take(4)
        .collect();
    let rest = match words.as_slice() {
        [Keyword::CREATE, Keyword::OR, Keyword::ALTER, rest @ ..] => rest,
        [Keyword::CREATE | Keyword::ALTER, rest @ ..] => rest,
        _ => return false,
    };
    matches!(
        rest.first(),
        Some(Keyword::PROC | Keyword::PROCEDURE | Keyword::FUNCTION | Keyword::TRIGGER | Keyword::VIEW)
    )
}

/// An Oracle buffer that is a PL/SQL unit (it runs to its `/` line): an
/// anonymous block, or `CREATE … PROCEDURE | FUNCTION | PACKAGE | TRIGGER
/// | TYPE`.
fn starts_plsql_unit(tokens: &[TokenWithSpan]) -> bool {
    let words: Vec<String> = tokens
        .iter()
        .filter_map(|t| match &t.token {
            Token::Word(w) => Some(w.value.to_ascii_uppercase()),
            Token::Whitespace(_) => None,
            _ => Some(String::new()),
        })
        .take(7)
        .collect();
    match words.first().map(String::as_str) {
        Some("DECLARE" | "BEGIN") => words.get(1).is_none_or(|w| !matches!(w.as_str(), "TRANSACTION" | "WORK" | "")),
        Some("CREATE") => {
            let object = words.iter().skip(1).position(|w| {
                matches!(w.as_str(), "PROCEDURE" | "FUNCTION" | "PACKAGE" | "TRIGGER" | "TYPE" | "TABLE" | "VIEW" | "INDEX" | "SEQUENCE" | "SYNONYM")
            });
            match object.map(|i| (words[i + 1].as_str(), words.get(i + 2).map(String::as_str))) {
                Some(("PROCEDURE" | "FUNCTION" | "PACKAGE" | "TRIGGER", _)) => true,
                // A type specification ends at its `;`; a body is a unit.
                Some(("TYPE", next)) => next == Some("BODY"),
                _ => false,
            }
        }
        _ => false,
    }
}

fn push_stmt(text: &str, start: usize, end: usize, leading: Option<(usize, usize)>, out: &mut Vec<Stmt>) {
    let slice = &text[start..end];
    let trimmed = slice.trim_start();
    let start = start + (slice.len() - trimmed.len());
    let end = start + trimmed.trim_end().len();
    if end > start {
        out.push(Stmt { start, end, leading });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str, d: Dialect) -> Vec<String> {
        let mut t = src.to_string();
        let stmts = split(&mut t, d);
        stmts.iter().map(|s| t[s.start..s.end].to_string()).collect()
    }

    #[test]
    fn semicolons_strings_comments() {
        let got = texts("select ';' from a; -- x; y\nselect 2;\n\n", Dialect::Postgres);
        assert_eq!(got, ["select ';' from a", "select 2"]);
    }

    #[test]
    fn dollar_bodies_and_blocks() {
        let src = "create function f() returns int as $$ begin return 1; end $$ language plpgsql;\nselect f();";
        assert_eq!(texts(src, Dialect::Postgres).len(), 2);
        let src = "create trigger t after insert on a begin update b set x = 1; end;\nselect 1;";
        assert_eq!(
            texts(src, Dialect::Sqlite),
            ["create trigger t after insert on a begin update b set x = 1; end", "select 1"]
        );
        let src = "begin;\nselect case when a then 1 end from t;\ncommit;";
        assert_eq!(texts(src, Dialect::Postgres).len(), 3);
    }

    #[test]
    fn go_batches() {
        let src = "CREATE PROCEDURE p AS\nSELECT 1;\nSELECT 2;\nGO\nSELECT 3\nGO 2\n";
        assert_eq!(texts(src, Dialect::MsSql), ["CREATE PROCEDURE p AS\nSELECT 1;\nSELECT 2;", "SELECT 3"]);
    }

    #[test]
    fn oracle_units_and_scripts() {
        let src = "CREATE TABLE t (a INT);\nCREATE OR REPLACE PROCEDURE p IS\n  x INT;\nBEGIN\n  NULL;\nEND;\n/\nSELECT 1 FROM dual;\n";
        let got = texts(src, Dialect::Oracle);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[1].starts_with("CREATE OR REPLACE PROCEDURE") && got[1].ends_with("END;"));
        let src = "DECLARE x INT64 DEFAULT 0;\nFOR r IN (SELECT 1) DO\n  SET x = 1;\n  IF x = 1 THEN SELECT 2; END IF;\nEND FOR;\nSELECT 3;";
        let got = texts(src, Dialect::BigQuery);
        assert_eq!(got.len(), 3, "{got:?}");
        let src = "--#SET TERMINATOR @\nCREATE PROCEDURE p() BEGIN SELECT 1; END@\nSELECT 2@\n";
        assert_eq!(texts(src, Dialect::Db2), ["CREATE PROCEDURE p() BEGIN SELECT 1; END", "SELECT 2"]);
    }

    #[test]
    fn mysql_delimiter() {
        let src = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END$$\nDELIMITER ;\nSELECT 3;";
        assert_eq!(texts(src, Dialect::MySql), ["CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END", "SELECT 3"]);
    }

    #[test]
    fn leading_comments() {
        let mut t = "-- name: GetA :one\nselect 1;\n-- far\n\nselect 2;".to_string();
        let s = split(&mut t, Dialect::Postgres);
        let (a, b) = s[0].leading.unwrap();
        assert_eq!(&t[a..b], "-- name: GetA :one");
        assert_eq!(s[1].leading, None);
    }
}
