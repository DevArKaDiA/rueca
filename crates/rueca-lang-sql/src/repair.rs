//! Same-length repairs (crate docs, *Statements and recovery*): constructs
//! sqlparser rejects although the rest of the statement is plain SQL are
//! rewritten in place — blanked, or replaced by an equivalent of the same
//! length — and the statement is parsed again. Byte positions do not move,
//! so every name keeps its place in the file. A repair never removes a
//! relation, column or routine name the pack would report, except storage
//! names (`TABLESPACE x`, `ON [PRIMARY]`).
//!
//! | Dialect | Construct | Repair |
//! |---|---|---|
//! | any (Jinja) | a placeholder right after an expression (`a, b {{ extra }} order by`), a macro that expands to a clause | blanked |
//! | any (Jinja) | a placeholder before `over (` (a macro that expands to a window function) | written as a call |
//! | PostgreSQL | named arguments `f(a := 1)` | `f(a => 1)` |
//! | PostgreSQL | `LIKE t INCLUDING ALL` / `EXCLUDING …` options | blanked |
//! | PostgreSQL | `TABLESPACE name`, `CREATE OR REPLACE PROCEDURE`'s `OR REPLACE` | blanked |
//! | PostgreSQL | arguments of `EXECUTE PROCEDURE f('a', 'b')` in `CREATE TRIGGER` | blanked |
//! | T-SQL | `CLUSTERED` / `NONCLUSTERED`, index and table options `WITH (PAD_INDEX = …)`, filegroups `ON [PRIMARY]` / `TEXTIMAGE_ON […]` | blanked |
//! | T-SQL | compound assignment `+=`, `-=`, `*=`, `/=` | `=` |
//! | T-SQL | query hints `OPTION (…)`, table hints `(NOLOCK)` / `WITH (TABLOCK)` on a target, `WITH XMLNAMESPACES (…)` | blanked |
//! | T-SQL | `FOR XML PATH(N'')`'s `N`; `FOREIGN KEY` of a column constraint; the column list of `INSERT … (a) DEFAULT VALUES` | blanked |
//! | T-SQL | computed column `c AS expr` | `c INT` |
//! | T-SQL | `ALTER TABLE t ADD a INT, b INT` | `ADD` written before each column, over its indentation |
//! | T-SQL | join hints `INNER HASH JOIN`, `FROM ::fn()`'s `::`, inline indexes `, INDEX ix (a)` of a table | blanked |
//! | Oracle | PL/SQL attributes `c%NOTFOUND` / `t.c%TYPE`, the type of `TREAT(x AS type)` | blanked |
//! | Oracle | collection conditions `a [NOT] MEMBER OF b`, `a MULTISET UNION b`, `a IS [NOT] EMPTY` | `a = b`, `a \|\| b`, `a IS [NOT] NULL` |
//! | Oracle | physical attributes of tables and indexes, `VARCHAR2(n CHAR)` length units, `ENABLE` / `DISABLE` after a constraint, `FORCE` / `EDITIONABLE` | blanked |
//! | DB2 / Oracle | `GENERATED … AS IDENTITY (options)` options | blanked |
//! | MySQL | `DEFINER = user@host`, `ALGORITHM = …`, `SQL SECURITY …`, `VARCHAR(n) BINARY` | blanked |
//! | SQLite | the words after the first of a type name of several words (`BLOB SUB_TYPE TEXT`) | blanked |
//! | any (Jinja) | a placeholder between select-list items with no comma of its own (a macro that expands to items and their comma) | blanked |

use sqlparser::tokenizer::Token;

use crate::dialect::Dialect;
use crate::facts::is_placeholder;
use crate::prep::blank;
use crate::scan::{Tok, tokens};

/// The repaired text of `chunk`, when a repair applies.
pub fn repaired(chunk: &str, dialect: Dialect) -> Option<String> {
    let toks = tokens(chunk, 0, chunk.len(), dialect);
    if toks.is_empty() {
        return None;
    }
    let mut r = Repair { toks: &toks, out: chunk.as_bytes().to_vec(), changed: false };
    r.jinja();
    match dialect {
        Dialect::Postgres | Dialect::Redshift | Dialect::DuckDb | Dialect::Generic => r.postgres(),
        Dialect::MsSql => r.tsql(),
        Dialect::Oracle => r.oracle(),
        Dialect::Db2 => r.db2(),
        Dialect::MySql => r.mysql(),
        Dialect::Sqlite => r.sqlite(),
        _ => {}
    }
    if dialect == Dialect::Generic {
        r.db2();
    }
    r.changed.then(|| String::from_utf8(r.out).unwrap_or_default())
}

struct Repair<'t> {
    toks: &'t [Tok],
    out: Vec<u8>,
    changed: bool,
}

const TSQL_OPTIONS: &[&str] = &[
    "PAD_INDEX",
    "STATISTICS_NORECOMPUTE",
    "IGNORE_DUP_KEY",
    "ALLOW_ROW_LOCKS",
    "ALLOW_PAGE_LOCKS",
    "FILLFACTOR",
    "DATA_COMPRESSION",
    "OPTIMIZE_FOR_SEQUENTIAL_KEY",
    "ONLINE",
    "SORT_IN_TEMPDB",
    "DROP_EXISTING",
    "MAXDOP",
    "STATISTICS_INCREMENTAL",
    "RESUMABLE",
    "SYSTEM_VERSIONING",
    "MEMORY_OPTIMIZED",
    "DURABILITY",
];

const TABLE_HINTS: &[&str] = &[
    "NOLOCK",
    "TABLOCK",
    "TABLOCKX",
    "ROWLOCK",
    "UPDLOCK",
    "HOLDLOCK",
    "READPAST",
    "READUNCOMMITTED",
    "READCOMMITTED",
    "REPEATABLEREAD",
    "SERIALIZABLE",
    "XLOCK",
    "PAGLOCK",
    "NOWAIT",
    "FORCESEEK",
    "FORCESCAN",
    "SNAPSHOT",
];

/// Oracle physical attributes that take one value.
const ORACLE_VALUED: &[&str] = &["PCTFREE", "PCTUSED", "INITRANS", "MAXTRANS", "TABLESPACE", "COMPRESS"];
/// Oracle attributes that are one word.
const ORACLE_FLAGS: &[&str] = &[
    "LOGGING",
    "NOLOGGING",
    "NOCOMPRESS",
    "NOPARALLEL",
    "PARALLEL",
    "NOCACHE",
    "MONITORING",
    "NOMONITORING",
    "EDITIONABLE",
    "NONEDITIONABLE",
];

impl Repair<'_> {
    fn is(&self, i: usize, word: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.is(word))
    }

    fn any(&self, i: usize, words: &[&str]) -> bool {
        words.iter().any(|w| self.is(i, w))
    }

    fn tok(&self, i: usize) -> Option<&Token> {
        self.toks.get(i).map(|t| &t.token)
    }

    fn blank(&mut self, from: usize, to: usize) {
        if from < to && to <= self.toks.len() {
            blank(&mut self.out, self.toks[from].range.0, self.toks[to - 1].range.1);
            self.changed = true;
        }
    }

    /// Index after the `)` matching the `(` at `open` (the end when none).
    fn close(&self, open: usize) -> usize {
        let mut depth = 0i32;
        for (j, t) in self.toks.iter().enumerate().skip(open) {
            match t.token {
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
        self.toks.len()
    }

    fn jinja(&mut self) {
        for i in 0..self.toks.len() {
            let t = &self.toks[i];
            if !is_placeholder(t.word()) || t.word().len() < 2 {
                continue;
            }
            if self.is(i + 1, "OVER") && t.word().len() >= 3 {
                let (_, e) = t.range;
                self.out[e - 2] = b'(';
                self.out[e - 1] = b')';
                self.changed = true;
                continue;
            }
            let after_expr = i.checked_sub(1).is_some_and(|p| match &self.toks[p].token {
                Token::Word(w) => w.quote_style.is_some() || !is_clause_word(&w.value),
                Token::RParen | Token::Number(..) | Token::SingleQuotedString(_) => true,
                _ => false,
            });
            // a macro that expands to select-list items with their comma
            // (`select a, {{ extra_columns() }} b from t`)
            let list_item = i.checked_sub(1).is_some_and(|p| {
                self.toks[p].token == Token::Comma || self.toks[p].is("SELECT") || self.toks[p].is("DISTINCT")
            }) && self.toks.get(i + 1).is_some_and(|n| match &n.token {
                Token::Word(w) => w.quote_style.is_some() || !is_clause_word(&w.value),
                Token::Number(..) | Token::SingleQuotedString(_) => true,
                _ => false,
            }) && self.tok(i + 2) != Some(&Token::Comma)
                && !self.is(i + 2, "FROM");
            if after_expr || list_item {
                self.blank(i, i + 1);
            }
        }
    }

    fn postgres(&mut self) {
        let create = self.is(0, "CREATE");
        for i in 0..self.toks.len() {
            if self.tok(i) == Some(&Token::Assignment) {
                let (s, _) = self.toks[i].range;
                self.out[s] = b'=';
                self.out[s + 1] = b'>';
                self.changed = true;
            } else if self.any(i, &["INCLUDING", "EXCLUDING"]) && self.toks.get(i + 1).is_some_and(|t| !t.word().is_empty())
            {
                self.blank(i, i + 2);
            } else if create && self.is(i, "TABLESPACE") && self.toks.get(i + 1).is_some_and(|t| !t.word().is_empty()) {
                self.blank(i, i + 2);
            } else if create && i == 1 && self.is(i, "OR") && self.is(i + 1, "REPLACE") && self.is(i + 2, "PROCEDURE") {
                self.blank(i, i + 2);
            } else if create
                && self.is(i, "EXECUTE")
                && self.any(i + 1, &["PROCEDURE", "FUNCTION"])
                && let Some(open) = (i + 2..self.toks.len().min(i + 8)).find(|&j| self.tok(j) == Some(&Token::LParen))
            {
                let close = self.close(open);
                if close > open + 2 {
                    self.blank(open + 1, close - 1);
                }
            }
        }
    }

    fn tsql(&mut self) {
        let ddl = self.any(0, &["CREATE", "ALTER", "DECLARE"]);
        let mut depth = 0i32;
        let mut i = 0;
        while i < self.toks.len() {
            match self.toks[i].token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ => {}
            }
            if self.any(i, &["CLUSTERED", "NONCLUSTERED"]) && (ddl || depth > 0) {
                self.blank(i, i + 1);
            } else if self.is(i, "WITH")
                && self.tok(i + 1) == Some(&Token::LParen)
                && self.any(i + 2, TSQL_OPTIONS)
                && ddl
            {
                let close = self.close(i + 1);
                self.blank(i, close);
                i = close;
                continue;
            } else if ddl
                && self.any(i, &["ON", "TEXTIMAGE_ON"])
                && i > 0
                && self.tok(i - 1) == Some(&Token::RParen)
                && depth <= 1
                && matches!(self.tok(i + 1), Some(Token::Word(_)))
                && !self.any(i + 1, &["DELETE", "UPDATE"])
                && self.tok(i + 2) != Some(&Token::Period)
            {
                let end = if self.tok(i + 2) == Some(&Token::LParen) { self.close(i + 2) } else { i + 2 };
                self.blank(i, end);
                i = end;
                continue;
            } else if matches!(self.tok(i), Some(Token::Plus | Token::Minus | Token::Mul | Token::Div))
                && self.tok(i + 1) == Some(&Token::Eq)
                && self.toks[i].range.1 == self.toks[i + 1].range.0
            {
                self.blank(i, i + 1);
            } else if self.is(i, "OPTION") && self.tok(i + 1) == Some(&Token::LParen) && depth == 0 {
                let close = self.close(i + 1);
                self.blank(i, close);
                i = close;
                continue;
            } else if self.tok(i) == Some(&Token::LParen)
                && self.any(i + 1, TABLE_HINTS)
                && i > 0
                && matches!(self.tok(i - 1), Some(Token::Word(_)))
            {
                // `t (NOLOCK)`, `INSERT #t WITH (TABLOCK) (…)`
                let close = self.close(i);
                if (i + 1..close - 1).all(|j| self.any(j, TABLE_HINTS) || self.tok(j) == Some(&Token::Comma)) {
                    let from = if self.is(i - 1, "WITH") { i - 1 } else { i };
                    self.blank(from, close);
                    depth -= 1;
                    i = close;
                    continue;
                }
            } else if self.is(i, "XMLNAMESPACES") && self.tok(i + 1) == Some(&Token::LParen) && i > 0 && self.is(i - 1, "WITH") {
                // `WITH XMLNAMESPACES (…) SELECT`, `WITH XMLNAMESPACES (…), cte AS`
                let close = self.close(i + 1);
                if self.tok(close) == Some(&Token::Comma) {
                    self.blank(i, close + 1);
                } else {
                    self.blank(i - 1, close);
                }
                i = close;
                continue;
            } else if matches!(self.tok(i), Some(Token::NationalStringLiteral(_)))
                && i > 1
                && self.tok(i - 1) == Some(&Token::LParen)
                && self.any(i - 2, &["PATH", "ROOT", "RAW", "AUTO", "ELEMENTS"])
            {
                // `FOR XML PATH(N'')`
                let s = self.toks[i].range.0;
                self.out[s] = b' ';
                self.changed = true;
            } else if self.any(i, &["HASH", "LOOP", "MERGE", "REMOTE"]) && self.is(i + 1, "JOIN") {
                // join hints `INNER HASH JOIN`
                self.blank(i, i + 1);
            } else if self.tok(i) == Some(&Token::DoubleColon) && i > 0 && self.any(i - 1, &["FROM", "JOIN", "APPLY"]) {
                // `FROM ::fn_x(…)`
                self.blank(i, i + 1);
            } else if self.is(i, "INDEX")
                && depth == 1
                && ddl
                && i > 0
                && !self.any(i - 1, &["CREATE", "UNIQUE", "DROP", "ALTER", "COLUMNSTORE"])
                && matches!(self.tok(i + 1), Some(Token::Word(_)))
            {
                // inline index of a table: `, INDEX ix [UNIQUE] [CLUSTERED] (a, b)`
                let mut d = 0i32;
                let mut end = i + 1;
                while end < self.toks.len() {
                    match self.toks[end].token {
                        Token::LParen => d += 1,
                        Token::RParen if d == 0 => break,
                        Token::RParen => d -= 1,
                        Token::Comma if d == 0 => break,
                        _ => {}
                    }
                    end += 1;
                }
                let from = if self.tok(i - 1) == Some(&Token::Comma) { i - 1 } else { i };
                self.blank(from, end);
                i = end;
                continue;
            } else if self.tok(i) == Some(&Token::Comma)
                && depth == 0
                && self.is(0, "ALTER")
                && self.is(1, "TABLE")
                && (2..i).any(|j| self.is(j, "ADD"))
                && matches!(self.tok(i + 1), Some(Token::Word(_)))
                && !self.any(i + 1, &["ADD", "CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK", "DEFAULT", "INDEX"])
            {
                // `ADD a INT, b INT`: each column gets its own `ADD`, written
                // over the indentation before it
                let at = self.toks[i + 1].range.0;
                if at >= 4 && self.out[at - 4..at].iter().all(|b| *b == b' ') && at - 4 >= self.toks[i].range.1 {
                    self.out[at - 4..at - 1].copy_from_slice(b"ADD");
                    self.changed = true;
                }
            } else if self.is(i, "FOREIGN") && self.is(i + 1, "KEY") && self.is(i + 2, "REFERENCES") {
                // column constraint `c INT FOREIGN KEY REFERENCES t (c)`
                self.blank(i, i + 2);
            } else if self.is(i, "AS")
                && ddl
                && i > 1
                && matches!(self.tok(i - 1), Some(Token::Word(_)))
                && ((depth == 1 && matches!(self.tok(i - 2), Some(Token::Comma | Token::LParen)))
                    || (depth == 0 && (self.is(i - 2, "ADD") || self.tok(i - 2) == Some(&Token::Comma)) && self.is(0, "ALTER")))
                && !self.is(i + 1, "TABLE")
            {
                // computed column `c AS expr [PERSISTED]`: typed as INT
                let mut d = 0i32;
                let mut end = i + 1;
                while end < self.toks.len() {
                    match self.toks[end].token {
                        Token::LParen => d += 1,
                        Token::RParen if d == 0 => break,
                        Token::RParen => d -= 1,
                        Token::Comma if d == 0 => break,
                        _ => {}
                    }
                    end += 1;
                }
                if end > i + 1 {
                    let s = self.toks[i].range.0;
                    self.blank(i, end);
                    self.out[s..s + 3].copy_from_slice(b"INT");
                    i = end;
                    continue;
                }
            } else if self.is(i, "DEFAULT")
                && self.is(i + 1, "VALUES")
                && i > 0
                && self.tok(i - 1) == Some(&Token::RParen)
                && self.is(0, "INSERT")
            {
                // `INSERT INTO t (a) DEFAULT VALUES`
                if let Some(open) = (0..i).rev().find(|&j| self.tok(j) == Some(&Token::LParen)) {
                    self.blank(open, i);
                }
            }
            i += 1;
        }
    }

    fn oracle(&mut self) {
        let ddl = self.any(0, &["CREATE", "ALTER"]);
        let mut i = 0;
        while i < self.toks.len() {
            if ddl && self.any(i, ORACLE_VALUED) && matches!(self.tok(i + 1), Some(Token::Number(..) | Token::Word(_))) {
                self.blank(i, i + 2);
                i += 2;
                continue;
            }
            if ddl && (self.any(i, ORACLE_FLAGS) || (self.is(i, "FORCE") && i < 4)) {
                self.blank(i, i + 1);
            } else if ddl && self.any(i, &["STORAGE", "LOB"]) && self.tok(i + 1) == Some(&Token::LParen) {
                let mut close = self.close(i + 1);
                // `LOB (c) STORE AS [name] (…)`
                if self.is(close, "STORE") && self.is(close + 1, "AS") {
                    close += 2;
                    while close < self.toks.len() && matches!(self.tok(close), Some(Token::Word(_))) {
                        close += 1;
                    }
                    if self.tok(close) == Some(&Token::LParen) {
                        close = self.close(close);
                    }
                }
                self.blank(i, close);
                i = close;
                continue;
            } else if self.any(i, &["CHAR", "BYTE"])
                && i > 0
                && matches!(self.tok(i - 1), Some(Token::Number(..)))
                && self.tok(i + 1) == Some(&Token::RParen)
            {
                self.blank(i, i + 1);
            } else if ddl
                && self.any(i, &["ENABLE", "DISABLE"])
                && matches!(self.tok(i + 1), None | Some(Token::Comma | Token::RParen | Token::SemiColon))
            {
                self.blank(i, i + 1);
            } else if ddl && self.is(i, "USING") && self.is(i + 1, "INDEX") {
                let mut end = i + 2;
                if self.tok(end) == Some(&Token::LParen) {
                    end = self.close(end);
                }
                self.blank(i, end);
                i = end;
                continue;
            } else if ddl && self.is(i, "SEGMENT") && self.is(i + 1, "CREATION") {
                self.blank(i, i + 3);
            } else if ddl && self.is(i, "ORGANIZATION") && self.any(i + 1, &["INDEX", "HEAP"]) {
                self.blank(i, i + 2);
            } else if self.is(i, "TREAT") && self.tok(i + 1) == Some(&Token::LParen) {
                // `TREAT(expr AS type)`: the cast target is blanked
                let close = self.close(i + 1);
                let mut d = 0i32;
                let cast = (i + 1..close).find(|&j| {
                    match self.toks[j].token {
                        Token::LParen => d += 1,
                        Token::RParen => d -= 1,
                        _ => {}
                    }
                    d == 1 && self.is(j, "AS")
                });
                if let Some(a) = cast {
                    self.blank(a, close - 1);
                }
            } else if self.is(i, "MEMBER") && self.is(i + 1, "OF") {
                // collection conditions, written as comparisons
                let not = i > 0 && self.is(i - 1, "NOT");
                let from = if not { i - 1 } else { i };
                let s = self.toks[from].range.0;
                self.blank(from, i + 2);
                self.out[s] = b'=';
            } else if self.is(i, "MULTISET") && self.any(i + 1, &["UNION", "EXCEPT", "INTERSECT"]) {
                let end = if self.any(i + 2, &["ALL", "DISTINCT"]) { i + 3 } else { i + 2 };
                let s = self.toks[i].range.0;
                self.blank(i, end);
                self.out[s] = b'|';
                self.out[s + 1] = b'|';
            } else if self.is(i, "EMPTY") && i > 0 && (self.is(i - 1, "IS") || (self.is(i - 1, "NOT") && i > 1 && self.is(i - 2, "IS"))) {
                let s = self.toks[i].range.0;
                self.out[s..s + 5].copy_from_slice(b"NULL ");
                self.changed = true;
            } else if self.tok(i) == Some(&Token::Mod)
                && self.any(i + 1, &["TYPE", "ROWTYPE", "FOUND", "NOTFOUND", "ISOPEN", "ROWCOUNT", "BULK_ROWCOUNT", "BULK_EXCEPTIONS"])
                && self.toks[i].range.1 == self.toks[i + 1].range.0
            {
                // PL/SQL attributes `c%NOTFOUND`, `t.c%TYPE`
                self.blank(i, i + 2);
            }
            i += 1;
        }
        self.db2();
    }

    /// `GENERATED {ALWAYS | BY DEFAULT} AS IDENTITY (START WITH 1 …)`.
    fn db2(&mut self) {
        for i in 0..self.toks.len() {
            if self.is(i, "IDENTITY") && i > 0 && self.is(i - 1, "AS") && self.tok(i + 1) == Some(&Token::LParen) {
                let close = self.close(i + 1);
                self.blank(i + 1, close);
            }
        }
    }

    /// SQLite type names of several words (`BLOB SUB_TYPE TEXT`, any
    /// identifiers are a type): the words after the first are blanked.
    fn sqlite(&mut self) {
        const AFTER_TYPE: &[&str] = &[
            "CONSTRAINT", "PRIMARY", "NOT", "NULL", "UNIQUE", "CHECK", "DEFAULT", "COLLATE", "REFERENCES", "GENERATED",
            "AS", "AUTOINCREMENT", "ON", "KEY", "FOREIGN", "PRECISION", "VARYING", "UNSIGNED",
        ];
        if !self.is(0, "CREATE") {
            return;
        }
        let mut depth = 0i32;
        let mut words = 0; // consecutive words in the current column item
        for i in 0..self.toks.len() {
            match &self.toks[i].token {
                Token::LParen => {
                    depth += 1;
                    words = 0;
                }
                Token::RParen => depth -= 1,
                Token::Comma if depth == 1 => words = 0,
                Token::Word(_) if depth == 1 => {
                    if AFTER_TYPE.iter().any(|w| self.is(i, w)) {
                        words = usize::MAX / 2;
                    } else {
                        words += 1;
                        if words >= 3 && words < usize::MAX / 2 {
                            self.blank(i, i + 1);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn mysql(&mut self) {
        // `VARCHAR(40) BINARY`: the collation shorthand of a column
        for i in 1..self.toks.len() {
            if self.is(i, "BINARY")
                && self.tok(i - 1) == Some(&Token::RParen)
                && (self.any(i + 1, &["DEFAULT", "NOT", "NULL", "CHARACTER", "COLLATE"])
                    || matches!(self.tok(i + 1), Some(Token::Comma | Token::RParen)))
            {
                self.blank(i, i + 1);
            }
        }
        if !self.any(0, &["CREATE", "ALTER"]) {
            return;
        }
        let mut i = 1;
        while i < self.toks.len().min(16) {
            if self.is(i, "DEFINER") && self.tok(i + 1) == Some(&Token::Eq) {
                // `user@host`, `'user'@'host'`, `CURRENT_USER[()]`
                let mut end = i + 3;
                if matches!(self.tok(end), Some(Token::AtSign)) {
                    end += 2;
                } else if self.tok(end) == Some(&Token::LParen) {
                    end = self.close(end);
                }
                self.blank(i, end);
                i = end;
            } else if self.is(i, "ALGORITHM") && self.tok(i + 1) == Some(&Token::Eq) {
                self.blank(i, i + 3);
                i += 3;
            } else if self.is(i, "SQL") && self.is(i + 1, "SECURITY") {
                self.blank(i, i + 3);
                i += 3;
            } else {
                i += 1;
            }
        }
    }
}

/// Words after which a Jinja placeholder is an operand, not a clause.
fn is_clause_word(word: &str) -> bool {
    const WORDS: &[&str] = &[
        "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "ON", "BY", "JOIN", "AS", "WHEN", "THEN", "ELSE", "IN", "LIKE",
        "BETWEEN", "HAVING", "LIMIT", "OFFSET", "UNION", "ALL", "DISTINCT", "INTO", "VALUES", "SET", "UPDATE",
        "TABLE", "VIEW", "CASE", "IS", "WITH", "USING", "OVER", "PARTITION", "INSERT", "DELETE", "MERGE", "EXCEPT",
        "INTERSECT", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "LATERAL", "QUALIFY", "END", "RETURNING",
        "TOP", "INTERVAL", "CAST", "IF", "EXISTS", "REPLACE", "CREATE", "ALTER", "DROP", "TO", "FOR", "GRANT",
    ];
    WORDS.iter().any(|w| w.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(sql: &str, d: Dialect) -> String {
        let out = repaired(sql, d).unwrap_or_else(|| sql.to_string());
        assert_eq!(out.len(), sql.len());
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn jinja_placeholders() {
        assert_eq!(fix("select a over (partition by b _____ order by c)", Dialect::Generic), "select a over (partition by b order by c)");
        assert_eq!(fix("select _______ over (order by c)", Dialect::Generic), "select _____() over (order by c)");
        assert_eq!(repaired("select ____ from ____", Dialect::Generic), None);
    }

    #[test]
    fn postgres() {
        assert_eq!(fix("select f(a := 1)", Dialect::Postgres), "select f(a => 1)");
        assert_eq!(fix("create table a (like b including all)", Dialect::Postgres), "create table a (like b )");
        assert_eq!(fix("create index on t (c) tablespace x", Dialect::Postgres), "create index on t (c)");
        assert_eq!(
            fix("create trigger t before update on a for each row execute procedure f('x', 'y')", Dialect::Postgres),
            "create trigger t before update on a for each row execute procedure f( )"
        );
    }

    #[test]
    fn tsql() {
        assert_eq!(
            fix("CREATE TABLE t (id INT PRIMARY KEY CLUSTERED) ON [PRIMARY]", Dialect::MsSql),
            "CREATE TABLE t (id INT PRIMARY KEY )"
        );
        assert_eq!(fix("SET @a += 1", Dialect::MsSql), "SET @a = 1");
        assert_eq!(fix("SELECT 1 FROM t (NOLOCK) OPTION (RECOMPILE)", Dialect::MsSql), "SELECT 1 FROM t");
        assert_eq!(
            fix("CREATE TABLE t (id INT, CONSTRAINT pk PRIMARY KEY (id) WITH (PAD_INDEX = OFF) ON [PRIMARY])", Dialect::MsSql),
            "CREATE TABLE t (id INT, CONSTRAINT pk PRIMARY KEY (id) )"
        );
    }

    #[test]
    fn oracle_db2_mysql() {
        assert_eq!(
            fix("CREATE TABLE t (a VARCHAR2(10 CHAR), CONSTRAINT c CHECK (a > 1) ENABLE) PCTFREE 10 TABLESPACE users", Dialect::Oracle),
            "CREATE TABLE t (a VARCHAR2(10 ), CONSTRAINT c CHECK (a > 1) )"
        );
        assert_eq!(
            fix("CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY (START WITH 1))", Dialect::Db2),
            "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY )"
        );
        assert_eq!(fix("CREATE DEFINER=`root`@`localhost` VIEW v AS SELECT 1", Dialect::MySql), "CREATE VIEW v AS SELECT 1");
    }
}
