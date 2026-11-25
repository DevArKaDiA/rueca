//! Dialect selection: an in-file directive, then the path, then the content
//! (crate docs, *Dialects*). Deterministic: the same path and bytes always
//! give the same dialect.

use sqlparser::dialect::{
    BigQueryDialect, ClickHouseDialect, DatabricksDialect, DuckDbDialect, GenericDialect, HiveDialect, MsSqlDialect,
    MySqlDialect, OracleDialect, PostgreSqlDialect, RedshiftSqlDialect, SQLiteDialect, SnowflakeDialect,
};

/// A SQL dialect the pack reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dialect {
    Postgres,
    MySql,
    Sqlite,
    MsSql,
    BigQuery,
    Snowflake,
    Redshift,
    DuckDb,
    /// Oracle SQL and PL/SQL (sqlparser's Oracle dialect for SQL; PL/SQL
    /// units are read by the pack).
    Oracle,
    /// IBM DB2 SQL and SQL PL: no sqlparser dialect, parsed as ANSI;
    /// routines and triggers are read by the pack.
    Db2,
    ClickHouse,
    Hive,
    /// Databricks / Spark SQL.
    Databricks,
    /// ANSI with the permissive extensions of sqlparser's generic dialect.
    Generic,
}

/// Order in which the other dialects are tried for a statement the file's
/// dialect rejects (after the file's own dialect).
const FALLBACK_ORDER: [Dialect; 8] = [
    Dialect::Postgres,
    Dialect::MySql,
    Dialect::MsSql,
    Dialect::Sqlite,
    Dialect::BigQuery,
    Dialect::Snowflake,
    Dialect::Generic,
    Dialect::Oracle,
];

impl Dialect {
    /// Lower-case name, as written in signatures and directives.
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Postgres => "postgres",
            Dialect::MySql => "mysql",
            Dialect::Sqlite => "sqlite",
            Dialect::MsSql => "tsql",
            Dialect::BigQuery => "bigquery",
            Dialect::Snowflake => "snowflake",
            Dialect::Redshift => "redshift",
            Dialect::DuckDb => "duckdb",
            Dialect::Oracle => "oracle",
            Dialect::Db2 => "db2",
            Dialect::ClickHouse => "clickhouse",
            Dialect::Hive => "hive",
            Dialect::Databricks => "databricks",
            Dialect::Generic => "ansi",
        }
    }

    /// Parses a dialect name as written in a directive or a path
    /// (`postgresql`, `pg`, `mariadb`, `mssql`, `sqlserver`, `bq`, …).
    pub fn parse(name: &str) -> Option<Dialect> {
        Some(match name.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" | "psql" | "pgsql" | "plpgsql" => Dialect::Postgres,
            "mysql" | "mariadb" => Dialect::MySql,
            "sqlite" | "sqlite3" => Dialect::Sqlite,
            "tsql" | "t-sql" | "mssql" | "sqlserver" | "sql_server" | "sql-server" => Dialect::MsSql,
            "bigquery" | "bq" => Dialect::BigQuery,
            "snowflake" => Dialect::Snowflake,
            "redshift" => Dialect::Redshift,
            "duckdb" => Dialect::DuckDb,
            "oracle" | "plsql" | "pl-sql" | "pl_sql" | "pls" | "pks" | "pkb" | "pck" | "tps" | "tpb" | "prc" | "fnc"
            | "trg" => Dialect::Oracle,
            "db2" | "db2luw" | "db2zos" => Dialect::Db2,
            "clickhouse" => Dialect::ClickHouse,
            "hive" | "hql" => Dialect::Hive,
            "databricks" | "spark" | "sparksql" => Dialect::Databricks,
            "ansi" | "generic" | "sql" => Dialect::Generic,
            _ => return None,
        })
    }

    /// The sqlparser dialect object.
    pub fn parser(self) -> Box<dyn sqlparser::dialect::Dialect> {
        match self {
            Dialect::Postgres => Box::new(PostgreSqlDialect {}),
            Dialect::MySql => Box::new(MySqlDialect {}),
            Dialect::Sqlite => Box::new(SQLiteDialect {}),
            Dialect::MsSql => Box::new(MsSqlDialect {}),
            Dialect::BigQuery => Box::new(BigQueryDialect {}),
            Dialect::Snowflake => Box::new(SnowflakeDialect {}),
            Dialect::Redshift => Box::new(RedshiftSqlDialect {}),
            Dialect::DuckDb => Box::new(DuckDbDialect {}),
            Dialect::Oracle => Box::new(OracleDialect {}),
            Dialect::ClickHouse => Box::new(ClickHouseDialect {}),
            Dialect::Hive => Box::new(HiveDialect {}),
            Dialect::Databricks => Box::new(DatabricksDialect {}),
            Dialect::Db2 | Dialect::Generic => Box::new(GenericDialect {}),
        }
    }

    /// The dialects tried for one statement: this one first, then the
    /// fallback order without repeats.
    pub fn attempts(self) -> Vec<Dialect> {
        let mut out = vec![self];
        out.extend(FALLBACK_ORDER.iter().copied().filter(|d| *d != self));
        out
    }

    /// Whether quoted identifiers compare case-insensitively too (MySQL
    /// table names on case-insensitive file systems, SQLite, SQL Server's
    /// default collations, BigQuery column names): the pack folds them.
    pub fn quoted_case_insensitive(self) -> bool {
        matches!(self, Dialect::MySql | Dialect::Sqlite | Dialect::MsSql | Dialect::BigQuery)
    }

    /// The schema an unqualified name lives in by default, dropped from
    /// qualified names (`public.users` = `users`).
    pub fn default_schema(self) -> Option<&'static str> {
        match self {
            Dialect::Postgres | Dialect::Redshift => Some("public"),
            Dialect::MsSql => Some("dbo"),
            Dialect::Sqlite => Some("main"),
            _ => None,
        }
    }
}

/// How the dialect of a file was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Directive,
    Path,
    Content,
    Default,
}

/// The dialect of the file at `path` with `text`, and how it was chosen.
pub fn detect(path: &str, text: &str) -> (Dialect, Source) {
    if let Some(d) = directive(text) {
        return (d, Source::Directive);
    }
    if let Some(d) = from_path(path) {
        return (d, Source::Path);
    }
    match from_content(text) {
        Some(d) => (d, Source::Content),
        None => (Dialect::Generic, Source::Default),
    }
}

/// A `dialect: <name>` / `dialect=<name>` comment in the first 20 lines
/// (`-- dialect: postgres`, sqlfluff's `-- sqlfluff:dialect:tsql`).
fn directive(text: &str) -> Option<Dialect> {
    text.lines().take(20).find_map(|line| {
        let line = line.trim_start();
        let body = line.strip_prefix("--").or_else(|| line.strip_prefix("/*")).or_else(|| line.strip_prefix('#'))?;
        let lower = body.to_ascii_lowercase();
        let at = lower.find("dialect")?;
        let rest = lower[at + "dialect".len()..].trim_start();
        let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('='))?.trim_start();
        let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        Dialect::parse(&word)
    })
}

/// Extension, then file name, then directories from the nearest outwards.
fn from_path(path: &str) -> Option<Dialect> {
    let lower = path.to_ascii_lowercase();
    let mut segs: Vec<&str> = lower.split('/').collect();
    let name = segs.pop().unwrap_or("");
    if let Some((stem, ext)) = name.rsplit_once('.') {
        if ext != "sql"
            && let Some(d) = Dialect::parse(ext)
        {
            return Some(d);
        }
        if let Some(d) = segment_dialect(stem) {
            return Some(d);
        }
    }
    segs.iter().rev().find_map(|s| segment_dialect(s))
}

/// Names of dialects as they appear in file and directory names
/// (`postgres-sakila-schema`, `Chinook_SqlServer`, `migrations/mysql`).
const PATH_WORDS: &[(&str, Dialect)] = &[
    ("postgres", Dialect::Postgres),
    ("pgsql", Dialect::Postgres),
    ("mysql", Dialect::MySql),
    ("mariadb", Dialect::MySql),
    ("sqlite", Dialect::Sqlite),
    ("sqlserver", Dialect::MsSql),
    ("sql-server", Dialect::MsSql),
    ("sql_server", Dialect::MsSql),
    ("mssql", Dialect::MsSql),
    ("tsql", Dialect::MsSql),
    ("t-sql", Dialect::MsSql),
    ("bigquery", Dialect::BigQuery),
    ("snowflake", Dialect::Snowflake),
    ("redshift", Dialect::Redshift),
    ("duckdb", Dialect::DuckDb),
    ("oracle", Dialect::Oracle),
    ("plsql", Dialect::Oracle),
    ("pl-sql", Dialect::Oracle),
    ("db2", Dialect::Db2),
    ("clickhouse", Dialect::ClickHouse),
    ("databricks", Dialect::Databricks),
    ("sparksql", Dialect::Databricks),
];

/// The dialect a path segment names: one of [`PATH_WORDS`] inside it, or
/// the whole segment `pg` / `bq`. The first word in the segment wins.
fn segment_dialect(seg: &str) -> Option<Dialect> {
    if matches!(seg, "pg" | "postgresql") {
        return Some(Dialect::Postgres);
    }
    if seg == "bq" {
        return Some(Dialect::BigQuery);
    }
    PATH_WORDS.iter().filter_map(|(w, d)| seg.find(w).map(|at| (at, *d))).min_by_key(|(at, _)| *at).map(|(_, d)| d)
}

/// Content signals per dialect: (dialect, case-insensitive needles, weight).
const SIGNALS: &[(Dialect, &[&str], u32)] = &[
    (
        Dialect::MsSql,
        &[
            "\ngo\n",
            "\ngo\r\n",
            "nvarchar(",
            "set nocount",
            "identity(",
            "@@rowcount",
            "@@error",
            "@@trancount",
            "@@identity",
            "@@version",
            "@@spid",
            "@@fetch_status",
            "@@procid",
            "exec ",
            "create proc ",
        ],
        3,
    ),
    (
        Dialect::MySql,
        &["auto_increment", "engine=", "engine =", "delimiter ", "unsigned", "on update current_timestamp"],
        3,
    ),
    (Dialect::Postgres, &["serial", "$$", "plpgsql", "jsonb", "create extension", "::", "returning ", "ilike"], 2),
    (Dialect::Sqlite, &["autoincrement", "pragma ", "without rowid"], 3),
    (Dialect::BigQuery, &["struct<", "array<", "int64", "float64", "safe_cast", "safe_divide", "`.`"], 3),
    (Dialect::Snowflake, &["variant", "flatten(", "iff(", "transient", "copy into", "parse_json"], 2),
    (
        Dialect::Oracle,
        &[
            "varchar2",
            "nvarchar2",
            "\n/\n",
            "\n/\r\n",
            "create or replace package",
            "pls_integer",
            "dbms_",
            "nvl(",
            " from dual",
            "sysdate",
            "execute immediate",
            "%rowtype",
            "%type",
            "raise_application_error",
            "whenever sqlerror",
            "set serveroutput",
            "\nprompt ",
            "sys_context(",
            "systimestamp",
            "show errors",
            "\n@@",
            "end;\n/",
            "end;\r\n/",
            "authid ",
        ],
        3,
    ),
    (
        Dialect::Db2,
        &[
            "--#set terminator",
            "organize by",
            "sysibm.",
            "syscat.",
            "language sql\n",
            "mode db2sql",
            "begin atomic",
            "fetch first 1 rows only",
            "current schema",
            "with ur",
        ],
        3,
    ),
];

/// The dialect whose signals weigh most in the first 64 KiB; ties go to
/// the earlier entry of [`SIGNALS`]; no signal: `None`. Backticks count
/// for MySQL unless a backticked name holds a `.` (BigQuery
/// `` `project.dataset.table` ``); `[name]` brackets count for T-SQL.
fn from_content(text: &str) -> Option<Dialect> {
    let end = floor_char_boundary(text, 64 * 1024);
    let lower = text[..end].to_ascii_lowercase();
    let mut best: Option<(u32, Dialect)> = None;
    for (d, needles, weight) in SIGNALS {
        let mut score: u32 = needles.iter().map(|n| lower.matches(n).count().min(20) as u32 * weight).sum();
        score += extra_signals(*d, &lower);
        if score > 0 && best.is_none_or(|(b, _)| score > b) {
            best = Some((score, *d));
        }
    }
    best.map(|(_, d)| d)
}

/// Backtick and bracket signals (see [`from_content`]).
fn extra_signals(d: Dialect, lower: &str) -> u32 {
    match d {
        Dialect::MySql => backticked(lower).filter(|s| !s.contains('.')).count().min(20) as u32,
        Dialect::BigQuery => backticked(lower).filter(|s| s.contains('.')).count().min(20) as u32 * 3,
        Dialect::MsSql => {
            lower.matches("].[").count().min(20) as u32 * 2 + lower.matches("[dbo]").count().min(20) as u32
        }
        _ => 0,
    }
}

/// The text between pairs of backticks on one line.
fn backticked(text: &str) -> impl Iterator<Item = &str> {
    text.lines().flat_map(|l| l.split('`').skip(1).step_by(2))
}

/// The largest char boundary ≤ `at`.
pub(crate) fn floor_char_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    (0..=at).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_path_content() {
        assert_eq!(detect("x.sql", "-- dialect: tsql\nselect 1"), (Dialect::MsSql, Source::Directive));
        assert_eq!(detect("x.sql", "-- sqlfluff:dialect:snowflake\n"), (Dialect::Snowflake, Source::Directive));
        assert_eq!(detect("db/app.pgsql", "select 1"), (Dialect::Postgres, Source::Path));
        assert_eq!(detect("a/postgres-sakila-db/schema.sql", ""), (Dialect::Postgres, Source::Path));
        assert_eq!(detect("Chinook_SqlServer.sql", ""), (Dialect::MsSql, Source::Path));
        assert_eq!(detect("migrations/mysql/1.sql", ""), (Dialect::MySql, Source::Path));
        assert_eq!(detect("q.bq", ""), (Dialect::BigQuery, Source::Path));
        assert_eq!(
            detect("x.sql", "create table t (id int auto_increment) engine=innodb;"),
            (Dialect::MySql, Source::Content)
        );
        assert_eq!(detect("x.sql", "create table t (id serial primary key);"), (Dialect::Postgres, Source::Content));
        assert_eq!(detect("x.sql", "create table [dbo].[t] (id int)\nGO\n"), (Dialect::MsSql, Source::Content));
        assert_eq!(detect("x.sql", "select * from `proj.ds.t`"), (Dialect::BigQuery, Source::Content));
        assert_eq!(detect("x.sql", "select * from `t`"), (Dialect::MySql, Source::Content));
        assert_eq!(detect("x.sql", "pragma foreign_keys = on;"), (Dialect::Sqlite, Source::Content));
        assert_eq!(detect("x.sql", "select 1"), (Dialect::Generic, Source::Default));
        assert_eq!(detect("oracle-sakila-db/schema.sql", ""), (Dialect::Oracle, Source::Path));
        assert_eq!(detect("db2-sakila-db/schema.sql", ""), (Dialect::Db2, Source::Path));
        assert_eq!(detect("source/core/ut_utils.pkb", ""), (Dialect::Oracle, Source::Path));
        assert_eq!(detect("x.sql", "-- dialect: plsql\nselect 1"), (Dialect::Oracle, Source::Directive));
        assert_eq!(
            detect("x.sql", "create table t (a varchar2(10));\nselect nvl(a, 'x') from dual;"),
            (Dialect::Oracle, Source::Content)
        );
        assert_eq!(
            detect("x.sql", "--#SET TERMINATOR @\ncreate table t (a int) organize by row@"),
            (Dialect::Db2, Source::Content)
        );
    }

    #[test]
    fn attempts_start_with_self() {
        let a = Dialect::Sqlite.attempts();
        assert_eq!(a[0], Dialect::Sqlite);
        assert_eq!(a.iter().filter(|d| **d == Dialect::Sqlite).count(), 1);
        assert_eq!(a.len(), 8);
        assert_eq!(Dialect::Redshift.attempts().len(), 9);
        assert_eq!(Dialect::Oracle.attempts().len(), 8);
    }
}
