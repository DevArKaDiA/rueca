# rueca-lang-sql

SQL language pack for [Rueca](https://github.com/DevArKaDiA/rueca): tables,
columns, views, routines, triggers, indexes, types and sequences, and the
queries, routines and migrations that use them, in the same graph as the
code. Status: **`preview`** under RFC 0004 (see *Status*).

- Parser: [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs)
  0.64 (Apache-2.0, pure Rust), one statement at a time, with source spans
  on identifiers. The pack cuts the statements, repairs (same length) the
  constructs sqlparser rejects inside plain SQL, and reads procedural code
  itself — T-SQL batches and routines, Oracle PL/SQL, MySQL and DB2
  routines, BigQuery scripts — handing every query, DML statement and
  expression inside to sqlparser. A token scanner reads what nothing
  parses (one parse error per statement).
- Files: `.sql`, `.psql`, `.pgsql`, `.plpgsql`, `.mysql`, `.tsql`, `.bq`,
  Oracle `.pks`, `.pkb`, `.pck`, `.pls`, `.plsql`, `.tps`, `.tpb`.
- Dialects, one per file (directive comment → path → content signals →
  ANSI): PostgreSQL, MySQL / MariaDB, SQLite, T-SQL (SQL Server), Oracle
  (SQL, SQL*Plus scripts, PL/SQL), DB2, BigQuery, Snowflake; Redshift,
  DuckDB, ClickHouse, Hive and Databricks through their sqlparser
  dialects; ANSI; dbt Jinja models and macros best effort.

The crate docs (`src/lib.rs`) are the reference: dialect selection,
statement splitting and recovery, names, definitions, references,
migrations, limits. Resolution is the `sql` module of `rueca-resolve`.

## What lands in the graph

| SQL | Node / edge |
|---|---|
| `CREATE [TEMP] TABLE`, `CREATE [MATERIALIZED] VIEW`, `CREATE VIRTUAL TABLE` | `Struct` (`users`, `billing.invoices`) with `Field` columns (`users.email`) |
| `CREATE FUNCTION` / `PROCEDURE` / `MACRO`, `CREATE TRIGGER` | `Function` (a trigger is qualified by its table: `film.last_updated`) |
| Oracle `CREATE PACKAGE [BODY]` / `TYPE BODY` and their subprograms | `Module` (`billing`) with `Function` children (`billing.close_invoice`; declarations of a specification marked as such) |
| `CREATE INDEX` / `SEQUENCE` / `TYPE … AS ENUM` / `DOMAIN` | `Constant` / `Variable` / `Enum` (values `Constant`) / `TypeAlias` |
| CTEs of a query inside a definition | `Variable`, child of the definition |
| `-- name: GetAuthor :one` (sqlc, yesql, aiosql, HugSQL) | `Function` for the query |
| dbt model / `{% macro %}` / `{% snapshot %}` | `Struct` named after the file / `Function` / `Struct` |
| `FROM` / `JOIN` / subqueries, `REFERENCES`, trigger and index targets | `REFERENCES` to the table or view |
| `INSERT` / `UPDATE` / `DELETE` / `MERGE` / `ALTER` / `DROP` / `TRUNCATE` targets | `WRITES` to the table |
| `alias.col`, `schema.table.col`, `col` with one table in scope, `INSERT` column lists, `UPDATE … SET` | `REFERENCES` / `WRITES` to the column |
| `f(…)` (not built in), `FROM f(…)`, `CALL` / `EXEC` / `PERFORM`, trigger `EXECUTE FUNCTION` | `CALLS` |
| dbt `ref('m')` / `source('s', 't')`, macro calls | `REFERENCES` to the model / `CALLS` to the macro |
| dbt `env_var('X')` / `var('x')` | `READS` of `$env:X` / of the `vars.x` key of `dbt_project.yml` |
| psql `\i` / `\ir`, sqlcmd `:r`, SQLite `.read`, MySQL `SOURCE`, SQL*Plus `@` / `@@` / `START` | `IMPORTS` |

The dialect is shown at the end of every top-level signature
(`CREATE TABLE users ( -- postgres`). Migrations: each migration file keeps
its own definitions; references resolve to the definition effective at
that point by migration order (Flyway `V1_2__x.sql`, timestamps,
`0003_x.up.sql`, `…/up.sql`, `…/migration.sql`); `ALTER TABLE … ADD
COLUMN` adds a column member attributed to its migration; `ALTER` / `DROP`
are writes from the migration file. The ordering lives in the resolver
(the model has no field for it); the file path carries it.

## Dialect coverage

Measured with `cargo run --release -p rueca-lang-sql --example dialects --
<tree> [--files]` (per detected dialect: files, files with a parse error,
statements, statements with an error, parse errors, statements read by
the pack's own readers; files over 2 MiB are skipped as in indexing), and
`--example unparsed -- <tree> [--full] [--grep text]` for the statements
behind the errors. A parse error is a statement — or a statement of a
routine body — that sqlparser rejects in every dialect after the repairs
and that is not one of the pack's own forms; it is still read by the
token scanner (definitions, relations, calls; no columns).

Two rates are printed. **Strict** (the one `FileFacts::parse_errors` and
the health metric use): a statement is parsed when sqlparser accepts it
(as written or after a same-length repair) or one of the pack's grammars
reads every token of it (`forms` productions, the procedural readers); a
statement only recognized by its first words is an error. **Lenient**
leaves those head-only statements out. On the health corpus (every SQL
file of the gated repositories at their pinned commits, 3,419 files): 26
files with a strict error (0.76%), 23 lenient (0.67%); what is left is
listed in `docs/decisions.md`. By detected dialect:

| Dialect | Files | With a parse error (strict, the gated count) | Lenient |
|---|---|---|---|
| ANSI / dbt Jinja | 1,864 | 0 | 0 |
| PostgreSQL | 601 | 10 (1.66%) | 10 |
| BigQuery | 401 | 1 (0.25%) | 1 |
| T-SQL | 304 | 15 (4.93%) | 12 |
| Snowflake | 98 | 0 | 0 |
| DuckDB | 69 | 0 | 0 |
| MySQL | 50 | 0 | 0 |
| Redshift | 15 | 0 | 0 |
| Oracle | 11 | 0 | 0 |
| DB2 | 3 | 0 | 0 |
| SQLite | 3 | 0 | 0 |
| **All** | **3,419** | **26 (0.76%)** | **23 (0.67%)** |

T-SQL alone is above 2% there (deliberately broken experiment scripts,
Service Broker and certificate statements, build templates), and Oracle is
barely in that corpus: on utPLSQL, outside it, 158 of 483 files keep a
strict error (object type specifications with member functions have no
production yet). The second table below is the lenient rate on the wider
evaluation corpus of the first round.

Bench corpus at its pinned commits — jOOQ/sakila `e089a5b` (seven dialect
copies), fivetran/dbt_hubspot v1.11.0 — plus the parser-evaluation corpus
(not bench repositories; commits in `docs/decisions.md`): pg_partman,
pgmq, pgTAP (PostgreSQL), SQL Server First Responder Kit, DarlingData,
tSQLt, sql-server-maintenance-solution (T-SQL), utPLSQL, OraOpenSource
Logger (Oracle), bigquery-utils (BigQuery), dbt-utils,
dbt-project-evaluator, dbt-expectations, dbt_artifacts, elementary (dbt
Jinja). Pooled, by detected dialect:

| Dialect | Files | Files with a parse error | Statements | With an error | Read by the pack's own readers | What still fails |
|---|---|---|---|---|---|---|
| PostgreSQL | 413 | 1 (0.2%) | 17,656 | 2 | 965 (`DO` blocks, procedures with defaults, `ALTER EXTENSION`, …) | a string spanning `''` at a line start in pgTAP's `privs.sql`; `$$` bodies are text: scanner only, never counted |
| MySQL / MariaDB | 94 | 2 (2.1%) | 123 | 2 | 22 (routine bodies) | dbt_artifacts Jinja inside `cast(… as {{ type }})` chains |
| SQLite | 3 | 0 | 112 | 0 | 0 | — |
| T-SQL (SQL Server) | 404 | 26 (6.4%) | 5,118 | 46 (69 errors) | 2,518 | `SELECT … FOR XML` without `FROM`, `GROUPING SETS`, `VECTOR_SEARCH`, `xml.modify()`, CLR `::` method calls, unterminated comments and deliberately broken experiments (tSQLt `Experiments/`), sqlcmd templates |
| Oracle (SQL, SQL*Plus, PL/SQL) | 515 | 12 (2.3%) | 1,161 | 65 (68 errors) | 10,095 | substitution variables that stand for keywords (`create &kind synonym`, 53 statements of one file), `XMLELEMENT(NAME "x", …)`, `XMLTABLE … PASSING`, `XMLSERIALIZE` |
| DB2 | 4 | 0 | 111 | 0 | 15 (triggers) | not measured beyond Sakila |
| BigQuery | 79 | 4 (5.1%) | 114 | 5 | 14 (scripts) | templates with `<placeholders>`, a deliberate syntax error, `#legacySQL` |
| Snowflake | 39 | 0 | 35 | 0 | 2 | JavaScript / Python UDF bodies are strings (not read); not measured at scale |
| ANSI / dbt Jinja (no dialect signal) | 1,052 | 3 (0.3%) | 809 | 3 | 6 | a test that fails on purpose (dbt-utils `test_star_no_columns`), a template placeholder file |
| **All** | **2,603** | **48 (1.8%)** | **25,239** | **123 (149 errors)** | **13,637** | |

Before these readers the same trees gave 626 of 2,185 files (28.6%; the
Oracle unit extensions were not indexed). On the bench corpus alone:
Sakila 0 of 25 files (was 10), dbt_hubspot 0 of 335 (was 22), and the
calibration repository (sqlc `examples/`) 2 of 39 (a GoogleSQL `THEN
RETURN` clause and `STRING(MAX)`).

dbt Jinja is masked, not rendered: `ref()` / `source()` / macro calls are
read from the Jinja itself; the first branch of `{% if %}` is parsed
(`ref`s in the others still count); what separates loop iterations (`{%
if not loop.last %} union all {% endif %}`) is blanked; in a file under
`macros/` only the macro blocks are read.

## Status

`stable` under RFC 0004 since 2026-11-25: the status is computed by
`rueca-bench report` from the gated suites (localization, structure,
files with parse errors, latency) and written to
`docs/bench/alpha-results.md`; floors are unchanged. Two things the word
does not cover: the structural gold is the bench's code-backed labeler
(sqlglot), not a compiler-grade oracle, and the localization set is mined
by the bench from GitHub, not validated by people.
