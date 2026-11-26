# Extra SQL S3 gold repositories (proposal)

Goal: gated callees population ≥ 300 (today 281 on jOOQ/sakila + fivetran/dbt_hubspot; callers 349).
Chosen on a-priori criteria only — permissive licence, a pinned release (or commit), SQL as the
content, a dialect / shape the two gated repositories lack, and a tree where definitions depend on
each other. No arm was run on any of them; the counts below are the labeler's own
(`sqlgold.sql_call_graph` + `scip.questions`, manifest `min_lines`, via `scripts/count_s3.py`).

| Repository | Pin | Licence | Dialect / shape | `include` | `dialects` | Files parsed | Callables | Gold edges | Questions callers / callees |
|---|---|---|---|---|---|---|---|---|---|
| pgmq/pgmq | `v1.13.2` = `51468f6ba2afeb4a4a9a67e9a532f37b590a0929` | PostgreSQL | PostgreSQL extension written in PL/pgSQL: one install script, functions calling functions, tables | `["pgmq-extension/sql/pgmq.sql"]` | `{ "" = "postgres" }` | 101 | 81 | 168 | 29 / 50 |
| tSQLt-org/tSQLt | no release tag on GitHub; `main` @ `4a921d0dacfb1d66b3db124c58158c80e5e910e6` (2024-12-07) | Apache-2.0 (`Build/License.txt`; GitHub shows no licence) | T-SQL unit-test framework: ~135 files of procedures / functions / views calling each other, `GO` batches | `["Source/"]` | `{ "" = "tsql" }` | 281 | 197 | 196 | 110 / 107 |
| MIT-LCP/mimic-code | `v3.0.1` = `c7e07560dc847e32cbb0b2890213e8e7cbd8bc7e` | MIT | Analytical PostgreSQL: `CREATE TABLE … AS SELECT` concepts depending on other concepts (application shape) | `["mimic-iv/concepts_postgres/"]` | `{ "" = "postgres" }` | 469 | 82 | 187 | 40 / 33 |

Pooled with the current gated sets: callees 281 + 50 + 107 + 33 = **471**, callers 349 + 29 + 110 + 40 = **528**
(pgmq + tSQLt alone: 438 / 488).

Notes for the manifest entries
- pgmq: the labeler reads every `.sql` of the clone as a possible target (101 files); the 76 upgrade
  scripts `pgmq--X--Y.sql` redefine the same functions. Only `pgmq.sql` is in scope; a reference
  from it binds inside the file first (labeler rule "earlier in the same file"), so the upgrade
  scripts do not make edges ambiguous. Rueca indexes the whole tree: its answers pointing at an
  upgrade-script copy would count as false neighbours unless the S3 scope filter drops them (RFC 0004 §5 says neighbours are restricted to callables in scope; not checked in s3.py).
  pgmq is also a proposed gated S1 repository (allowed: gated S1 + gated S3).
- tSQLt: no GitHub release exists (only CI build tags from 2015); pin the commit, as jOOQ/sakila is. `Source/` only: `Tests*/`, `Experiments/`, `Examples/`, `Build/` out of scope (`Tests` is a test path anyway).
- mimic-code: `concepts_postgres/` is generated from the BigQuery `concepts/` **with sqlglot** (the labeler's own parser), so the gold parses it unusually well — a bias in the labeler's favour, not in Rueca's. `concepts_duckdb/` (65 callables, 39 / 28 questions with `"duckdb"`) is a near copy of the same graph and was left out to avoid counting it twice. Also a proposed gated S1 repository.

Evaluated and not proposed
| Repository | Pin | Why not |
|---|---|---|
| pgpartman/pg_partman | `v5.5.1` | labeler yields 50 callables, 22 edges, 6 / 14 questions: definitions are written against the `@extschema@` placeholder, which the labeler does not read as a schema |
| dbt-labs/dbt-utils | `1.4.1` | 116 macros but 32 edges (17 / 20): macros call each other through `adapter.dispatch`, which the labeler does not model |
| dbt-labs/dbt-project-evaluator | `v2.0.2` | models moved to `checks/` in 2.x; the labeler names dbt models by `models/` paths: 19 callables, 13 / 4 |
| BrentOzarULTD/SQL-Server-First-Responder-Kit | `20261114` | 2 callables: the kit creates each procedure with `EXEC('CREATE PROCEDURE …')` then `ALTER PROCEDURE`, which the labeler does not count as a definition |
| mysql/mysql-sys | — | GPL-2.0, archived (would have been the MySQL routines / views candidate; none permissive found) |
