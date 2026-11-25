//! SQL language pack for Rueca: tables, columns, views, routines, triggers,
//! indexes, types and sequences, and the queries, routines and migrations
//! that use them, in PostgreSQL, MySQL / MariaDB, SQLite, T-SQL (SQL
//! Server), BigQuery and Snowflake (Redshift and DuckDB read with their
//! sqlparser dialects), plus dbt Jinja models and macros. See
//! `rueca_extract::LanguagePack` for the contract.
//!
//! Parser: [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs)
//! 0.64 (Apache-2.0, pure Rust) — a tokenizer and a parser per dialect,
//! with source spans on identifiers (docs/decisions.md records why it was
//! chosen over tree-sitter SQL grammars, with the measured failure rates).
//! It parses one statement at a time; statements it rejects in every
//! dialect go to a token-level scanner.
//!
//! Files: `.sql`, `.psql`, `.pgsql`, `.plpgsql`, `.mysql`, `.tsql`, `.bq`
//! ([`Language::Sql`]).
//!
//! # Dialects
//!
//! One dialect per file, chosen deterministically (`dialect` module):
//!
//! 1. a directive comment in the first 20 lines — `-- dialect: postgres`,
//!    sqlfluff's `-- sqlfluff:dialect:tsql`;
//! 2. the path: the extension (`.psql` / `.pgsql` / `.plpgsql` →
//!    PostgreSQL, `.mysql`, `.tsql`, `.bq`), then a dialect word in the file
//!    name, then in the directories, nearest first (`postgres`, `pg`,
//!    `mysql`, `mariadb`, `sqlite`, `sqlserver` / `sql-server` / `mssql` /
//!    `tsql`, `bigquery` / `bq`, `snowflake`, `redshift`, `duckdb`);
//! 3. the content (first 64 KiB): weighted signals — `GO` lines,
//!    `NVARCHAR`, `[a].[b]` → T-SQL; `AUTO_INCREMENT`, `ENGINE=`,
//!    `DELIMITER`, backticked names → MySQL; `SERIAL`, `$$`, `::`,
//!    `plpgsql`, `JSONB` → PostgreSQL; `AUTOINCREMENT`, `PRAGMA`,
//!    `WITHOUT ROWID` → SQLite; `` `project.dataset.table` ``, `STRUCT<`,
//!    `INT64` → BigQuery; `VARIANT`, `FLATTEN(`, `IFF(` → Snowflake;
//! 4. else ANSI (sqlparser's generic dialect).
//!
//! There is no configuration setting: extraction is a pure function of
//! the path and the bytes (the facts cache key), and the store
//! configuration does not reach packs. A statement the file's dialect
//! rejects is retried in PostgreSQL, MySQL, T-SQL, SQLite, BigQuery,
//! Snowflake and ANSI, in that order. The dialect is shown at the end of
//! every top-level definition's signature (`CREATE TABLE users ( --
//! postgres`).
//!
//! # Statements and recovery
//!
//! The file is cut into statements before parsing (`split` module):
//! T-SQL `GO` lines and MySQL `DELIMITER` regions cut batches; inside a
//! batch a top-level `;` ends a statement except inside `BEGIN … END` /
//! `CASE … END`; a T-SQL batch that creates a procedure, function, trigger
//! or view is one statement. Client commands are masked first (psql `\…`,
//! `COPY … FROM stdin` data, sqlcmd `:r`, SQLite `.read`, MySQL `SOURCE`);
//! the file includes (`\i`, `\ir`, `:r`, `.read`, `SOURCE`) are imports.
//!
//! A statement no dialect parses is read by a token scanner (`scan`
//! module): its `CREATE` definition (table columns from the column list),
//! relation names after `FROM` / `JOIN` / `INTO` / `UPDATE` /
//! `REFERENCES` / `ALTER TABLE` / `DROP TABLE` / `TRUNCATE`, and calls
//! (`CALL` / `EXEC` / `PERFORM name`, `name(`). Each such statement counts
//! as one parse error (the file is `partial`). Routine bodies sqlparser
//! keeps as text — PostgreSQL `$$ … $$` / `'…'` bodies (PL/pgSQL and SQL)
//! — are read by the same scanner, so they give relation and call
//! references but no column references.
//!
//! # Names
//!
//! The module name is empty: a database namespace is global, so local
//! qualified names are global (`users`, `billing.invoices`,
//! `users.email`). Unquoted identifiers fold to lower case; quoted ones
//! keep their case in PostgreSQL, Snowflake, Redshift, DuckDB and ANSI
//! (a quoted all-upper-case Snowflake name equals the unquoted one) and
//! fold in MySQL, SQLite, T-SQL and BigQuery; `"x"`, `` `x` `` and `[x]`
//! are unquoted alike. A leading default schema is dropped (`public.`,
//! `dbo.`, `main.`).
//!
//! # Definitions
//!
//! Every definition is `exported`; tests by path (`rueca_model::is_test_path`).
//!
//! | Statement | Label | Qualified name |
//! |---|---|---|
//! | `CREATE [TEMP] TABLE t`, `CREATE VIRTUAL TABLE t`, `CREATE [MATERIALIZED] VIEW v` | `Struct` | `t` / `s.t` |
//! | a column (`CREATE TABLE`, `ALTER TABLE t ADD COLUMN c`) | `Field`, child of the table when the table is in the same file, else top level | `t.c` |
//! | `CREATE FUNCTION` / `PROCEDURE` / `PROC` / `MACRO` / `TRIGGER` | `Function` | `f` / `s.f` |
//! | `CREATE INDEX i` | `Constant` | `i` |
//! | `CREATE SEQUENCE s` | `Variable` | `s` |
//! | `CREATE TYPE e AS ENUM (…)` | `Enum`, values `Constant` children | `e`, `e.value` |
//! | `CREATE TYPE` (other), `CREATE DOMAIN` | `TypeAlias` | `t` |
//! | a CTE of a query inside a definition | `Variable`, child of it | `view.cte` |
//! | `-- name: GetAuthor :one` before a statement (sqlc, yesql, aiosql, HugSQL `-- :name`) | `Function` | `GetAuthor` |
//! | dbt model (file with `ref` / `source` / `config`, or Jinja under `models/`) | `Struct` spanning the file | file stem |
//! | `{% macro m(…) %}`, `{% test %}`, `{% materialization %}` | `Function` | `m` |
//! | `{% snapshot s %}` | `Struct` | `s` |
//!
//! Schemas, roles, grants, policies and extensions are not definitions.
//!
//! # References
//!
//! | Use | Reference | Enclosing |
//! |---|---|---|
//! | relation in `SELECT` / `JOIN` / subqueries, `CREATE INDEX … ON t`, trigger `ON t`, `REFERENCES t` | `Read` `t` (receiver = schema) | the definition the statement creates, else the enclosing model / named query, else the file |
//! | target of `INSERT` / `UPDATE` / `DELETE` / `MERGE` / `ALTER TABLE` / `DROP TABLE` / `TRUNCATE` | `Write` `t` | same |
//! | `alias.col`, `schema.table.col`, `col` when the `SELECT` (or `UPDATE` / `DELETE`) reads exactly one named table | `Read` `col`, receiver `$type:<table>` | same |
//! | `INSERT INTO t (a, b)`, `UPDATE t SET a = …` | `Write` `a`, receiver `$type:t` | same |
//! | `f(…)` not built in, `CALL p`, `EXEC p`, `EXECUTE FUNCTION f()`, trigger `EXECUTE PROCEDURE f()` | `Call` `f` | same |
//! | a CTE name | `Read` (resolved to the CTE) | only inside a definition |
//! | dbt `ref('m')` / `source('s', 't')` | `Read` `m` / `t` (receiver `s`) | the model or macro |
//! | Jinja macro call `m(…)` / `pkg.m(…)` | `Call` `m` (receiver `pkg`) | same |
//! | `env_var('X')` / `var('x')` | `EnvVar` `X` / `ConfigKey` `vars.x` | same |
//!
//! Column references inside `$$` bodies and unparsed statements are not
//! emitted. Columns of a query with several tables need a qualifier.
//!
//! # Migrations
//!
//! Each migration file contributes its own definitions; the resolver picks
//! among same-named tables by migration order (`rueca-resolve` `sql`
//! module). `ALTER` / `DROP` are `Write` references to the table, from the
//! migration file.
//!
//! # Limits
//!
//! Input over `Budget::max_bytes` is rejected (`TooLarge`); invalid UTF-8
//! is decoded lossily and counted as a parse error. SQL in string literals
//! of other languages is not read. Oracle PL/SQL, DB2 and other dialects
//! without a sqlparser dialect are read as ANSI.

mod ast;
mod block;
mod builtins;
mod dialect;
mod extract;
mod facts;
mod names;
mod prep;
mod read;
mod repair;
mod scan;
mod split;
mod text;
mod tsql;
mod utility;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use dialect::{Dialect, Source as DialectSource, detect as detect_dialect};
pub use extract::{Stats, stats, unparsed_statements};
pub use facts::MAX_DOC_CHARS;

/// The SQL pack. Stateless; construct with `SqlPack` or `SqlPack::default()`.
#[derive(Debug, Default)]
pub struct SqlPack;

impl LanguagePack for SqlPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Sql]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Sql {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract(path, source, budget)
    }
    /// Empty: a database namespace is global.
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        String::new()
    }
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        resolve_include(from_path, &import.module, project)
    }
    fn is_builtin(&self, name: &str) -> bool {
        builtins::is_function(&name.to_lowercase())
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }
    /// The dialect and dbt model names come from the path.
    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

/// A file a client command includes: relative to the including file's
/// directory, then to the repository root, then to each ancestor
/// directory (nearest first).
fn resolve_include(from_path: &str, module: &str, project: &ProjectFiles) -> Option<String> {
    let module = module.trim().replace('\\', "/");
    let module = module.trim_start_matches("./");
    if module.is_empty() || module.starts_with('/') {
        return None;
    }
    let dir = from_path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut candidates = vec![join(dir, module), module.to_string()];
    let mut d = dir;
    while let Some((parent, _)) = d.rsplit_once('/') {
        candidates.push(join(parent, module));
        d = parent;
    }
    candidates.into_iter().map(|c| normalize(&c)).find(|c| project.contains(c))
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() { rel.to_string() } else { format!("{dir}/{rel}") }
}

/// `a/./b/../c` → `a/c`.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests;
