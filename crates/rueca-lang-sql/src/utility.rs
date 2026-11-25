//! Statements known by their head alone (crate docs, *Statements and
//! recovery*): statement kinds sqlparser has no grammar for, no
//! production of [`crate::forms`] read in full, and whose facts are all
//! in their head — administration, objects that are not definitions of
//! the model (events, queues, certificates, …), definitions named by
//! their head.
//!
//! Matching here is **not parsing**: the first words match a known form
//! and the parentheses balance, nothing more. Such a statement gives the
//! references the token scanner reads (`ALTER TABLE t` writes `t`) and
//! counts as a parse error of the strict rate (`Reader::head_only`); the
//! lenient rate leaves it out. Queries, DML, tables, views and routines
//! never match.

use sqlparser::tokenizer::Token;

use crate::scan::Tok;

/// How the pack reads a statement sqlparser rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Own {
    /// Not one of the pack's forms: a parse error.
    No,
    /// References only.
    Refs,
    /// A definition named by the statement's head, and its references.
    Head,
    /// `DO $$ … $$`: references of the body (a byte range).
    Body(usize, usize),
    /// `CREATE [OR REPLACE] FUNCTION | PROCEDURE name(…) … AS $$ … $$`: the
    /// routine named by the head, and the references of its body kept as
    /// text (a byte range), as for the routines sqlparser parses.
    Routine(usize, usize),
}

/// Statements whose first word decides.
const SIMPLE: &[&str] = &[
    "USE",
    "GRANT",
    "REVOKE",
    "DENY",
    "COMMIT",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    "ABORT",
    "START",
    "LOCK",
    "UNLOCK",
    "VACUUM",
    "ANALYZE",
    "REINDEX",
    "CLUSTER",
    "MOVE",
    "CLOSE",
    "FETCH",
    "DEALLOCATE",
    "LISTEN",
    "NOTIFY",
    "UNLISTEN",
    "RESET",
    "DISCARD",
    "CHECKPOINT",
    "SECURITY",
    "REFRESH",
    "REASSIGN",
    "SHOW",
    "PRAGMA",
    "ATTACH",
    "DETACH",
    "FLUSH",
    "OPTIMIZE",
    "REPAIR",
    "KILL",
    "PURGE",
    "AUDIT",
    "NOAUDIT",
    "PRINT",
    "RAISERROR",
    "THROW",
    "GOTO",
    "BREAK",
    "CONTINUE",
    "WAITFOR",
    "DBCC",
    "RECONFIGURE",
    "BACKUP",
    "RESTORE",
    "REVERT",
    "OPEN",
    "SAVE",
    "SHUTDOWN",
    "BULK",
    "CONNECT",
    "DISCONNECT",
    "COMMENT",
    "EXEC",
    "EXECUTE",
    "HANDLER",
    "XA",
    "INSTALL",
    "UNINSTALL",
    "CHANGE",
    "PUT",
    "GET",
    "LIST",
    "REMOVE",
    "UNDROP",
    "TRUNCATE",
    "SIGNAL",
    "RESIGNAL",
    "LEAVE",
    "ITERATE",
    "RAISE",
    "NULL",
    "EXIT",
    "PIPE",
    "RUNSTATS",
    "REORG",
    "TERMINATE",
    "QUIT",
    "SEND",
    "RECEIVE",
];

/// Objects of `CREATE` / `ALTER` that are not definitions of the model.
const NOT_DEFINITIONS: &[&str] = &[
    "DATABASE",
    "SCHEMA",
    "EXTENSION",
    "PUBLICATION",
    "SUBSCRIPTION",
    "CAST",
    "OPERATOR",
    "AGGREGATE",
    "RULE",
    "EVENT",
    "FOREIGN",
    "SERVER",
    "USER",
    "ROLE",
    "GROUP",
    "LOGIN",
    "TABLESPACE",
    "STATISTICS",
    "COLLATION",
    "CONVERSION",
    "TEXT",
    "LANGUAGE",
    "PROCEDURAL",
    "TRUSTED",
    "ACCESS",
    "TRANSFORM",
    "POLICY",
    "ASSEMBLY",
    "CERTIFICATE",
    "MASTER",
    "SYMMETRIC",
    "ASYMMETRIC",
    "CREDENTIAL",
    "ENDPOINT",
    "FULLTEXT",
    "PARTITION",
    "QUEUE",
    "SERVICE",
    "MESSAGE",
    "CONTRACT",
    "ROUTE",
    "XML",
    "AUTHORIZATION",
    "AVAILABILITY",
    "RESOURCE",
    "WORKLOAD",
    "SECURITY",
    "APPLICATION",
    "DEFAULT",
    "SYSTEM",
    "SESSION",
    "DIRECTORY",
    "CONTEXT",
    "PROFILE",
    "WAREHOUSE",
    "STAGE",
    "PIPE",
    "STREAM",
    "FILE",
    "INTEGRATION",
    "SHARE",
    "NETWORK",
    "ACCOUNT",
    "CONNECTION",
    "CAPACITY",
    "RESERVATION",
    "ASSIGNMENT",
    "ROW",
    "MASKING",
    "TAG",
    "SEARCH",
    "COLUMN",
    "BROKER",
    "REMOTE",
    "LOGFILE",
    "LARGE",
    "BUFFERPOOL",
    "NICKNAME",
    "WRAPPER",
    "ROLLBACK",
    "RESTORE",
    "FLASHBACK",
    "DISKGROUP",
    "PLUGGABLE",
    "EDITION",
    "OUTLINE",
    "DIMENSION",
    "CLUSTER",
    "CONTROLFILE",
    "PFILE",
    "SPFILE",
    "JAVA",
    "LIBRARY",
    "INDEXTYPE",
    "ANALYTIC",
    "AUDIT",
    "LOCKDOWN",
    "KEY",
];

/// Words between `CREATE` / `ALTER` and the object word.
const MODIFIERS: &[&str] = &[
    "OR",
    "REPLACE",
    "ALTER",
    "PUBLIC",
    "TEMP",
    "TEMPORARY",
    "UNIQUE",
    "GLOBAL",
    "LOCAL",
    "UNLOGGED",
    "CLUSTERED",
    "NONCLUSTERED",
    "COLUMNSTORE",
    "BITMAP",
    "FULLTEXT",
    "SPATIAL",
    "EDITIONABLE",
    "NONEDITIONABLE",
    "FORCE",
    "NO",
    "PRIMARY",
    "ONLINE",
    "OFFLINE",
    "CONCURRENTLY",
    "IF",
    "NOT",
    "EXISTS",
    "SHARED",
    "PRIVATE",
    "HASH",
    "VECTOR",
];

/// Parts of `ALTER TABLE t ADD …` that are not columns.
const NOT_COLUMNS: &[&str] = &[
    "CONSTRAINT",
    "PRIMARY",
    "UNIQUE",
    "FOREIGN",
    "CHECK",
    "INDEX",
    "KEY",
    "FULLTEXT",
    "SPATIAL",
    "PARTITION",
    "DEFAULT",
    "PERIOD",
    "SYSTEM",
    "SUPPLEMENTAL",
    "ROW",
];

/// How the pack reads the statement with significant tokens `toks`.
pub fn own(toks: &[Tok]) -> Own {
    let Some(first) = toks.first() else { return Own::No };
    if !balanced(toks) {
        return Own::No;
    }
    let w = first.word().to_ascii_uppercase();
    let is = |i: usize, word: &str| toks.get(i).is_some_and(|t| t.is(word));
    let has = |word: &str| toks.iter().any(|t| t.is(word));
    match w.as_str() {
        "DO" => do_body(toks).map_or(Own::No, |(s, e)| Own::Body(s, e)),
        "SET" => refs(!has("SELECT")),
        "BEGIN" | "END" => refs(transaction(toks)),
        "UPDATE" => refs(is(1, "STATISTICS")),
        "ENABLE" | "DISABLE" => refs(is(1, "TRIGGER")),
        "DROP" => Own::Refs,
        "RENAME" => refs(!has("SELECT")),
        "CREATE" | "ALTER" => definition(toks, w == "ALTER"),
        "DECLARE" => refs(!has("SELECT") || has("TABLE")),
        "RETURN" => refs(!has("SELECT")),
        _ if SIMPLE.contains(&w.as_str()) => Own::Refs,
        _ => Own::No,
    }
}

fn refs(yes: bool) -> Own {
    if yes { Own::Refs } else { Own::No }
}

/// `CREATE` / `ALTER` forms the pack reads itself.
fn definition(toks: &[Tok], alter: bool) -> Own {
    let Some(at) = (1..toks.len().min(10)).find(|&i| !MODIFIERS.iter().any(|m| toks[i].is(m))) else { return Own::No };
    let object = toks[at].word().to_ascii_uppercase();
    let has = |word: &str| toks.iter().any(|t| t.is(word));
    let is = |i: usize, word: &str| toks.get(i).is_some_and(|t| t.is(word));
    match object.as_str() {
        "EXTERNAL" => refs(!is(at + 1, "TABLE")),
        o if NOT_DEFINITIONS.contains(&o) => Own::Refs,
        "INDEX" | "SEQUENCE" | "SYNONYM" if !alter => Own::Head,
        "TYPE" if !alter => {
            if is_type_head(toks, at) {
                Own::Head
            } else {
                Own::No
            }
        }
        "TABLE" if alter => refs(!adds_column(toks, at)),
        "INDEX" | "SEQUENCE" | "TYPE" | "DOMAIN" | "MATERIALIZED" | "SYNONYM" | "PACKAGE" => refs(alter),
        "FUNCTION" | "PROCEDURE" if !alter && text_body(toks).is_some() => {
            let (s, e) = text_body(toks).unwrap_or_default();
            Own::Routine(s, e)
        }
        "FUNCTION" | "PROCEDURE" | "PROC" | "VIEW" | "TRIGGER" => {
            refs(alter && !has("AS") && !has("BEGIN") && !has("RETURN") && !has("IS"))
        }
        _ => Own::No,
    }
}

/// `CREATE TYPE t FROM base` (T-SQL alias type), `CREATE TYPE t AS OBJECT
/// (…)` / `AS TABLE OF x` / `AS VARRAY…` (Oracle), `AS TABLE (…)` (T-SQL
/// table type): the head names the type.
fn is_type_head(toks: &[Tok], at: usize) -> bool {
    let mut after = (at + 1..toks.len()).find(|&i| {
        !matches!(toks[i].token, Token::Period)
            && !(matches!(toks[i].token, Token::Word(_)) && (i == at + 1 || matches!(toks[i - 1].token, Token::Period)))
    });
    // Oracle `AUTHID CURRENT_USER | DEFINER` before `AS OBJECT`.
    if let Some(i) = after
        && toks[i].is("AUTHID")
    {
        after = (i + 2 < toks.len()).then_some(i + 2);
    }
    let Some(i) = after else { return true };
    let is = |i: usize, word: &str| toks.get(i).is_some_and(|t| t.is(word));
    is(i, "FROM")
        || is(i, "EXTERNAL")
        || is(i, "FORCE")
        || is(i, "UNDER")
        || ((is(i, "AS") || is(i, "IS"))
            && (is(i + 1, "OBJECT") || is(i + 1, "TABLE") || is(i + 1, "VARRAY") || is(i + 1, "VARYING")))
}

/// Whether `ALTER TABLE name ADD …` adds a column (it must be parsed).
fn adds_column(toks: &[Tok], table_at: usize) -> bool {
    let Some(add) = (table_at + 1..toks.len()).find(|&i| toks[i].is("ADD")) else { return false };
    // T-SQL `WITH CHECK ADD CONSTRAINT`, Oracle `ADD (CONSTRAINT …)`.
    let mut next = add + 1;
    if toks.get(next).is_some_and(|t| t.token == Token::LParen) {
        next += 1;
    }
    !toks.get(next).is_some_and(|t| NOT_COLUMNS.iter().any(|w| t.is(w)))
}

/// `BEGIN [TRAN | TRANSACTION | WORK | …]`, `END [TRANSACTION]`: a
/// transaction statement, not a block.
fn transaction(toks: &[Tok]) -> bool {
    const AFTER: &[&str] = &[
        "TRAN",
        "TRANSACTION",
        "WORK",
        "ISOLATION",
        "DEFERRED",
        "IMMEDIATE",
        "EXCLUSIVE",
        "DISTRIBUTED",
        "DIALOG",
        "CONVERSATION",
        "READ",
        "NOT",
        "DEFERRABLE",
    ];
    toks.len() == 1
        || (toks.len() <= 12 && AFTER.iter().any(|w| toks[1].is(w)))
        || toks[1].is("DIALOG")
        || toks[1].is("CONVERSATION")
}

/// The byte range inside the quotes of a routine body written as text:
/// the string right after an `AS` outside parentheses.
fn text_body(toks: &[Tok]) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        match t.token {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }
        if depth == 0 && t.is("AS") {
            return string_range(toks.get(i + 1)?);
        }
    }
    None
}

fn string_range(t: &Tok) -> Option<(usize, usize)> {
    match &t.token {
        Token::DollarQuotedString(s) => {
            let tag = s.tag.as_ref().map_or(0, String::len) + 2;
            Some((t.range.0 + tag, t.range.1.saturating_sub(tag).max(t.range.0 + tag)))
        }
        Token::SingleQuotedString(_) => Some((t.range.0 + 1, t.range.1.saturating_sub(1).max(t.range.0 + 1))),
        _ => None,
    }
}

/// The byte range inside the quotes of `DO [LANGUAGE x] $$ … $$`.
fn do_body(toks: &[Tok]) -> Option<(usize, usize)> {
    toks.iter().take(4).find_map(string_range)
}

fn balanced(toks: &[Tok]) -> bool {
    let mut depth = 0i32;
    for t in toks {
        match t.token {
            Token::LParen => depth += 1,
            Token::RParen => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::scan::tokens;

    fn kind(sql: &str, d: Dialect) -> Own {
        own(&tokens(sql, 0, sql.len(), d))
    }

    #[test]
    fn forms() {
        assert_eq!(kind("ALTER EXTENSION pgtap ADD FUNCTION f()", Dialect::Postgres), Own::Refs);
        assert_eq!(kind("CREATE PUBLICATION p FOR TABLE t", Dialect::Postgres), Own::Refs);
        assert_eq!(kind("ALTER TABLE t ALTER COLUMN c SET STATISTICS 100", Dialect::Postgres), Own::Refs);
        assert_eq!(kind("ALTER TABLE t ADD c int NOT NULL DEFAULT (1", Dialect::Postgres), Own::No);
        assert_eq!(kind("ALTER TABLE t ADD c int", Dialect::Postgres), Own::No);
        assert_eq!(
            kind("ALTER TABLE t WITH CHECK ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES b (a)", Dialect::MsSql),
            Own::Refs
        );
        assert_eq!(kind("SET STATISTICS TIME, IO OFF", Dialect::MsSql), Own::Refs);
        assert_eq!(kind("SET @a = (SELECT 1 FROM)", Dialect::MsSql), Own::No);
        assert_eq!(kind("BEGIN TRANSACTION", Dialect::MsSql), Own::Refs);
        assert_eq!(kind("BEGIN SELECT 1 END", Dialect::MsSql), Own::No);
        assert_eq!(kind("CREATE UNIQUE NONCLUSTERED INDEX i ON t (a)", Dialect::MsSql), Own::Head);
        assert_eq!(kind("CREATE TYPE t AS TABLE OF u", Dialect::Oracle), Own::Head);
        assert_eq!(kind("CREATE TABLE t (a int", Dialect::Oracle), Own::No);
        assert_eq!(kind("CREATE TABLE t AS SELEC 1", Dialect::Postgres), Own::No);
        assert_eq!(kind("SELECT 1 FROM", Dialect::Postgres), Own::No);
        assert!(matches!(kind("DO $$ BEGIN PERFORM f(); END $$", Dialect::Postgres), Own::Body(5, 29)));
        assert!(matches!(
            kind(
                "CREATE PROCEDURE p(a int DEFAULT 1) AS $$ BEGIN PERFORM f(); END $$ LANGUAGE plpgsql",
                Dialect::Postgres
            ),
            Own::Routine(41, 65)
        ));
        assert_eq!(kind("CREATE FUNCTION f(a int) RETURNS int AS BEGIN RETURN 1; END", Dialect::Postgres), Own::No);
    }
}
