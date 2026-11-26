//! T-SQL batches and routines (crate docs, *Procedural code*). T-SQL
//! statements need no terminator and a routine body runs to the end of
//! its batch, so sqlparser — which parses a batch whole — loses a whole
//! procedure to one construct it does not know. This reader walks the
//! tokens of a batch (or of one `;`-terminated piece of it) instead:
//!
//! - control flow is read here: `BEGIN … END`, `BEGIN TRY … END TRY BEGIN
//!   CATCH … END CATCH`, `IF cond stmt [ELSE stmt]`, `WHILE cond stmt`,
//!   `label:`; conditions are parsed as expressions by sqlparser;
//! - `CREATE | ALTER [OR ALTER] PROC | PROCEDURE | FUNCTION | TRIGGER`:
//!   the head gives the definition (parameters are not validated), the
//!   body after `AS` is read statement by statement with the routine as
//!   the enclosing definition; `CREATE | ALTER VIEW … AS query` parses its
//!   query;
//! - every other statement runs to the next token that starts a statement
//!   at parenthesis depth 0 outside `CASE … END` ([`Tsql::starts`]) and is
//!   parsed on its own: sqlparser (with repairs), then the pack's own
//!   forms ([`crate::utility`]), else one parse error.

use rueca_model::LocalDefId;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::Token;

use crate::ast::Ctx;
use crate::dialect::Dialect;
use crate::read::{Mode, Reader};
use crate::scan::{self, Tok};

/// Reads the statement at `ctx.range` as T-SQL; `false` when it cannot be
/// tokenized.
pub fn read(r: &mut Reader<'_, '_>, ctx: &Ctx) -> bool {
    let toks = scan::tokens(r.masked, ctx.range.0, ctx.range.1, Dialect::MsSql);
    if toks.is_empty() {
        return false;
    }
    let ups: Vec<String> = toks.iter().map(|t| t.word().to_ascii_uppercase()).collect();
    let mut t = Tsql {
        r,
        toks: &toks,
        ups: &ups,
        enclosing: ctx.enclosing,
        mode: Mode::Define,
        doc: ctx.doc.clone(),
        end: ctx.range.1,
    };
    t.sequence(0);
    true
}

struct Tsql<'r, 'a, 's, 't> {
    r: &'r mut Reader<'a, 's>,
    toks: &'t [Tok],
    /// Upper-case unquoted words, `""` for other tokens.
    ups: &'t [String],
    enclosing: Option<LocalDefId>,
    mode: Mode,
    doc: Option<String>,
    end: usize,
}

/// What the statement being delimited has shown so far.
struct State {
    head: String,
    seen_set: bool,
    seen_source: bool,
    seen_dml: bool,
    seen_action: bool,
    seen_to: bool,
    at: usize,
}

impl State {
    /// The source of an `INSERT` (`SELECT`, `EXEC`), when it has none
    /// yet: taken.
    fn takes_source(&mut self) -> bool {
        let takes = self.head == "INSERT" && !self.seen_source;
        self.seen_source |= takes;
        takes
    }

    /// The `SET` of an `UPDATE`, when it has none yet: taken.
    fn takes_set(&mut self) -> bool {
        let takes = self.head == "UPDATE" && !self.seen_set;
        self.seen_set |= takes;
        takes
    }

    /// The action of an `ALTER` (`SET`, `DROP`, `ALTER`), when it has none
    /// yet: taken.
    fn takes_action(&mut self) -> bool {
        let takes = self.head == "ALTER" && !self.seen_action;
        self.seen_action |= takes;
        takes
    }

    /// The statement of a CTE list (`WITH … SELECT`), when it has none
    /// yet: taken, and the statement goes on as that kind.
    fn takes_dml(&mut self, word: &str) -> bool {
        let takes = self.head == "WITH" && !self.seen_dml;
        if takes {
            self.seen_dml = true;
            self.head = word.to_string();
        }
        takes
    }

    /// A word that starts no statement: an action of `ALTER`, the source
    /// of an `INSERT`.
    fn note(&mut self, word: &str) {
        if self.head == "ALTER" && ALTER_ACTIONS.contains(&word) {
            self.seen_action = true;
        }
        if self.head == "INSERT" && matches!(word, "VALUES" | "DEFAULT") {
            self.seen_source = true;
        }
    }
}

/// Words that always start a statement (at depth 0, after the guards of
/// [`Tsql::starts`]).
const STARTERS: &[&str] = &[
    "DECLARE",
    "PRINT",
    "RAISERROR",
    "THROW",
    "GOTO",
    "BREAK",
    "CONTINUE",
    "WAITFOR",
    "TRUNCATE",
    "RETURN",
    "COMMIT",
    "ROLLBACK",
    "DBCC",
    "CHECKPOINT",
    "RECONFIGURE",
    "BACKUP",
    "RESTORE",
    "KILL",
    "DEALLOCATE",
    "GRANT",
    "DENY",
    "REVOKE",
    "REVERT",
    "WHILE",
    "SHUTDOWN",
    "CREATE",
    "BEGIN",
    "SEND",
    "RECEIVE",
];

/// Words after which the next word is an operand or a name, never the
/// start of a statement.
const NAME_AFTER: &[&str] = &[
    "AS",
    "TABLE",
    "PROCEDURE",
    "PROC",
    "FUNCTION",
    "VIEW",
    "TRIGGER",
    "INDEX",
    "DATABASE",
    "BY",
    "AND",
    "OR",
    "WHERE",
    "JOIN",
    "INTO",
    "NOT",
    "IN",
    "LIKE",
    "IS",
    "BETWEEN",
    "WHEN",
    "UNION",
    "ALL",
    "EXCEPT",
    "INTERSECT",
    "EXISTS",
    "TOP",
    "DISTINCT",
    "SELECT",
    "ON",
    "FOR",
    "WITH",
    "FROM",
    "APPLY",
    "HAVING",
];

/// Action words of `ALTER object name …`.
const ALTER_ACTIONS: &[&str] = &[
    "ADD",
    "REBUILD",
    "DISABLE",
    "ENABLE",
    "REORGANIZE",
    "CHECK",
    "NOCHECK",
    "SWITCH",
    "RENAME",
    "MODIFY",
    "COLLATE",
    "WITH",
];

const TRANSACTION: &[&str] = &["TRAN", "TRANSACTION", "DISTRIBUTED", "DIALOG", "CONVERSATION"];

#[derive(Clone, Copy, PartialEq)]
enum Routine {
    Body,
    Function,
    View,
}

impl Tsql<'_, '_, '_, '_> {
    fn up(&self, i: usize) -> &str {
        self.ups.get(i).map_or("", String::as_str)
    }

    fn tok(&self, i: usize) -> Option<&Token> {
        self.toks.get(i).map(|t| &t.token)
    }

    fn range(&self, from: usize, to: usize) -> (usize, usize) {
        (self.toks[from].range.0, self.toks[to - 1].range.1)
    }

    /// Statements from `i` to the end; a stray `END` is an error.
    fn sequence(&mut self, mut i: usize) {
        while i < self.toks.len() {
            if self.up(i) == "END" {
                // `END CONVERSATION @handle` is a statement; any other
                // `END` here closes nothing.
                let e = self.boundary(i + 1, "END").max(i + 1);
                let range = self.range(i, e);
                if self.up(i + 1) == "CONVERSATION" {
                    self.r.own += 1;
                } else {
                    self.r.fail(range, self.enclosing, Mode::Refs, None);
                }
                i = e;
            } else {
                i = self.statement(i);
            }
        }
    }

    /// Statements up to the `END` of the block; the index after it.
    fn block(&mut self, mut i: usize) -> usize {
        while i < self.toks.len() {
            if self.up(i) == "END" {
                return i + 1;
            }
            i = self.statement(i);
        }
        i
    }

    /// One statement at `i`; the index after it (`i` itself only at an
    /// `END`, which belongs to the caller's block).
    fn statement(&mut self, i: usize) -> usize {
        if self.toks[i].token == Token::SemiColon {
            return i + 1;
        }
        if let Some(next) = self.compound(i) {
            return next;
        }
        if self.tok(i + 1) == Some(&Token::Colon) && matches!(self.tok(i), Some(Token::Word(_))) {
            return i + 2;
        }
        self.plain(i)
    }

    /// Control flow, routines and returned queries at `i`: the index
    /// after them; `None` for a plain statement.
    fn compound(&mut self, i: usize) -> Option<usize> {
        let n = self.toks.len();
        match self.up(i) {
            "END" => Some(i),
            "BEGIN" if matches!(self.up(i + 1), "TRY" | "CATCH") => {
                let j = self.block(i + 2);
                Some(if matches!(self.up(j), "TRY" | "CATCH") { j + 1 } else { j })
            }
            "BEGIN" if !TRANSACTION.contains(&self.up(i + 1)) => Some(self.block(i + 1)),
            "IF" | "WHILE" => Some(self.conditional(i)),
            "ELSE" => Some(if i + 1 < n { self.statement(i + 1).max(i + 1) } else { n }),
            "CREATE" | "ALTER" => self.routine_kind(i).map(|kind| self.routine(i, kind)),
            "RETURN" if self.returns_query(i) => Some(self.returned_query(i)),
            _ => None,
        }
    }

    /// `RETURN query` at `i`: the query; the index after it.
    fn returned_query(&mut self, i: usize) -> usize {
        let e = self.boundary(i + 1, "RETURN").max(i + 2);
        let range = self.range(i + 1, e);
        if !self.r.sql(range, self.enclosing, Mode::Refs, None) {
            self.r.fail(range, self.enclosing, Mode::Refs, None);
        }
        e
    }

    /// A statement that is not control flow, at `i`: the index after it.
    fn plain(&mut self, i: usize) -> usize {
        let e = self.boundary(i + 1, self.up(i));
        let range = self.range(i, e);
        let doc = if self.mode == Mode::Define { self.doc.take() } else { None };
        if i == 0 && self.implicit_exec(i) {
            // `sp_who2 'active'`: a procedure name first in its batch.
            if let Some((name, _)) = scan::read_name(self.toks, i, Dialect::MsSql) {
                self.r.b.call_ref(&name, self.enclosing);
            }
            self.r.own += 1;
        } else if !self.own_leaf(i, e) {
            self.r.leaf(range, self.enclosing, self.mode, doc);
        }
        e
    }

    /// Statements at tokens `[i, e)` this reader takes apart itself:
    /// `INSERT … EXEC …` (the target and the call), and cursors `DECLARE c
    /// CURSOR … FOR query` / `SET @c = CURSOR … FOR query` (the query).
    fn own_leaf(&mut self, i: usize, e: usize) -> bool {
        let depth0 = |t: &Self, words: &[&str]| {
            let mut depth = 0i32;
            (i + 1..e).find(|&k| {
                match t.toks[k].token {
                    Token::LParen => depth += 1,
                    Token::RParen => depth -= 1,
                    _ => {}
                }
                depth == 0 && words.contains(&t.up(k))
            })
        };
        match self.up(i) {
            "INSERT" => {
                let Some(k) = depth0(self, &["EXEC", "EXECUTE", "SELECT", "VALUES"]) else { return false };
                if !matches!(self.up(k), "EXEC" | "EXECUTE") {
                    return false;
                }
                scan::refs(self.r.b, self.r.masked, self.range(i, e), self.enclosing);
            }
            "DECLARE" | "SET" => {
                let Some(c) = (i + 1..e.min(i + 6)).find(|&k| self.up(k) == "CURSOR") else { return false };
                let Some(f) = (c + 1..e).find(|&k| self.up(k) == "FOR") else { return false };
                if f + 1 >= e {
                    return false;
                }
                // `FOR query [FOR UPDATE [OF …]]`
                let end = (f + 2..e)
                    .find(|&k| self.up(k) == "FOR" && matches!(self.up(k + 1), "UPDATE" | "READ"))
                    .unwrap_or(e);
                let query = self.range(f + 1, end);
                if !self.r.sql(query, self.enclosing, Mode::Refs, None) {
                    self.r.fail(query, self.enclosing, Mode::Refs, None);
                    return true;
                }
            }
            "ALTER" => return self.added_columns(i, e),
            _ => return false,
        }
        self.r.own += 1;
        true
    }

    /// `ALTER TABLE t ADD a INT, b INT, …` at tokens `[i, e)`, which
    /// sqlparser rejects as a whole: each column is parsed as its own
    /// `ALTER TABLE t ADD` (the other columns blanked). `false` when the
    /// statement is not such a list or one of its columns does not parse.
    fn added_columns(&mut self, i: usize, e: usize) -> bool {
        let range = self.range(i, e);
        let Some(items) = self.add_items(i, e) else { return false };
        if items.len() < 2 || self.r.sql_parses(range, &[]) {
            return false;
        }
        let others = |k: usize| -> Vec<(usize, usize)> {
            let mut blanks = Vec::new();
            if k > 0 {
                blanks.push((items[0].0, items[k].0));
            }
            if k + 1 < items.len() {
                blanks.push((items[k].1, range.1));
            }
            blanks
        };
        if !(0..items.len()).all(|k| self.r.sql_parses(range, &others(k))) {
            return false;
        }
        let doc = if self.mode == Mode::Define { self.doc.take() } else { None };
        for k in 0..items.len() {
            self.r.sql_edit(range, self.enclosing, self.mode, doc.as_ref(), &others(k));
        }
        self.r.own += 1;
        true
    }

    /// The byte ranges of the items of `ALTER TABLE name ADD item, item, …`
    /// at tokens `[i, e)`.
    fn add_items(&self, i: usize, e: usize) -> Option<Vec<(usize, usize)>> {
        if self.up(i + 1) != "TABLE" {
            return None;
        }
        let (_, after) = scan::read_name(self.toks, i + 2, Dialect::MsSql)?;
        if self.up(after) != "ADD" || after + 1 >= e {
            return None;
        }
        let mut items = Vec::new();
        let (mut depth, mut start) = (0i32, after + 1);
        for k in after + 1..e {
            match self.toks[k].token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::Comma if depth == 0 && k > start => {
                    items.push(self.range(start, k));
                    start = k + 1;
                }
                _ => {}
            }
        }
        if start < e && self.toks[e - 1].token != Token::SemiColon {
            items.push(self.range(start, e));
        }
        Some(items)
    }

    /// `IF cond stmt [ELSE stmt]`, `WHILE cond stmt`.
    fn conditional(&mut self, i: usize) -> usize {
        let n = self.toks.len();
        let c = self.boundary(i + 1, "IF");
        if c > i + 1 {
            let range = self.range(i + 1, c);
            if !self.r.expr(range, self.enclosing) {
                self.r.fail(range, self.enclosing, Mode::Refs, None);
            }
        }
        let mut j = if c < n { self.statement(c) } else { n };
        if self.up(i) == "IF" {
            let k = if self.tok(j) == Some(&Token::SemiColon) { j + 1 } else { j };
            if self.up(k) == "ELSE" {
                j = self.statement(k);
            }
        }
        j.max(c)
    }

    fn routine_kind(&self, i: usize) -> Option<Routine> {
        let mut j = i + 1;
        if self.up(j) == "OR" && self.up(j + 1) == "ALTER" {
            j += 2;
        }
        match self.up(j) {
            "PROC" | "PROCEDURE" | "TRIGGER" => Some(Routine::Body),
            "FUNCTION" => Some(Routine::Function),
            "VIEW" => Some(Routine::View),
            _ => None,
        }
    }

    /// A routine or view from `i` to the end of the tokens.
    fn routine(&mut self, i: usize, kind: Routine) -> usize {
        let n = self.toks.len();
        let range = (self.toks[i].range.0, self.end);
        let doc = self.doc.take();
        let Some(body) = self.body_start(i, kind) else {
            self.r.leaf_own(range, self.enclosing, self.mode, doc);
            return n;
        };
        let def = match self.mode {
            Mode::Define => scan::head(self.r.b, &self.toks[i..], range, doc).map(|(d, _)| d),
            Mode::Refs => self.enclosing,
        };
        let Some(def) = def else {
            self.r.fail(range, self.enclosing, self.mode, None);
            return n;
        };
        if body >= n || self.up(body) == "EXTERNAL" {
            return n;
        }
        if kind == Routine::View {
            let query = self.range(body, n);
            if !self.r.sql(query, Some(def), Mode::Refs, None) {
                self.r.fail(query, Some(def), Mode::Refs, None);
            }
            return n;
        }
        let saved = (self.enclosing, self.mode);
        (self.enclosing, self.mode) = (Some(def), Mode::Refs);
        self.sequence(body);
        (self.enclosing, self.mode) = saved;
        n
    }

    /// Index of the first token of the body: after the `AS` that ends the
    /// head (not `EXECUTE AS`), or at a function's `BEGIN` / `RETURN`.
    fn body_start(&self, i: usize, kind: Routine) -> Option<usize> {
        let mut depth = 0i32;
        for j in i + 1..self.toks.len() {
            match self.toks[j].token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ => {}
            }
            if depth != 0 {
                continue;
            }
            match self.up(j) {
                "AS" if !matches!(self.up(j - 1), "EXECUTE" | "EXEC") => return Some(j + 1),
                "BEGIN" | "RETURN" if kind == Routine::Function => return Some(j),
                _ => {}
            }
        }
        None
    }

    /// `RETURN SELECT …`, `RETURN WITH cte AS (…) …`, `RETURN (SELECT …)`:
    /// the query of an inline table-valued function.
    fn returns_query(&self, i: usize) -> bool {
        matches!(self.up(i + 1), "SELECT" | "WITH")
            || (self.tok(i + 1) == Some(&Token::LParen) && matches!(self.up(i + 2), "SELECT" | "WITH"))
    }

    /// A procedure call without `EXEC`, first in its piece of the batch:
    /// a name that is no keyword, then only arguments — literals,
    /// variables, `@name = value`, `DEFAULT`, `OUTPUT`.
    fn implicit_exec(&self, i: usize) -> bool {
        let named = matches!(&self.toks[i].token, Token::Word(w) if (w.keyword == Keyword::NoKeyword || w.quote_style.is_some()) && !w.value.starts_with('@'));
        let Some((_, after)) = scan::read_name(self.toks, i, Dialect::MsSql) else { return false };
        let e = self.boundary(i + 1, self.up(i));
        named
            && (after..e).all(|k| match &self.toks[k].token {
                Token::Word(w) => {
                    w.value.starts_with('@') || matches!(self.up(k), "DEFAULT" | "OUTPUT" | "OUT" | "NULL")
                }
                Token::Comma
                | Token::Eq
                | Token::Minus
                | Token::Number(..)
                | Token::SingleQuotedString(_)
                | Token::NationalStringLiteral(_)
                | Token::HexStringLiteral(_) => true,
                _ => false,
            })
    }

    /// The index of the token that ends the statement (or condition)
    /// whose first word is `head` and whose tokens from `from` are still
    /// to read: a `;`, or the first token that starts another statement.
    fn boundary(&self, from: usize, head: &str) -> usize {
        let mut st = State {
            head: head.to_string(),
            seen_set: head == "UPDATE" && self.up(from) == "STATISTICS",
            seen_source: false,
            seen_dml: false,
            seen_action: false,
            seen_to: false,
            at: from,
        };
        let (mut depth, mut case) = (0i32, 0i32);
        let mut j = from;
        while j < self.toks.len() {
            let tok = &self.toks[j].token;
            if depth == 0 && case == 0 && (*tok == Token::SemiColon || self.starts(j, &mut st)) {
                return j;
            }
            match tok {
                Token::LParen => depth += 1,
                Token::RParen => depth = (depth - 1).max(0),
                _ => {}
            }
            match self.up(j) {
                "CASE" => case += 1,
                "END" if case > 0 => case -= 1,
                _ => {}
            }
            j += 1;
        }
        j
    }

    /// Whether token `j` (at depth 0, outside `CASE`) starts a statement,
    /// given what the current one has shown (`st`, updated).
    fn starts(&self, j: usize, st: &mut State) -> bool {
        let w = self.up(j);
        let prev = if j > 0 { self.up(j - 1) } else { "" };
        if let Some(decided) = self.continues(j, st, w, prev) {
            return decided;
        }
        let head = st.head.clone();
        if let Some(decided) = self.dml_starts(j, st, w, prev, &head) {
            return decided;
        }
        self.other_starts(j, st, w, prev, &head)
    }

    /// Positions decided before looking at the word: a token that is no
    /// word, a label, `END` / `ELSE`, an operand or a name, the
    /// privileges of `GRANT`.
    fn continues(&self, j: usize, st: &mut State, w: &str, prev: &str) -> Option<bool> {
        if w.is_empty() {
            if self.tok(j) == Some(&Token::LParen) && st.head == "INSERT" && self.up(j + 1) == "SELECT" {
                st.seen_source = true;
            }
            return Some(false);
        }
        if self.tok(j + 1) == Some(&Token::Colon) && !w.starts_with('@') {
            return Some(true);
        }
        if matches!(w, "END" | "ELSE") {
            return Some(true);
        }
        if self.is_operand(j) || self.tok(j + 1) == Some(&Token::Period) {
            return Some(false);
        }
        if matches!(st.head.as_str(), "GRANT" | "DENY" | "REVOKE") && !st.seen_to {
            st.seen_to = matches!(w, "TO" | "FROM");
            return Some(false);
        }
        // `SET NOCOUNT ON` ends at its `ON`: the next word is a statement.
        let after_set_on = st.head == "SET" && prev == "ON";
        (NAME_AFTER.contains(&prev) && !after_set_on).then_some(false)
    }

    /// Whether the token before `j` leaves an expression open (an
    /// operator, a comma, `(`): `j` is an operand.
    fn is_operand(&self, j: usize) -> bool {
        j > 0
            && !matches!(
                self.toks[j - 1].token,
                Token::Word(_)
                    | Token::RParen
                    | Token::Number(..)
                    | Token::SingleQuotedString(_)
                    | Token::NationalStringLiteral(_)
            )
    }

    /// Words that start a statement or continue the current one depending
    /// on what it has shown: DML, `EXEC`, `SET`, `DROP`, `ALTER`, `WITH`.
    fn dml_starts(&self, j: usize, st: &mut State, w: &str, prev: &str, head: &str) -> Option<bool> {
        let merge = head == "MERGE";
        Some(match w {
            "SELECT" => !(st.takes_source() || (head == "RETURN" && j == st.at) || st.takes_dml(w)),
            "EXEC" | "EXECUTE" => !st.takes_source(),
            "INSERT" | "DELETE" => !merge && !st.takes_dml(w),
            "UPDATE" => !merge && self.tok(j + 1) != Some(&Token::LParen) && !st.takes_dml(w),
            "MERGE" => {
                self.up(j + 1) != "JOIN"
                    && !matches!(prev, "INNER" | "LEFT" | "RIGHT" | "FULL" | "OUTER")
                    && !st.takes_dml(w)
            }
            "SET" => !(st.takes_set() || st.takes_action()) && !merge && !matches!(self.up(j + 1), "NULL" | "DEFAULT"),
            "DROP" | "ALTER" => !st.takes_action(),
            "WITH" => self.with_starts(j, st, head),
            _ => return None,
        })
    }

    /// `WITH` at `j`: the CTE list of a returned query continues it; else
    /// a statement when it opens a CTE list.
    fn with_starts(&self, j: usize, st: &mut State, head: &str) -> bool {
        if head == "RETURN" && j == st.at {
            st.head = "WITH".to_string();
            return false;
        }
        self.is_cte(j)
    }

    /// The other words: starters by their next or previous word, the
    /// words that always start a statement, and what a word that starts
    /// none tells about the current statement.
    fn other_starts(&self, j: usize, st: &mut State, w: &str, prev: &str, head: &str) -> bool {
        match w {
            "IF" => !(matches!(head, "DROP" | "CREATE" | "ALTER") && matches!(self.up(j + 1), "EXISTS" | "NOT")),
            "USE" | "OPEN" | "CLOSE" => matches!(self.tok(j + 1), Some(Token::Word(_))),
            "SAVE" => matches!(self.up(j + 1), "TRAN" | "TRANSACTION"),
            "FETCH" => !matches!(prev, "ROWS" | "ROW" | "ONLY"),
            "ENABLE" | "DISABLE" if head == "ALTER" => {
                st.seen_action = true;
                false
            }
            "ENABLE" | "DISABLE" => self.up(j + 1) == "TRIGGER",
            "BULK" => self.up(j + 1) == "INSERT",
            _ if STARTERS.contains(&w) => true,
            _ => {
                st.note(w);
                false
            }
        }
    }

    /// `WITH name [(columns)] AS (` or `WITH XMLNAMESPACES (`.
    fn is_cte(&self, j: usize) -> bool {
        if self.up(j + 1) == "XMLNAMESPACES" {
            return true;
        }
        if !matches!(self.tok(j + 1), Some(Token::Word(_))) {
            return false;
        }
        let mut k = j + 2;
        if self.tok(k) == Some(&Token::LParen) {
            let mut depth = 0i32;
            while k < self.toks.len() {
                match self.toks[k].token {
                    Token::LParen => depth += 1,
                    Token::RParen => depth -= 1,
                    _ => {}
                }
                k += 1;
                if depth == 0 {
                    break;
                }
            }
        }
        self.up(k) == "AS" && self.tok(k + 1) == Some(&Token::LParen)
    }
}
