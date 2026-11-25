//! `;`-terminated procedural code (crate docs, *Procedural code*): Oracle
//! PL/SQL (packages, procedures, functions, triggers, type bodies,
//! anonymous blocks), MySQL / MariaDB and DB2 SQL PL routines and
//! triggers, BigQuery scripts and procedures. sqlparser has no grammar
//! for most of it, so the pack reads the structure itself and hands every
//! SQL statement, query and expression inside to sqlparser:
//!
//! - heads: `CREATE [OR REPLACE] … PACKAGE [BODY] | TYPE BODY | PROCEDURE |
//!   FUNCTION | TRIGGER name …` give the definition (a package is a
//!   `Module`; its subprograms are `Function`s named `package.name`,
//!   declarations in a specification marked as such); parameters and
//!   characteristics are not validated;
//! - declarations (PL/SQL `IS | AS … BEGIN`, `DECLARE … BEGIN`): cursors
//!   parse their query, initial values parse as expressions, nested
//!   subprograms are read like top-level ones;
//! - control flow: `BEGIN … [EXCEPTION WHEN … THEN …] END`, `IF … THEN …
//!   [ELSIF | ELSEIF …] [ELSE …] END IF`, `CASE … END CASE`, `LOOP`,
//!   `WHILE … LOOP | DO`, `FOR … LOOP | DO` (a query in `FOR r IN (…)`
//!   is parsed), `REPEAT … UNTIL`, `FORALL`, labels, handlers; conditions
//!   parse as expressions;
//! - statements: queries and DML are parsed by sqlparser with the
//!   variable lists of `[BULK COLLECT] INTO …` / `RETURNING … INTO …`
//!   blanked; `x := expr` and `RETURN expr` parse their expression;
//!   procedure calls parse as call expressions, chains of member calls
//!   (`a.b(x).c(y)`) are read by their shape; `NULL`, `EXIT`, `RAISE`,
//!   cursor and transaction control are read by their first word.
//!
//! Whatever is not read this way counts as one parse error per statement.

use rueca_model::{Label, LocalDefId};
use sqlparser::tokenizer::Token;

use crate::ast::Ctx;
use crate::dialect::Dialect;
use crate::facts::NewDef;
use crate::read::{Mode, Reader};
use crate::scan::{self, Tok};

/// Reads the statement at `ctx.range` when it is procedural code; `false`
/// when it is not one of this module's forms.
pub fn read(r: &mut Reader<'_, '_>, ctx: &Ctx) -> bool {
    let toks = scan::tokens(r.masked, ctx.range.0, ctx.range.1, r.b.dialect);
    if toks.is_empty() {
        return false;
    }
    let ups: Vec<String> = toks.iter().map(|t| t.word().to_ascii_uppercase()).collect();
    let mut b = Block {
        r,
        toks: &toks,
        ups: &ups,
        enclosing: ctx.enclosing,
        package: None,
        doc: ctx.doc.clone(),
        range: ctx.range,
    };
    match b.up(0) {
        "CREATE" | "ALTER" => b.create(),
        "DECLARE" if b.find(1, &["BEGIN"]) < toks.len() => b.script(),
        "BEGIN" if !b.transaction(0) => b.script(),
        "IF" | "FOR" | "WHILE" | "LOOP" | "REPEAT" if b.find(1, &["END"]) < toks.len() => b.script(),
        _ => false,
    }
}

struct Block<'r, 'a, 's, 't> {
    r: &'r mut Reader<'a, 's>,
    toks: &'t [Tok],
    ups: &'t [String],
    enclosing: Option<LocalDefId>,
    /// The package (or type body) being read and its qualified name.
    package: Option<(LocalDefId, String)>,
    doc: Option<String>,
    range: (usize, usize),
}

const DML: &[&str] = &["SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE"];

/// Statements read by their first word, without references.
const CONTROL: &[&str] = &[
    "RAISE", "GOTO", "LEAVE", "ITERATE", "COMMIT", "ROLLBACK", "SAVEPOINT", "CLOSE", "PIPE", "PRAGMA", "SIGNAL",
    "RESIGNAL", "GET", "FETCH", "EXECUTE", "PREPARE", "DEALLOCATE", "RELEASE", "LOCK",
];

impl Block<'_, '_, '_, '_> {
    fn up(&self, i: usize) -> &str {
        self.ups.get(i).map_or("", String::as_str)
    }

    fn tok(&self, i: usize) -> Option<&Token> {
        self.toks.get(i).map(|t| &t.token)
    }

    fn len(&self) -> usize {
        self.toks.len()
    }

    fn span(&self, from: usize, to: usize) -> (usize, usize) {
        (self.toks[from].range.0, self.toks[to - 1].range.1)
    }

    /// Index of the first of `words` from `from` at parenthesis depth 0
    /// outside `CASE … END`; the token count when there is none.
    fn find(&self, from: usize, words: &[&str]) -> usize {
        self.scan(from, |b, j| words.contains(&b.up(j)))
    }

    /// Index of the `;` that ends the statement starting at `from`.
    fn semi(&self, from: usize) -> usize {
        self.scan(from, |b, j| b.tok(j) == Some(&Token::SemiColon))
    }

    fn scan(&self, from: usize, hit: impl Fn(&Self, usize) -> bool) -> usize {
        let (mut depth, mut case) = (0i32, 0i32);
        for j in from..self.len() {
            if depth == 0 && case == 0 && hit(self, j) {
                return j;
            }
            match self.toks[j].token {
                Token::LParen => depth += 1,
                Token::RParen => depth = (depth - 1).max(0),
                _ => {}
            }
            match self.up(j) {
                "CASE" if self.up(j.wrapping_sub(1)) != "END" => case += 1,
                "END" if case > 0 => case -= 1,
                _ => {}
            }
        }
        self.len()
    }

    /// Index after the `)` matching the `(` at `open`.
    fn close(&self, open: usize) -> usize {
        let mut depth = 0i32;
        for j in open..self.len() {
            match self.toks[j].token {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return j + 1;
                    }
                }
                _ => {}
            }
        }
        self.len()
    }

    /// `BEGIN [TRANSACTION | WORK | …];`: not a block.
    fn transaction(&self, i: usize) -> bool {
        matches!(self.tok(i + 1), None | Some(Token::SemiColon))
            || matches!(self.up(i + 1), "TRANSACTION" | "WORK" | "TRAN" | "DEFERRED" | "IMMEDIATE" | "EXCLUSIVE")
    }

    /// An anonymous block or a script: statements to the end.
    fn script(&mut self) -> bool {
        self.statements(0, &[]);
        true
    }

    // -----------------------------------------------------------------
    // Definitions
    // -----------------------------------------------------------------

    fn create(&mut self) -> bool {
        const OBJECTS: &[&str] =
            &["PROCEDURE", "FUNCTION", "TRIGGER", "PACKAGE", "TYPE", "TABLE", "VIEW", "INDEX", "SEQUENCE", "SCHEMA"];
        let Some(at) = (1..self.len().min(20)).find(|&k| OBJECTS.contains(&self.up(k))) else { return false };
        match self.up(at) {
            "PACKAGE" => self.package(at + if self.up(at + 1) == "BODY" { 2 } else { 1 }),
            "TYPE" if self.up(at + 1) == "BODY" => self.package(at + 2),
            "PROCEDURE" | "FUNCTION" => self.routine(at),
            "TRIGGER" => self.trigger(at),
            _ => false,
        }
    }

    /// `PACKAGE [BODY] name IS | AS declarations [BEGIN …] END [name];`
    fn package(&mut self, name_at: usize) -> bool {
        let Some((name, after)) = scan::read_name(self.toks, name_at, self.r.b.dialect) else { return false };
        let is = self.find(after, &["IS", "AS"]);
        if is >= self.len() {
            return false;
        }
        let segs = self.r.b.segments(&name);
        let Some(short) = segs.last().cloned() else { return false };
        let qn = segs.join(".");
        let def = NewDef {
            label: Label::Module,
            name: short,
            qualified_name: qn.clone(),
            parent: None,
            range: self.range,
            name_range: name.last_range(),
            signature: Some(self.r.b.signature(self.range, true)),
            doc: self.doc.take(),
        };
        let d = self.r.b.add_def(def);
        self.package = Some((d, qn));
        self.enclosing = Some(d);
        let b = self.declarations(is + 1);
        if self.up(b) == "BEGIN" {
            self.statement(b);
        }
        true
    }

    /// A stored procedure or function: PL/SQL (`IS | AS` declarations
    /// `BEGIN … END`) or SQL PL / MySQL / BigQuery (`BEGIN … END` or one
    /// statement after the head).
    fn routine(&mut self, at: usize) -> bool {
        let n = self.len();
        let begin = self.find(at + 1, &["BEGIN"]);
        let is = self.find(at + 1, &["IS", "AS"]);
        let body = if is < begin && begin < n {
            // PL/SQL; `AS 'text'` / `AS $$…$$` / `AS (expr)` are not blocks.
            if !matches!(self.tok(is + 1), Some(Token::Word(_))) || matches!(self.up(is + 1), "LANGUAGE" | "EXTERNAL") {
                return false;
            }
            is + 1
        } else if begin < n {
            begin
        } else if is < n {
            return false;
        } else {
            // One statement as the body (`RETURNS INT RETURN a + 1`).
            let params = (at + 1..n).find(|&k| self.tok(k) == Some(&Token::LParen)).map_or(at + 1, |k| self.close(k));
            let k = self.find(params, &["RETURN", "SELECT", "INSERT", "UPDATE", "DELETE", "SET", "CALL"]);
            if k >= n {
                return false;
            }
            k
        };
        let Some((d, _)) = scan::head(self.r.b, self.toks, self.range, self.doc.take()) else { return false };
        self.enclosing = Some(d);
        let b = if body == is + 1 && is < begin { self.declarations(body) } else { body };
        if b < n {
            self.statement(b);
        }
        true
    }

    /// A trigger whose action is a block or one statement.
    fn trigger(&mut self, at: usize) -> bool {
        let n = self.len();
        let mut body = self.find(at + 1, &["DECLARE", "BEGIN"]);
        if body >= n {
            if self.find(at + 1, &["EXECUTE", "CALL"]) < n {
                return false;
            }
            // `FOR EACH ROW [MODE DB2SQL] [WHEN (cond)] statement`
            let each = self.find(at + 1, &["ROW", "STATEMENT"]);
            if each >= n {
                return false;
            }
            body = each + 1;
            if self.up(body) == "MODE" {
                body += 2;
            }
            if self.up(body) == "WHEN" && self.tok(body + 1) == Some(&Token::LParen) {
                body = self.close(body + 1);
            }
            if body >= n {
                return false;
            }
        }
        let Some((d, _)) = scan::head(self.r.b, self.toks, self.range, self.doc.take()) else { return false };
        self.enclosing = Some(d);
        self.statement(body);
        true
    }

    /// The declarative part of a block, package or subprogram, from `i`
    /// to its `BEGIN` or `END` (the index of that word).
    fn declarations(&mut self, mut i: usize) -> usize {
        let n = self.len();
        while i < n {
            while matches!(self.up(i), "MEMBER" | "STATIC" | "CONSTRUCTOR" | "MAP" | "ORDER" | "OVERRIDING" | "FINAL" | "INSTANTIABLE")
                || (self.up(i) == "NOT" && matches!(self.up(i + 1), "OVERRIDING" | "FINAL" | "INSTANTIABLE"))
            {
                i += 1;
            }
            match self.up(i) {
                "BEGIN" | "END" => return i,
                "PROCEDURE" | "FUNCTION" => i = self.subprogram(i),
                "CURSOR" => {
                    let s = self.semi(i);
                    let is = self.find(i, &["IS"]);
                    if is < s {
                        self.query(is + 1, s);
                    }
                    i = s + 1;
                }
                "TYPE" | "SUBTYPE" | "PRAGMA" => i = self.semi(i) + 1,
                _ => {
                    let s = self.semi(i);
                    let value = self.scan(i, |b, j| b.tok(j) == Some(&Token::Assignment) || b.up(j) == "DEFAULT");
                    if value < s {
                        self.value(value + 1, s);
                    }
                    i = s + 1;
                }
            }
        }
        n
    }

    /// A subprogram of a package or of a declarative part; the index
    /// after it.
    fn subprogram(&mut self, i: usize) -> usize {
        let n = self.len();
        let Some((name, after)) = scan::read_name(self.toks, i + 1, self.r.b.dialect) else { return self.semi(i) + 1 };
        let head_end = self.scan(after, |b, j| b.tok(j) == Some(&Token::SemiColon) || matches!(b.up(j), "IS" | "AS"));
        let segs = self.r.b.segments(&name);
        let Some(short) = segs.last().cloned() else { return self.semi(i) + 1 };
        let qn = match &self.package {
            Some((_, pkg)) => format!("{pkg}.{short}"),
            None => short.clone(),
        };
        let start = self.toks[i].range.0;
        let head = (start, self.toks[head_end.min(n) - 1].range.1);
        let def = NewDef {
            label: Label::Function,
            name: short,
            qualified_name: qn,
            parent: self.package.as_ref().map(|(d, _)| *d),
            range: head,
            name_range: name.last_range(),
            signature: Some(self.r.b.signature(head, false)),
            doc: None,
        };
        let d = self.r.b.add_def(def);
        if head_end >= n || self.tok(head_end) == Some(&Token::SemiColon) {
            self.r.b.facts.definitions[d as usize].is_declaration = true;
            return head_end + 1;
        }
        if matches!(self.up(head_end + 1), "LANGUAGE" | "EXTERNAL") {
            return self.semi(head_end) + 1;
        }
        let saved = self.enclosing;
        self.enclosing = Some(d);
        let b = self.declarations(head_end + 1);
        let end = if self.up(b) == "BEGIN" { self.statement(b) } else { b };
        self.enclosing = saved;
        let last = end.clamp(1, n) - 1;
        self.r.b.facts.definitions[d as usize].span = self.r.b.span((start, self.toks[last].range.1));
        end.max(head_end + 1)
    }

    // -----------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------

    /// Statements from `i` up to one of `stops` at the start of a
    /// statement (its index), or the end.
    fn statements(&mut self, mut i: usize, stops: &[&str]) -> usize {
        while i < self.len() {
            if stops.contains(&self.up(i)) {
                return i;
            }
            i = self.statement(i).max(i + 1);
        }
        i
    }

    /// `END [word] [label] ;` at `j`: the index after it.
    fn end(&self, mut j: usize, words: &[&str]) -> usize {
        if self.up(j) != "END" {
            return j;
        }
        j += 1;
        if words.contains(&self.up(j)) {
            j += 1;
        }
        if matches!(self.tok(j), Some(Token::Word(_))) && self.tok(j + 1) == Some(&Token::SemiColon) {
            j += 1;
        }
        if self.tok(j) == Some(&Token::SemiColon) { j + 1 } else { j }
    }

    /// `<<label>>` or `label:` before a block statement.
    fn skip_label(&self, i: usize) -> usize {
        if self.tok(i) == Some(&Token::ShiftLeft) && self.tok(i + 2) == Some(&Token::ShiftRight) {
            return i + 3;
        }
        if self.tok(i + 1) == Some(&Token::Colon) && matches!(self.up(i + 2), "LOOP" | "WHILE" | "REPEAT" | "BEGIN" | "FOR") {
            return i + 2;
        }
        i
    }

    /// One statement at `i`; the index after it.
    fn statement(&mut self, i: usize) -> usize {
        let n = self.len();
        let i = self.skip_label(i);
        if i >= n {
            return n;
        }
        match self.up(i) {
            "DECLARE" => {
                if let Some(k) = self.handler(i) {
                    return self.statement(k);
                }
                let s = self.semi(i);
                let begin = self.find(i + 1, &["BEGIN"]);
                if begin < n && s < begin && !matches!(self.r.b.dialect, Dialect::MySql | Dialect::BigQuery) {
                    // PL/SQL `DECLARE declarations BEGIN … END;`
                    let b = self.declarations(i + 1);
                    return if self.up(b) == "BEGIN" { self.statement(b) } else { b.max(i + 1) };
                }
            }
            "BEGIN" if !self.transaction(i) => {
                let mut j = i + 1;
                if self.up(j) == "NOT" {
                    j += 1;
                }
                if self.up(j) == "ATOMIC" {
                    j += 1;
                }
                j = self.statements(j, &["END", "EXCEPTION"]);
                if self.up(j) == "EXCEPTION" {
                    j += 1;
                    while self.up(j) == "WHEN" {
                        let then = self.find(j + 1, &["THEN"]);
                        j = self.statements(then + 1, &["WHEN", "END"]);
                    }
                }
                return self.end(j, &[]);
            }
            "IF" => {
                let mut j = i;
                loop {
                    let then = self.find(j + 1, &["THEN"]);
                    self.condition(j + 1, then);
                    j = self.statements(then + 1, &["ELSIF", "ELSEIF", "ELSE", "END"]);
                    match self.up(j) {
                        "ELSIF" | "ELSEIF" => {}
                        "ELSE" => {
                            j = self.statements(j + 1, &["END"]);
                            break;
                        }
                        _ => break,
                    }
                }
                return self.end(j, &["IF"]);
            }
            "CASE" => {
                let mut j = i + 1;
                if self.up(j) != "WHEN" {
                    let when = self.find(j, &["WHEN"]);
                    self.condition(j, when);
                    j = when;
                }
                while self.up(j) == "WHEN" {
                    let then = self.find(j + 1, &["THEN"]);
                    self.condition(j + 1, then);
                    j = self.statements(then + 1, &["WHEN", "ELSE", "END"]);
                }
                if self.up(j) == "ELSE" {
                    j = self.statements(j + 1, &["END"]);
                }
                return self.end(j, &["CASE"]);
            }
            "LOOP" => {
                let j = self.statements(i + 1, &["END"]);
                return self.end(j, &["LOOP"]);
            }
            "WHILE" => {
                let d = self.find(i + 1, &["LOOP", "DO"]);
                self.condition(i + 1, d);
                let j = self.statements(d + 1, &["END"]);
                return self.end(j, &["LOOP", "WHILE"]);
            }
            "FOR" => {
                let d = self.find(i + 1, &["LOOP", "DO"]);
                self.for_head(i + 1, d);
                let j = self.statements(d + 1, &["END"]);
                return self.end(j, &["LOOP", "FOR"]);
            }
            "REPEAT" => {
                let j = self.statements(i + 1, &["UNTIL"]);
                let e = self.find(j + 1, &["END"]);
                self.condition(j + 1, e);
                return self.end(e, &["REPEAT"]);
            }
            "FORALL" => {
                let s = self.semi(i);
                let dml = self.find(i + 1, &["INSERT", "UPDATE", "DELETE", "MERGE"]);
                if dml < s {
                    self.dml(dml, s);
                } else {
                    self.r.fail(self.span(i, s.max(i + 1)), self.enclosing, Mode::Refs, None);
                }
                return s + 1;
            }
            _ => {}
        }
        let s = self.semi(i);
        if s > i {
            self.leaf(i, s);
        }
        s + 1
    }

    /// `DECLARE {CONTINUE | EXIT | UNDO} HANDLER FOR conditions statement`:
    /// the index of the statement.
    fn handler(&self, i: usize) -> Option<usize> {
        if self.up(i + 2) != "HANDLER" || self.up(i + 3) != "FOR" {
            return None;
        }
        let mut k = i + 4;
        while k < self.len() {
            match self.up(k) {
                "SQLSTATE" | "VALUE" | "NOT" | "FOUND" | "SQLEXCEPTION" | "SQLWARNING" => k += 1,
                _ if matches!(self.tok(k), Some(Token::Comma | Token::SingleQuotedString(_) | Token::Number(..))) => k += 1,
                _ if matches!(self.tok(k + 1), Some(Token::Comma)) => k += 1,
                _ => break,
            }
        }
        Some(k)
    }

    /// A condition or value at tokens `[from, to)`: an expression.
    fn condition(&mut self, from: usize, to: usize) {
        let to = to.min(self.len());
        if to <= from {
            return;
        }
        let range = self.span(from, to);
        if !self.r.expr(range, self.enclosing) {
            self.r.fail(range, self.enclosing, Mode::Refs, None);
        }
    }

    fn value(&mut self, from: usize, to: usize) {
        self.condition(from, to);
    }

    /// A query at tokens `[from, to)`.
    fn query(&mut self, from: usize, to: usize) {
        let to = to.min(self.len());
        if to <= from {
            return;
        }
        self.dml(from, to);
    }

    /// `FOR r IN (query) LOOP`: the query; ranges and cursors are read
    /// by their shape only.
    fn for_head(&mut self, from: usize, to: usize) {
        let to = to.min(self.len());
        let Some(k) = (from..to).find(|&k| self.up(k) == "IN") else { return };
        if self.tok(k + 1) == Some(&Token::LParen) && matches!(self.up(k + 2), "SELECT" | "WITH") {
            let close = self.close(k + 1);
            if close - 1 > k + 2 {
                self.query(k + 2, close - 1);
            }
        }
    }

    /// A query or DML statement at tokens `[from, to)`: sqlparser, with
    /// the variable lists of `INTO` blanked.
    fn dml(&mut self, from: usize, to: usize) {
        let range = self.span(from, to);
        let mut blanks = Vec::new();
        let select = matches!(self.up(from), "SELECT" | "WITH");
        let (mut depth, mut returning) = (0i32, false);
        let mut k = from;
        while k < to {
            match self.toks[k].token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                match self.up(k) {
                    "RETURNING" | "RETURN" => returning = true,
                    "INTO" if returning => {
                        blanks.push(self.span(k, to));
                        break;
                    }
                    "INTO" | "BULK" if select => {
                        let stop = (k + 1..to)
                            .find(|&j| matches!(self.up(j), "FROM" | "WHERE" | "USING" | "LIMIT"))
                            .unwrap_or(to);
                        blanks.push(self.span(k, stop));
                        k = stop;
                        continue;
                    }
                    _ => {}
                }
            }
            k += 1;
        }
        if !self.r.sql_edit(range, self.enclosing, Mode::Refs, None, &blanks) {
            self.r.fail(range, self.enclosing, Mode::Refs, None);
        }
    }

    /// One statement at tokens `[i, s)` that is not control flow.
    fn leaf(&mut self, i: usize, s: usize) {
        let range = self.span(i, s);
        let head = self.up(i);
        if DML.contains(&head) && !(head == "REPLACE" && self.tok(i + 1) == Some(&Token::LParen)) {
            return self.dml(i, s);
        }
        match head {
            "NULL" if s == i + 1 => {}
            "EXIT" | "CONTINUE" => {
                if let Some(w) = (i + 1..s).find(|&k| self.up(k) == "WHEN") {
                    self.condition(w + 1, s);
                }
            }
            "RETURN" => self.condition(i + 1, s),
            "OPEN" => {
                let f = self.find(i + 1, &["FOR"]);
                if f < s && matches!(self.up(f + 1), "SELECT" | "WITH") {
                    self.query(f + 1, s);
                }
            }
            "DECLARE" => {
                let f = self.find(i + 1, &["FOR", "IS"]);
                if f < s && (i + 1..f).any(|k| self.up(k) == "CURSOR") {
                    return self.query(f + 1, s);
                }
                let value = self.find(i + 1, &["DEFAULT"]);
                if value < s {
                    self.value(value + 1, s);
                }
            }
            "SET" => {
                if !self.r.sql(range, self.enclosing, Mode::Refs, None) {
                    let eq = self.scan(i + 1, |b, j| b.tok(j) == Some(&Token::Eq));
                    let more = (eq + 1..s).any(|k| self.tok(k) == Some(&Token::Eq));
                    if eq < s && !more {
                        self.value(eq + 1, s);
                    } else {
                        return self.r.leaf_own(range, self.enclosing, Mode::Refs, None);
                    }
                }
                return;
            }
            "CALL" => return self.r.leaf(range, self.enclosing, Mode::Refs, None),
            _ if CONTROL.contains(&head) => {}
            _ => {
                let assign = self.scan(i, |b, j| b.tok(j) == Some(&Token::Assignment));
                if assign < s {
                    self.value(assign + 1, s);
                } else if !self.call(i, s) {
                    return self.r.leaf(range, self.enclosing, Mode::Refs, None);
                }
            }
        }
        self.r.own += 1;
    }

    /// A procedure call statement at tokens `[i, s)`: `name[(arguments)]`
    /// (parsed as a call expression) or a chain of member calls
    /// `a.b(x).c(y)` (read by its shape: names, periods and balanced
    /// argument lists; calls inside come from the token scanner).
    fn call(&mut self, i: usize, s: usize) -> bool {
        let Some((name, after)) = scan::read_name(self.toks, i, self.r.b.dialect) else { return false };
        if after == s {
            self.r.b.call_ref(&name, self.enclosing);
            return true;
        }
        if self.tok(after) != Some(&Token::LParen) {
            return false;
        }
        let range = self.span(i, s);
        if self.close(after) == s && self.r.expr(range, self.enclosing) {
            return true;
        }
        // chain: `( … )` groups, `.`, words
        let mut k = after;
        while k < s {
            match self.tok(k) {
                Some(Token::LParen) => k = self.close(k),
                Some(Token::Period | Token::Word(_)) => k += 1,
                _ => return false,
            }
        }
        if k != s {
            return false;
        }
        scan::refs(self.r.b, self.r.masked, range, self.enclosing);
        true
    }
}
