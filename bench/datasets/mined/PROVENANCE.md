# Mined S1 sets for shell and SQL — provenance

Mined 2026-11-25 with `mining/scripts/mine_s1.py` (GitHub API through `gh api`, every response
cached under `mining/data/api`, so a re-run is offline and byte-identical). Only PRs merged before
`2026-11-25T00:00:00Z` (`CUTOFF`). No tool (Rueca, BM25, cbm) was run on any repository or instance:
repositories, rules and quotas were fixed from the counts in this file alone.

## Files

| File | Rows | sha256 |
|---|---|---|
| `rueca_mined_shell.jsonl` (every eligible pair of the 12 chosen repositories, 5.1 MB) | 796 | `ca0f1ab01431528cfdd36be42cd1355af04c78d2160d9d3953d484119cfb3bf9` |
| `rueca_mined_sql.jsonl` (every eligible pair of the 15 chosen repositories, 7.2 MB) | 897 | `db2f9e7b60957349ffa1ae56909de27837f3d183ad82ff9edfeac0684218c7a0` |
| `rueca_mined_shell_selected.jsonl` (only the 87 rows the quotas pick, 0.65 MB) | 87 | `e7fca8b0301da19c143725bacff64ae0ff8ce92c56a77a119a34a6daab8b0f04` |
| `rueca_mined_sql_selected.jsonl` (only the 93 rows the quotas pick, 0.86 MB) | 93 | `9b5cfe30b0adf008d61f0914892214c5e3aa6c9fe9fc926b879c56c80f0b66c9` |
| `s1_repos.toml` | proposed `[[s1.repos]]` entries | |
| `selected_shell.txt`, `selected_sql.txt` | `split repo instance_id` the quotas pick (`scripts/select_s1.py`: first `quota` by sha256(instance_id), as `datasets.select_instances`) | |
| `per_repo/<lang>/<owner>__<repo>.jsonl` + `.stats.json` | rows and per-rule counts of every candidate | |
| `funcgold_<lang>.json` | files / callables each selected patch touches (bench labelers) | |
| `s3_sql_candidates.md` | extra SQL S3 repositories | |

JSONL: one JSON object per `\n`-terminated line, ASCII-escaped (`ensure_ascii`), keys sorted, rows
sorted by `instance_id`. Read lines by `\n` only (a first version written with raw Unicode broke
`str.splitlines()` on U+2028 inside an issue body; hence the escaping). Columns: the SWE-bench ones
the harness uses — `instance_id` (`owner__repo-<PR number>`, repository case kept, like
`astral-sh__ruff-15543`), `repo`, `base_commit` (40-hex merge base of the PR), `patch` (the PR's
three-dot unified diff, binary files dropped, tests and docs kept as in Multi-SWE-bench's
`fix_patch`), `problem_statement` (title + `\n` + body of each closed issue, joined by a blank line;
no comments) — plus `language`, `pr_number`, `issue_numbers`, `merge_commit`, `pr_merged_at`,
`issue_created_at`, `license` (SPDX of the repository; two read from the licence file where GitHub
says NOASSERTION: First Responder Kit = MIT, pg_partman = PostgreSQL), `pr_url`, `gold_files`
(existing non-test language files of the patch by the miner's path rules; informative, the harness
recomputes gold).

## API queries

- GraphQL, per repository, 40 PRs per page, `orderBy: CREATED_AT ASC`:
  `repository.pullRequests(states: MERGED)` → `number title createdAt mergedAt url baseRefOid
  headRefOid changedFiles mergeCommit.oid closingIssuesReferences(first: 5){number title body
  createdAt author{login __typename} repository.nameWithOwner} files(first: 100){path changeType}`,
  and `repository.licenseInfo.spdxId`.
- REST, only for PRs that pass the text and path rules: `GET /repos/{r}/pulls/{n}` with
  `Accept: application/vnd.github.diff` (the patch), `GET /repos/{r}/compare/{baseRefOid}...{headRefOid}`
  (`merge_base_commit.sha` = `base_commit`), `GET /repos/{r}/commits/{base}` (reachability).
- `GET /repos/{r}` and `GET /repos/{r}/git/trees/{default branch}?recursive=1` for licence and file counts (`scripts/repo_facts.py`).
- For the selected instances, `scripts/funcgold.py` read every gold file with `git show <base>:<path>` from a blobless clone: all 180 base commits are fetchable.

## Screening rules (in order; a PR is counted at the first rule that drops it)

| Column | Rule |
|---|---|
| `closing` | merged PRs with ≥ 1 `closingIssuesReferences` issue of the same repository (the rest: `no_issue` = merged − closing) |
| `bot` | every closed issue opened by a bot (bot issues are removed from the statement otherwise) |
| `issue_after` | a closed issue was created at / after the PR |
| `short` | title + body prose < 100 characters once HTML comments, fenced code, markdown headings, check-box lines and bold-only label lines are stripped |
| `leak` | the issue text has a ```` ```diff ```` / ```` ```patch ```` fence, a `+++ b/` / `--- a/` line or an `@@ -n +n` hunk header (path mentions are kept, as in SWE-bench) |
| `truncated` | more than 100 changed files (file list incomplete) |
| `no_lang` | no existing (modified / renamed / deleted, not added) non-test, non-doc file of the language |
| `not_mostly` | language files ≤ 50% of the changed non-doc, non-test files |
| `too_many` | more than 10 language files changed |
| `no_diff` | diff or merge base not served |
| `too_big` | patch > 100 KB after dropping binary files |
| `no_lang_patch` | the unified diff itself has no existing language file |
| `unreachable` | base commit not served by the commits API |
| `dup_issue` | a PR merged earlier already closes one of the same issues (earliest merged wins) |

Path rules (`mine_s1.py`): language = the bench's extensions (`[languages.shell]`: `.sh .bash .zsh
.ksh .bats`; `[languages.sql]`: `.sql .psql .pgsql .plpgsql .mysql .tsql .bq`). Test = the
harness's `is_test_path` (segments `test tests __tests__ __mocks__ __fixtures__ e2e spec specs
testdata`, top-level `testing`, the two families' `test_files` patterns) **plus** `integration_tests`,
`integration-tests`, `fixtures`, `test_helper`, `t` (stricter than the harness, so no pair is kept
only for a dbt integration-test model). Doc / neutral = `.md .rst .txt .adoc`, images, `.html
.css`, licence / changelog / readme names, `docs/ doc/ .github/ .circleci/ man/`, CI config files;
for SQL also `.yml .yaml .csv` (dbt property files and seeds travel with every model change).

## Repository choice (a-priori criteria)

1. the language is the repository's content (not scripts around another code base);
2. permissive licence (MIT / Apache-2.0 / BSD / PostgreSQL / ISC); copyleft left out;
3. ≥ 5 eligible pairs (≥ 3 was the floor; every chosen one has ≥ 5);
4. multi-file: ≥ 20 non-test language files at HEAD, or at the instances' commits (asdf was bash until v0.16); repositories where one or a few files take every patch are rejected;
5. dialect variety across the gated set, and the same dialect families in calibration;
6. RFC 0004 §7.7: calibration repositories disjoint from every gated S1 / S3 repository; existing calibration S3 repositories (bats-core, sqlc) only as calibration; gated S3 repositories may be gated S1 (Bash-it/bash-it is);
7. quota ≤ 12 per repository, lower for repositories with few eligible pairs or one dominant dialect family (dbt), calibration 5 each.

### shell: chosen

| Repository | Licence | Dialect | Lang files at HEAD (non-test) | merged PRs | closing an issue | eligible | quota | split |
|---|---|---|---|---|---|---|---|---|
| ohmyzsh/ohmyzsh | MIT | zsh | 418 (407) | 3831 | 416 | 297 | 12 | gated |
| spaceship-prompt/spaceship-prompt | MIT | zsh | 121 (84) | 478 | 46 | 36 | 6 | gated |
| sorin-ionescu/prezto | MIT | zsh | 41 (41) | 380 | 44 | 21 | 5 | gated |
| Bash-it/bash-it | MIT | bash | 375 (339) | 1379 | 107 | 80 | 10 | gated |
| docker-mailserver/docker-mailserver | MIT | bash | 117 (45) | 1804 | 300 | 100 | 10 | gated |
| asdf-vm/asdf | MIT | bash (≤ v0.15; Go since) | 39 (5) | 904 | 200 | 79 | 8 | gated |
| sdkman/sdkman-cli | Apache-2.0 | bash | 26 (23) | 254 | 29 | 23 | 5 | gated |
| ChrisTitusTech/linutil | MIT | POSIX sh | 202 (202) | 680 | 146 | 73 | 8 | gated |
| shellspec/shellspec | MIT | POSIX sh (bash/ksh/zsh portable) | 253 (140) | 77 | 40 | 25 | 8 | gated |
| zdharma-continuum/zinit | MIT | zsh | 12 (12) | 383 | 45 | 33 | 5 | calibration |
| ohmybash/oh-my-bash | MIT | bash | 235 (234) | 216 | 32 | 21 | 5 | calibration |
| bats-core/bats-core | MIT | bash | 288 (15) | 449 | 124 | 8 | 5 | calibration |

### sql: chosen

| Repository | Licence | Dialect | Lang files at HEAD (non-test) | merged PRs | closing an issue | eligible | quota | split |
|---|---|---|---|---|---|---|---|---|
| dbt-labs/dbt-utils | Apache-2.0 | dbt Jinja (multi-adapter) | 116 (51) | 403 | 112 | 54 | 8 | gated |
| dbt-labs/dbt-project-evaluator | Apache-2.0 | dbt Jinja (multi-adapter) | 123 (116) | 276 | 114 | 73 | 8 | gated |
| dbt-labs/dbt-external-tables | Apache-2.0 | dbt Jinja (Snowflake/BigQuery/Redshift/Spark) | 47 (40) | 134 | 50 | 31 | 5 | gated |
| calogica/dbt-date | Apache-2.0 | dbt Jinja (multi-adapter) | 51 (42) | 64 | 14 | 9 | 4 | gated |
| brooklyn-data/dbt_artifacts | Apache-2.0 | dbt Jinja (Snowflake/BigQuery/Databricks) | 107 (88) | 242 | 44 | 14 | 4 | gated |
| get-select/dbt-snowflake-monitoring | MIT | dbt Jinja (Snowflake) | 33 (31) | 113 | 14 | 11 | 4 | gated |
| tuva-health/tuva-core | Apache-2.0 | dbt Jinja (Snowflake/BigQuery/Redshift/Databricks/DuckDB) | 472 (380) | 722 | 151 | 69 | 8 | gated |
| BrentOzarULTD/SQL-Server-First-Responder-Kit | MIT (LICENSE.md) | T-SQL | 34 (34) | 1660 | 615 | 538 | 12 | gated |
| pgmq/pgmq | PostgreSQL | PostgreSQL (PL/pgSQL) | 101 (88) | 374 | 63 | 19 | 6 | gated |
| pgpartman/pg_partman | PostgreSQL (LICENSE.txt) | PostgreSQL (PL/pgSQL) | 242 (146) | 77 | 9 | 6 | 6 | gated |
| MIT-LCP/mimic-code | MIT | BigQuery + PostgreSQL / DuckDB copies | 562 (562) | 314 | 20 | 13 | 8 | gated |
| GoogleCloudPlatform/bigquery-utils | Apache-2.0 | BigQuery | 66 (65) | 323 | 14 | 5 | 5 | gated |
| dbt-labs/dbt-codegen | Apache-2.0 | dbt Jinja | 48 (8) | 93 | 44 | 16 | 5 | calibration |
| erikdarlingdata/DarlingData | MIT | T-SQL | 84 (74) | 637 | 36 | 33 | 5 | calibration |
| NikolayS/postgres_dba | BSD-3-Clause | PostgreSQL (psql scripts) | 40 (40) | 47 | 7 | 6 | 5 | calibration |

Totals: shell 72 gated on 9 repositories (zsh 23, bash 33, POSIX sh 16) + 15 calibration on 3; SQL 78 gated on 12 repositories (dbt 41, T-SQL 12, PostgreSQL 12, BigQuery / PostgreSQL analytics 13) + 15 calibration on 3.

## Selected instances: what the patches touch

Bench labelers on the gold files at the base commit (`scripts/funcgold.py`, `scripts/summarize.py`). "harness labeler" = what `gold.compute_s1_gold` does today (SQL: `sql_functions` with the default `ansi` dialect); "dialect-aware" = the same labeler given the repository's dialect (dbt / tsql / postgres / bigquery).

### shell

| split | repository | instances | touch a callable (harness labeler) | (dialect-aware) | gold files | multi-file gold | issue names a gold file or its stem |
|---|---|---|---|---|---|---|---|
| gated | ohmyzsh/ohmyzsh | 12 | 9 | 9 | 14 | 1 | 8 |
| gated | spaceship-prompt/spaceship-prompt | 6 | 3 | 3 | 6 | 0 | 3 |
| gated | sorin-ionescu/prezto | 5 | 0 | 0 | 5 | 0 | 1 |
| gated | Bash-it/bash-it | 10 | 8 | 8 | 13 | 2 | 2 |
| gated | docker-mailserver/docker-mailserver | 10 | 9 | 9 | 21 | 5 | 7 |
| gated | asdf-vm/asdf | 8 | 6 | 6 | 9 | 1 | 5 |
| gated | sdkman/sdkman-cli | 5 | 5 | 5 | 14 | 3 | 0 |
| gated | ChrisTitusTech/linutil | 8 | 7 | 7 | 9 | 1 | 2 |
| gated | shellspec/shellspec | 8 | 6 | 6 | 25 | 6 | 6 |
| calibration | zdharma-continuum/zinit | 5 | 2 | 2 | 10 | 2 | 3 |
| calibration | ohmybash/oh-my-bash | 5 | 4 | 4 | 9 | 1 | 0 |
| calibration | bats-core/bats-core | 5 | 5 | 5 | 6 | 1 | 3 |

Gated: 72 instances, 53 touch a callable, 19 with more than one gold file, 34 whose issue names a gold file (or its stem). Calibration: 15 / 11 / 4 / 6.

### sql

| split | repository | instances | touch a callable (harness labeler) | (dialect-aware) | gold files | multi-file gold | issue names a gold file or its stem |
|---|---|---|---|---|---|---|---|
| gated | dbt-labs/dbt-utils | 8 | 7 | 7 | 9 | 1 | 8 |
| gated | dbt-labs/dbt-project-evaluator | 8 | 8 | 8 | 9 | 1 | 3 |
| gated | dbt-labs/dbt-external-tables | 5 | 5 | 5 | 5 | 0 | 1 |
| gated | calogica/dbt-date | 4 | 4 | 4 | 8 | 1 | 4 |
| gated | brooklyn-data/dbt_artifacts | 4 | 4 | 4 | 10 | 2 | 2 |
| gated | get-select/dbt-snowflake-monitoring | 4 | 4 | 4 | 9 | 3 | 2 |
| gated | tuva-health/tuva-core | 8 | 8 | 8 | 25 | 4 | 3 |
| gated | BrentOzarULTD/SQL-Server-First-Responder-Kit | 12 | 0 | 0 | 16 | 2 | 12 |
| gated | pgmq/pgmq | 6 | 2 | 5 | 9 | 2 | 4 |
| gated | pgpartman/pg_partman | 6 | 1 | 6 | 11 | 3 | 3 |
| gated | MIT-LCP/mimic-code | 8 | 2 | 2 | 14 | 3 | 8 |
| gated | GoogleCloudPlatform/bigquery-utils | 5 | 1 | 1 | 10 | 1 | 3 |
| calibration | dbt-labs/dbt-codegen | 5 | 5 | 5 | 5 | 0 | 3 |
| calibration | erikdarlingdata/DarlingData | 5 | 1 | 1 | 6 | 1 | 5 |
| calibration | NikolayS/postgres_dba | 5 | 0 | 0 | 5 | 0 | 2 |

Gated: 78 instances, 46 touch a callable with the harness labeler (54 dialect-aware), 23 multi-file, 53 whose issue names a gold file or its stem. Calibration: 15 / 6 / 1 / 10.

## Known biases and limitations

- **Linked issues only.** `closingIssuesReferences` exists when the PR (or a maintainer) linked the issue; repositories that fix by direct commit or cherry-pick (acme.sh: 14 of 1,695 merged PRs; zsh-users/*, prezto partly) are under-sampled, and linked issues skew to recent years and to maintainers who use closing keywords.
- **Issue quality is not human-validated** (no SWE-bench-Verified-style screen, no LLM screen). The text rules only remove empty / templated / leaking issues. Feature requests are kept with bug reports (both are localization tasks).
- **Issues that name the file.** 34 / 72 gated shell and 53 / 78 gated SQL instances mention a gold file name or its stem (an ohmyzsh plugin, a dbt macro, `sp_BlitzIndex`). That is how these projects are reported; it favours lexical arms (BM25 included) equally. First Responder Kit: 12 / 12.
- **T-SQL function gold is empty with the current labeler.** First Responder Kit and DarlingData define each procedure by `EXEC('CREATE PROCEDURE …')` + `ALTER PROCEDURE` / `CREATE OR ALTER`: `sqlgold` finds no callable there (0 / 12 and 1 / 5), so these instances only enter File Acc@5. Their procedures are one per file (4–20 k lines), so a function-level label would equal the file anyway.
- **SQL function gold depends on the dialect.** `gold.py` calls the SQL labeler with `ansi`; with the repository's dialect pgmq goes 2 → 5 and pg_partman 1 → 6 instances with function gold (PL/pgSQL `$$` bodies). The harness would need a per-repository dialect for S1 (as `dialects` does for S3) to use them; without it they are excluded from Function Acc@10 only.
- **pgmq**: 18 of 19 patches edit `pgmq-extension/sql/pgmq.sql`; the 76 upgrade scripts hold copies of the same functions (hard distractors, real ones). **mimic-code**: concepts exist as BigQuery sources and generated PostgreSQL / DuckDB copies; patches usually touch the BigQuery file (sometimes the copies).
- **asdf** is a Go program since v0.16 (5 non-test shell files at HEAD); its instances are from the bash era (base commits hold `lib/**/*.bash`). The health corpus takes each S1 repository at the base commit of its first instance.
- **Extension-less scripts** (pyenv / rbenv `libexec/*`, git-extras `bin/*`, dokku plugin commands, omarchy `bin/*`) are outside the bench's shell file set, so those repositories yield almost nothing (`no_lang`) — a property of the file set, not of the miner.
- **Health corpus side effect** (not a mining bias): gated S1 repositories join the parse-error corpus; ohmyzsh, spaceship and prezto are zsh, the dbt packages are Jinja-heavy, First Responder Kit is procedural T-SQL.
- No ksh repository with linked issue → PR pairs was found (shellspec is the only one that targets ksh, as portable POSIX sh). No MySQL-, SQLite- or Oracle-flavoured SQL repository passed: mysql/mysql-sys is GPL-2.0, archived, 0 linked PRs; utPLSQL keeps its code in `.pks` / `.pkb` (outside the SQL extensions; its 6 eligible pairs are install scripts); OraOpenSource/* have 0–1 linked PRs.
- Two repositories were renamed since their candidates were listed: `basecamp/omarchy` → `omacom/omarchy`, `tuva-health/tuva` → `tuva-health/tuva-core` (mined under the current names; tuva-core is chosen). `metaplane/dbt-expectations` is the successor of calogica/dbt-expectations (3 eligible, not chosen).

## Rejected repositories

| Repository | Eligible | Reason |
|---|---|---|
| nvm-sh/nvm | 70 | one script (`nvm.sh`) takes every patch: file localization is trivial (already a gated S3 repository) |
| angristan/openvpn-install, Homebrew/install, dehydrated-io/dehydrated, rcaloras/bash-preexec | 38, 29, 1, 4 | single-script repositories |
| pi-hole/pi-hole | 69 | EUPL-1.2 (copyleft); 19 non-test shell files |
| RetroPie/RetroPie-Setup | 29 | GPL-3.0 |
| acmesh-official/acme.sh | 12 | GPL-3.0 (kept as the existing S3 set only) |
| omacom/omarchy | 35 | most of its commands are extension-less `bin/` scripts outside the file set (207 PRs dropped by `no_lang`); a distribution's configuration, 1 year of history |
| sindresorhus/pure | 21 | 3 non-test shell files |
| dokku/dokku, pyenv/pyenv, rbenv/rbenv, rbenv/ruby-build, rvm/rvm, tj/git-extras, tfutils/tfenv, wfxr/forgit | 14, 2, 0, 0, 0, 1, 8, 5 | code in extension-less scripts or in another language (Go) |
| zsh-users/zsh-syntax-highlighting, romkatv/powerlevel10k, pivpn/pivpn, zsh-users/antigen, akinomyoga/ble.sh, zplug/zplug, zsh-users/zsh-autosuggestions, kward/shunit2, others | ≤ 9 | few pairs; not needed for dialect coverage (kept as spares: zsh-syntax-highlighting 9, p10k 8, pivpn 9) |
| community-scripts/ProxmoxVE | — | listing stopped (≫ 5,000 merged PRs; GraphQL budget); not mined |
| olahallengren/sql-server-maintenance-solution | 51 | 9 SQL files; every PR edits the same four procedures plus their concatenation `MaintenanceSolution.sql` |
| amachanic/sp_whoisactive | 16 | GPL-3.0, single file |
| Snowflake-Labs/dbt_constraints | 18 | 8 non-test SQL files (12 of 18 patches in one macro file) |
| nearform/temporal_tables | 7 | 3 non-test SQL files |
| utPLSQL/utPLSQL | 6 | code in `.pks` / `.pkb`; eligible pairs are install scripts |
| OHDSI/Achilles | 11 | an R package (SQL templates under `inst/sql`), no licence detected by GitHub |
| fivetran/dbt_* (11 packages), dbt-audit-helper, dbt-profiler, dbt-unit-testing, automate-dv, dbt-expectations, dbt-data-reliability, graphile/worker, pgsodium, amazon-redshift-utils, sqlc | ≤ 4 | too few pairs (fivetran PRs are large release PRs: `too_big` / not linked); sqlc's SQL is test data (1 pair), so SQL calibration S1 does not reuse the calibration S3 repository |
| tSQLt-org/tSQLt, theory/pgtap, jOOQ/sakila, mysql/mysql-sys, pgq/pgq, OraOpenSource/Logger, oos-utils, dbt-msft/tsql-utils | 0 | no eligible pair (`dbt-labs/metrics` no longer exists: 404) |

## Per-repository counts, every candidate

### shell: every candidate

| Repository | Licence | Lang files at HEAD (non-test) | merged_prs | closing | bot | issue_after | short | leak | no_lang | not_mostly | too_many | truncated | no_diff | too_big | no_lang_patch | unreachable | dup_issue | eligible | quota | split |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ohmyzsh/ohmyzsh | MIT | 418 (407) | 3831 | 416 | 1 | 24 | 4 | 6 | 60 | 20 | 3 | 0 | 0 | 0 | 0 | 0 | 1 | 297 | 12 | gated |
| docker-mailserver/docker-mailserver | MIT | 117 (45) | 1804 | 300 | 0 | 6 | 3 | 2 | 116 | 69 | 2 | 2 | 0 | 0 | 0 | 0 | 0 | 100 | 10 | gated |
| Bash-it/bash-it | MIT | 375 (339) | 1379 | 107 | 0 | 2 | 3 | 1 | 15 | 2 | 3 | 0 | 0 | 0 | 0 | 0 | 1 | 80 | 10 | gated |
| asdf-vm/asdf | MIT | 39 (5) | 904 | 200 | 0 | 5 | 3 | 4 | 94 | 11 | 2 | 0 | 0 | 0 | 0 | 0 | 2 | 79 | 8 | gated |
| ChrisTitusTech/linutil | MIT | 202 (202) | 680 | 146 | 0 | 11 | 5 | 0 | 48 | 3 | 5 | 0 | 0 | 0 | 0 | 0 | 1 | 73 | 8 | gated |
| spaceship-prompt/spaceship-prompt | MIT | 121 (84) | 478 | 46 | 0 | 2 | 0 | 2 | 4 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 36 | 6 | gated |
| zdharma-continuum/zinit | MIT | 12 (12) | 383 | 45 | 0 | 0 | 3 | 0 | 5 | 2 | 0 | 0 | 0 | 1 | 0 | 0 | 1 | 33 | 5 | calibration |
| shellspec/shellspec | MIT | 253 (140) | 77 | 40 | 0 | 0 | 6 | 0 | 2 | 4 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 25 | 8 | gated |
| sdkman/sdkman-cli | Apache-2.0 | 26 (23) | 254 | 29 | 0 | 0 | 0 | 0 | 5 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 23 | 5 | gated |
| ohmybash/oh-my-bash | MIT | 235 (234) | 216 | 32 | 0 | 1 | 1 | 0 | 5 | 1 | 1 | 0 | 0 | 2 | 0 | 0 | 0 | 21 | 5 | calibration |
| sorin-ionescu/prezto | MIT | 41 (41) | 380 | 44 | 0 | 2 | 1 | 1 | 14 | 4 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 21 | 5 | gated |
| bats-core/bats-core | MIT | 288 (15) | 449 | 124 | 0 | 4 | 2 | 1 | 84 | 25 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8 | 5 | calibration |
| nvm-sh/nvm | MIT | 8 (6) | 816 | 111 | 0 | 8 | 2 | 1 | 24 | 2 | 0 | 0 | 0 | 1 | 0 | 0 | 3 | 70 | — | — |
| pi-hole/pi-hole | EUPL-1.2 (copyleft) | 30 (19) | 2282 | 96 | 0 | 7 | 0 | 0 | 16 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 69 | — | — |
| angristan/openvpn-install | MIT | 7 (1) | 269 | 52 | 0 | 0 | 2 | 1 | 8 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 38 | — | — |
| omacom/omarchy | MIT | 799 (492) | 2527 | 280 | 0 | 19 | 0 | 10 | 207 | 8 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 35 | — | — |
| Homebrew/install | BSD-2-Clause | 2 (2) | 584 | 37 | 0 | 1 | 0 | 1 | 6 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 29 | — | — |
| RetroPie/RetroPie-Setup | GPL-3.0 (copyleft) | 289 (289) | 1890 | 39 | 0 | 0 | 0 | 1 | 5 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 29 | — | — |
| sindresorhus/pure | MIT | 27 (3) | 173 | 28 | 0 | 0 | 1 | 1 | 4 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 21 | — | — |
| dokku/dokku | MIT | 165 (3) | 5261 | 607 | 0 | 5 | 9 | 4 | 565 | 8 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 14 | — | — |
| acmesh-official/acme.sh | GPL-3.0 | 304 (304) | 1695 | 14 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 12 | — | — |
| pivpn/pivpn | MIT | 26 (25) | 211 | 9 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 9 | — | — |
| zsh-users/zsh-syntax-highlighting | BSD-3-Clause | 301 (296) | 128 | 12 | 0 | 0 | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 9 | — | — |
| romkatv/powerlevel10k | MIT | 16 (16) | 172 | 12 | 0 | 0 | 0 | 0 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8 | — | — |
| tfutils/tfenv | MIT | 24 (6) | 210 | 43 | 0 | 0 | 0 | 0 | 24 | 11 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8 | — | — |
| zsh-users/antigen | MIT | 49 (49) | 300 | 8 | 0 | 0 | 0 | 0 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 6 | — | — |
| wfxr/forgit | MIT | 21 (3) | 309 | 60 | 0 | 0 | 0 | 1 | 43 | 11 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 5 | — | — |
| akinomyoga/ble.sh | BSD-3-Clause | 150 (149) | 24 | 5 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 4 | — | — |
| rcaloras/bash-preexec | MIT | 4 (1) | 72 | 4 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 4 | — | — |
| pyenv/pyenv | MIT | 62 (9) | 1205 | 179 | 0 | 2 | 7 | 2 | 162 | 3 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 2 | — | — |
| zplug/zplug | MIT | 52 (50) | 159 | 5 | 0 | 0 | 1 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | — | — |
| zsh-users/zsh-autosuggestions | MIT | 14 (14) | 116 | 3 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | — | — |
| basherpm/basher | MIT | 32 (7) | 43 | 3 | 0 | 0 | 0 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| dehydrated-io/dehydrated | MIT | 1 (1) | 74 | 3 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| nextcloud/nextcloudpi | NOASSERTION | 146 (144) | 242 | 6 | 0 | 0 | 0 | 0 | 4 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| tj/git-extras | MIT | 25 (5) | 684 | 93 | 0 | 8 | 6 | 0 | 72 | 6 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| tmux-plugins/tpm | MIT | 18 (12) | 29 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| kward/shunit2 | Apache-2.0 | 21 (2) | 29 | 2 | 0 | 1 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| marlonrichert/zsh-autocomplete | MIT | 4 (2) | 46 | 9 | 0 | 0 | 1 | 0 | 7 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| rbenv/rbenv | MIT | 26 (2) | 218 | 19 | 0 | 0 | 0 | 1 | 18 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| rbenv/ruby-build | MIT | 15 (1) | 888 | 63 | 0 | 0 | 4 | 3 | 54 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| rvm/rvm | NOASSERTION | 44 (8) | 1477 | 94 | 0 | 2 | 1 | 0 | 91 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |

### sql: every candidate

| Repository | Licence | Lang files at HEAD (non-test) | merged_prs | closing | bot | issue_after | short | leak | no_lang | not_mostly | too_many | truncated | no_diff | too_big | no_lang_patch | unreachable | dup_issue | eligible | quota | split |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| BrentOzarULTD/SQL-Server-First-Responder-Kit | MIT (LICENSE.md) | 34 (34) | 1660 | 615 | 0 | 7 | 12 | 0 | 28 | 3 | 3 | 0 | 0 | 4 | 0 | 0 | 20 | 538 | 12 | gated |
| dbt-labs/dbt-project-evaluator | Apache-2.0 | 123 (116) | 276 | 114 | 0 | 1 | 6 | 0 | 23 | 2 | 8 | 0 | 0 | 0 | 0 | 0 | 1 | 73 | 8 | gated |
| tuva-health/tuva-core | Apache-2.0 | 472 (380) | 722 | 151 | 0 | 4 | 5 | 1 | 40 | 0 | 19 | 4 | 0 | 8 | 0 | 0 | 1 | 69 | 8 | gated |
| dbt-labs/dbt-utils | Apache-2.0 | 116 (51) | 403 | 112 | 0 | 9 | 2 | 0 | 44 | 0 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 54 | 8 | gated |
| erikdarlingdata/DarlingData | MIT | 84 (74) | 637 | 36 | 0 | 0 | 0 | 0 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 1 | 33 | 5 | calibration |
| dbt-labs/dbt-external-tables | Apache-2.0 | 47 (40) | 134 | 50 | 0 | 7 | 1 | 0 | 8 | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 31 | 5 | gated |
| pgmq/pgmq | PostgreSQL | 101 (88) | 374 | 63 | 0 | 1 | 6 | 0 | 32 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 19 | 6 | gated |
| dbt-labs/dbt-codegen | Apache-2.0 | 48 (8) | 93 | 44 | 0 | 5 | 0 | 3 | 20 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 16 | 5 | calibration |
| brooklyn-data/dbt_artifacts | Apache-2.0 | 107 (88) | 242 | 44 | 0 | 0 | 2 | 0 | 13 | 3 | 11 | 0 | 0 | 0 | 0 | 0 | 1 | 14 | 4 | gated |
| MIT-LCP/mimic-code | MIT | 562 (562) | 314 | 20 | 0 | 0 | 0 | 0 | 4 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 1 | 13 | 8 | gated |
| get-select/dbt-snowflake-monitoring | MIT | 33 (31) | 113 | 14 | 0 | 1 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 11 | 4 | gated |
| calogica/dbt-date | Apache-2.0 | 51 (42) | 64 | 14 | 0 | 0 | 0 | 0 | 2 | 0 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 9 | 4 | gated |
| NikolayS/postgres_dba | BSD-3-Clause | 40 (40) | 47 | 7 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 6 | 5 | calibration |
| pgpartman/pg_partman | PostgreSQL (LICENSE.txt) | 242 (146) | 77 | 9 | 0 | 0 | 0 | 1 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 6 | 6 | gated |
| GoogleCloudPlatform/bigquery-utils | Apache-2.0 | 66 (65) | 323 | 14 | 0 | 0 | 0 | 0 | 7 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 5 | 5 | gated |
| olahallengren/sql-server-maintenance-solution | MIT | 9 (9) | 177 | 59 | 0 | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 0 | 0 | 2 | 51 | — | — |
| Snowflake-Labs/dbt_constraints | Apache-2.0 | 113 (8) | 56 | 24 | 0 | 1 | 0 | 0 | 2 | 2 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 18 | — | — |
| amachanic/sp_whoisactive | GPL-3.0 | 3 (3) | 31 | 17 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 16 | — | — |
| OHDSI/Achilles | NOASSERTION | 414 (414) | 241 | 29 | 0 | 0 | 2 | 0 | 10 | 1 | 1 | 1 | 0 | 1 | 0 | 0 | 2 | 11 | — | — |
| nearform/temporal_tables | NOASSERTION | 21 (3) | 44 | 12 | 0 | 1 | 0 | 0 | 3 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 7 | — | — |
| utPLSQL/utPLSQL | Apache-2.0 | 78 (69) | 593 | 99 | 0 | 4 | 1 | 0 | 65 | 20 | 1 | 2 | 0 | 0 | 0 | 0 | 0 | 6 | — | — |
| data-mie/dbt-profiler | Apache-2.0 | 32 (15) | 75 | 8 | 0 | 0 | 2 | 0 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 4 | — | — |
| elementary-data/dbt-data-reliability | Apache-2.0 | 338 (237) | 819 | 6 | 0 | 0 | 1 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 4 | — | — |
| Datavault-UK/automate-dv | Apache-2.0 | 122 (122) | 63 | 14 | 0 | 1 | 0 | 0 | 2 | 2 | 2 | 1 | 0 | 3 | 0 | 0 | 0 | 3 | — | — |
| EqualExperts/dbt-unit-testing | MIT | 113 (15) | 65 | 5 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | — | — |
| awslabs/amazon-redshift-utils | Apache-2.0 | 165 (164) | 384 | 5 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | — | — |
| metaplane/dbt-expectations | Apache-2.0 | 91 (78) | 14 | 5 | 0 | 1 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | — | — |
| dbt-labs/dbt-audit-helper | Apache-2.0 | 59 (20) | 66 | 7 | 0 | 0 | 0 | 0 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | — | — |
| fivetran/dbt_ad_reporting | Apache-2.0 | 55 (37) | 105 | 7 | 0 | 1 | 0 | 0 | 3 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 2 | — | — |
| fivetran/dbt_fivetran_utils | NOASSERTION | 31 (29) | 91 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | — | — |
| fivetran/dbt_netsuite | Apache-2.0 | 157 (145) | 109 | 14 | 0 | 0 | 0 | 0 | 1 | 1 | 0 | 2 | 0 | 8 | 0 | 0 | 0 | 2 | — | — |
| fivetran/dbt_stripe | Apache-2.0 | 120 (101) | 85 | 11 | 0 | 1 | 0 | 0 | 2 | 1 | 1 | 0 | 0 | 4 | 0 | 0 | 0 | 2 | — | — |
| fivetran/dbt_github | Apache-2.0 | 71 (62) | 53 | 4 | 0 | 0 | 0 | 0 | 1 | 0 | 1 | 0 | 0 | 1 | 0 | 0 | 0 | 1 | — | — |
| fivetran/dbt_hubspot | Apache-2.0 | 335 (288) | 116 | 17 | 0 | 0 | 0 | 0 | 3 | 1 | 0 | 2 | 0 | 10 | 0 | 0 | 0 | 1 | — | — |
| fivetran/dbt_quickbooks | Apache-2.0 | 221 (203) | 126 | 20 | 0 | 1 | 0 | 0 | 2 | 0 | 4 | 1 | 0 | 11 | 0 | 0 | 0 | 1 | — | — |
| fivetran/dbt_zendesk | Apache-2.0 | 142 (131) | 147 | 14 | 0 | 0 | 0 | 0 | 3 | 0 | 3 | 0 | 0 | 7 | 0 | 0 | 0 | 1 | — | — |
| graphile/worker | MIT | 22 (20) | 334 | 58 | 0 | 1 | 5 | 2 | 48 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| michelp/pgsodium | NOASSERTION | 62 (39) | 60 | 2 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| sqlc-dev/sqlc | MIT | 2332 (41) | 1783 | 361 | 0 | 2 | 15 | 2 | 330 | 6 | 0 | 5 | 0 | 0 | 0 | 0 | 0 | 1 | — | — |
| OraOpenSource/Logger | MIT | 24 (22) | 7 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| OraOpenSource/oos-utils | MIT | 10 (9) | 21 | 1 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| dbt-msft/tsql-utils | MIT | 52 (44) | 44 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| fivetran/dbt_jira | Apache-2.0 | 96 (86) | 103 | 12 | 0 | 0 | 1 | 0 | 2 | 0 | 2 | 0 | 0 | 7 | 0 | 0 | 0 | 0 | — | — |
| fivetran/dbt_salesforce | Apache-2.0 | 69 (48) | 53 | 4 | 0 | 0 | 0 | 0 | 2 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| fivetran/dbt_shopify | Apache-2.0 | 375 (324) | 105 | 11 | 0 | 0 | 1 | 0 | 2 | 1 | 2 | 0 | 0 | 5 | 0 | 0 | 0 | 0 | — | — |
| jOOQ/sakila | BSD-2-Clause | 34 (34) | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| mysql/mysql-sys | NOASSERTION | 173 (173) | 50 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| pgq/pgq | ISC | 73 (73) | 8 | 2 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| tSQLt-org/tSQLt | Apache-2.0 (Build/License.txt) | 281 (210) | 61 | 3 | 0 | 0 | 0 | 0 | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
| theory/pgtap | NOASSERTION | 56 (14) | 93 | 11 | 0 | 0 | 0 | 0 | 6 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | — | — |
