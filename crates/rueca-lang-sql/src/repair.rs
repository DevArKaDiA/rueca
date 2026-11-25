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
//! | any (Jinja) | a placeholder between a query and `select` (a macro that expands to a set operator) | `union` |
//! | any (Jinja) | a placeholder between `with` and `select` (a macro that expands to the CTE list) | `_ as (select 1)` |
//! | any (Jinja) | a placeholder before `over (` (a macro that expands to a window function) | written as a call |
//! | PostgreSQL | named arguments `f(a := 1)` | `f(a => 1)` |
//! | PostgreSQL | `LIKE t INCLUDING ALL` / `EXCLUDING …` options | blanked |
//! | PostgreSQL | `TABLESPACE name`, `CREATE OR REPLACE PROCEDURE`'s `OR REPLACE`, YugabyteDB `CREATE INDEX NONCONCURRENTLY` | blanked |
//! | MySQL, Oracle | the partitioning clause of `CREATE TABLE` (`PARTITION BY … (PARTITION p …)`) | blanked |
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
//! | DB2 | special registers `CURRENT DATE` / `CURRENT TIMESTAMP` | `CURRENT_DATE` / `CURRENT_TIMESTAMP` |
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

/// Where the T-SQL rules are in the statement.
struct Tsql {
    /// The statement is a `CREATE`, `ALTER` or `DECLARE`.
    ddl: bool,
    /// Parenthesis depth at the current token.
    depth: i32,
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

    /// Writes `text` at byte `at` and spaces up to `end`.
    fn overwrite(&mut self, at: usize, text: &[u8], end: usize) {
        self.out[at..at + text.len()].copy_from_slice(text);
        for b in &mut self.out[at + text.len()..end] {
            *b = b' ';
        }
        self.changed = true;
    }

    /// Index of the token that ends the list item starting at `from`: a
    /// `,` or an unmatched `)` (the token count when there is none).
    fn item_end(&self, from: usize) -> usize {
        let mut d = 0i32;
        let mut end = from;
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
        end
    }

    /// Whether token `p` can end an expression (a name that is no clause
    /// word, `)`, a literal).
    fn ends_expression(&self, p: usize) -> bool {
        match &self.toks[p].token {
            Token::Word(w) => w.quote_style.is_some() || !is_clause_word(&w.value),
            Token::RParen | Token::Number(..) | Token::SingleQuotedString(_) => true,
            _ => false,
        }
    }

    /// Whether token `k` can start an operand (a name that is no clause
    /// word, a literal).
    fn starts_operand(&self, k: usize) -> bool {
        match self.tok(k) {
            Some(Token::Word(w)) => w.quote_style.is_some() || !is_clause_word(&w.value),
            Some(Token::Number(..) | Token::SingleQuotedString(_)) => true,
            _ => false,
        }
    }

    fn jinja(&mut self) {
        for i in 0..self.toks.len() {
            let w = self.toks[i].word();
            if is_placeholder(w) && w.len() >= 2 {
                let _ = self.cte_list(i) || self.set_operator(i) || self.window_call(i) || self.clause(i);
            }
        }
    }

    /// `with {{ ctes() }} select …`: a macro that expands to the CTE list.
    fn cte_list(&mut self, i: usize) -> bool {
        let (ps, pe) = self.toks[i].range;
        let before_query =
            self.is(i + 1, "SELECT") || (self.tok(i + 1) == Some(&Token::LParen) && self.is(i + 2, "SELECT"));
        let hit = i > 0 && self.is(i - 1, "WITH") && before_query && pe - ps >= 15;
        if hit {
            self.overwrite(ps, b"_ as (select 1)", pe);
        }
        hit
    }

    /// `select … {{ union() }} select …`: a macro that expands to a set
    /// operator.
    fn set_operator(&mut self, i: usize) -> bool {
        let (ps, pe) = self.toks[i].range;
        let hit = i > 0 && self.ends_expression(i - 1) && self.is(i + 1, "SELECT") && pe - ps >= 5;
        if hit {
            self.overwrite(ps, b"union", pe);
        }
        hit
    }

    /// `{{ window_function() }} over (…)`: written as a call.
    fn window_call(&mut self, i: usize) -> bool {
        let hit = self.is(i + 1, "OVER") && self.toks[i].word().len() >= 3;
        if hit {
            let (_, e) = self.toks[i].range;
            self.out[e - 2] = b'(';
            self.out[e - 1] = b')';
            self.changed = true;
        }
        hit
    }

    /// A placeholder right after an expression (a macro that expands to a
    /// clause), or between select-list items with no comma of its own (a
    /// macro that expands to items and their comma: `select a, {{
    /// extra_columns() }} b from t`).
    fn clause(&mut self, i: usize) -> bool {
        let after_expr = i > 0 && self.ends_expression(i - 1);
        if after_expr || self.list_item(i) {
            self.blank(i, i + 1);
        }
        true
    }

    fn list_item(&self, i: usize) -> bool {
        let after_separator =
            i > 0 && (self.toks[i - 1].token == Token::Comma || self.any(i - 1, &["SELECT", "DISTINCT"]));
        after_separator
            && self.starts_operand(i + 1)
            && self.tok(i + 2) != Some(&Token::Comma)
            && !self.is(i + 2, "FROM")
    }

    fn postgres(&mut self) {
        let create = self.is(0, "CREATE");
        for i in 0..self.toks.len() {
            let _ = self.named_argument(i)
                || self.pg_option(i, create)
                || self.nonconcurrently(i, create)
                || self.trigger_arguments(i, create);
        }
    }

    /// `f(a := 1)` → `f(a => 1)`.
    fn named_argument(&mut self, i: usize) -> bool {
        let hit = self.tok(i) == Some(&Token::Assignment);
        if hit {
            let (s, _) = self.toks[i].range;
            self.out[s] = b'=';
            self.out[s + 1] = b'>';
            self.changed = true;
        }
        hit
    }

    /// `INCLUDING ALL`, `TABLESPACE name`, `OR REPLACE` before `PROCEDURE`.
    fn pg_option(&mut self, i: usize, create: bool) -> bool {
        let valued = self.any(i, &["INCLUDING", "EXCLUDING"]) || (create && self.is(i, "TABLESPACE"));
        let has_value = self.toks.get(i + 1).is_some_and(|t| !t.word().is_empty());
        let or_replace =
            create && i == 1 && self.is(i, "OR") && self.is(i + 1, "REPLACE") && self.is(i + 2, "PROCEDURE");
        let hit = (valued && has_value) || or_replace;
        if hit {
            self.blank(i, i + 2);
        }
        hit
    }

    /// YugabyteDB `CREATE INDEX NONCONCURRENTLY`.
    fn nonconcurrently(&mut self, i: usize, create: bool) -> bool {
        let hit = create && self.is(i, "NONCONCURRENTLY") && self.is(i - 1, "INDEX");
        if hit {
            self.blank(i, i + 1);
        }
        hit
    }

    /// `EXECUTE PROCEDURE f('a', 'b')` of a trigger: the arguments.
    fn trigger_arguments(&mut self, i: usize, create: bool) -> bool {
        if !(create && self.is(i, "EXECUTE") && self.any(i + 1, &["PROCEDURE", "FUNCTION"])) {
            return false;
        }
        let Some(open) = (i + 2..self.toks.len().min(i + 8)).find(|&j| self.tok(j) == Some(&Token::LParen)) else {
            return false;
        };
        let close = self.close(open);
        if close > open + 2 {
            self.blank(open + 1, close - 1);
        }
        true
    }

    fn tsql(&mut self) {
        let mut c = Tsql { ddl: self.any(0, &["CREATE", "ALTER", "DECLARE"]), depth: 0 };
        let mut i = 0;
        while i < self.toks.len() {
            match self.toks[i].token {
                Token::LParen => c.depth += 1,
                Token::RParen => c.depth -= 1,
                _ => {}
            }
            i = self.tsql_rule(i, &mut c);
        }
    }

    /// The first rule that applies at token `i` (in this order); the
    /// index to go on from.
    fn tsql_rule(&mut self, i: usize, c: &mut Tsql) -> usize {
        self.clustered(i, c)
            .or_else(|| self.with_options(i, c))
            .or_else(|| self.filegroup(i, c))
            .or_else(|| self.compound_assignment(i))
            .or_else(|| self.option_hint(i, c))
            .or_else(|| self.table_hints(i, c))
            .or_else(|| self.xml_namespaces(i))
            .or_else(|| self.tsql_small(i))
            .or_else(|| self.inline_index(i, c))
            .or_else(|| self.added_columns(i, c))
            .or_else(|| self.column_foreign_key(i))
            .or_else(|| self.computed_column(i, c))
            .or_else(|| self.default_values(i))
            .unwrap_or(i + 1)
    }

    /// `CLUSTERED` / `NONCLUSTERED`.
    fn clustered(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        if !(self.any(i, &["CLUSTERED", "NONCLUSTERED"]) && (c.ddl || c.depth > 0)) {
            return None;
        }
        self.blank(i, i + 1);
        Some(i + 1)
    }

    /// Index and table options `WITH (PAD_INDEX = …)`.
    fn with_options(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        let hit =
            self.is(i, "WITH") && self.tok(i + 1) == Some(&Token::LParen) && self.any(i + 2, TSQL_OPTIONS) && c.ddl;
        if !hit {
            return None;
        }
        let close = self.close(i + 1);
        self.blank(i, close);
        Some(close)
    }

    /// Filegroups and partition schemes `ON [PRIMARY]`, `TEXTIMAGE_ON […]`,
    /// `ON scheme (column)`.
    fn filegroup(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        let hit = c.ddl
            && self.any(i, &["ON", "TEXTIMAGE_ON"])
            && i > 0
            && self.tok(i - 1) == Some(&Token::RParen)
            && c.depth <= 1
            && matches!(self.tok(i + 1), Some(Token::Word(_)))
            && !self.any(i + 1, &["DELETE", "UPDATE"])
            && self.tok(i + 2) != Some(&Token::Period);
        if !hit {
            return None;
        }
        let end = if self.tok(i + 2) == Some(&Token::LParen) { self.close(i + 2) } else { i + 2 };
        self.blank(i, end);
        Some(end)
    }

    /// `+=`, `-=`, `*=`, `/=` → `=`.
    fn compound_assignment(&mut self, i: usize) -> Option<usize> {
        let hit = matches!(self.tok(i), Some(Token::Plus | Token::Minus | Token::Mul | Token::Div))
            && self.tok(i + 1) == Some(&Token::Eq)
            && self.toks[i].range.1 == self.toks[i + 1].range.0;
        if !hit {
            return None;
        }
        self.blank(i, i + 1);
        Some(i + 1)
    }

    /// Query hints `OPTION (…)`.
    fn option_hint(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        if !(self.is(i, "OPTION") && self.tok(i + 1) == Some(&Token::LParen) && c.depth == 0) {
            return None;
        }
        let close = self.close(i + 1);
        self.blank(i, close);
        Some(close)
    }

    /// `t (NOLOCK)`, `INSERT #t WITH (TABLOCK) (…)`.
    fn table_hints(&mut self, i: usize, c: &mut Tsql) -> Option<usize> {
        let hit = self.tok(i) == Some(&Token::LParen)
            && self.any(i + 1, TABLE_HINTS)
            && i > 0
            && matches!(self.tok(i - 1), Some(Token::Word(_)));
        if !hit {
            return None;
        }
        let close = self.close(i);
        if !(i + 1..close - 1).all(|j| self.any(j, TABLE_HINTS) || self.tok(j) == Some(&Token::Comma)) {
            return Some(i + 1);
        }
        let from = if self.is(i - 1, "WITH") { i - 1 } else { i };
        self.blank(from, close);
        c.depth -= 1;
        Some(close)
    }

    /// `WITH XMLNAMESPACES (…) SELECT`, `WITH XMLNAMESPACES (…), cte AS`.
    fn xml_namespaces(&mut self, i: usize) -> Option<usize> {
        let hit =
            self.is(i, "XMLNAMESPACES") && self.tok(i + 1) == Some(&Token::LParen) && i > 0 && self.is(i - 1, "WITH");
        if !hit {
            return None;
        }
        let close = self.close(i + 1);
        if self.tok(close) == Some(&Token::Comma) {
            self.blank(i, close + 1);
        } else {
            self.blank(i - 1, close);
        }
        Some(close)
    }

    /// One-token rules: `FOR XML PATH(N'')`'s `N`, join hints, the `::` of
    /// `FROM ::fn()`.
    fn tsql_small(&mut self, i: usize) -> Option<usize> {
        let xml_string = matches!(self.tok(i), Some(Token::NationalStringLiteral(_)))
            && i > 1
            && self.tok(i - 1) == Some(&Token::LParen)
            && self.any(i - 2, &["PATH", "ROOT", "RAW", "AUTO", "ELEMENTS"]);
        if xml_string {
            let s = self.toks[i].range.0;
            self.out[s] = b' ';
            self.changed = true;
            return Some(i + 1);
        }
        let join_hint = self.any(i, &["HASH", "LOOP", "MERGE", "REMOTE"]) && self.is(i + 1, "JOIN");
        let scope = self.tok(i) == Some(&Token::DoubleColon) && i > 0 && self.any(i - 1, &["FROM", "JOIN", "APPLY"]);
        if !(join_hint || scope) {
            return None;
        }
        self.blank(i, i + 1);
        Some(i + 1)
    }

    /// Inline index of a table: `, INDEX ix [UNIQUE] [CLUSTERED] (a, b)`.
    fn inline_index(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        let hit = self.is(i, "INDEX")
            && c.depth == 1
            && c.ddl
            && i > 0
            && !self.any(i - 1, &["CREATE", "UNIQUE", "DROP", "ALTER", "COLUMNSTORE"])
            && matches!(self.tok(i + 1), Some(Token::Word(_)));
        if !hit {
            return None;
        }
        let end = self.item_end(i + 1);
        let from = if self.tok(i - 1) == Some(&Token::Comma) { i - 1 } else { i };
        self.blank(from, end);
        Some(end)
    }

    /// `ALTER TABLE t ADD a INT, b INT`: each column gets its own `ADD`,
    /// written over the indentation before it.
    fn added_columns(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        const NOT_COLUMNS: &[&str] =
            &["ADD", "CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK", "DEFAULT", "INDEX"];
        let hit = self.tok(i) == Some(&Token::Comma)
            && c.depth == 0
            && self.is(0, "ALTER")
            && self.is(1, "TABLE")
            && (2..i).any(|j| self.is(j, "ADD"))
            && matches!(self.tok(i + 1), Some(Token::Word(_)))
            && !self.any(i + 1, NOT_COLUMNS);
        if !hit {
            return None;
        }
        let at = self.toks[i + 1].range.0;
        if at >= 4 && self.out[at - 4..at].iter().all(|b| *b == b' ') && at - 4 >= self.toks[i].range.1 {
            self.out[at - 4..at - 1].copy_from_slice(b"ADD");
            self.changed = true;
        }
        Some(i + 1)
    }

    /// Column constraint `c INT FOREIGN KEY REFERENCES t (c)`.
    fn column_foreign_key(&mut self, i: usize) -> Option<usize> {
        if !(self.is(i, "FOREIGN") && self.is(i + 1, "KEY") && self.is(i + 2, "REFERENCES")) {
            return None;
        }
        self.blank(i, i + 2);
        Some(i + 1)
    }

    /// Computed column `c AS expr [PERSISTED]`: typed as INT.
    fn computed_column(&mut self, i: usize, c: &Tsql) -> Option<usize> {
        if !(self.is(i, "AS") && c.ddl && i > 1 && matches!(self.tok(i - 1), Some(Token::Word(_)))) {
            return None;
        }
        let in_list = c.depth == 1 && matches!(self.tok(i - 2), Some(Token::Comma | Token::LParen));
        let added =
            c.depth == 0 && (self.is(i - 2, "ADD") || self.tok(i - 2) == Some(&Token::Comma)) && self.is(0, "ALTER");
        if !(in_list || added) || self.is(i + 1, "TABLE") {
            return None;
        }
        let end = self.item_end(i + 1);
        if end <= i + 1 {
            return Some(i + 1);
        }
        let s = self.toks[i].range.0;
        self.blank(i, end);
        self.out[s..s + 3].copy_from_slice(b"INT");
        Some(end)
    }

    /// `INSERT INTO t (a) DEFAULT VALUES`: the column list.
    fn default_values(&mut self, i: usize) -> Option<usize> {
        let hit = self.is(i, "DEFAULT")
            && self.is(i + 1, "VALUES")
            && i > 0
            && self.tok(i - 1) == Some(&Token::RParen)
            && self.is(0, "INSERT");
        if !hit {
            return None;
        }
        if let Some(open) = (0..i).rev().find(|&j| self.tok(j) == Some(&Token::LParen)) {
            self.blank(open, i);
        }
        Some(i + 1)
    }

    fn oracle(&mut self) {
        self.partitioning();
        let ddl = self.any(0, &["CREATE", "ALTER"]);
        let mut i = 0;
        while i < self.toks.len() {
            i = self.oracle_rule(i, ddl);
        }
        self.db2();
    }

    /// The first rule that applies at token `i` (in this order); the
    /// index to go on from.
    fn oracle_rule(&mut self, i: usize, ddl: bool) -> usize {
        self.physical_attribute(i, ddl)
            .or_else(|| self.storage_clause(i, ddl))
            .or_else(|| self.oracle_word(i, ddl))
            .or_else(|| self.using_index(i, ddl))
            .or_else(|| self.table_organization(i, ddl))
            .or_else(|| self.treat(i))
            .or_else(|| self.collection_condition(i))
            .or_else(|| self.attribute(i))
            .unwrap_or(i + 1)
    }

    /// `PCTFREE 10`, `TABLESPACE users`, `PARALLEL 4`; `NOLOGGING`, `FORCE`.
    fn physical_attribute(&mut self, i: usize, ddl: bool) -> Option<usize> {
        if !ddl {
            return None;
        }
        let valued = self.any(i, ORACLE_VALUED) && matches!(self.tok(i + 1), Some(Token::Number(..) | Token::Word(_)));
        let parallel = self.is(i, "PARALLEL") && matches!(self.tok(i + 1), Some(Token::Number(..)));
        if valued || parallel {
            self.blank(i, i + 2);
            return Some(i + 2);
        }
        if self.any(i, ORACLE_FLAGS) || (self.is(i, "FORCE") && i < 4) {
            self.blank(i, i + 1);
            return Some(i + 1);
        }
        None
    }

    /// `STORAGE (…)`, `LOB (c) STORE AS [name] (…)`.
    fn storage_clause(&mut self, i: usize, ddl: bool) -> Option<usize> {
        if !(ddl && self.any(i, &["STORAGE", "LOB"]) && self.tok(i + 1) == Some(&Token::LParen)) {
            return None;
        }
        let mut close = self.close(i + 1);
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
        Some(close)
    }

    /// `VARCHAR2(10 CHAR)`'s unit; `ENABLE` / `DISABLE` after a constraint.
    fn oracle_word(&mut self, i: usize, ddl: bool) -> Option<usize> {
        let unit = self.any(i, &["CHAR", "BYTE"])
            && i > 0
            && matches!(self.tok(i - 1), Some(Token::Number(..)))
            && self.tok(i + 1) == Some(&Token::RParen);
        let state = ddl
            && self.any(i, &["ENABLE", "DISABLE"])
            && matches!(self.tok(i + 1), None | Some(Token::Comma | Token::RParen | Token::SemiColon));
        if !(unit || state) {
            return None;
        }
        self.blank(i, i + 1);
        Some(i + 1)
    }

    /// `USING INDEX [(…)]`.
    fn using_index(&mut self, i: usize, ddl: bool) -> Option<usize> {
        if !(ddl && self.is(i, "USING") && self.is(i + 1, "INDEX")) {
            return None;
        }
        let mut end = i + 2;
        if self.tok(end) == Some(&Token::LParen) {
            end = self.close(end);
        }
        self.blank(i, end);
        Some(end)
    }

    /// `SEGMENT CREATION IMMEDIATE`, `ORGANIZATION INDEX | HEAP`.
    fn table_organization(&mut self, i: usize, ddl: bool) -> Option<usize> {
        if !ddl {
            return None;
        }
        if self.is(i, "SEGMENT") && self.is(i + 1, "CREATION") {
            self.blank(i, i + 3);
            return Some(i + 1);
        }
        if self.is(i, "ORGANIZATION") && self.any(i + 1, &["INDEX", "HEAP"]) {
            self.blank(i, i + 2);
            return Some(i + 1);
        }
        None
    }

    /// `TREAT(expr AS type)`: the cast target is blanked.
    fn treat(&mut self, i: usize) -> Option<usize> {
        if !(self.is(i, "TREAT") && self.tok(i + 1) == Some(&Token::LParen)) {
            return None;
        }
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
        Some(i + 1)
    }

    /// Collection conditions, written as comparisons: `a [NOT] MEMBER OF
    /// b`, `a MULTISET UNION b`, `a IS [NOT] EMPTY`.
    fn collection_condition(&mut self, i: usize) -> Option<usize> {
        self.member_of(i).or_else(|| self.multiset(i)).or_else(|| self.is_empty(i))
    }

    /// `a [NOT] MEMBER OF b` → `a = b`.
    fn member_of(&mut self, i: usize) -> Option<usize> {
        if !(self.is(i, "MEMBER") && self.is(i + 1, "OF")) {
            return None;
        }
        let not = i > 0 && self.is(i - 1, "NOT");
        let from = if not { i - 1 } else { i };
        let s = self.toks[from].range.0;
        self.blank(from, i + 2);
        self.out[s] = b'=';
        Some(i + 1)
    }

    /// `a MULTISET UNION [ALL | DISTINCT] b` → `a || b`.
    fn multiset(&mut self, i: usize) -> Option<usize> {
        if !(self.is(i, "MULTISET") && self.any(i + 1, &["UNION", "EXCEPT", "INTERSECT"])) {
            return None;
        }
        let end = if self.any(i + 2, &["ALL", "DISTINCT"]) { i + 3 } else { i + 2 };
        let s = self.toks[i].range.0;
        self.blank(i, end);
        self.out[s] = b'|';
        self.out[s + 1] = b'|';
        Some(i + 1)
    }

    /// `a IS [NOT] EMPTY` → `a IS [NOT] NULL`.
    fn is_empty(&mut self, i: usize) -> Option<usize> {
        let negated = i > 1 && self.is(i - 1, "NOT") && self.is(i - 2, "IS");
        let after_is = i > 0 && (self.is(i - 1, "IS") || negated);
        if !(self.is(i, "EMPTY") && after_is) {
            return None;
        }
        let s = self.toks[i].range.0;
        self.out[s..s + 5].copy_from_slice(b"NULL ");
        self.changed = true;
        Some(i + 1)
    }

    /// PL/SQL attributes `c%NOTFOUND`, `t.c%TYPE`.
    fn attribute(&mut self, i: usize) -> Option<usize> {
        const ATTRIBUTES: &[&str] =
            &["TYPE", "ROWTYPE", "FOUND", "NOTFOUND", "ISOPEN", "ROWCOUNT", "BULK_ROWCOUNT", "BULK_EXCEPTIONS"];
        let hit = self.tok(i) == Some(&Token::Mod)
            && self.any(i + 1, ATTRIBUTES)
            && self.toks[i].range.1 == self.toks[i + 1].range.0;
        if !hit {
            return None;
        }
        self.blank(i, i + 2);
        Some(i + 1)
    }

    /// `GENERATED {ALWAYS | BY DEFAULT} AS IDENTITY (START WITH 1 …)`.
    fn db2(&mut self) {
        for i in 0..self.toks.len() {
            if self.is(i, "IDENTITY") && i > 0 && self.is(i - 1, "AS") && self.tok(i + 1) == Some(&Token::LParen) {
                let close = self.close(i + 1);
                self.blank(i + 1, close);
            } else if self.is(i, "CURRENT")
                && self.any(i + 1, &["DATE", "TIME", "TIMESTAMP", "USER", "SCHEMA"])
                && self.toks[i].range.1 + 1 == self.toks[i + 1].range.0
            {
                // DB2 special registers `CURRENT DATE` → `CURRENT_DATE`
                let at = self.toks[i].range.1;
                self.out[at] = b'_';
                self.changed = true;
            }
        }
    }

    /// SQLite type names of several words (`BLOB SUB_TYPE TEXT`, any
    /// identifiers are a type): the words after the first are blanked.
    fn sqlite(&mut self) {
        const AFTER_TYPE: &[&str] = &[
            "CONSTRAINT",
            "PRIMARY",
            "NOT",
            "NULL",
            "UNIQUE",
            "CHECK",
            "DEFAULT",
            "COLLATE",
            "REFERENCES",
            "GENERATED",
            "AS",
            "AUTOINCREMENT",
            "ON",
            "KEY",
            "FOREIGN",
            "PRECISION",
            "VARYING",
            "UNSIGNED",
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
                        if (3..usize::MAX / 2).contains(&words) {
                            self.blank(i, i + 1);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// `CREATE TABLE t (…) … PARTITION BY RANGE (c) (PARTITION p VALUES …)`
    /// (MySQL, Oracle): the partitioning clause, to the end.
    fn partitioning(&mut self) {
        if !(self.is(0, "CREATE") && (1..self.toks.len().min(5)).any(|k| self.is(k, "TABLE"))) {
            return;
        }
        let mut depth = 0i32;
        for i in 0..self.toks.len() {
            match self.toks[i].token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ => {}
            }
            if depth == 0
                && self.is(i, "PARTITION")
                && self.is(i + 1, "BY")
                && i > 0
                && self.tok(i - 1) == Some(&Token::RParen)
            {
                let end =
                    (i..self.toks.len()).find(|&k| self.tok(k) == Some(&Token::SemiColon)).unwrap_or(self.toks.len());
                self.blank(i, end);
                return;
            }
        }
    }

    fn mysql(&mut self) {
        self.partitioning();
        // `VARCHAR(40) BINARY`: the collation shorthand of a column
        for i in 1..self.toks.len() {
            if self.binary_shorthand(i) {
                self.blank(i, i + 1);
            }
        }
        if !self.any(0, &["CREATE", "ALTER"]) {
            return;
        }
        let mut i = 1;
        while i < self.toks.len().min(16) {
            i = self.definition_option(i);
        }
    }

    fn binary_shorthand(&self, i: usize) -> bool {
        let before_option = self.any(i + 1, &["DEFAULT", "NOT", "NULL", "CHARACTER", "COLLATE"])
            || matches!(self.tok(i + 1), Some(Token::Comma | Token::RParen));
        self.is(i, "BINARY") && self.tok(i - 1) == Some(&Token::RParen) && before_option
    }

    /// `DEFINER = user@host`, `ALGORITHM = …`, `SQL SECURITY …` at token
    /// `i`: blanked; the index to go on from.
    fn definition_option(&mut self, i: usize) -> usize {
        if self.is(i, "DEFINER") && self.tok(i + 1) == Some(&Token::Eq) {
            // `user@host`, `'user'@'host'`, `CURRENT_USER[()]`
            let mut end = i + 3;
            if matches!(self.tok(end), Some(Token::AtSign)) {
                end += 2;
            } else if self.tok(end) == Some(&Token::LParen) {
                end = self.close(end);
            }
            self.blank(i, end);
            return end;
        }
        let algorithm = self.is(i, "ALGORITHM") && self.tok(i + 1) == Some(&Token::Eq);
        if algorithm || (self.is(i, "SQL") && self.is(i + 1, "SECURITY")) {
            self.blank(i, i + 3);
            return i + 3;
        }
        i + 1
    }
}

/// Words after which a Jinja placeholder is an operand, not a clause.
fn is_clause_word(word: &str) -> bool {
    const WORDS: &[&str] = &[
        "SELECT",
        "FROM",
        "WHERE",
        "AND",
        "OR",
        "NOT",
        "ON",
        "BY",
        "JOIN",
        "AS",
        "WHEN",
        "THEN",
        "ELSE",
        "IN",
        "LIKE",
        "BETWEEN",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "UNION",
        "ALL",
        "DISTINCT",
        "INTO",
        "VALUES",
        "SET",
        "UPDATE",
        "TABLE",
        "VIEW",
        "CASE",
        "IS",
        "WITH",
        "USING",
        "OVER",
        "PARTITION",
        "INSERT",
        "DELETE",
        "MERGE",
        "EXCEPT",
        "INTERSECT",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "OUTER",
        "CROSS",
        "LATERAL",
        "QUALIFY",
        "END",
        "RETURNING",
        "TOP",
        "INTERVAL",
        "CAST",
        "IF",
        "EXISTS",
        "REPLACE",
        "CREATE",
        "ALTER",
        "DROP",
        "TO",
        "FOR",
        "GRANT",
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
        assert_eq!(
            fix("select a over (partition by b _____ order by c)", Dialect::Generic),
            "select a over (partition by b order by c)"
        );
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
            fix(
                "CREATE TABLE t (id INT, CONSTRAINT pk PRIMARY KEY (id) WITH (PAD_INDEX = OFF) ON [PRIMARY])",
                Dialect::MsSql
            ),
            "CREATE TABLE t (id INT, CONSTRAINT pk PRIMARY KEY (id) )"
        );
    }

    #[test]
    fn oracle_db2_mysql() {
        assert_eq!(
            fix(
                "CREATE TABLE t (a VARCHAR2(10 CHAR), CONSTRAINT c CHECK (a > 1) ENABLE) PCTFREE 10 TABLESPACE users",
                Dialect::Oracle
            ),
            "CREATE TABLE t (a VARCHAR2(10 ), CONSTRAINT c CHECK (a > 1) )"
        );
        assert_eq!(
            fix("CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY (START WITH 1))", Dialect::Db2),
            "CREATE TABLE t (id INT GENERATED ALWAYS AS IDENTITY )"
        );
        assert_eq!(
            fix("CREATE DEFINER=`root`@`localhost` VIEW v AS SELECT 1", Dialect::MySql),
            "CREATE VIEW v AS SELECT 1"
        );
        assert_eq!(fix("INSERT INTO t VALUES (CURRENT DATE)", Dialect::Db2), "INSERT INTO t VALUES (CURRENT_DATE)");
    }
}
