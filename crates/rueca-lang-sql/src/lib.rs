//! SQL language pack for Rueca: tables, columns, views, routines, triggers,
//! packages, indexes, types and sequences, and the queries, routines and
//! migrations that use them, in PostgreSQL, MySQL / MariaDB, SQLite, T-SQL
//! (SQL Server), Oracle (SQL and PL/SQL), DB2, BigQuery and Snowflake
//! (Redshift, DuckDB, ClickHouse, Hive and Databricks read with their
//! sqlparser dialects), plus dbt Jinja models and macros. See
//! `rueca_extract::LanguagePack` for the contract.
//!
//! Parser: [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs)
//! 0.64 (Apache-2.0, pure Rust) — a tokenizer and a parser per dialect,
//! with source spans on identifiers (docs/decisions.md records why it was
//! chosen over tree-sitter SQL grammars, with the measured failure rates).
//! It parses one statement at a time and has no grammar for most
//! procedural code, so the pack cuts the statements and reads the
//! procedural structure itself (*Statements and recovery*, *Procedural
//! code*); queries, DML and expressions always go through sqlparser.
//!
//! Files: `.sql`, `.psql`, `.pgsql`, `.plpgsql`, `.mysql`, `.tsql`, `.bq`,
//! and Oracle's `.pks`, `.pkb`, `.pck`, `.pls`, `.plsql`, `.tps`, `.tpb`
//! ([`Language::Sql`]).
//!
//! # Dialects
//!
//! One dialect per file, chosen deterministically (`dialect` module):
//!
//! 1. a directive comment in the first 20 lines — `-- dialect: postgres`,
//!    sqlfluff's `-- sqlfluff:dialect:tsql`;
//! 2. the path: the extension (`.psql` / `.pgsql` / `.plpgsql` →
//!    PostgreSQL, `.mysql`, `.tsql`, `.bq`, `.pks` / `.pkb` / `.pck` /
//!    `.pls` / `.plsql` / `.tps` / `.tpb` → Oracle), then a dialect word in
//!    the file name, then in the directories, nearest first (`postgres`,
//!    `pg`, `mysql`, `mariadb`, `sqlite`, `sqlserver` / `sql-server` /
//!    `mssql` / `tsql`, `oracle` / `plsql`, `db2`, `bigquery` / `bq`,
//!    `snowflake`, `redshift`, `duckdb`, `clickhouse`, `databricks`);
//! 3. the content (first 64 KiB): weighted signals — `GO` lines,
//!    `NVARCHAR(`, `@@ROWCOUNT`, `[a].[b]` → T-SQL; `AUTO_INCREMENT`,
//!    `ENGINE=`, `DELIMITER`, backticked names → MySQL; `SERIAL`, `$$`,
//!    `::`, `plpgsql`, `JSONB` → PostgreSQL; `AUTOINCREMENT`, `PRAGMA`,
//!    `WITHOUT ROWID` → SQLite; `VARCHAR2`, `/` lines, `END;` before `/`,
//!    `DBMS_`, `NVL(`, `FROM DUAL`, `%TYPE`, `PROMPT`, `@@file`, `WHENEVER
//!    SQLERROR` → Oracle; `--#SET TERMINATOR`, `ORGANIZE BY`, `SYSIBM.`,
//!    `BEGIN ATOMIC`, `MODE DB2SQL` → DB2; `` `project.dataset.table` ``,
//!    `STRUCT<`, `INT64` → BigQuery; `VARIANT`, `FLATTEN(`, `IFF(` →
//!    Snowflake;
//! 4. else ANSI (sqlparser's generic dialect).
//!
//! | Dialect | sqlparser dialect | Read by the pack itself |
//! |---|---|---|
//! | PostgreSQL (`postgres`), Redshift, DuckDB | own | psql commands and variables, `DO` blocks, routines with a text body sqlparser rejects, extension scripts (`@extschema@`) |
//! | MySQL / MariaDB (`mysql`) | MySQL | `DELIMITER`, routine and trigger bodies |
//! | SQLite (`sqlite`) | SQLite | dot commands |
//! | T-SQL (`tsql`) | MsSql | `GO` batches, sqlcmd commands and `$(variables)`, statement boundaries without `;`, routines, control flow |
//! | Oracle (`oracle`) | Oracle | SQL*Plus commands, `/` buffers, `&substitution` variables, conditional compilation, PL/SQL packages, routines, triggers, type bodies, anonymous blocks |
//! | DB2 (`db2`) | none: generic | `--#SET TERMINATOR`, SQL PL routines and triggers |
//! | BigQuery (`bigquery`) | BigQuery | scripts (`DECLARE`, `IF`, `FOR … DO`, `BEGIN … EXCEPTION`), procedures |
//! | Snowflake (`snowflake`), ClickHouse, Hive, Databricks | own | — |
//! | ANSI (`ansi`) | generic | as DB2 |
//!
//! There is no configuration setting: extraction is a pure function of
//! the path and the bytes (the facts cache key), and the store
//! configuration does not reach packs. A statement the file's dialect
//! rejects is retried in PostgreSQL, MySQL, T-SQL, SQLite, BigQuery,
//! Snowflake, ANSI and Oracle, in that order. The dialect is shown at the
//! end of every top-level definition's signature (`CREATE TABLE users ( --
//! postgres`).
//!
//! # Statements and recovery
//!
//! The file is masked and cut into statements before parsing (`prep` and
//! `split` modules). Client commands are blanked (psql `\…`, `COPY … FROM
//! stdin` data, sqlcmd `:r` / `:setvar`, SQLite `.read`, MySQL `SOURCE`,
//! SQL*Plus `SET` / `PROMPT` / `WHENEVER` / `@file` between statements);
//! the files they include (`\i`, `\ir`, `:r`, `.read`, `SOURCE`, `@`,
//! `@@`, `START`) are imports; client substitutions are rewritten in
//! place (`$(Name)`, `&name`, `@extschema@`, `:'name'`). T-SQL `GO`
//! lines, Oracle `/` lines, MySQL `DELIMITER` regions and DB2 `--#SET
//! TERMINATOR` cut batches; inside a batch a top-level `;` ends a
//! statement except inside `BEGIN … END` / `CASE … END` and inside a
//! script's `IF … END IF` / `FOR … END FOR` / `WHILE` / `LOOP` /
//! `REPEAT`; a T-SQL batch that creates a procedure, function, trigger or
//! view is one statement, and so is an Oracle PL/SQL unit up to its `/`.
//!
//! Each statement is then read (`read` module), in order:
//!
//! 1. **sqlparser**, in the file's dialect, then the others;
//! 2. the same after the **same-length repairs** of the `repair` module:
//!    constructs sqlparser rejects inside otherwise plain SQL (physical
//!    attributes, hints, `CLUSTERED`, `INCLUDING ALL`, Jinja placeholders
//!    that expand to clauses, …) are blanked or replaced by an equivalent
//!    of the same length, so positions do not move;
//! 3. **the pack's own readers**: procedural code (*Procedural code*
//!    below), and statements whose facts are all in their first words —
//!    session settings, permissions, transaction and cursor control,
//!    administration, `CREATE` / `ALTER` of objects that are not
//!    definitions of the model (`utility` module): the first words must
//!    match a known form and the parentheses balance; the token scanner
//!    gives their references. Queries, DML, tables, views and routine
//!    bodies are never accepted this way;
//! 4. else the statement is **one parse error** (the file is `partial`)
//!    and the token scanner (`scan` module) reads what it can: its
//!    `CREATE` definition (table columns from the column list), relation
//!    names after `FROM` / `JOIN` / `INTO` / `UPDATE` / `REFERENCES` /
//!    `ALTER TABLE` / `DROP TABLE` / `TRUNCATE`, and calls (`CALL` / `EXEC`
//!    / `PERFORM name`, `name(`).
//!
//! Inside procedural code the unit is the statement of the body: a
//! routine with one statement nothing parses has one parse error and
//! every other statement of it is read.
//!
//! Routine bodies sqlparser keeps as text — PostgreSQL `$$ … $$` / `'…'`
//! bodies (PL/pgSQL and SQL), `DO` blocks — are read by the token scanner
//! only: relation and call references, no column references, and no
//! parse errors counted inside them.
//!
//! # Procedural code
//!
//! - **T-SQL** (`tsql` module): statements need no terminator, so the
//!   reader walks the tokens of a batch: `BEGIN … END`, `TRY` / `CATCH`,
//!   `IF` / `ELSE`, `WHILE`, labels; `CREATE` / `ALTER PROCEDURE |
//!   FUNCTION | TRIGGER` give the definition from their head and their
//!   body is read with the routine as the enclosing definition; every
//!   other statement runs to the next word that starts a statement at
//!   parenthesis depth 0 and is parsed by sqlparser on its own;
//!   conditions are parsed as expressions.
//! - **`;`-terminated languages** (`block` module): Oracle PL/SQL
//!   (packages — a `Module` whose subprograms are `Function`s named
//!   `package.name`, declarations in a specification marked as such —
//!   procedures, functions, triggers, type bodies, anonymous blocks),
//!   MySQL and DB2 SQL PL routines and triggers, BigQuery scripts and
//!   procedures: declarations, `IF` / `CASE` / loops / `FORALL` /
//!   exception handlers are read here; queries and DML are parsed by
//!   sqlparser (the variable lists of `SELECT … INTO a, b` and `RETURNING
//!   … INTO` blanked first), assignments, conditions and `RETURN` values
//!   as expressions, procedure calls as call expressions.
//!
//! Not validated: parameter lists and characteristics of routine heads,
//! types of declared variables, `TYPE` / `SUBTYPE` / `PRAGMA`
//! declarations, cursor control (`OPEN c`, `FETCH … INTO`, `CLOSE`), and
//! the arguments of chained member calls (`a.b(x).c(y)`), which the token
//! scanner reads.
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
//! | Oracle `CREATE PACKAGE [BODY] p`, `CREATE TYPE BODY t` | `Module` | `p` / `s.p` |
//! | a procedure or function of a package or type body (and nested subprograms) | `Function`, child of the package; `is_declaration` in a specification | `p.f` |
//! | `CREATE INDEX i` | `Constant` | `i` |
//! | `CREATE SEQUENCE s` | `Variable` | `s` |
//! | `CREATE TYPE e AS ENUM (…)` | `Enum`, values `Constant` children | `e`, `e.value` |
//! | `CREATE TYPE` (other: T-SQL alias and table types, Oracle object and collection types), `CREATE DOMAIN` | `TypeAlias` | `t` |
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
//! of other languages is not read, and neither is dynamic SQL (`EXEC
//! (@sql)`, `EXECUTE IMMEDIATE`). Oracle conditional compilation keeps
//! every branch; SQL*Plus substitution variables that stand for keywords
//! (`create &kind synonym`) leave the statement unparsed. A schema written
//! through a placeholder (`{{ target.schema }}.t`, `@extschema@.f`,
//! `&&owner..t`) is dropped from the name. H2, Firebird, Teradata and
//! other dialects without a detection rule are read as ANSI.

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
pub use extract::{Stats, head_only_statements, stats, unparsed_statements};
pub use facts::MAX_DOC_CHARS;

/// The SQL pack. Stateless; construct with `SqlPack` or `SqlPack::default()`.
#[derive(Debug, Default)]
pub struct SqlPack;

impl LanguagePack for SqlPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Sql]
    }
    fn extractor_version(&self) -> u32 {
        2
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
