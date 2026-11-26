# rueca-lang-sql

SQL language pack for [Rueca](https://github.com/DevArKaDiA/rueca): tables,
columns, views, routines, triggers, indexes, types and sequences, and the
queries, routines and migrations that use them, in the same graph as the
code. Status: **`preview`** under RFC 0004 (see *Status*).

- Parser: [`sqlparser`](https://github.com/apache/datafusion-sqlparser-rs)
  0.64 (Apache-2.0, pure Rust), one statement at a time, with source spans
  on identifiers; a token scanner reads what no dialect parses.
- Files: `.sql`, `.psql`, `.pgsql`, `.plpgsql`, `.mysql`, `.tsql`, `.bq`.
- Dialects: PostgreSQL, MySQL / MariaDB, SQLite, T-SQL (SQL Server),
  BigQuery, Snowflake (Redshift and DuckDB through their sqlparser
  dialects), ANSI; dbt Jinja models and macros best effort.

The crate docs (`src/lib.rs`) are the reference: dialect selection,
statement splitting and recovery, names, definitions, references,
migrations, limits. Resolution is the `sql` module of `rueca-resolve`.

## What lands in the graph

| SQL | Node / edge |
|---|---|
| `CREATE [TEMP] TABLE`, `CREATE [MATERIALIZED] VIEW`, `CREATE VIRTUAL TABLE` | `Struct` (`users`, `billing.invoices`) with `Field` columns (`users.email`) |
| `CREATE FUNCTION` / `PROCEDURE` / `MACRO`, `CREATE TRIGGER` | `Function` (a trigger is qualified by its table: `film.last_updated`) |
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
| psql `\i` / `\ir`, sqlcmd `:r`, SQLite `.read`, MySQL `SOURCE` | `IMPORTS` |

The dialect is shown at the end of every top-level signature
(`CREATE TABLE users ( -- postgres`). Migrations: each migration file keeps
its own definitions; references resolve to the definition effective at
that point by migration order (Flyway `V1_2__x.sql`, timestamps,
`0003_x.up.sql`, `…/up.sql`, `…/migration.sql`); `ALTER TABLE … ADD
COLUMN` adds a column member attributed to its migration; `ALTER` / `DROP`
are writes from the migration file. The ordering lives in the resolver
(the model has no field for it); the file path carries it.

## Dialect coverage

Measured with `cargo run -p rueca-lang-sql --example dialects -- <tree>`
(statements no dialect parses, by the dialect the pack detects; files over
2 MiB are skipped as in indexing) on the bench corpus at its pinned
commits — jOOQ/sakila `e089a5b`, fivetran/dbt_hubspot v1.11.0, sqlc
v1.31.1 (calibration) — plus, for BigQuery and T-SQL procedural code, the
parser-evaluation corpus (GoogleCloudPlatform/bigquery-utils `d5bde4f`,
olahallengren/sql-server-maintenance-solution `4161757`). An unparsed
statement is still read by the token scanner (definitions, relations,
calls; no columns).

| Dialect | Support | Unparsed statements (bench corpus) | Files with one or more | Known unsupported constructs |
|---|---|---|---|---|
| PostgreSQL | fully parsed | 67 / 2,932 (2.3%) | 26 / 1,218 | `$$` routine bodies (PL/pgSQL and SQL) are kept as text by sqlparser and read by the token scanner (relations and calls, no columns); `CREATE PROCEDURAL LANGUAGE`, `ALTER TYPE … OWNER TO`, `ALTER … SET SCHEMA`, `EXECUTE PROCEDURE f('literal', …)` in triggers |
| MySQL / MariaDB | fully parsed except routines | 20 / 645 (3.1%) | 10 / 280 | `CREATE PROCEDURE` / `FUNCTION` with `BEGIN … END` bodies and characteristics (`READS SQL DATA`, `DETERMINISTIC`) are not parsed by sqlparser: header and body read by the scanner; `DELIMITER` handled by the splitter; `CHARACTER SET … COLLATE` column options |
| SQLite | fully parsed | 7 / 619 (1.1%) | 4 / 199 | FTS5 virtual-table options, `CHARACTER(n)` types |
| T-SQL (SQL Server) | parsed, procedural code best effort | 21 / 127 (16.5%); maintenance-solution 29 / 64 (45%) | 2 / 5; 9 / 9 | `GO` batches split; procedures without parentheses around parameters, `ADD CONSTRAINT … DEFAULT … FOR`, `NONCLUSTERED` indexes, large procedural bodies (dynamic SQL, cursors) fall back to the scanner |
| BigQuery | parsed, scripting best effort | none in the bench corpus; bigquery-utils 19 / 114 (16.7%) | 11 / 47 | scripting (`FOR … DO`, `EXCEPTION WHEN`, `DECLARE … DEFAULT`), legacy SQL `[project:dataset.table]` |
| Snowflake | parsed | 0 / 2 (dbt_hubspot); bigquery-utils 0 / 17 | 0 / 9 | JavaScript / Python UDF bodies are strings (not read); not measured at scale (no Snowflake corpus in the bench) |
| dbt Jinja (any warehouse) | best effort | 22 / 162 (13.6%, dbt_hubspot) | 22 / 331 | Jinja is masked, not rendered: macros that expand to clauses (window frames, column lists with commas, whole queries in a CTE) can leave a statement unparsed; `ref()` / `source()` / macro calls are read from the Jinja itself, so dependencies survive; only the first branch of `{% if %}` is parsed (`ref`s in the others still count) |
| ANSI (no dialect signal) | parsed with sqlparser's generic dialect | 0 / 431 (sqlc) | 0 / 680 | Oracle PL/SQL, DB2, H2 and other dialects without a sqlparser dialect are read as ANSI: 148 / 457 (32.4%) on Sakila's Oracle / DB2 / other copies |

The micro-benchmark (`bench/micro/sql`, 29 cases, every dialect) has 4
unparsed statements of 89, all MySQL routines and a SQLite column type.

## Status

`preview` under RFC 0004: no localization dataset has SQL instances (the
localization floors are pending, and missing results count as failing,
§6.2), files with parse errors exceed the 2% floor on the gated corpus,
and the structural gold is the bench's code-backed labeler (sqlglot)
rather than a compiler-grade oracle. Floors are unchanged; the scorecard
row is in `docs/bench/alpha-results.md`.

## Not done

SQL in string literals of other languages (a follow-up: the bridge from
code to tables), Liquibase XML changelogs, column references in `$$`
bodies and unparsed statements, search-path evaluation (`SET search_path`),
synonyms, `GRANT` / policies as edges, dbt `packages.yml` package identity
(no RFC 0006 package keys), psql `:var` variables.
