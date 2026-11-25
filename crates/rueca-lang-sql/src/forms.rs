//! Productions for statements sqlparser has no grammar for (crate docs,
//! *Statements and recovery*). Each production is a grammar for one
//! statement form and must consume **every** token of the statement:
//! names, typed argument lists, option keywords from closed sets, and
//! expressions, which are handed to sqlparser's expression parser. A
//! statement no production matches in full is not accepted here.
//!
//! | Form | Grammar |
//! |---|---|
//! | flow (T-SQL) | `BREAK` \| `CONTINUE` \| `REVERT` \| `CHECKPOINT` \| `RECONFIGURE [WITH OVERRIDE]` \| `GOTO label` \| `WAITFOR DELAY \| TIME expr` \| `USE name` \| `KILL n` |
//! | transactions (T-SQL) | `BEGIN [DISTRIBUTED] TRAN[SACTION] [name] [WITH MARK ['s']]` \| `COMMIT \| ROLLBACK [TRAN[SACTION] \| WORK] [name]` \| `SAVE TRAN[SACTION] name` |
//! | messages (T-SQL) | `PRINT expr` \| `RAISERROR (expr, …) [WITH option, …]` \| `THROW [expr, expr, expr]` |
//! | settings (T-SQL) | `SET option [, option] ON \| OFF` (closed list) \| `SET STATISTICS IO \| TIME \| XML \| PROFILE [, …] ON \| OFF` \| `SET IDENTITY_INSERT name ON \| OFF` \| `SET TRANSACTION ISOLATION LEVEL level` \| `SET ROWCOUNT \| LOCK_TIMEOUT \| DATEFIRST \| TEXTSIZE \| DEADLOCK_PRIORITY \| LANGUAGE \| DATEFORMAT value` \| `SET @v [op]= expr` |
//! | cursors (T-SQL) | `OPEN \| CLOSE \| DEALLOCATE [GLOBAL] name` \| `FETCH [NEXT \| PRIOR \| FIRST \| LAST \| ABSOLUTE n \| RELATIVE n] [FROM] [GLOBAL] name [INTO @v, …]` |
//! | calls (T-SQL) | `EXEC[UTE] [@r =] name \| @v [[@p =] expr \| DEFAULT [OUT[PUT]], …] [WITH RECOMPILE]` \| `EXEC[UTE] (expr) [AS LOGIN \| USER = 's'] [AT name]` \| `EXECUTE AS CALLER \| SELF \| OWNER \| LOGIN = 's' \| USER = 's'` |
//! | administration (T-SQL) | `DBCC name [(expr, …)] [WITH option, …]` \| `BACKUP \| RESTORE DATABASE \| LOG name TO \| FROM device, … [WITH option [= expr], …]` \| `ALTER DATABASE name SET option [value] [WITH ROLLBACK IMMEDIATE \| NO_WAIT]` \| `CREATE DATABASE name [COLLATE c] [WITH option ON \| OFF, …]` \| `CREATE ASSEMBLY name [AUTHORIZATION owner] FROM 'file' \| 0x… WITH PERMISSION_SET = set` \| `ALTER ASSEMBLY name WITH PERMISSION_SET = set` \| `CREATE XML SCHEMA COLLECTION name AS expr` \| `ALTER AUTHORIZATION ON [class ::] name TO name` \| `ALTER SERVER ROLE role ADD \| DROP MEMBER name` \| `CREATE USER name [FOR \| FROM LOGIN name \| WITHOUT LOGIN]` \| `CREATE LOGIN name FROM CERTIFICATE \| ASYMMETRIC KEY name` \| `CREATE LOGIN name WITH option = value [HASHED], …` \| `UPDATE STATISTICS name [name]` \| `ALTER INDEX name \| ALL ON table DISABLE \| REBUILD \| REORGANIZE` |
//! | permissions | `GRANT \| DENY \| REVOKE privilege, … [ON [class ::] name] TO \| FROM name, … [WITH GRANT OPTION] [CASCADE]` |
//! | `DROP` | `DROP kind [IF EXISTS] name [(types)], … [CASCADE \| RESTRICT \| FORCE]`, kinds from a closed list |
//! | types | `CREATE TYPE name FROM type [[NOT] NULL]` \| `… EXTERNAL NAME a.b` \| `… AS TABLE (columns)` \| `… AS \| IS OBJECT (attributes)` (the list parses as the columns of a table) \| `… AS \| IS TABLE OF type` \| `ALTER TYPE name OWNER TO n \| SET SCHEMA n \| RENAME TO n \| ADD ATTRIBUTE a type \| DROP ATTRIBUTE [IF EXISTS] a \| RENAME ATTRIBUTE a TO b [CASCADE \| RESTRICT]` |
//! | synonyms | `CREATE [OR REPLACE] [PUBLIC] SYNONYM name FOR name` |
//! | tables | `ALTER TABLE [IF EXISTS] [ONLY] name action, …` with `SET SCHEMA n`, `OWNER TO n`, `ALTER [COLUMN] c SET STATISTICS n`, `ALTER COLUMN c type [[NOT] NULL]`, `ADD CONSTRAINT n DEFAULT expr FOR c`, `[WITH CHECK \| NOCHECK] ADD CONSTRAINT n FOREIGN KEY (c, …) REFERENCES t (c, …) [ON DELETE \| UPDATE action]`, `[NO]CHECK CONSTRAINT n \| ALL`, `ALTER CONSTRAINT n [NOT] DEFERRABLE [INITIALLY DEFERRED \| IMMEDIATE]`, `ENABLE \| DISABLE TRIGGER n \| ALL`, `ENABLE \| DISABLE ROW LEVEL SECURITY` |
//! | loads | MySQL `LOAD DATA [LOCAL] INFILE 'f' [REPLACE \| IGNORE] INTO TABLE t [CHARACTER SET c] [FIELDS …] [LINES …] [IGNORE n LINES] [(columns)] [SET c = expr, …]` \| MonetDB `COPY [n [OFFSET n] RECORDS] INTO t FROM 'f' [USING DELIMITERS 's', …] [NULL AS 's']` \| DuckDB `EXPORT \| IMPORT DATABASE 'dir' [(options)]` |
//! | keys and credentials (T-SQL) | `CREATE MASTER KEY ENCRYPTION BY PASSWORD = 's'` \| `OPEN MASTER KEY DECRYPTION BY PASSWORD = 's'` \| `CLOSE MASTER KEY` \| `CREATE DATABASE SCOPED CREDENTIAL name WITH option = value, …` \| `ALTER LOGIN name ENABLE \| DISABLE \| WITH option = value, …` |
//! | languages (PostgreSQL) | `CREATE [OR REPLACE] [TRUSTED] [PROCEDURAL] LANGUAGE name [HANDLER f] [INLINE f] [VALIDATOR f]` \| `ALTER [PROCEDURAL] LANGUAGE name OWNER TO \| RENAME TO n` |
//! | PostgreSQL | `DO [LANGUAGE l] 'body' [LANGUAGE l]` \| `CREATE [OR REPLACE] FUNCTION \| PROCEDURE name ([mode] [name] type [DEFAULT \| = expr], …) [RETURNS [SETOF] type \| TABLE (columns)] attribute… AS 'body' attribute…` (attributes from a closed list; the body is text, read by the scanner as for the routines sqlparser parses) \| `ALTER EXTENSION name UPDATE [TO version]` \| `ALTER EXTENSION name ADD \| DROP kind name [(types)]` \| `CREATE PUBLICATION name [FOR ALL TABLES \| FOR TABLE name, …]` \| `ALTER INDEX name SET TABLESPACE n` |

use sqlparser::tokenizer::Token;

use crate::dialect::Dialect;
use crate::facts::Name;
use crate::read::Reader;
use crate::scan::{self, Tok};

/// What a production read.
#[derive(Default)]
pub struct Parsed {
    /// Token ranges `[from, to)` that are expressions (parsed by sqlparser).
    pub exprs: Vec<(usize, usize)>,
    pub calls: Vec<Name>,
    pub reads: Vec<Name>,
    pub writes: Vec<Name>,
    /// Byte range of a routine body kept as text.
    pub body: Option<(usize, usize)>,
    /// The statement defines what its head names (read by the scanner).
    pub define: bool,
}

/// The reading of the statement with significant tokens `toks`, when a
/// production matches all of it and every expression in it parses.
pub fn parse(r: &Reader<'_, '_>, toks: &[Tok]) -> Option<Parsed> {
    if toks.is_empty() {
        return None;
    }
    let ups: Vec<String> = toks.iter().map(|t| t.word().to_ascii_uppercase()).collect();
    let mut c = Cursor { toks, ups: &ups, i: 0, out: Parsed::default(), dialect: r.b.dialect };
    c.statement()?;
    // a trailing `;` belongs to the statement
    c.eat(&Token::SemiColon);
    if !c.end() {
        return None;
    }
    let ok = c.out.exprs.iter().all(|&(a, b)| b > a && r.expr_parses((toks[a].range.0, toks[b - 1].range.1)));
    ok.then_some(c.out)
}

fn need(ok: bool) -> Option<()> {
    ok.then_some(())
}

struct Cursor<'t> {
    toks: &'t [Tok],
    ups: &'t [String],
    i: usize,
    out: Parsed,
    dialect: Dialect,
}

/// T-SQL `SET option ON | OFF`.
const SET_FLAGS: &[&str] = &[
    "NOCOUNT",
    "XACT_ABORT",
    "ANSI_NULLS",
    "QUOTED_IDENTIFIER",
    "ANSI_PADDING",
    "ANSI_WARNINGS",
    "ARITHABORT",
    "CONCAT_NULL_YIELDS_NULL",
    "NUMERIC_ROUNDABORT",
    "IMPLICIT_TRANSACTIONS",
    "NOEXEC",
    "PARSEONLY",
    "FMTONLY",
    "SHOWPLAN_ALL",
    "SHOWPLAN_TEXT",
    "SHOWPLAN_XML",
    "ANSI_DEFAULTS",
    "ANSI_NULL_DFLT_ON",
    "ANSI_NULL_DFLT_OFF",
    "CURSOR_CLOSE_ON_COMMIT",
    "FORCEPLAN",
    "REMOTE_PROC_TRANSACTIONS",
    "ARITHIGNORE",
];
const SET_VALUES: &[&str] =
    &["ROWCOUNT", "LOCK_TIMEOUT", "DATEFIRST", "TEXTSIZE", "DEADLOCK_PRIORITY", "LANGUAGE", "DATEFORMAT"];

/// Object kinds of `DROP` (the words before the names).
const DROP_KINDS: &[&[&str]] = &[
    &["PACKAGE", "BODY"],
    &["TYPE", "BODY"],
    &["MATERIALIZED", "VIEW"],
    &["FOREIGN", "TABLE"],
    &["PUBLIC", "SYNONYM"],
    &["DATABASE", "LINK"],
    &["EVENT", "TRIGGER"],
    &["MESSAGE", "TYPE"],
    &["TYPE"],
    &["SYNONYM"],
    &["PACKAGE"],
    &["LOGIN"],
    &["USER"],
    &["ROLE"],
    &["ASSEMBLY"],
    &["SERVICE"],
    &["QUEUE"],
    &["CONTRACT"],
    &["CERTIFICATE"],
    &["PROCEDURE"],
    &["PROC"],
    &["FUNCTION"],
    &["AGGREGATE"],
    &["TRIGGER"],
    &["VIEW"],
    &["TABLE"],
    &["INDEX"],
    &["SEQUENCE"],
    &["SCHEMA"],
    &["DATABASE"],
    &["DOMAIN"],
    &["EXTENSION"],
    &["PUBLICATION"],
    &["SUBSCRIPTION"],
    &["OPERATOR"],
    &["CAST"],
    &["RULE"],
    &["POLICY"],
    &["STATISTICS"],
    &["DEFAULT"],
];

/// Object kinds of `ALTER EXTENSION … ADD | DROP`.
const EXTENSION_KINDS: &[&[&str]] = &[
    &["MATERIALIZED", "VIEW"],
    &["FOREIGN", "TABLE"],
    &["EVENT", "TRIGGER"],
    &["OPERATOR", "CLASS"],
    &["OPERATOR", "FAMILY"],
    &["FUNCTION"],
    &["PROCEDURE"],
    &["ROUTINE"],
    &["AGGREGATE"],
    &["TABLE"],
    &["VIEW"],
    &["SEQUENCE"],
    &["TYPE"],
    &["DOMAIN"],
    &["SCHEMA"],
    &["OPERATOR"],
    &["CAST"],
    &["COLLATION"],
    &["INDEX"],
];

/// Attributes of a PostgreSQL routine that are one keyword sequence.
const ROUTINE_FLAGS: &[&[&str]] = &[
    &["CALLED", "ON", "NULL", "INPUT"],
    &["RETURNS", "NULL", "ON", "NULL", "INPUT"],
    &["EXTERNAL", "SECURITY", "DEFINER"],
    &["EXTERNAL", "SECURITY", "INVOKER"],
    &["SECURITY", "DEFINER"],
    &["SECURITY", "INVOKER"],
    &["NOT", "LEAKPROOF"],
    &["IMMUTABLE"],
    &["STABLE"],
    &["VOLATILE"],
    &["STRICT"],
    &["LEAKPROOF"],
    &["WINDOW"],
];

impl Cursor<'_> {
    fn up(&self, k: usize) -> &str {
        self.ups.get(k).map_or("", String::as_str)
    }

    fn tok(&self, k: usize) -> Option<&Token> {
        self.toks.get(k).map(|t| &t.token)
    }

    fn end(&self) -> bool {
        self.i >= self.toks.len()
    }

    /// Consumes the keyword `w`.
    fn kw(&mut self, w: &str) -> bool {
        let hit = self.up(self.i) == w;
        self.i += usize::from(hit);
        hit
    }

    /// Consumes one of `words`; which one.
    fn any(&mut self, words: &[&'static str]) -> Option<&'static str> {
        let w = words.iter().copied().find(|w| self.up(self.i) == *w)?;
        self.i += 1;
        Some(w)
    }

    /// Consumes the keyword sequence `words`.
    fn seq(&mut self, words: &[&str]) -> bool {
        let hit = words.iter().enumerate().all(|(k, w)| self.up(self.i + k) == *w);
        if hit {
            self.i += words.len();
        }
        hit
    }

    /// Consumes one of the keyword sequences.
    fn one_of(&mut self, seqs: &[&[&str]]) -> bool {
        seqs.iter().any(|s| self.seq(s))
    }

    fn eat(&mut self, t: &Token) -> bool {
        let hit = self.tok(self.i) == Some(t);
        self.i += usize::from(hit);
        hit
    }

    /// A word that is not a `@variable`.
    fn is_plain_word(&self, k: usize) -> bool {
        matches!(self.tok(k), Some(Token::Word(w)) if !w.value.starts_with('@'))
    }

    fn is_var(&self, k: usize) -> bool {
        matches!(self.tok(k), Some(Token::Word(w)) if w.value.starts_with('@') && w.quote_style.is_none())
    }

    /// Consumes a `@variable`.
    fn var(&mut self) -> bool {
        let hit = self.is_var(self.i);
        self.i += usize::from(hit);
        hit
    }

    /// Consumes a qualified name `a.b.c`.
    fn name(&mut self) -> Option<Name> {
        if !self.is_plain_word(self.i) {
            return None;
        }
        let (name, after) = scan::read_name(self.toks, self.i, self.dialect)?;
        self.i = after;
        Some(name)
    }

    /// Consumes one word (a name of one part, an option value).
    fn word(&mut self) -> bool {
        let hit = self.is_plain_word(self.i);
        self.i += usize::from(hit);
        hit
    }

    fn string(&mut self) -> bool {
        let hit = matches!(
            self.tok(self.i),
            Some(Token::SingleQuotedString(_) | Token::NationalStringLiteral(_) | Token::DollarQuotedString(_))
        );
        self.i += usize::from(hit);
        hit
    }

    fn number(&mut self) -> bool {
        let neg = usize::from(self.tok(self.i) == Some(&Token::Minus));
        let hit = matches!(self.tok(self.i + neg), Some(Token::Number(..)));
        if hit {
            self.i += neg + 1;
        }
        hit
    }

    /// A literal, a variable or a word: a scalar option value.
    fn scalar(&mut self) -> bool {
        self.string()
            || self.number()
            || self.var()
            || matches!(self.tok(self.i), Some(Token::HexStringLiteral(_))) && {
                self.i += 1;
                true
            }
            || self.word()
    }

    /// Index after the `)` matching the `(` at `open`.
    fn close(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        for k in open..self.toks.len() {
            match self.toks[k].token {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k + 1);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// An expression from the cursor up to (not including) the first
    /// token at parenthesis depth 0 for which `stop` holds, or the end;
    /// recorded for sqlparser. Empty: `None`.
    fn expr_until(&mut self, stop: impl Fn(&Self, usize) -> bool) -> Option<()> {
        let from = self.i;
        let mut depth = 0i32;
        let mut k = from;
        while k < self.toks.len() {
            match self.toks[k].token {
                Token::LParen => depth += 1,
                Token::RParen => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Token::SemiColon if depth == 0 => break,
                _ => {}
            }
            if depth == 0 && !matches!(self.toks[k].token, Token::RParen) && stop(self, k) {
                break;
            }
            k += 1;
        }
        if k == from || depth != 0 {
            return None;
        }
        self.out.exprs.push((from, k));
        self.i = k;
        Some(())
    }

    /// A comma-separated list of expressions up to a token for which
    /// `stop` holds (or the end, or an unmatched `)`).
    fn expr_list(&mut self, stop: impl Fn(&Self, usize) -> bool + Copy) -> Option<()> {
        loop {
            self.expr_until(|c, k| c.tok(k) == Some(&Token::Comma) || stop(c, k))?;
            if !self.eat(&Token::Comma) {
                return Some(());
            }
        }
    }

    /// A data type: a name, optionally qualified, the continuation words
    /// of the multi-word SQL types (`DOUBLE PRECISION`, `CHARACTER VARYING`,
    /// `TIMESTAMP WITH TIME ZONE`, `INT UNSIGNED`), `(n[, m])` / `(MAX)`,
    /// `[]`, `%TYPE`; never one of `stops`.
    fn data_type(&mut self, stops: &[&str]) -> Option<()> {
        if stops.contains(&self.up(self.i)) || !self.is_plain_word(self.i) {
            return None;
        }
        self.i += 1;
        while self.tok(self.i) == Some(&Token::Period) && self.is_plain_word(self.i + 1) {
            self.i += 2;
        }
        loop {
            if self.any(&["PRECISION", "VARYING", "UNSIGNED"]).is_some() {
                continue;
            }
            if matches!(self.up(self.i), "WITH" | "WITHOUT")
                && self.up(self.i + 1) == "TIME"
                && self.up(self.i + 2) == "ZONE"
            {
                self.i += 3;
                continue;
            }
            match self.tok(self.i) {
                Some(Token::LParen) => {
                    let close = self.close(self.i)?;
                    let inner_ok = close > self.i + 2
                        && (self.i + 1..close - 1).all(|k| {
                            matches!(self.tok(k), Some(Token::Number(..) | Token::Comma))
                                || matches!(self.up(k), "MAX" | "CHAR" | "BYTE")
                        });
                    need(inner_ok)?;
                    self.i = close;
                }
                Some(Token::LBracket) if self.tok(self.i + 1) == Some(&Token::RBracket) => self.i += 2,
                Some(Token::Mod) if matches!(self.up(self.i + 1), "TYPE" | "ROWTYPE") => self.i += 2,
                _ => return Some(()),
            }
        }
    }

    /// `[name] type`: a parameter or a column of a `RETURNS TABLE` list.
    fn named_type(&mut self, stops: &[&str]) -> Option<()> {
        let second_is_type = self.is_plain_word(self.i)
            && self.is_plain_word(self.i + 1)
            && !stops.contains(&self.up(self.i + 1))
            && !matches!(self.up(self.i + 1), "PRECISION" | "VARYING" | "UNSIGNED" | "WITH" | "WITHOUT");
        if second_is_type {
            self.i += 1;
        }
        self.data_type(stops)
    }

    /// `( name, … )`: a list of plain names.
    fn name_list(&mut self) -> Option<()> {
        need(self.eat(&Token::LParen))?;
        loop {
            self.name()?;
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        need(self.eat(&Token::RParen))
    }

    /// `( type, … )` or `()`: the argument types of a routine name.
    fn type_list(&mut self) -> Option<()> {
        need(self.eat(&Token::LParen))?;
        if self.eat(&Token::RParen) {
            return Some(());
        }
        loop {
            self.any(&["IN", "OUT", "INOUT", "VARIADIC"]);
            self.data_type(&[])?;
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        need(self.eat(&Token::RParen))
    }

    // -----------------------------------------------------------------

    fn statement(&mut self) -> Option<()> {
        match self.up(0) {
            "BREAK" | "CONTINUE" | "REVERT" | "CHECKPOINT" => {
                self.i = 1;
                Some(())
            }
            "RECONFIGURE" => {
                self.i = 1;
                self.seq(&["WITH", "OVERRIDE"]);
                Some(())
            }
            "GOTO" | "USE" => {
                self.i = 1;
                need(self.word())
            }
            "KILL" => {
                self.i = 1;
                need(self.number() || self.var())
            }
            "WAITFOR" => {
                self.i = 1;
                self.any(&["DELAY", "TIME"])?;
                self.expr_until(|_, _| false)
            }
            "BEGIN" | "COMMIT" | "ROLLBACK" | "SAVE" => self.transaction(),
            "PRINT" => {
                self.i = 1;
                self.expr_until(|_, _| false)
            }
            "RAISERROR" => self.raiserror(),
            "THROW" => {
                self.i = 1;
                if self.end() || self.tok(self.i) == Some(&Token::SemiColon) {
                    return Some(());
                }
                self.expr_list(|_, _| false)
            }
            "SET" => self.set(),
            "OPEN" | "CLOSE" if self.up(1) == "MASTER" => {
                // `OPEN MASTER KEY DECRYPTION BY PASSWORD = 's'`, `CLOSE MASTER KEY`
                self.i = 1;
                need(self.seq(&["MASTER", "KEY"]))?;
                if self.seq(&["DECRYPTION", "BY", "PASSWORD"]) {
                    need(self.eat(&Token::Eq))?;
                    need(self.string())?;
                }
                Some(())
            }
            "OPEN" | "CLOSE" | "DEALLOCATE" => {
                self.i = 1;
                self.kw("GLOBAL");
                need(self.var() || self.word())
            }
            "FETCH" => self.fetch(),
            "EXEC" | "EXECUTE" => self.exec(),
            "DBCC" => self.dbcc(),
            "BACKUP" | "RESTORE" => self.backup(),
            "GRANT" | "DENY" | "REVOKE" => self.grant(),
            "DROP" => self.drop(),
            "UPDATE" => {
                self.i = 1;
                need(self.kw("STATISTICS"))?;
                let n = self.name()?;
                self.out.reads.push(n);
                if self.up(self.i) != "WITH" {
                    self.word();
                }
                if self.kw("WITH") {
                    loop {
                        need(self.word())?;
                        if self.eat(&Token::Eq) {
                            need(self.number())?;
                        }
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                }
                Some(())
            }
            "DO" => self.do_block(),
            "LOAD" => self.load_data(),
            "EXPORT" | "IMPORT" => {
                // DuckDB `EXPORT DATABASE 'dir' [(option value, …)]`, `IMPORT DATABASE 'dir'`
                self.i = 1;
                need(self.kw("DATABASE"))?;
                need(self.string())?;
                if self.eat(&Token::LParen) {
                    loop {
                        need(self.word())?;
                        self.scalar();
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    need(self.eat(&Token::RParen))?;
                }
                Some(())
            }
            "COPY" => self.monet_copy(),
            "CREATE" => self.create(),
            "ALTER" => self.alter(),
            _ => None,
        }
    }

    fn transaction(&mut self) -> Option<()> {
        let head = self.up(0).to_string();
        self.i = 1;
        if head == "BEGIN" {
            self.kw("DISTRIBUTED");
        }
        let tran = self.any(&["TRAN", "TRANSACTION"]).is_some();
        match head.as_str() {
            "BEGIN" | "SAVE" => need(tran)?,
            _ => {
                if !tran {
                    self.kw("WORK");
                }
            }
        }
        if head == "SAVE" || (tran && !self.end() && self.up(self.i) != "WITH") {
            need(self.var() || self.word())?;
        }
        if head == "BEGIN" && self.seq(&["WITH", "MARK"]) {
            self.string();
        }
        Some(())
    }

    /// `RAISERROR (args) [WITH option, …]`.
    fn raiserror(&mut self) -> Option<()> {
        self.i = 1;
        need(self.eat(&Token::LParen))?;
        self.expr_list(|_, _| false)?;
        need(self.eat(&Token::RParen))?;
        if self.kw("WITH") {
            loop {
                self.any(&["NOWAIT", "LOG", "SETERROR"])?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        Some(())
    }

    fn set(&mut self) -> Option<()> {
        self.i = 1;
        if self.var() {
            // `SET @v = expr`, `SET @v += expr`
            if !self.eat(&Token::Eq) {
                need(matches!(self.tok(self.i), Some(Token::Plus | Token::Minus | Token::Mul | Token::Div)))?;
                self.i += 1;
                need(self.eat(&Token::Eq))?;
            }
            return self.expr_until(|_, _| false);
        }
        if self.kw("STATISTICS") {
            loop {
                self.any(&["IO", "TIME", "XML", "PROFILE"])?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            return self.any(&["ON", "OFF"]).map(|_| ());
        }
        if self.kw("IDENTITY_INSERT") {
            let n = self.name()?;
            self.out.writes.push(n);
            return self.any(&["ON", "OFF"]).map(|_| ());
        }
        if self.seq(&["TRANSACTION", "ISOLATION", "LEVEL"]) {
            let ok = self.seq(&["READ", "UNCOMMITTED"])
                || self.seq(&["READ", "COMMITTED"])
                || self.seq(&["REPEATABLE", "READ"])
                || self.kw("SNAPSHOT")
                || self.kw("SERIALIZABLE");
            return need(ok);
        }
        if self.any(SET_VALUES).is_some() {
            return need(self.scalar());
        }
        loop {
            self.any(SET_FLAGS)?;
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.any(&["ON", "OFF"]).map(|_| ())
    }

    fn fetch(&mut self) -> Option<()> {
        self.i = 1;
        if self.any(&["ABSOLUTE", "RELATIVE"]).is_some() {
            need(self.number() || self.var())?;
        } else {
            self.any(&["NEXT", "PRIOR", "FIRST", "LAST"]);
        }
        self.kw("FROM");
        self.kw("GLOBAL");
        need(self.var() || self.word())?;
        if self.kw("INTO") {
            loop {
                need(self.var())?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        Some(())
    }

    fn exec(&mut self) -> Option<()> {
        self.i = 1;
        if self.kw("AS") {
            // `EXECUTE AS CALLER | SELF | OWNER | LOGIN = 's' | USER = 's'`
            if self.any(&["CALLER", "SELF", "OWNER"]).is_some() {
                return Some(());
            }
            self.any(&["LOGIN", "USER"])?;
            need(self.eat(&Token::Eq))?;
            return need(self.string() || self.var());
        }
        if self.eat(&Token::LParen) {
            // dynamic SQL: `EXEC (@sql + N'…')`
            self.expr_until(|_, _| false)?;
            need(self.eat(&Token::RParen))?;
            if self.kw("AS") {
                self.any(&["LOGIN", "USER"])?;
                need(self.eat(&Token::Eq))?;
                need(self.string())?;
            }
            if self.kw("AT") {
                need(self.word())?;
            }
            return Some(());
        }
        if self.is_var(self.i) && self.tok(self.i + 1) == Some(&Token::Eq) {
            self.i += 2;
        }
        if !self.var() {
            let n = self.name()?;
            self.out.calls.push(n);
        }
        let stop = |c: &Self, k: usize| c.up(k) == "WITH";
        if !self.end() && self.tok(self.i) != Some(&Token::SemiColon) && self.up(self.i) != "WITH" {
            loop {
                if self.is_var(self.i) && self.tok(self.i + 1) == Some(&Token::Eq) {
                    self.i += 2;
                }
                if !self.kw("DEFAULT") {
                    self.expr_until(|c, k| {
                        c.tok(k) == Some(&Token::Comma) || matches!(c.up(k), "OUTPUT" | "OUT") || stop(c, k)
                    })?;
                }
                self.any(&["OUTPUT", "OUT"]);
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        if self.kw("WITH") {
            need(self.kw("RECOMPILE"))?;
        }
        Some(())
    }

    /// `DBCC name [(args)] [WITH option, …]`.
    fn dbcc(&mut self) -> Option<()> {
        self.i = 1;
        need(self.word())?;
        if self.eat(&Token::LParen) && !self.eat(&Token::RParen) {
            self.expr_list(|_, _| false)?;
            need(self.eat(&Token::RParen))?;
        }
        if self.kw("WITH") {
            loop {
                need(self.word())?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        Some(())
    }

    /// `BACKUP | RESTORE DATABASE | LOG name TO | FROM device, … [WITH option [= value], …]`.
    fn backup(&mut self) -> Option<()> {
        let restore = self.up(0) == "RESTORE";
        self.i = 1;
        if !(restore && self.any(&["VERIFYONLY", "HEADERONLY", "FILELISTONLY", "LABELONLY"]).is_some()) {
            self.any(&["DATABASE", "LOG"])?;
            need(self.var() || self.word())?;
        }
        if self.any(&["TO", "FROM"]).is_some() {
            self.devices()?;
            while self.seq(&["MIRROR", "TO"]) {
                self.devices()?;
            }
        } else {
            // `RESTORE DATABASE name WITH RECOVERY`
            need(restore)?;
        }
        if self.kw("WITH") {
            loop {
                need(self.word())?;
                if self.eat(&Token::Eq) {
                    need(self.scalar())?;
                }
                if self.kw("TO") {
                    // `MOVE 'logical' TO 'physical'`
                    need(self.string() || self.var())?;
                }
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        Some(())
    }

    /// `DISK = 'file' | name, …`: backup devices.
    fn devices(&mut self) -> Option<()> {
        loop {
            if self.any(&["DISK", "URL", "TAPE"]).is_some() {
                need(self.eat(&Token::Eq))?;
                need(self.string() || self.var())?;
            } else {
                need(self.var() || self.word())?;
            }
            if !self.eat(&Token::Comma) {
                return Some(());
            }
        }
    }

    /// MySQL `LOAD DATA [LOCAL] INFILE 'file' [REPLACE | IGNORE] INTO TABLE
    /// t [CHARACTER SET c] [FIELDS | COLUMNS clauses] [LINES clauses]
    /// [IGNORE n LINES | ROWS] [(column | @variable, …)] [SET column =
    /// expr, …]`.
    fn load_data(&mut self) -> Option<()> {
        self.i = 1;
        need(self.kw("DATA"))?;
        self.any(&["LOCAL", "LOW_PRIORITY", "CONCURRENT"]);
        self.kw("LOCAL");
        need(self.kw("INFILE"))?;
        need(self.string())?;
        self.any(&["REPLACE", "IGNORE"]);
        need(self.seq(&["INTO", "TABLE"]))?;
        let t = self.name()?;
        self.out.writes.push(t);
        if self.seq(&["CHARACTER", "SET"]) {
            need(self.word())?;
        }
        if self.any(&["FIELDS", "COLUMNS"]).is_some() {
            let from = self.i;
            loop {
                let ok = self.seq(&["TERMINATED", "BY"])
                    || self.seq(&["OPTIONALLY", "ENCLOSED", "BY"])
                    || self.seq(&["ENCLOSED", "BY"])
                    || self.seq(&["ESCAPED", "BY"]);
                if !ok {
                    break;
                }
                need(self.string())?;
            }
            need(self.i > from)?;
        }
        if self.kw("LINES") {
            let from = self.i;
            while self.seq(&["STARTING", "BY"]) || self.seq(&["TERMINATED", "BY"]) {
                need(self.string())?;
            }
            need(self.i > from)?;
        }
        if self.kw("IGNORE") {
            need(self.number())?;
            self.any(&["LINES", "ROWS"])?;
        }
        if self.eat(&Token::LParen) {
            loop {
                need(self.var() || self.word())?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            need(self.eat(&Token::RParen))?;
        }
        if self.kw("SET") {
            loop {
                need(self.word())?;
                need(self.eat(&Token::Eq))?;
                self.expr_until(|c, k| c.tok(k) == Some(&Token::Comma))?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        Some(())
    }

    /// MonetDB `COPY [n [OFFSET n] RECORDS] INTO t FROM 'file' [USING
    /// DELIMITERS 's', …] [NULL AS 's'] [LOCKED]`.
    fn monet_copy(&mut self) -> Option<()> {
        self.i = 1;
        if self.number() {
            if self.kw("OFFSET") {
                need(self.number())?;
            }
            need(self.kw("RECORDS"))?;
        }
        need(self.kw("INTO"))?;
        let t = self.name()?;
        self.out.writes.push(t);
        need(self.kw("FROM"))?;
        need(self.string())?;
        if self.seq(&["USING", "DELIMITERS"]) {
            loop {
                need(self.string())?;
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        if self.seq(&["NULL", "AS"]) {
            need(self.string())?;
        }
        self.kw("LOCKED");
        Some(())
    }

    /// `GRANT | DENY | REVOKE privilege, … [ON [class ::] name] TO | FROM
    /// grantee, … [WITH GRANT OPTION] [CASCADE]`.
    fn grant(&mut self) -> Option<()> {
        self.i = 1;
        loop {
            let from = self.i;
            while self.is_plain_word(self.i) && !matches!(self.up(self.i), "ON" | "TO" | "FROM") && self.i - from < 5 {
                self.i += 1;
            }
            need(self.i > from)?;
            if self.tok(self.i) == Some(&Token::LParen) {
                self.name_list()?;
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        if self.kw("ON") {
            // `OBJECT ::`, `LOGIN ::`, `DATABASE SCOPED CREDENTIAL ::`
            let class = (self.i..self.toks.len().min(self.i + 4)).find(|&k| self.tok(k) == Some(&Token::DoubleColon));
            if let Some(k) = class {
                need((self.i..k).all(|j| self.is_plain_word(j)))?;
                self.i = k + 1;
            }
            self.name()?;
        }
        self.any(&["TO", "FROM"])?;
        loop {
            self.name()?;
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.seq(&["WITH", "GRANT", "OPTION"]);
        self.kw("CASCADE");
        Some(())
    }

    fn drop(&mut self) -> Option<()> {
        self.i = 1;
        let table = matches!(self.up(1), "TABLE" | "VIEW");
        need(self.one_of(DROP_KINDS))?;
        self.seq(&["IF", "EXISTS"]);
        loop {
            let n = self.name()?;
            if table {
                self.out.writes.push(n);
            }
            if self.tok(self.i) == Some(&Token::LParen) {
                self.type_list()?;
            }
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.any(&["CASCADE", "RESTRICT", "FORCE"]);
        Some(())
    }

    /// `DO [LANGUAGE l] 'body' [LANGUAGE l]`.
    fn do_block(&mut self) -> Option<()> {
        self.i = 1;
        if self.kw("LANGUAGE") {
            need(self.word())?;
        }
        self.out.body = Some(self.text_body()?);
        if self.kw("LANGUAGE") {
            need(self.word())?;
        }
        Some(())
    }

    /// A routine body written as a string: the bytes inside its quotes.
    fn text_body(&mut self) -> Option<(usize, usize)> {
        let t = self.toks.get(self.i)?;
        let range = match &t.token {
            Token::DollarQuotedString(s) => {
                let tag = s.tag.as_ref().map_or(0, String::len) + 2;
                (t.range.0 + tag, t.range.1.saturating_sub(tag).max(t.range.0 + tag))
            }
            Token::SingleQuotedString(_) => (t.range.0 + 1, t.range.1.saturating_sub(1).max(t.range.0 + 1)),
            _ => return None,
        };
        self.i += 1;
        Some(range)
    }

    fn create(&mut self) -> Option<()> {
        self.i = 1;
        let replace = self.seq(&["OR", "REPLACE"]);
        match self.up(self.i) {
            "FUNCTION" | "PROCEDURE" => self.routine(),
            "PUBLIC" | "SYNONYM" => {
                self.kw("PUBLIC");
                need(self.kw("SYNONYM"))?;
                self.name()?;
                need(self.kw("FOR"))?;
                let target = self.name()?;
                self.out.reads.push(target);
                Some(())
            }
            "TRUSTED" | "PROCEDURAL" | "LANGUAGE" => {
                // `CREATE [OR REPLACE] [TRUSTED] [PROCEDURAL] LANGUAGE name [HANDLER f] [INLINE f] [VALIDATOR f]`
                self.kw("TRUSTED");
                self.kw("PROCEDURAL");
                need(self.kw("LANGUAGE"))?;
                need(self.word())?;
                while self.any(&["HANDLER", "INLINE", "VALIDATOR"]).is_some() {
                    let f = self.name()?;
                    self.out.calls.push(f);
                }
                Some(())
            }
            _ if replace => None,
            "ASSEMBLY" => {
                // `CREATE ASSEMBLY name [AUTHORIZATION owner] FROM 'file' | 0x… WITH PERMISSION_SET = set`
                self.i += 1;
                need(self.word())?;
                if self.kw("AUTHORIZATION") {
                    need(self.word())?;
                }
                need(self.kw("FROM"))?;
                let bits = matches!(self.tok(self.i), Some(Token::HexStringLiteral(_) | Token::Number(..)));
                self.i += usize::from(bits);
                need(bits || self.string() || self.var())?;
                need(self.seq(&["WITH", "PERMISSION_SET"]))?;
                need(self.eat(&Token::Eq))?;
                self.any(&["SAFE", "EXTERNAL_ACCESS", "UNSAFE"]).map(|_| ())
            }
            "XML" => {
                // `CREATE XML SCHEMA COLLECTION name AS expr`
                self.i += 1;
                need(self.seq(&["SCHEMA", "COLLECTION"]))?;
                self.name()?;
                need(self.kw("AS"))?;
                self.expr_until(|_, _| false)
            }
            "MASTER" => {
                self.i += 1;
                need(self.seq(&["KEY", "ENCRYPTION", "BY", "PASSWORD"]))?;
                need(self.eat(&Token::Eq))?;
                need(self.string())
            }
            "DATABASE" if self.up(self.i + 1) == "SCOPED" => {
                self.i += 1;
                need(self.seq(&["SCOPED", "CREDENTIAL"]))?;
                need(self.word())?;
                need(self.kw("WITH"))?;
                self.options()
            }
            "TYPE" => self.create_type(),
            "PUBLICATION" => {
                self.i += 1;
                need(self.word())?;
                if self.kw("FOR") {
                    if self.seq(&["ALL", "TABLES"]) {
                        return Some(());
                    }
                    need(self.kw("TABLE"))?;
                    loop {
                        self.kw("ONLY");
                        let n = self.name()?;
                        self.out.reads.push(n);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                }
                Some(())
            }
            "USER" => {
                self.i += 1;
                need(self.word())?;
                if self.seq(&["WITHOUT", "LOGIN"]) {
                    return Some(());
                }
                if self.any(&["FOR", "FROM"]).is_some() {
                    need(self.kw("LOGIN"))?;
                    need(self.word())?;
                }
                Some(())
            }
            "LOGIN" => {
                self.i += 1;
                need(self.word())?;
                if self.kw("FROM") {
                    let ok = self.kw("CERTIFICATE") || self.seq(&["ASYMMETRIC", "KEY"]);
                    need(ok)?;
                    return need(self.word());
                }
                need(self.kw("WITH"))?;
                self.options()
            }
            "DATABASE" => {
                self.i += 1;
                need(self.word())?;
                if self.kw("COLLATE") {
                    need(self.word())?;
                }
                if self.kw("WITH") {
                    loop {
                        need(self.word())?;
                        self.any(&["ON", "OFF"])?;
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                }
                Some(())
            }
            _ => None,
        }
    }

    /// `option = value [HASHED], …`.
    fn options(&mut self) -> Option<()> {
        loop {
            need(self.word())?;
            need(self.eat(&Token::Eq))?;
            need(self.scalar())?;
            self.kw("HASHED");
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Some(())
    }

    /// `CREATE TYPE name …` (module docs).
    fn create_type(&mut self) -> Option<()> {
        self.i += 1;
        self.name()?;
        self.out.define = true;
        if self.kw("FROM") {
            self.data_type(&["NULL", "NOT"])?;
            self.kw("NOT");
            self.kw("NULL");
            return Some(());
        }
        if self.seq(&["EXTERNAL", "NAME"]) {
            return self.name().map(|_| ());
        }
        if self.kw("AUTHID") {
            self.any(&["CURRENT_USER", "DEFINER"])?;
        }
        self.any(&["AS", "IS"])?;
        if self.seq(&["TABLE", "OF"]) {
            return self.data_type(&[]);
        }
        self.any(&["TABLE", "OBJECT"])?;
        // `( name type [NOT NULL], … )`: names and types only
        need(self.eat(&Token::LParen))?;
        loop {
            need(self.word())?;
            self.data_type(&["NOT", "NULL", "PRIMARY"])?;
            self.kw("NOT");
            self.kw("NULL");
            self.seq(&["PRIMARY", "KEY"]);
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        need(self.eat(&Token::RParen))
    }

    /// A PostgreSQL routine whose body is a string (module docs).
    fn routine(&mut self) -> Option<()> {
        let function = self.up(self.i) == "FUNCTION";
        self.i += 1;
        self.name()?;
        self.out.define = true;
        need(self.eat(&Token::LParen))?;
        if !self.eat(&Token::RParen) {
            loop {
                self.any(&["IN", "OUT", "INOUT", "VARIADIC"]);
                self.named_type(&["DEFAULT"])?;
                if self.kw("DEFAULT") || self.eat(&Token::Eq) {
                    self.expr_until(|c, k| c.tok(k) == Some(&Token::Comma))?;
                }
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            need(self.eat(&Token::RParen))?;
        }
        if function && self.kw("RETURNS") {
            if self.kw("TABLE") {
                need(self.eat(&Token::LParen))?;
                loop {
                    self.named_type(&[])?;
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                need(self.eat(&Token::RParen))?;
            } else {
                self.kw("SETOF");
                self.data_type(&[
                    "LANGUAGE",
                    "AS",
                    "IMMUTABLE",
                    "STABLE",
                    "VOLATILE",
                    "STRICT",
                    "SECURITY",
                    "PARALLEL",
                    "COST",
                    "ROWS",
                    "SET",
                    "CALLED",
                    "LEAKPROOF",
                    "WINDOW",
                    "NOT",
                    "RETURNS",
                    "EXTERNAL",
                ])?;
            }
        }
        let mut body = None;
        while !self.end() && self.tok(self.i) != Some(&Token::SemiColon) {
            if self.kw("AS") {
                need(body.is_none())?;
                body = Some(self.text_body()?);
                if self.eat(&Token::Comma) {
                    // C functions: `AS 'object file', 'link symbol'`
                    need(self.string())?;
                }
            } else if self.any(&["LANGUAGE", "PARALLEL", "SUPPORT"]).is_some() {
                need(self.word() || self.string())?;
            } else if self.any(&["COST", "ROWS"]).is_some() {
                need(self.number())?;
            } else if self.kw("SET") {
                self.name()?;
                if self.seq(&["FROM", "CURRENT"]) {
                    continue;
                }
                need(self.kw("TO") || self.eat(&Token::Eq))?;
                loop {
                    need(self.scalar())?;
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
            } else {
                need(self.one_of(ROUTINE_FLAGS))?;
            }
        }
        self.out.body = Some(body?);
        Some(())
    }

    fn alter(&mut self) -> Option<()> {
        self.i = 1;
        match self.up(1) {
            "EXTENSION" => {
                self.i = 2;
                need(self.word())?;
                if self.kw("UPDATE") {
                    if self.kw("TO") {
                        need(self.string() || self.word())?;
                    }
                    return Some(());
                }
                self.any(&["ADD", "DROP"])?;
                need(self.one_of(EXTENSION_KINDS))?;
                self.name()?;
                if self.tok(self.i) == Some(&Token::LParen) {
                    self.type_list()?;
                }
                Some(())
            }
            "TYPE" => {
                self.i = 2;
                self.name()?;
                if self.seq(&["OWNER", "TO"]) || self.seq(&["SET", "SCHEMA"]) || self.seq(&["RENAME", "TO"]) {
                    return need(self.word());
                }
                if self.seq(&["ADD", "ATTRIBUTE"]) {
                    need(self.word())?;
                    self.data_type(&["CASCADE", "RESTRICT"])?;
                } else if self.seq(&["DROP", "ATTRIBUTE"]) {
                    self.seq(&["IF", "EXISTS"]);
                    need(self.word())?;
                } else if self.seq(&["RENAME", "ATTRIBUTE"]) {
                    need(self.word())?;
                    need(self.kw("TO"))?;
                    need(self.word())?;
                } else {
                    return None;
                }
                self.any(&["CASCADE", "RESTRICT"]);
                Some(())
            }
            "DOMAIN" | "SEQUENCE" | "VIEW" | "SCHEMA" => {
                self.i = 2;
                self.name()?;
                let ok = self.seq(&["OWNER", "TO"]) || self.seq(&["SET", "SCHEMA"]) || self.seq(&["RENAME", "TO"]);
                need(ok)?;
                need(self.word())
            }
            "PROCEDURAL" | "LANGUAGE" => {
                self.i = 1;
                self.kw("PROCEDURAL");
                need(self.kw("LANGUAGE"))?;
                need(self.word())?;
                let ok = self.seq(&["OWNER", "TO"]) || self.seq(&["RENAME", "TO"]);
                need(ok)?;
                need(self.word())
            }
            "LOGIN" => {
                self.i = 2;
                need(self.word())?;
                if self.any(&["ENABLE", "DISABLE"]).is_some() {
                    return Some(());
                }
                need(self.kw("WITH"))?;
                self.options()
            }
            "AUTHORIZATION" => {
                // `ALTER AUTHORIZATION ON [class ::] name TO name`
                self.i = 2;
                need(self.kw("ON"))?;
                if self.tok(self.i + 1) == Some(&Token::DoubleColon) {
                    need(self.word())?;
                    self.i += 1;
                }
                self.name()?;
                need(self.kw("TO"))?;
                need(self.word())
            }
            "SERVER" => {
                // `ALTER SERVER ROLE role ADD | DROP MEMBER name`
                self.i = 2;
                need(self.kw("ROLE"))?;
                need(self.word())?;
                self.any(&["ADD", "DROP"])?;
                need(self.kw("MEMBER"))?;
                need(self.word())
            }
            "ASSEMBLY" => {
                self.i = 2;
                need(self.word())?;
                need(self.seq(&["WITH", "PERMISSION_SET"]))?;
                need(self.eat(&Token::Eq))?;
                self.any(&["SAFE", "EXTERNAL_ACCESS", "UNSAFE"]).map(|_| ())
            }
            "DATABASE" => {
                self.i = 2;
                need(self.word())?;
                need(self.kw("SET"))?;
                need(self.word())?;
                if self.eat(&Token::Eq) || (!self.end() && self.up(self.i) != "WITH") {
                    need(self.scalar())?;
                }
                if self.kw("WITH") {
                    let ok = self.seq(&["ROLLBACK", "IMMEDIATE"]) || self.kw("NO_WAIT");
                    need(ok)?;
                }
                Some(())
            }
            "INDEX" => {
                self.i = 2;
                if !self.kw("ALL") {
                    self.name()?;
                }
                if self.seq(&["SET", "TABLESPACE"]) {
                    return need(self.word());
                }
                need(self.kw("ON"))?;
                let t = self.name()?;
                self.out.writes.push(t);
                self.any(&["DISABLE", "REBUILD", "REORGANIZE"]).map(|_| ())
            }
            "TABLE" => self.alter_table(),
            _ => None,
        }
    }

    fn alter_table(&mut self) -> Option<()> {
        self.i = 2;
        self.seq(&["IF", "EXISTS"]);
        self.kw("ONLY");
        let t = self.name()?;
        self.out.writes.push(t);
        loop {
            self.table_action()?;
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        Some(())
    }

    fn table_action(&mut self) -> Option<()> {
        if self.seq(&["SET", "SCHEMA"]) || self.seq(&["OWNER", "TO"]) {
            return need(self.word());
        }
        if self.seq(&["ENABLE", "ROW", "LEVEL", "SECURITY"]) || self.seq(&["DISABLE", "ROW", "LEVEL", "SECURITY"]) {
            return Some(());
        }
        if self.any(&["ENABLE", "DISABLE"]).is_some() {
            need(self.kw("TRIGGER"))?;
            return need(self.word());
        }
        if self.any(&["CHECK", "NOCHECK"]).is_some() {
            need(self.kw("CONSTRAINT"))?;
            return need(self.word());
        }
        if self.kw("ALTER") {
            if self.kw("CONSTRAINT") {
                need(self.word())?;
                self.kw("NOT");
                self.kw("DEFERRABLE");
                if self.kw("INITIALLY") {
                    self.any(&["DEFERRED", "IMMEDIATE"])?;
                }
                return Some(());
            }
            self.kw("COLUMN");
            need(self.word())?;
            if self.seq(&["SET", "STATISTICS"]) {
                return need(self.number());
            }
            // T-SQL: a new type and nullability
            self.data_type(&["NULL", "NOT", "COLLATE"])?;
            if self.kw("COLLATE") {
                need(self.word())?;
            }
            self.kw("NOT");
            self.kw("NULL");
            return Some(());
        }
        if self.kw("WITH") {
            self.any(&["CHECK", "NOCHECK"])?;
        }
        need(self.seq(&["ADD", "CONSTRAINT"]))?;
        need(self.word())?;
        if self.kw("DEFAULT") {
            self.expr_until(|c, k| c.up(k) == "FOR")?;
            need(self.kw("FOR"))?;
            return need(self.word());
        }
        need(self.seq(&["FOREIGN", "KEY"]))?;
        self.name_list()?;
        need(self.kw("REFERENCES"))?;
        let target = self.name()?;
        self.out.reads.push(target);
        if self.tok(self.i) == Some(&Token::LParen) {
            self.name_list()?;
        }
        while self.kw("ON") {
            self.any(&["DELETE", "UPDATE"])?;
            let ok = self.kw("CASCADE")
                || self.seq(&["NO", "ACTION"])
                || self.seq(&["SET", "NULL"])
                || self.seq(&["SET", "DEFAULT"])
                || self.kw("RESTRICT");
            need(ok)?;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Builder;

    fn accepts(sql: &str, d: Dialect) -> bool {
        let mut b = Builder::new(sql, d, false);
        let r = Reader::new(&mut b, sql, &[]);
        let toks = scan::tokens(sql, 0, sql.len(), d);
        parse(&r, &toks).is_some()
    }

    /// Each production: statements it reads in full, and malformed ones
    /// of the same form it must reject.
    fn check(d: Dialect, good: &[&str], bad: &[&str]) {
        for sql in good {
            assert!(accepts(sql, d), "should be read: {sql}");
        }
        for sql in bad {
            assert!(!accepts(sql, d), "should be rejected: {sql}");
        }
    }

    #[test]
    fn tsql_flow_and_transactions() {
        check(
            Dialect::MsSql,
            &[
                "BREAK",
                "CONTINUE;",
                "REVERT",
                "GOTO Done",
                "RECONFIGURE WITH OVERRIDE",
                "WAITFOR DELAY '00:00:05'",
                "USE master",
                "KILL 53",
                "BEGIN TRAN",
                "BEGIN DISTRIBUTED TRANSACTION t1",
                "BEGIN TRAN t WITH MARK 'x'",
                "COMMIT",
                "COMMIT TRANSACTION @name",
                "ROLLBACK TRAN @TranName",
                "ROLLBACK WORK",
                "SAVE TRAN @TranName",
            ],
            &[
                "BREAK 1",
                "GOTO",
                "GOTO a b",
                "WAITFOR DELAY",
                "WAITFOR (RECEIVE TOP(1) @h = conversation_handle FROM q)",
                "BEGIN SELECT 1 END",
                "SAVE TRAN",
                "ROLLBACK TRAN a b",
                "KILL them",
            ],
        );
    }

    #[test]
    fn tsql_messages_settings_cursors() {
        check(
            Dialect::MsSql,
            &[
                "PRINT 'a' + CAST(@n AS VARCHAR(10))",
                "RAISERROR('bad %s', 16, 1, @name) WITH NOWAIT, LOG",
                "THROW",
                "THROW 50000, 'msg', 1",
                "SET NOCOUNT ON",
                "SET ANSI_NULLS, QUOTED_IDENTIFIER ON",
                "SET STATISTICS TIME, IO OFF",
                "SET IDENTITY_INSERT dbo.t ON",
                "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED",
                "SET DEADLOCK_PRIORITY LOW",
                "SET @n += LEN(@s) + 1",
                "OPEN c",
                "CLOSE GLOBAL c",
                "DEALLOCATE @cur",
                "FETCH NEXT FROM c INTO @a, @b",
                "FETCH ABSOLUTE 3 FROM c",
            ],
            &[
                "PRINT",
                "PRINT 'a' +",
                "RAISERROR('x', 16, 1) WITH WHATEVER",
                "RAISERROR 'x'",
                "THROW 50000,",
                "SET NOCOUNT",
                "SET NOCOUNT MAYBE",
                "SET STATISTICS BANANAS ON",
                "SET TRANSACTION ISOLATION LEVEL CHAOS",
                "SET @n = ",
                "SET @re = RegEx::Expr(N'x').CaseSen",
                "OPEN",
                "FETCH NEXT FROM c INTO a, b",
                "FETCH NEXT FROM c INTO @a @b",
            ],
        );
    }

    #[test]
    fn tsql_calls_and_administration() {
        check(
            Dialect::MsSql,
            &[
                "EXEC dbo.Audit @Id",
                "EXEC tSQLt.TableToText @result OUT, '#DoesExist', '', NULL",
                "EXEC @rc = sp_executesql @sql, N'@cnt INT OUTPUT', @cnt = @n OUTPUT",
                "EXECUTE @proc @a = 1, @b = DEFAULT WITH RECOMPILE",
                "EXEC (@sql + N' WHERE 1 = 1') AT Linked",
                "EXECUTE AS USER = 'bob'",
                "DBCC FREEPROCCACHE",
                "DBCC CHECKDB('db') WITH NO_INFOMSGS, ALL_ERRORMSGS",
                "BACKUP LOG db TO DISK = @File, DISK = @Stripe WITH INIT, NAME = 'log'",
                "BACKUP DATABASE db TO DISK=@a MIRROR TO DISK=@b WITH COPY_ONLY,INIT",
                "RESTORE VERIFYONLY FROM DISK=@File",
                "RESTORE DATABASE [db] WITH RECOVERY",
                "ALTER DATABASE db SET RECOVERY FULL",
                "ALTER DATABASE [db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE",
                "CREATE DATABASE db COLLATE Latin1_General_CS_AS WITH TRUSTWORTHY OFF",
                "ALTER ASSEMBLY a WITH PERMISSION_SET = SAFE",
                "CREATE ASSEMBLY [a] AUTHORIZATION [dbo] FROM 0x4D5A WITH PERMISSION_SET = SAFE",
                "CREATE USER u WITHOUT LOGIN",
                "CREATE USER [u] FROM LOGIN [l]",
                "CREATE LOGIN l FROM CERTIFICATE c",
                "CREATE LOGIN [l] WITH PASSWORD = 0x0100 HASHED, CHECK_POLICY = OFF",
                "ALTER LOGIN l DISABLE",
                "UPDATE STATISTICS #t WITH ROWCOUNT = 10, PAGECOUNT = 2",
                "ALTER INDEX ix ON dbo.t DISABLE",
                "CREATE MASTER KEY ENCRYPTION BY PASSWORD = 'x'",
                "OPEN MASTER KEY DECRYPTION BY PASSWORD = 'x'",
                "CREATE DATABASE SCOPED CREDENTIAL [c] WITH IDENTITY = 'i', SECRET = N's'",
                "CREATE XML SCHEMA COLLECTION s.c AS N''",
                "ALTER AUTHORIZATION ON DATABASE::[db] TO [sa]",
                "ALTER SERVER ROLE [sysadmin] ADD MEMBER [u]",
            ],
            &[
                "EXEC",
                "EXEC dbo.p @a = ",
                "EXEC dbo.p @a 1",
                "EXEC dbo.CreateExp (@n, 'x', @re OUT)",
                "EXECUTE AS NOBODY",
                "DBCC",
                "DBCC CHECKDB('db' WITH",
                "BACKUP db TO DISK = 'x'",
                "BACKUP DATABASE db",
                "ALTER DATABASE db SET",
                "ALTER DATABASE db RECOVERY FULL",
                "ALTER ASSEMBLY a WITH PERMISSION_SET = WILD",
                "CREATE USER u WITH LOGIN",
                "CREATE LOGIN <login_name, sysname, x> FROM WINDOWS",
                "CREATE LOGIN l WITH PASSWORD",
                "UPDATE STATISTICS",
                "ALTER INDEX ix ON t EXPLODE",
                "ALTER SERVER ROLE r ADD u",
            ],
        );
    }

    #[test]
    fn permissions_drops_types_synonyms() {
        check(
            Dialect::MsSql,
            &[
                "GRANT VIEW DEFINITION ON dbo.t TO reader",
                "GRANT VIEW SERVER STATE TO u",
                "GRANT IMPERSONATE ON LOGIN::[l] TO [u]",
                "REVOKE SELECT, UPDATE (a, b) ON dbo.t FROM u, v CASCADE",
                "DENY EXECUTE ON OBJECT::dbo.p TO u",
                "DROP SYNONYM dbo.s",
                "DROP LOGIN l",
                "DROP TYPE IF EXISTS s.t, s.u",
                "DROP ASSEMBLY a",
                "CREATE TYPE s.T1 AS TABLE(A INT, B VARCHAR(10) NOT NULL)",
                "CREATE TYPE s.Name FROM NVARCHAR(256) NOT NULL",
                "CREATE TYPE s.P EXTERNAL NAME asm.[ns.Cls]",
                "CREATE SYNONYM s.a FOR s.b",
            ],
            &[
                "GRANT TO u",
                "GRANT SELECT ON TO u",
                "GRANT SELECT ON t",
                "GRANT IMPERSONATE ON LOGIN::[l] TO <login_name, sysname, x>",
                "DROP PROCEDURE",
                "DROP WHATEVER x",
                "DROP TYPE a b",
                "CREATE TYPE s.T1 AS TABLE(A INT,)",
                "CREATE TYPE s.T1 FROM",
                "CREATE SYNONYM a b",
            ],
        );
        check(
            Dialect::Oracle,
            &[
                "CREATE OR REPLACE PUBLIC SYNONYM ut FOR ut3.ut",
                "DROP PACKAGE BODY billing",
                "DROP TYPE film_t FORCE",
                "CREATE TYPE actor_t AS OBJECT (actor_id NUMERIC, first_name VARCHAR(45))",
                "CREATE TYPE actors_t AS TABLE OF actor_t",
            ],
            &["CREATE PUBLIC SYNONYM ut", "DROP PACKAGE", "CREATE TYPE t AS OBJECT (a)", "CREATE TYPE t AS TABLE OF"],
        );
    }

    #[test]
    fn tables() {
        check(
            Dialect::MsSql,
            &[
                "ALTER TABLE store ALTER COLUMN manager_staff_id TINYINT NOT NULL",
                "ALTER TABLE db.dbo.t ALTER COLUMN c bit NULL",
                "ALTER TABLE t ADD CONSTRAINT [DF] DEFAULT (getdate()) FOR last_update",
                "ALTER TABLE t WITH CHECK ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (a) ON DELETE CASCADE",
                "ALTER TABLE t CHECK CONSTRAINT fk",
                "ALTER TABLE t DISABLE TRIGGER ALL",
            ],
            &[
                "ALTER TABLE t ADD c INT",
                "ALTER TABLE t ADD CONSTRAINT df DEFAULT FOR c",
                "ALTER TABLE t ADD CONSTRAINT df DEFAULT (getdate() FOR c",
                "ALTER TABLE t WITH CHECK ADD CONSTRAINT fk FOREIGN KEY REFERENCES u",
                "ALTER TABLE t ALTER COLUMN",
                "ALTER TABLE t FROBNICATE",
            ],
        );
        check(
            Dialect::Postgres,
            &[
                "ALTER TABLE s.t ALTER COLUMN a SET STATISTICS 1000, ALTER COLUMN b SET STATISTICS 0",
                "ALTER TABLE t SET SCHEMA s",
                "ALTER TABLE ONLY t OWNER TO postgres",
                "ALTER TABLE t ALTER CONSTRAINT fk DEFERRABLE INITIALLY DEFERRED",
                "ALTER TABLE t ENABLE ROW LEVEL SECURITY",
            ],
            &[
                "ALTER TABLE t SET SCHEMA",
                "ALTER TABLE t ALTER COLUMN a SET STATISTICS many",
                "ALTER TABLE t OWNER postgres",
            ],
        );
    }

    #[test]
    fn postgres() {
        check(
            Dialect::Postgres,
            &[
                "DO $$ BEGIN PERFORM f(); END $$",
                "DO LANGUAGE plpgsql $x$ BEGIN NULL; END $x$;",
                "CREATE PROCEDURE s.p(p_wait int DEFAULT 0, p_name text DEFAULT NULL, INOUT r numeric(10,2) = 1.5) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$",
                "CREATE OR REPLACE FUNCTION f(a int[], VARIADIC b text[]) RETURNS SETOF s.t LANGUAGE sql STABLE STRICT SECURITY DEFINER SET search_path = s, pg_temp AS $$ SELECT 1 $$",
                "CREATE FUNCTION f() RETURNS TABLE (a int, b text) AS 'select 1, 2' LANGUAGE sql COST 10 ROWS 5 PARALLEL SAFE",
                "CREATE FUNCTION f(internal) RETURNS trigger AS 'MODULE_PATHNAME', 'f_c' LANGUAGE C",
                "ALTER EXTENSION pgmq UPDATE TO '1.13.1'",
                "ALTER EXTENSION pgtap ADD FUNCTION plan(integer)",
                "ALTER EXTENSION e DROP MATERIALIZED VIEW s.v",
                "ALTER TYPE s.t ADD ATTRIBUTE headers JSONB",
                "ALTER TYPE s.t RENAME ATTRIBUTE a TO b CASCADE",
                "ALTER TYPE public.mpaa_rating OWNER TO postgres",
                "CREATE PUBLICATION p FOR TABLE s.t, ONLY s.u",
                "CREATE PUBLICATION p FOR ALL TABLES",
                "ALTER INDEX s.i SET TABLESPACE fast",
                "CREATE OR REPLACE PROCEDURAL LANGUAGE plpgsql",
                "ALTER PROCEDURAL LANGUAGE plpgsql OWNER TO postgres",
            ],
            &[
                "DO",
                "DO $$ x $$ LANGUAGE",
                "DO BEGIN NULL; END",
                "CREATE PROCEDURE p(a int DEFAULT) AS $$ x $$",
                "CREATE PROCEDURE p(a int) LANGUAGE plpgsql",
                "CREATE FUNCTION f(a int) RETURNS int AS BEGIN RETURN 1; END",
                "CREATE FUNCTION f(a int) RETURNS int SOMETIMES AS $$ x $$",
                "CREATE FUNCTION f(a int RETURNS int AS $$ x $$",
                "ALTER EXTENSION e UPDATE TO",
                "ALTER EXTENSION e ADD THING x",
                "ALTER EXTENSION e ADD FUNCTION f(",
                "ALTER TYPE t ADD ATTRIBUTE",
                "ALTER TYPE t EXPLODE",
                "CREATE PUBLICATION p FOR TABLE",
                "ALTER PROCEDURAL LANGUAGE plpgsql OWNER postgres",
            ],
        );
    }

    #[test]
    fn loads() {
        check(
            Dialect::MySql,
            &[
                "LOAD DATA LOCAL INFILE 'a.csv' INTO TABLE t FIELDS TERMINATED BY ',' ESCAPED BY '\\\\' OPTIONALLY ENCLOSED BY '\"' LINES TERMINATED BY '\\n' IGNORE 1 LINES (@a, b) SET a = trim(@a), c = IF(@a = '', NULL, @a)",
                "LOAD DATA INFILE 'a.csv' REPLACE INTO TABLE db.t",
            ],
            &[
                "LOAD DATA LOCAL INFILE INTO TABLE t",
                "LOAD DATA LOCAL INFILE 'a.csv' INTO t",
                "LOAD DATA LOCAL INFILE 'a.csv' INTO TABLE t FIELDS",
                "LOAD DATA LOCAL INFILE 'a.csv' INTO TABLE t SET a = ",
            ],
        );
        check(
            Dialect::Generic,
            &["COPY 134 OFFSET 2 RECORDS INTO s.t FROM '/x.csv' USING DELIMITERS ',','\\n','\"' NULL AS ''"],
            &["COPY 134 OFFSET RECORDS INTO t FROM 'x'", "COPY 1 RECORDS INTO t"],
        );
        check(
            Dialect::DuckDb,
            &["EXPORT DATABASE 'out'", "EXPORT DATABASE 'out' (FORMAT parquet)", "IMPORT DATABASE 'out'"],
            &["EXPORT DATABASE", "IMPORT DATABASE out dir"],
        );
    }
}
