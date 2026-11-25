# Decisions log

## 2026-11-25 — Shell: one dialect per file, tree-sitter-zsh for zsh files

The shell pack read every file with tree-sitter-bash; zsh and ksh were
"best effort" and 52.8% of ohmyzsh's zsh files kept a parse error. Shell
now has dialects the way SQL has (`rueca-lang-shell` `dialect.rs`):

- **Dialects: `sh`, `bash`, `zsh`, `ksh`, `bats`**, one per file, a pure
  function of the path and the bytes; the first rule that answers wins:
  the `.bats` extension; a ShellCheck `shell=` directive in the leading
  comments (ShellCheck itself puts it above the `#!` line); the `#!` line
  (`dash` / `ash` / busybox → `sh`, `ksh93` / `mksh` / `pdksh` → `ksh`);
  zsh's `#compdef` / `#autoload`; the file name (`.zsh`, `.zsh-theme`,
  `.zshrc`, `.bash`, `.bashrc`, `.ksh`, `.kshrc`, `.profile`, …); an editor
  modeline; a directory named after one shell; content signals; else `sh`.
  The `#!` line wins over the extension (a `.sh` file that starts with
  `#!/bin/zsh` is zsh). The dialect ends every function signature as a
  comment (`deploy() # bash`), SQL's `-- postgres` for shell; there is no
  store setting and no new field in the model.
- **Content signals never make a multi-shell script zsh.** Each distinct
  signal counts once (zsh: `setopt`, `autoload`, `zstyle`, `${(f)x}`, …;
  bash: `shopt`, `BASH_SOURCE`, `mapfile`, …; ksh: `set -A`, `${.sh.…}`;
  shared non-POSIX syntax: `[[ … ]]`, arrays, `<<<`), one shell's own must
  outweigh the others', and a file that tests `$ZSH_VERSION` /
  `$BASH_VERSION` (nvm.sh, completion scripts) is `bash` when it uses
  anything beyond POSIX, else `sh`. On the 3,471 files measured below the
  rules that decided were: `#!` line 1,753, file name 852, extension
  (`.bats`) 520, directive 330, content 10, default 3, zsh header 3.
- **A grammar per dialect, adopted on measurement.** Candidates for zsh:
  `tree-sitter-zsh` 0.64.3 (georgeharker, MIT, on crates.io and PyPI,
  released 2026-11-17, a fork of tree-sitter-bash that adds zsh's syntax;
  it depends only on `tree-sitter-language`, like the bash grammar),
  against tree-sitter-bash 0.25.1 with the pack's repairs. The other
  crates and repositories found (`konomanoasa-tree-sitter-zsh` 0.8.0, 119
  downloads; tree-sitter-grammars/tree-sitter-zsh, no licence file; small
  personal forks) were not measured. Files with at least one ERROR /
  MISSING node, same files (by extension, before the dialect rules):

  | Files | tree-sitter-bash raw | + the pack's repairs (what shipped) | tree-sitter-zsh raw |
  |---|---|---|---|
  | ohmyzsh `.zsh` / `.zsh-theme` (545) | 298 | 288 (52.8%) | 17 (3.1%) |
  | prezto `.zsh` (41) | 33 | 32 | 1 |
  | spaceship-prompt (122) | 53 | 43 | 3 |
  | zsh-syntax-highlighting (301) | 20 | 13 | 6 |
  | zsh-autosuggestions (13) | 10 | 10 | 0 |
  | powerlevel10k (16) | 16 | 16 | 5 |
  | bash-it `.bash` (328) | 5 | 4 | 13 |
  | bats-core `.bats` (224) | 2 | 2 | 16 |
  | dokku `.bats` (154) | 0 | 0 | 24 |
  | acme.sh `.sh` (304) | 8 | 8 | 12 |

  So: `zsh` files → tree-sitter-zsh; `sh`, `bash`, `ksh`, `bats` files →
  tree-sitter-bash (the zsh grammar is worse on bash code, bats files
  above all). No ksh grammar exists on crates.io and no permissively
  licensed ksh corpus of any size was found: ksh files are read as bash
  (the Bourne subset both share) and ksh93's own forms are errors.
- **The other grammar only when it reads the whole file.** When the
  dialect's grammar leaves errors and the other one leaves none, the
  other tree is extracted (15 of the 3,471 files: bash the fork accepts
  and its parent rejects — `*"a"*"b")` case patterns, `"${x:-$(f)}"` —
  and a zsh file with a bash `#!` line). When both leave errors the
  dialect's tree is kept: comparing error counts of two broken trees
  moved nvm.sh (23 errors as bash, 8 as zsh) to a grammar that was not
  written for it. Bats files are only read as bash.
- **Extraction is the same code for both trees.** The zsh grammar names
  some nodes differently (`variable_ref` for `$x`, `simple_variable_name`,
  operator nodes under `expansion`, names declared without a value as
  `word` arguments, `foreach` / short `for` loops, `integer` / `float`
  declarations); the walker and the locals table accept both, and the
  bindings query has a zsh twin (`queries/locals-zsh.scm`). Checked by
  extracting every file both grammars read without errors through each
  (`examples/grammars.rs --facts`): ohmyzsh 6 of 266 files differ, prezto
  0 of 9, spaceship-prompt 1 of 79, zsh-syntax-highlighting 1 of 288; in
  the differing files the zsh tree is the better one (`integer x` is a
  declaration, not a call; `${(P)NAME}` reads `NAME`) except two themes
  whose multi-line prompt assignments the zsh grammar splits differently.
- **Repairs per grammar** (`repair.rs`; same length, so spans stay exact;
  signatures, value previews and docs now quote the file as written, not
  the repaired text). New for the bash grammar: `;` in an expansion's
  pattern, a here-string after an output redirection (`cat > f <<<"x"`,
  12 bats files of pyenv, rbenv and asdf). For the zsh grammar: `$$` before more
  text, the arguments of an anonymous function (`} "$1" &|`), commented-out
  assignments (`#NAME=value words`: the grammar read an assignment and a
  command), unquoted `%{` prompt escapes (`C=%{$fg[red]%}`: the grammar
  ended the value at `%`). For both: `for x do`, `0x$h` in arithmetic,
  `} 0<file`. Every rule is a construct, none names a file. The bench
  labeler ports the dialect rules and the repairs (`shelldialect.py`) and
  parses with the same grammar: on the 3,471 files its dialect, rule,
  grammar, repaired text (hash) and error flag equal the pack's.
- **Result** (files with a parse error / files, extension-less scripts
  and tests included, `examples/dialects.rs`; checkouts: acme.sh
  `5567d7f`, asdf v0.14.1, bash-it v3.2.0, bats-core v1.13.0, dokku
  `a31ba25`, git-extras `02938b5`, nvm v0.41.0, ohmyzsh `1eebae3`,
  pi-hole `f7c0220`, powerlevel10k `50a35cc`, prezto `a7c6167`, pyenv
  `f5bc939`, rbenv `07e9b1e`, spaceship-prompt `ef7f71d`,
  zsh-autosuggestions `85919cd`, zsh-completions `989b232`,
  zsh-syntax-highlighting `e6683f2`):

  | Dialect | Grammar | Files | With errors | Before (one grammar, by extension) |
  |---|---|---|---|---|
  | sh | tree-sitter-bash | 720 | 4 (0.56%) | |
  | bash | tree-sitter-bash | 1,155 | 10 (0.87%) | `.sh` / `.bash` / `.ksh`: 28 / 812 (3.4%) |
  | zsh | tree-sitter-zsh | 1,075 | 24 (2.23%) | `.zsh` / `.zsh-theme`: 403 / 1,041 (38.7%) |
  | ksh | tree-sitter-bash | 0 | — | |
  | bats | tree-sitter-bash | 521 | 4 (0.77%) | `.bats`: 16 / 528 (3.0%) |
  | all | | 3,471 | 42 (1.21%) | 447 / 2,381 (18.8%) |

  ohmyzsh alone: 9 / 572 (1.6%; zsh 8 / 556), from 303 / 586. zsh stays
  above 2% on this pool because of powerlevel10k (5 of 17: `p10k.zsh` is
  9,000 lines of nested expansions and glob flags) and
  zsh-syntax-highlighting's parser tests. What is still unparsed, by
  construct: short loops without `do` (`for x in a b` then one command),
  `function {a,b}_x()`, dynamic function names (`f_$x() {…}`), `(#b)` /
  `(#s)` glob flags in `[[ … ]]`, `$x:h/…`, `${x: : -1}` (zsh); `[ a '('
  b ')' ]`, `[ "$a" "$@" ]`, case patterns such as `--*' '*)`, arithmetic
  over several lines with subscripts, `${.sh.file}`, nvm.sh (bash; the
  grammar's here-document scanner). A tree can carry tree-sitter's
  `has_error` flag without any ERROR / MISSING node (an empty `word` the
  zsh scanner emits: 7 of the 3,471 files); it counts as read, as in every
  other pack (`rueca-extract` `count_errors`) and in the health metric.
- **Cost.** tree-sitter-zsh's generated parser is 43 MB of C (the bash
  grammar's is 9.5 MB); ohmyzsh's 572 files are read in 0.37 s and
  acme.sh's 304 in 0.30 s (dialect, repairs, both grammars when needed).

## 2026-11-25 — SQL pack: sqlparser per statement over a tree-sitter grammar, one dialect per file, a code-backed gold; `preview`

SQL landed as `rueca-lang-sql` (RFC 0003 §10.4g) with a `sql` module in
`rueca-resolve`. Choices worth recording:

- **Parser: `sqlparser` 0.64 (Apache-2.0), one statement at a time, plus a
  token scanner; not a tree-sitter SQL grammar.** RFC 0002 §3.3 already
  named sqlparser for SQL DDL; the choice was re-checked with evidence.
  Candidates: DerekStride's grammar (`tree-sitter-sequel` 0.3.13, MIT; the
  T-SQL and SQLite forks on crates.io are single-dialect) and sqlparser
  (dialects: PostgreSQL, MySQL, SQLite, MsSql, BigQuery, Snowflake,
  Redshift, DuckDB, generic, …). Files with at least one error, per
  dialect (directory of the file), same files — sakila `e089a5b`, sqlc
  v1.31.1, dbt_hubspot v1.11.0, jaffle-shop-classic `fd7bfac`,
  bigquery-utils `d5bde4f`, sql-server-maintenance-solution `4161757`:

  | Dialect | Files | tree-sitter-sequel (whole file) | the pack: sqlparser per statement, dialect fallback | Statements unparsed |
  |---|---|---|---|---|
  | PostgreSQL | 1,073 | 117 (10.9%) | 23 (2.1%) | 33 / 2,315 |
  | MySQL | 278 | 27 (9.7%) | 10 (3.6%) | 20 / 643 |
  | SQLite | 199 | 27 (13.6%) | 4 (2.0%) | 7 / 619 |
  | T-SQL | 12 | 11 | 11 | 50 / 183 |
  | BigQuery | 66 | 47 (71%) | 11 (16.7%) | 19 / 143 |
  | dbt Jinja | 340 | 334 (98%) | 22 (6.5%) | 22 / 170 |

  Positions: sqlparser 0.64 carries a span on every identifier token
  (line and character column, converted to bytes per statement chunk), which
  is all definitions and references need. Recovery: tree-sitter recovers
  inside one file, sqlparser does not, so the pack cuts statements itself
  (`GO`, `DELIMITER`, `BEGIN … END` aware), parses each in the file's
  dialect then in the others, and reads the rest with a token scanner;
  one bad statement loses nothing else. Default features off (no
  `recursive` stack growth); cargo-deny clean; `cargo +1.90 check` passes.
  The pack is the second one, after proto, that does not use tree-sitter;
  it plugs into the same `LanguagePack` contract.
- **One dialect per file, deterministic, no setting.** A directive comment
  (`-- dialect: x`, sqlfluff's `-- sqlfluff:dialect:x`), else the path
  (extension, then a dialect word in the file or directory names), else
  weighted content signals, else ANSI. A per-path setting in
  `<store>/rueca.toml` was weighed and left out: extraction is a pure
  function of path and bytes (the facts cache key) and the store
  configuration never reaches packs; the directive covers the cases the
  path and content miss. The dialect is shown in every top-level
  signature, the only place the model has for it.
- **A database namespace is global.** The module name is empty; names are
  normalized (unquoted fold to lower case, quoted kept where the dialect is
  case-sensitive, default schemas `public` / `dbo` / `main` dropped). The
  resolver picks among same-named definitions by place: earlier in the
  file, migration order, later in the file, nearest directory; a name that
  matches only by its last segments (`users` ↔ `app.users`) is a
  `unique_name` guess at 0.6; anything still ambiguous resolves to nothing
  (Sakila's copies of one table in six dialects are the test).
- **Migrations: one node per `CREATE`, the effective one by order.** The
  model has no place to delete a definition another file drops, so each
  `CREATE` stays a node and references go to the definition effective at
  their point: the latest at or before the referencing migration (Flyway
  `V1_2__`, timestamps, numeric prefixes, `…/up.sql`), the latest overall
  from outside the migrations; down / undo migrations only for themselves.
  `ALTER TABLE … ADD COLUMN` adds a column member in its migration;
  `ALTER` / `DROP` are `WRITES` from the migration file.
- **Edges are dependencies.** Relations read are `REFERENCES`, written
  (`INSERT` … `DROP`) `WRITES`, routines `CALLS`; columns only when the
  table is certain (a qualifier through aliases, or one table in scope).
  The bench's structural questions for SQL therefore trace `CALLS`,
  `REFERENCES` and `WRITES` (manifest `trace_edges`) and count tables,
  views and dbt models (`Struct`) as callables (`callable_labels`): the
  callgraph format is unchanged, an SQL edge is any dependency.
- **dbt is masked, not rendered.** `ref` / `source` become identifiers in
  place, other expressions placeholders, macro blocks are definitions;
  dependencies are also read from the Jinja itself, so a model whose SQL
  does not parse keeps its `ref` edges. Rendering would need dbt, its
  adapters and the project's packages.
- **Gold: a code-backed labeler on sqlglot**, independent of the pack's
  parser (`bench/src/rueca_bench/sqlgold.py`, sqlglot 30.35.0, MIT): the
  dialect of each tree comes from the manifest, not from the pack's
  detection; it does not model migration order (the micro-benchmark's
  hand labels do, and the cross-check shows the two edges it misses).
  Gated: Sakila in six dialects, dbt_hubspot; calibration: sqlc's
  examples. Tuned on calibration and micro only: a table-valued function
  in `FROM` is a call (sqlc `say_hello`), pg_dump's `-- Name: x; Type:`
  headers are not query names.
- **Status `preview`**: no localization dataset has SQL instances
  (missing = failing, RFC 0004 §6.2), files with parse errors are above
  the 2% floor on the gated corpus (Sakila's Oracle / DB2 copies,
  MySQL routines, T-SQL procedural code, fivetran Jinja), the callees
  population is below 300, and the structural gold is code-backed. Floors
  unchanged. SQL in other languages' string literals (code → table
  bridge) is a follow-up.

## 2026-11-25 — Ruby: global constant paths, Ruby's own lookup, scip-ruby gold, `stable`

The Ruby pack (`rueca-lang-ruby`, RFC 0003 §10.4f) has no module names:
constants are global and classes open, so definitions are named by
constant path (like PHP and C++) and Ruby references are resolved by a
`ruby` module of `rueca-resolve` (like PHP's own path) rather than the
generic ladder:

- **Names.** `A::B` for classes and modules (lexical and compact paths
  joined), `A::B#m` for instance methods and `A::B.m` for singleton ones
  (`def self.m`, `class << self`, `module_function`) — Ruby's documentation
  convention; a symbol id splits at its first `#`, so the `#` inside a
  local name is unambiguous. A class reopened in several files is several
  nodes sharing one table entry: a method of one reopening is found from a
  call anywhere, and the node a constant reference points to is the one
  Zeitwerk would load (`foo/bar.rb` for `Foo::Bar`), else the one with a
  superclass, else the first.
- **Constants** follow Ruby: `Module.nesting` innermost first (a compact
  `class A::B` body does not see `A`'s constants), the ancestors of the
  innermost class, the top level; superclasses are resolved around the
  class, mixins inside its body. **Methods** follow the ancestors
  (`prepend`, the class, `include` last first, the superclass), singleton
  methods `extend` and the superclass chain; a class with an ancestor
  outside the project is open, so a method it lacks is external, never a
  guess. Aliases stand for the method they name (scip-ruby resolves them
  the same way).
- **Locals vs calls.** A bare identifier is a call unless the vendored
  nvim-treesitter `locals.scm` (adapted: hard scopes for `def` / `class` /
  `module`, soft ones for blocks, assignment order) makes it a local.
- **Receivers** are typed in the file from `Foo.new`, Sorbet `sig`s, RBS
  comments (`#: (Foo a) -> Bar`) and `T.let`; declared return types are kept
  in the signature after ` #: -> ` so the resolver types call results
  (`$ret:`, the Rust pack's form). Untyped receivers keep the by-name rung
  at `heuristic` 0.5, the Python policy: on the calibration repositories
  (mid-tuning) dropping it raised callees precision by 1.9 (packwerk) and
  3.6 (rack) points and cut packwerk's callees recall by 3.2 (calls on
  RBS-typed receivers the pack does not type); the confidence ≥ 0.6 row
  shows the guess-free numbers.
- **Repair.** tree-sitter-ruby 0.23.1 does not parse an endless method
  whose body is a command call (`def m = raise E, "x"`, Sorbet-style
  abstract methods); a same-length rewrite (`def m =raise(E, "x")`)
  repairs it before parsing (packwerk: 7 of 63 library files parsed with
  errors before, none after).

**Gold.** scip-ruby 0.5.1 (Sourcegraph, on Sorbet) as a release binary
pinned by sha256 for macOS arm64 and Linux x86-64, like scip-clang: it is
static (no Ruby, Bundler or gems on the host), so no Docker image is
needed. It indexes untyped code (also `# typed: false` files) with Sorbet's
inference: constants, `self` calls, `Foo.new` and locals resolve; untyped
receivers, `super` (no occurrence at all), `prepend` dispatch and instance
variables typed in `initialize` do not. The gold is defined as what Sorbet
resolves, with the Python pack's scoring conventions: Rueca's by-name and
`super` edges outside it count as false positives. The micro key (61
snippets, Ruby semantics) agrees with scip-ruby on 70 of its 76 edges, one
extra; the six differences are those blind spots. Repositories: jekyll
4.4.1 (untyped) and ruby-lsp 0.26.11 (Sorbet / RBS typed) gated, rack
3.2.9 and packwerk 3.3.1 calibration. S1: SWE-bench Multilingual is the
only dataset with Ruby (SWE-rebench V2, Multi-SWE-bench and SWE-PolyBench
have none): rubocop, fluentd, fastlane and jekyll gated (40 instances, 33
change a function), faker and fpm calibration (4). Fastlane was first
calibration; the first report showed 27 gated function instances, below
RFC 0004 §7.2's 30, so it moved to the gated split (a population fix: no
Ruby ranking weight is tuned). The S1 repositories wait in `[org]
pending_languages`, as C#'s and Kotlin's.

**Tuning** used rack, packwerk and the micro-benchmark only (the include
nesting, the endless-method repair, the core-method list of the by-name
rung). The gated sets were scored once, after that, and not tuned on.

**Result** (binary `feat/lang-ruby` @ 2e353ab): `stable` — callers /
callees precision 98.4 / 95.4 (jekyll's callees 91.1: by-name guesses on
untyped receivers; ruby-lsp 98.7), recall 96.8 / 96.7, definition found
100.0, micro F1 96.7, 0.2% of files with parse errors, p95 19 ms;
localization not significantly below BM25 (File Acc@5 55.0 vs 55.0,
Function Acc@10 24.2 vs 18.2 over 40 / 33 issues). cbm on the same
questions: callers / callees precision 88.6 / 61.9, recall 49.6 / 57.0.
Not done: metaprogramming beyond literal `define_method` / `alias_method`
(Rails DSLs, `method_missing`, RSpec `let`), ActiveSupport `Concern` class
methods, union types, `super` inside modules, shebang scripts (after the
shell pack's detection lands), HTTP contracts (Rails / Sinatra routes).

## 2026-11-25 — Kotlin: the JVM family, imports by package, scip-java's Kotlin indexer, `stable`

The Kotlin pack (`rueca-lang-kotlin`, RFC 0003 §10.4e) follows the Java
model — packages, classes with members, overload suffixes, implicit
`this`, `OVERRIDES` — with these decisions:

- **Kotlin joins Java's family** (`LanguageFamily::Java`, documented as
  the JVM family) instead of a family of its own. Packages are keyed by
  family and module name, so a package of Kotlin and Java files is one
  module: Kotlin uses a Java class of its package, Java a Kotlin one, the
  query layer's qualified names and overload lookups apply unchanged, and
  the organization linker's `java:<package>` keys cover both. Weighed: a
  `Kotlin` family (interop then needs a cross-family bridge in every
  lookup). The only Java-side change is that its import resolution also
  takes `C.kt` when no `C.java` matches (Java-only repositories are
  unaffected).
- **Imports bind packages by name** through the `namespace_imports` hook
  the C# pack introduced (same name and signature): Kotlin does not tie
  packages to directories (its conventions omit the common root package
  from paths). The resolver code is a small `jvm` module keyed by (family,
  module); on a rebase with the C# pack, one implementation of
  `namespace_import_targets` stays.
- **Kotlin-specific resolution sits behind `is_kotlin` checks** in
  `rueca-resolve/src/jvm.rs`: companion members through their class,
  extension functions (named `Recv.f`, so an extension of a project type
  of the package is also its member), enum entry members, implicit
  receivers of `apply` / `run` / `with` lambdas and of extension bodies
  (`$with:T`), call results through the Rust pack's `$ret:` form, Kotlin
  arity ranges (defaults, `vararg`) and overload choice by argument types
  refined by the project's class hierarchy. Shared rules
  (`overload_by_types`, the Java ladder) are untouched, so no other
  language's numbers move. Ambiguous overloads keep Java's fan-out (×0.8):
  on the calibration repository binding none instead gave callers recall
  71.6 (precision 97.5) against 84.4 / 94.4.
- **Constructors follow the gold's convention**: the primary constructor,
  written or implicit, spans the class header (where the supertype call
  and default arguments are written); `init` blocks belong to the class,
  like Java initializers; callable references `::f` are reads, not calls.

## 2026-11-25 — Swift language pack (RFC 0003 §10.4d), `stable` under the RFC 0004 floors

- **Grammar**: `tree-sitter-swift` 0.7.4 (alex-pinkus, MIT), the grammar
  nvim-treesitter uses; the crate ships its generated parser at ABI 15 and
  depends only on `tree-sitter-language`, so it links with the 0.27
  runtime and passes cargo-deny and MSRV 1.90. The grammar attaches a call
  to a whole `+` / `*` / prefix-operator expression (`a + b.f(1)` parses as
  `(a + b.f)(1)`): the pack and the bench's call-site test both take the
  right-most operand as the callee.
- **Modules are SwiftPM targets.** The module of a file is the target a
  `Package.swift` `path:` declares (the manifest is read with the Swift
  grammar, never evaluated), else `Sources/<Target>/…`, else the top-level
  directory (Xcode projects, best effort). `import Foo` binds target `Foo`
  wherever its files are, through `LanguagePack::namespace_imports` —
  re-implemented here with the name and signature the Kotlin pack gave it,
  so the two branches meet without a conflict of meaning. A declaration of
  the importing module shadows an imported one (Swift's lookup order).
- **Selectors in qualified names.** Every callable but `deinit` carries its
  argument labels (`Point.init(x:y:)`), Swift's own full name and its
  overloading axis; callables of one scope sharing labels add their
  parameter types after a `|`. Overloads are chosen by the call's labels
  (defaults skipped, variadics, a trailing closure fills a closure
  parameter only), and a callable whose parameters cannot take the labels
  is never linked: a struct keeps its memberwise initializer beside an
  extension's `init`, and that call has no definition in the source.
- **Extensions** are not definitions: their members are top-level `T.m`,
  attached by the resolver to `T` in their target (the package hook, now
  also for suffixed names), an imported target or the only Swift type of
  that name; a call on a type outside the snapshot (`"s".shouted()`) finds
  a unique extension member of that type name.
- **Gold: the compiler's index store.** No maintained SCIP indexer builds
  Swift packages without Xcode; `swift build` with index-while-building in
  the official `swift:6.4.3` image pinned by digest, read through its
  libIndexStore (`indexstore_dump.py`, ctypes), is compiler-grade and runs
  every third-party build step inside Docker. Implicit occurrences
  (property accessors, memberwise initializers) are dropped; edges are
  derived like every SCIP gold. Validated on the hand-labelled
  micro-benchmark before use: 88 / 88 edges, 60 / 60 snippets identical.
  Gated repositories: Alamofire 5.12.2 and swift-algorithms 1.2.1 (pure
  SwiftPM, no plugins or macros); Plot 0.14.0 = calibration. S1: SWE-rebench
  V2 Swift rows rated `A` (55 gated, 15 calibration); SWE-bench Multilingual
  and Multi-SWE-bench carry no Swift. CocoaPods / Carthage manifests,
  Info.plist / xcconfig keys and Vapor routes are not done.
- **Tuning** used Plot and the micro-benchmark only (contextual types of
  `.member(…)` calls from the declared return type, `for` over typed
  arrays, generic constraints, the trailing-closure rule). The gated sets
  were run once for the published numbers; the first attempt did not
  produce scores (a stack overflow on `let x = x.f()`, a local shadowing
  itself, crashed indexing on gated and S1 repositories alike); the fix
  bounds receiver typing through initializers and was made without looking
  at any gated score.
- **Result**: `stable`, two floors by a narrow margin — callees precision
  90.8 (89.1–92.4) and recall 75.5 (72.7–78.3), callers 93.1 / 78.7,
  definition found 97.9, micro callee F1 99.4, 1.8% of files with parse
  errors, p95 21 ms; localization trails BM25 (File Acc@5 70.9 vs 80.0)
  without being significantly below it. Swift joins the organization
  scenario at its next full run (`[org] pending_languages`).

**Gold.** scip-java 0.14.2's Kotlin indexer (Kotlin Analysis API 2.4.0,
K2) runs without a build tool from a `scip-java.json` listing sources and
sha256-pinned classpath jars, in the Temurin image the Java gold already
pins, with no network; about 5 s per repository, nothing skipped. Two
repairs in the derivation: a primary constructor written with
`constructor` on its own line is defined on that keyword and now binds to
the callable whose extent (the class header) holds it (kotlinpoet's
builders, 68 edges); and the Rueca arm prefers a callable when a class and
its constructor start on one line. Blind spots kept and disclosed:
constructor delegation (`: this(…)`, `: super(…)`) has no occurrence and a
supertype call references the class, so those true edges count against an
arm. Repositories: moshi 2.0.0-alpha.1 and clikt 5.2.1 gated, kotlinpoet
2.4.0 calibration (Kotlin Multiplatform `expect` / `actual` calls do not
resolve in its gold). S1: SWE-bench Multilingual has no Kotlin;
Multi-SWE-bench's Kotlin files give 46 gated instances (ktlint, detekt,
ORT, TeXiFy-IDEA, DataFrame, OkHttp, Shadow) and SWE-rebench V2 15
calibration ones; the S1 repositories wait in `[org] pending_languages`
like C#'s.

**Grammar.** `tree-sitter-kotlin-sg` 0.4.1 (fwcd's grammar, ast-grep's
crates.io release) over `tree-sitter-kotlin-ng` 1.1.0: fewer files with
parse errors on five of six corpus repositories (30 vs 86 in total), and
nvim-treesitter's `locals.scm` is written for it; fwcd's own crate pins
`tree-sitter < 0.23`.

**Tuning** used only kotlinpoet (calibration) and the micro-benchmark.
One mid-course run of the gated repositories was looked at as a status
check (callees precision 89.7 then, below the floor); the changes after it
came from calibration failures only (extension bodies as receivers,
unknown-receiver extensions only when no method shares the name, overloads
through the class hierarchy, standard-library result types, call-result
arguments).

**Result** (binary `feat/lang-kotlin` @ 3265ba4): `stable` — callers /
callees precision 96.4 / 90.0 (callees at the floor, interval 87.9–92.0),
recall 86.6 / 85.5, definition found 99.0, micro F1 100.0, 0.15% of files
with parse errors, p95 26 ms; localization not significantly below BM25
(File Acc@5 56.5 vs 58.7, paired interval −10.9–6.5; Function Acc@10 45.5
vs 31.8). cbm on the same questions: callers / callees precision 88.1 /
76.4, recall 55.1 / 44.6. Not done: HTTP / RPC contracts (Spring and Ktor
routes), inferred return types of other files' functions without a
declared type, generic element types, DSL lambdas other than the scope
functions, `expect` / `actual` pairing.

## 2026-11-25 — C#: namespaces bound by name, scip-dotnet gold in Docker, `stable`

The C# pack (`rueca-lang-csharp`, RFC 0003 §10.4d) follows the Java model
— classes with members, overload suffixes, implicit `this`, `OVERRIDES` —
with three decisions specific to C#:

- **Namespaces are module names, not paths.** A namespace spans files and
  directories freely, so `resolve_import` cannot map `using A.B;` to a
  directory the way Java's on-demand imports do. A new opt-in hook,
  `LanguagePack::namespace_imports`, tells the resolver that an import
  names a module: it binds the package whose module name is `A.B`
  (`IMPORTS` to the directory holding most of its files). Weighed: a
  pack-side guess from `RootNamespace` + folder names (wrong whenever a
  project departs from the convention) and emitting synthetic imports for
  the enclosing namespaces (an `IMPORTS` edge per parent namespace, noise);
  the hook is additive and no other pack sets it. Namespaces stay module
  names, not `Module` definitions, as Java packages.
- **One resolver module, `csharp`, entered once per reference.** Enclosing
  namespaces, `global using` per project (nearest `*.csproj`), `using
  static`, aliases, namespace-qualified receivers, partial types (their
  parts share members and bases), extension methods, `$ret:` receivers
  typed from declared return types, and an overload choice by C#
  conversion rules (exact type, then type parameter, then conversion;
  `object` and `params` expansions last) sit behind `is_csharp` checks at
  the entry points; the shared rules (`overload_by_types`, `may_accept`,
  the Java ladder) are untouched, so no other language's numbers move.
- **`#if` that splits a declaration.** tree-sitter-c-sharp walks every
  branch, which is right where a directive stands as a declaration or a
  statement but leaves `ERROR` nodes where it splits one (`void F() #if X
  => G() #endif ;`, serilog's `ILogger`: 134 errors, its default interface
  methods unseen). Such a file is parsed again with only the first branch
  of each conditional, blanked in place (offsets unchanged), and the copy
  is kept when it has fewer errors: 0 files with parse errors over the 10
  corpus repositories.

**Gold.** scip-dotnet 0.2.16 (Roslyn) runs in its official image pinned
by digest, on a scratch copy with a generated solution of the listed
projects, multi-targeting projects made single-target (`net8.0`;
MSBuildWorkspace finds no `Compile` target in an outer multi-target
build), NuGet audit off, packages restored from nuget.org into the
bench cache. Its index has three shapes the derivation handles behind
grammar flags only C# sets: `new T(…)` is a reference to the type (mapped
to `T`'s constructors, one class when overloaded), local functions are
`local` symbols (bound per document), and a call of a constructed generic
method carries the first overload's symbol (`Write().` for `Write<T>(…)`,
sent to the generic overloads that take the call's argument count; before
that repair serilog's callees precision read 70.7). Blind spots kept and
disclosed: constructor initializers, target-typed `new()` and explicit
generic calls have no occurrence, so those true edges count against an
arm. Repositories: serilog 4.4.0 (library) and eShopOnWeb (application)
gated, YamlDotNet 18.1.0 calibration. S1: SWE-bench Multilingual and
Multi-SWE-bench have no C#; SWE-rebench V2's `A`-screened rows give 39
gated instances (kiota 25, moq 7, six small repositories) and 11
calibration ones.

**Organization scenario.** Its pin rule makes every gated S1 repository a
member; adding the eleven C# repositories would change the organization
whose numbers are published without re-measuring them (every arm, hours).
They are listed in `[org] pending_languages` instead and join at the next
full run of the scenario (owner's call when to run it).

**Tuning** used only the micro-benchmark and YamlDotNet (calibration):
`this` / `base` as arguments (unnamed keyword nodes), comments and default
values kept out of signatures so a long parameter list keeps its arity,
constrained type parameters as receivers, overload scoring.

**Result** (binary `feat/lang-csharp` @ 1532771): `stable` — callers /
callees precision 96.9 / 95.5, recall 95.2 / 96.6, definition found
100.0, micro F1 99.3, 0.0% files with parse errors, p95 32 ms;
localization not significantly below BM25 (File Acc@5 48.7 vs 48.7,
Function Acc@10 26.3 vs 28.9). Not done: contract hints (ASP.NET routes,
`HttpClient`), property chains across types, generic arguments as receiver
types; `Directory.Packages.props` adds no package identity (central
versions only).

## 2026-11-25 — Shell pack: bash's lookup by name, scripts as files, a code-backed gold; `preview`

Shell landed as `rueca-lang-shell` (RFC 0003 §10.4c) with a `shell`
module in `rueca-resolve`. Choices worth recording:

- **Functions are file-level, whatever their nesting.** A shell function
  is global once its definition runs; a function defined inside another
  (or in an `if`) is named by its name alone and visible to the whole
  file. Two definitions of one name in a file (zsh / bash variants) are
  one node, the first.
- **Resolution follows bash's lookup at run time**: the file's own
  definition (`same_module` 0.90), else the only definition among the
  files it sources, transitively (`import_resolved` 0.95), else the only
  non-test shell function of that name (`unique_name` 0.60). The own file
  wins over a sourced library (scripts source first, define after). A name
  two sourced files define resolves to nothing: which one runs depends on
  the order the script sources them, which the resolver does not model
  (the micro-benchmark keeps the run-order answer and counts the miss).
- **`source` paths are evaluated, not guessed.** The pack evaluates the
  script's directory (`$(dirname "$0")`, `${BASH_SOURCE%/*}`, `cd … &&
  pwd`, zsh `${0:A:h}`) and directory variables the file assigns; an
  unknown prefix (`"$NVM_DIR/nvm.sh"`) keeps its literal tail, which
  resolves only when one file of the snapshot ends with it. A dynamic path
  (`source "$f"`) imports nothing; its functions are then reached, if at
  all, by unique name.
- **A script run by path is a call of the script's `File` node**
  (`$script` receiver, a reserved text of `rueca-extract`): `trace` on
  `bin/deploy` lists who runs it, and the script's own top-level calls
  were already `File` → function edges. No per-script pseudo-definition:
  it would compete with real functions in search and collide with the
  common `main`.
- **`trap` handlers are references, not calls** (a callback, like a
  function passed as a value in the other languages); `eval 'literal'`
  is a call. `command f`, `exec f`, `xargs f`, `sudo f` never run a shell
  function and give nothing.
- **Env reads by convention and binding.** `$NAME` is an environment read
  when the file binds `NAME` nowhere and the name is upper case and not a
  shell variable; a read of a variable a sourced file assigns becomes a
  reference to it. Shell scoping is dynamic, so lower-case names (a
  caller's `local`s) are never env reads. Providers (`export`, `declare
  -x`, `NAME=value cmd`) are `EnvVar` definitions like the data pack's;
  the bridge has no slot for `${NAME:-default}`'s default.
- **Extension-less scripts by their `#!` line.** The indexer reads the
  first line from the bytes it already loaded, only for names without an
  extension (`Language::from_shebang`): no extra I/O, no content sniffing
  for named files. The bench's file sets stay extension-based, so these
  scripts are indexed but outside the S3 populations.
- **Same-length repairs before parsing** (`repair.rs`), as the C/C++ pack
  does: `${s:$i:1}`, `&` / `<` / `>` in an expansion's pattern, a
  line-start `\.` (nvm's alias bypass, which tree-sitter-bash 0.25 joins to
  the previous line), zsh parameter flags. Found on the calibration
  repository and ohmyzsh; the bench labeler applies the same rules so the
  gold reads the same text. Remaining errors are counted: on ohmyzsh (not
  a bench repository) 545 of 586 files are zsh and 52.8% of those
  have an ERROR left (glob qualifiers, `for x (…)`, nested `${${…}}`, subscript
  flags), against 1.4% of bats-core's files (smoke test, extension-less
  scripts included) and 1.95% of the gated sets' (health suite).
- **Gold.** No compiler-grade call graph exists for shell. The structural
  gold is the bench's code-backed labeler (`shellgold.py`): bash's lookup
  over files, `source` paths matched by their literal tail (independent of
  the pack's evaluation), on nvm, acme.sh and bash-it (bats-core
  calibration), plus 43 hand-labelled micro snippets cross-checked against
  the labeler (51 / 54 key edges, no extra). Its agreement with the pack
  shows the pack follows bash's rules on real code; it is not an
  independent oracle.
- **Status `preview`.** RFC 0004 §6.2 makes a language `stable` only when every
  gated floor is met; no localization dataset has shell instances
  (SWE-bench Multilingual, Verified, Lite, SWE-PolyBench, Multi-SWE-bench,
  SWE-rebench V2's 20 languages), so the localization floors stay
  *pending*, which counts as failing. The structure, micro and health
  floors are met (callers / callees precision 100.0 / 100.0, recall 99.9 /
  99.9 over 1,738 / 2,170 questions, definitions 100.0, micro callee F1
  99.1, 1.95% of files with parse errors — ⚠, the interval crosses 2% —,
  p95 14 ms), but against a code-backed gold; floors are not lowered and
  no exception is made for a language without a dataset.

## 2026-11-24 — Code mode: Starlark stays as built; the API friction is fixed before merge (RFC 0009)

The owner's decisions on RFC 0009 phase 3 (§14.6). Starlark stays the
optional `code-mode-starlark` build feature exactly as built, with its own
CI job; `deny.toml` is untouched. The four API friction items of §14.4 (5)
are fixed at the source before merge, for every tool:

- **Two names.** Every object with an `id` carries `local`, the file-local
  qualified name the id is built from (the id's `#` part; a file's path),
  next to `qn`, the global one (contract rows keep `symbol`). Scripts build
  `path#Qualname` from fields instead of slicing ids.
- **Origin, one rule.** The same objects carry the `repo` and `ref` of
  their snapshot in every answer, and `trace`'s root has `dir: "root"`.
  Weighed: adding them only to multi-snapshot answers would bring back the
  `.get(…)` workarounds; adding them only in code mode would leave JSON
  clients parsing ids. The text rendering is unchanged, so the cost is
  JSON bytes only; a single-repository answer reads the same.
- **`changed` per symbol.** A multi-ref symbol is `changed` when its span's
  bytes differ between the requested refs, or it is absent from one; equal
  blobs short-circuit and a changed file is read once per call. A symbol
  moved by lines added above it is listed per version (its lines differ)
  but not `changed`. `get_file_outline` marks each symbol of each version.
  This settles the price named in *Collapse across refs by file blob*
  (2026-11-16); collapsing stays by blob.
- **Tests nested in tests.** The resolver marks a definition nested under a
  test function, method or test block as a test (no new facts, so no
  extractor bump for it); a test class does not pass the flag on
  (`TestClient` is a library name too). The TypeScript pack marks
  definitions inside `beforeEach`-style hook callbacks (extractor version
  7). Checked with fixtures: TypeScript `describe` / hooks, Python nested
  functions in `test_*`, Rust nested functions in `#[test]` (fixed); Go
  `t.Run` closures (not definitions; their calls already belong to
  `Test…`). Java local classes in test code were already tests; C++ local
  classes and lambdas are not definitions. Indexes need a re-index.

Measured with the reference tool-call plans (every answer unchanged): ms25
reads 1,614 tokens instead of 11,625 (one outline at both refs instead of
the file twice); `trace` text shrinks where test code was nested (ms17
7,826 → 2,502, ms19 3,795 → 3,385, ms16 1,042 → 686, ms05 1,626 → 1,602).
`describe_api` grows to 4,260 (JavaScript) and 4,338 (Starlark) tokens.

Re-measured with the fixed engine (stores re-indexed):

- **Multi-step suite**: 18/25 correct in every Rueca arm, as before (no
  task changed correctness in any arm); tokens read per correct answer:
  plans 3,639 → 2,743, JavaScript 444 → 429, Starlark 448 → 434 (with
  `describe_api` amortised 671 → 666 / 680 → 675). The gate still passes:
  −84.4% / −84.2% against the plans (−87.8% / −87.7% before), −75.7% /
  −75.4% amortised; median overhead 0.63 / 0.33 ms. The cut shrank because
  the plans got cheaper.
- **Multi-ref scenario** (organization suite, first scored run): unchanged
  symbols of the changed file flagged `changed`: 6 with the engine before
  the fixes, 0 after; every changed symbol flagged, every unchanged file's
  symbol collapsed (168 / 168).
- **Structure (S3)**, Rueca arm re-measured on 21 of 30 sets (every set
  whose gold builds without Docker, except C++: its gold rebuilt here
  differs from the published one by two questions, so C++ stays the
  published run; its answers did not change either), compared with the
  engine just before the fixes on the same sets: `trace` drops the
  test-file callers nested in tests on four sets (hono callers returned
  1,068 → 767, click 575 → 422, flask 302 → 242, jinja 800 → 779; every
  dropped node is in a test file); every other set answers identically.
  Scored precision / recall do not move (the scorer already left test
  files out); the published C row moves by 0.1 point (Lua callers recall
  96.5 → 96.6), which predates the fixes (the engine before them gives the
  same answers). Java, PHP and C++ sets and every cbm / BM25 row are
  carried from the published results.
- **Tokens per true edge** (`trace`, Rueca vs cbm's published rows):
  TS/JS 64 → 57 (cbm 54), Python 57 → 55 (cbm 48); the engine before the
  fixes gives 64.1 and 56.8 on the same sets, so this is the fix. Other
  languages unchanged. The JSON diagnostic grows (`local`, `repo`, `ref`
  on every symbol: median trace JSON 373 → 440 tokens for Python); the
  text an agent reads only shrinks.
- **Localization (S1)** confirmed unmoved: the Rueca arm on all 169
  Python, TS/JS and Rust instances with both engines gives the same File
  Acc@k and Function Acc@k on every instance (six rankings differ below
  the top 10); S1 stays the published run.

## 2026-11-24 — Code mode phase 3: verdict and recommendation; the owner decides (RFC 0009)

Phase 3 of RFC 0009 is done: the multi-step suite (phase 2) and a fluency
probe give a per-language verdict (RFC 0009 §14). Nothing changes in the
code's defaults: code mode stays off unless `--experimental-code-mode` is
given, and Starlark stays the non-default `code-mode-starlark` feature.

- **Gate**: both languages pass. Tokens read per correct answer −87.8%
  (JavaScript) and −87.7% (Starlark) against the tool-call plans (−81.6% /
  −81.3% with `describe_api` amortised), no task regressed (18/25 in all
  three Rueca arms), median `execute` overhead 0.56 / 0.32 ms. Five
  listing-heavy tasks dominate the pooled ratio; without them the cut is
  76%. (Numbers before the API fixes; re-measured in the entry above.)
- **Fluency probe** (10 tasks × 2 languages, one fresh model per cell with
  only the statement and `describe_api`, n = 1): 9/10 correct on the first
  try in each language, no script error of any kind. The one failure
  (ms19, both languages) is a silent wrong answer from an ambiguous answer
  shape, which one repair did not fix. Model-written scripts were 2.5–3.2×
  longer than the reference scripts, so the suite's cut is an upper bound
  (76% / 81% on the probe's tasks).
- **Recommendation** (the owner decides): keep code mode opt-in and
  experimental with **JavaScript only**; remove Starlark (advisory policy
  failure, memory limit not enforceable in-process, +5.7 MB) unless the
  owner wants it kept as the non-default feature it is now; do not make
  code mode default-on before the API friction of RFC 0009 §14.4 is fixed,
  agents have a cheap way to surface evidence, and an agentic evaluation
  has run. No helpers were added.

## 2026-11-24 — Code mode phase 1: `execute` and `describe_api` behind a flag; Starlark opt-in (RFC 0009)

`rueca mcp --experimental-code-mode` (or `RUECA_EXPERIMENTAL_CODE_MODE=1`)
lists and answers `execute` and `describe_api` after the eight tools; off,
the tool list, schemas and answers are byte-identical to before (tested
against the golden transcripts). Service mode never enables it.

- **Starlark is a non-default cargo feature** (`code-mode-starlark` on the
  `rueca` binary and `rueca-mcp`, `starlark` on `rueca-codemode`, whose
  default is `["js"]`). The owner rejected ignoring the three
  "unmaintained" advisories of `starlark` 0.14.3's dependencies and
  loosening `deny.toml`; with the feature off, `cargo deny check` passes
  as CI runs it. A CI job keeps the feature building and tested; a build
  without it answers `language: "starlark"` with an error saying how to
  rebuild. This supersedes "both default-on" in the entry below.
- **The API is generated, results included.** `rueca_query::ToolDef` gains
  `output_shape` (a JSON Schema of each tool's `results`, never sent to MCP
  clients); `describe_api` builds the TypeScript declarations and the
  Starlark reference from the input schemas and these shapes, and a test
  checks real answers against the shapes both ways.
- **Scripts read `results`, renumbered.** A call resolves to its
  envelope's `results`; the snapshots of every call are merged into
  `execute`'s `snapshots` and each `s` / `in` is renumbered to them; a call
  the tool cut is reported in `partial`. Inside scripts a call's
  `budget_tokens` defaults to the maximum (its answer reaches the script,
  not the agent); `execute`'s `budget_tokens` cuts `result` (largest array
  first, then strings, then `null`); `timeout_ms` is clamped to 10 s.
- **`rueca-codemode` is published** with the other crates: `rueca-mcp`
  depends on it.

## 2026-11-24 — Code mode: both languages ship behind one `Sandbox` for the experiment (RFC 0009)

RFC 0009's experimental `execute` tool runs scripts in **both** JavaScript
(QuickJS via `rquickjs`) and Starlark (`starlark`) during the whole
experiment, behind one `Sandbox` trait in the new crate `rueca-codemode`
(cargo features `js` and `starlark`, both default-on while it runs).
`execute` and `describe_api` take `language` (`"js"` by default). The
language is not picked up front: phase 3 writes a per-language verdict
from the multi-step suite and a fluency probe, and recommends keeping
both, one, or neither (RFC 0009 §14); making code mode on-by-default is
the owner's decision.

Phase 0 (RFC 0009 §13): both engines start and call the host well within
the overhead budget (JS ~0.1 ms start, 2.5 µs per call; Starlark ~1.5 µs
start, 0.4 µs per call); JavaScript's time and memory limits are
enforceable in-process; Starlark's memory limit is not (a one-line
script can abort the process), and `starlark` 0.14.3 fails `cargo deny`
on three "unmaintained" advisories (`derivative`, `paste`, `fxhash`).
Both must be resolved before Starlark can reach `main`; the options are
listed in RFC 0009 §13.

## 2026-11-24 — Hybrid search, opt-in: the owner's defaults (RFC 0007 phase 1)

`rueca index --embed` (or `RUECA_EMBED=1`) embeds every callable, type and
Markdown section into a store vector cache and the segment;
`search_symbols` gains `mode` (`lexical` | `hybrid` | `semantic`) and the
envelope `retrieval`. The owner decided the defaults on the phase 0 / 1
evidence below; they are the code's defaults.

**The owner's decisions:**

- **Model and text**: Qwen3-Embedding-0.6B `Q8_0` (`qwen3-0.6b-q8`),
  canonical text `full`, token budget 512 (as in the phase 0 entry below).
- **Fusion**: a convex combination of the two scores, **min-max**
  normalized over each list's top N, **α = 0.5** (BM25's weight), **N =
  100** candidates per list: `s = (α·b̂ + (1 − α)·d̂) · prior · route`.
  RFC 0007 §7.2's division by the theoretical minimum stays a sweep variant
  (`normalization = "theoretical"`): cosines of unrelated code sit far above
  −1, so it squeezes the dense signal into a narrow band near 1.
- **Search: exhaustive int8 scan; HNSW deferred.** Evidence from a
  benchmark outside the repository (synthetic 1,024-dimension clustered
  vectors, 200 queries, top 10, M1 Pro, recall against exact f32):

  | Vectors | int8 scan (Rueca) | `usearch` HNSW, f16 | `hnsw_rs` |
  |---|---|---|---|
  | 10k | 0.26 ms, recall 0.99 | 0.15 ms | — |
  | 100k | 1.3 ms, recall 0.99 | 0.36 ms | build 137 s |
  | 1M | 8.3 ms, recall 0.99 | 0.6–0.8 ms, recall 0.61–0.81, build 16 min | — |

  The scan is exact, needs no build and fits immutable segments; embedding
  the query (about 40 ms) dominates the latency anyway. HNSW waits for
  organizations of several million vectors in service mode.
- **Opt-in locally.** Semantic search is off unless an argument or the
  environment enables it: `rueca index --embed[=PRESET]`, or `RUECA_EMBED=1`
  (`true`, `on`, `yes`) / `RUECA_EMBED=<preset>`; `RUECA_EMBED` unset,
  empty, `0`, `false`, `off` or `no` leaves it off. **Precedence: argument >
  environment > configuration**: `--no-embed` beats `RUECA_EMBED=1`,
  `--embed` beats `RUECA_EMBED=0`, and `[retrieval]` in `<store>/rueca.toml`
  only chooses the preset and parameters — it never enables embedding by
  itself (the owner's wording: "off unless enabled by argument or
  environment variable"). The preset, once enabled: `--embed=PRESET`, then
  `RUECA_EMBED=<preset>`, then `RUECA_EMBED_PRESET`, then `[retrieval]
  preset`, then `qwen3-0.6b-q8`. Without enablement nothing is downloaded
  or embedded and every answer is today's lexical one (plus `retrieval:
  {"mode": "lexical"}` in the envelope).
- **Missing weights**: an enabled `rueca index` without the weights is an
  error naming `rueca models pull`, raised before anything is published;
  indexing never downloads (only `rueca models pull` does), so enabling it
  cannot start a 639 MB download by surprise.
- **Queries**: hybrid automatically when a selected snapshot has vectors and
  its model loads, else lexical, with the reason in `retrieval.warnings`;
  an explicit `hybrid` / `semantic` that cannot be served is an error
  naming `rueca index --embed` or `rueca models pull`. Query time never
  downloads weights. `retrieval` names the mode used and the model (preset
  and model-key prefix); `rueca status` names the preset that embedded each
  snapshot (`VECTORS`).
- **Release binaries** carry the runtime (feature `embeddings`); it runs
  only when enabled. Service mode turns it on later: `rueca serve`'s
  indexers, `rueca index --org` and the MCP server's auto-index stay
  lexical for now.
- **Rejected**: Model2Vec / static embeddings, int4 as the default, tiny
  encoders as the default (none passed the bar).

**Evidence** (S1 calibration split, 118 instances, File Acc@5 / Function
Acc@10, paired against BM25):

| Query | BM25 | Rueca lexical | Hybrid, Qwen3-0.6B, α 0.5 | Hybrid, granite-47m |
|---|---|---|---|---|
| Issue text (`raw`) | 78.8 / 57.9 | 73.7 / 61.7 | **83.1 / 70.1** | 83.1 / 64.5 |
| Keywords (`kw`) | 72.9 / 57.9 | 75.4 / 53.3 | 73.7 / 56.1 | — |

Hybrid − BM25 on `raw`: Function Acc@10 +12.1 (95% interval +2.8 … +21.5),
File Acc@5 +4.2 (−2.5 … +11.0). Semantic search helps natural-language
questions, not keyword ones. **Cost**: indexing Rueca itself (9,319
symbols) took 400 s on an M1 Pro (Metal), about 23 symbols/s; a CLI query
about 0.25 s including the model load.

**How it is built** (choices and deviations from the RFC):

- **Publication**: lexical first, then a copy of the bundle under a new id
  with the `vectors.*` components, swapped in the catalog (RFC §5.6); one
  publication when every vector is cached. Appending to the published file
  was rejected: segments are immutable and mapped by readers.
- **Cache layout**: `<store>/vectors/<embed key>/` (the embed key, not the
  model key: entries are full-width f16, shared by every width and
  encoding), 256 shards of append-only `.keys` / `.f16` pairs with a
  per-shard `flock`, rows written before keys, lock-free readers, repair by
  truncation — instead of RFC §5.5's pack files plus an index with one
  writer per store, so concurrent indexers of one store (RFC 0008) share
  it. Reused across branches, refs, worktrees and repositories; no
  collection of unreferenced entries yet.
- **Lazy checksums**: `vectors.meta` and `vectors.nodes` at open, `keys`,
  `scale`, `data` on the first dense use in a process (RFC §6.5).
- **Snapshots without vectors** in a hybrid answer are scored `b̂ · prior ·
  route` (RFC §10) and named in `retrieval.lexical` by repository id;
  `semantic` leaves them out with a warning. A failed model load is
  remembered for the process (the MCP server needs a restart after
  `rueca models pull`).
- **Text builder**: version 1, the qualified name is Rueca's global one,
  the doc the extracted docstring when there is one, the cut `max_tokens ×
  4` characters before the model's token cut.
- **Tests** inject the weight-free `HashingEmbedder` through
  `index_worktree_with_embedder` / `index_ref_with_embedder` and
  `Engine::with_query_embedder`; nothing in the binary selects a test
  embedder. The real-weights test runs only with `RUECA_EMBED_TESTS=1`.
- **`rueca query --batch`** is not in this landing: the bench's hybrid arm
  runs one `rueca query` per call (the model load is part of the 0.25 s).

**Not in phase 1** (follow-ups, on branches): graph expansion, the
centrality prior and the reranker (phase 2), organization scope, centroid
routing, remote presets and `rueca query --batch` (phase 3) —
`feat/semantic-search-next`; small-encoder presets, text profiles and the
units filter — `feat/semantic-static`.

## 2026-11-24 — Embedding runtime: default model, canonical text, token budget, llama.cpp (RFC 0007 phase 0)

The owner decided the embedding defaults of RFC 0007 on the phase 0
evidence; `rueca-embed`, `rueca models` / `rueca embed` and the bench's
`vector` arm land with them. Semantic search stays opt-in: nothing
downloads or embeds unless the user runs `rueca models pull` or `rueca
embed`.

- **Default model: Qwen3-Embedding-0.6B `Q8_0`**, preset `qwen3-0.6b-q8`:
  the official GGUF file pinned by revision and sha256
  (`06507c7b…e439`, 639,150,592 bytes), 1,024 dimensions. The preset table
  holds the Qwen3-Embedding family only (`qwen3-0.6b-q8`, `qwen3-0.6b-f16`,
  `qwen3-4b-q4` / `-q8`, `qwen3-8b-q4` / `-q8`); another model is one more
  entry.
- **Canonical text: `full`** (RFC 0007 §5.2: language, label and
  qualified name; path; signature; doc comment; body head).
- **Token budget: 512** per document. Queries keep their own budget, 512
  capped by the preset's context, whatever the documents' budget (not
  keyed: no query vector is stored).
- **Runtime: llama.cpp through `llama-cpp-2`** (pinned `=0.1.161`, RFC
  0007 §4.2), behind the `embeddings` feature, on by default in `rueca`, so
  release binaries carry it; it runs only when one of the commands above is
  used. It builds llama.cpp from source (CMake, C/C++, libclang), which CI
  does; `--no-default-features` gives a lexical-only binary.
- **Default query instruction**: the issue-shaped one ("Given a GitHub
  issue, retrieve the source code functions that must change to fix it"),
  chosen by the phase 0 instruction sweep on the calibration split.
  Documents carry no instruction, so it is in no key.
- **Rejected by the owner:** Model2Vec / static embeddings; int4 as the
  default; tiny encoders as the default (none passed the bar).

**Evidence** (S1 calibration split, 118 instances, File Acc@5 / Function
Acc@10, issue text `raw`): the BM25 arm scores 78.8 / 57.9; the vector and
hybrid rows are recorded with hybrid search (RFC 0007 phase 1). **Cost**
with the default: indexing Rueca itself (9,319 symbols) took 400 s on an M1
Pro (Metal), about 23 symbols/s; a CLI query took about 0.25 s including
the model load.

**Not in this landing** (kept on the branches `feat/semantic-search-next`
and `feat/semantic-static`): the CoIR runner, the SWE-bench Lite
diagnostic, encoder and small-encoder presets, the reranker, remote
presets, text profiles.

Also: `deny.toml` lists the four shipped targets under `[graph]` (the
platform verifier of the weights download bundles root certificates only
on wasm32), and `url` moves to 2.5.9 for RUSTSEC-2026-0592.

## 2026-11-18 — Rust: a `use` leaf before its module; the bench after the Rust fixes

hyper 0.14 writes `pub use self::to_bytes::to_bytes;` before `mod
to_bytes;`. A module lookup stopped at the first binding of the name, the
`use` leaf, whose own path needs the module (a cycle, so not found), and
gave up: `hyper::body::to_bytes(…)` stayed unresolved (12 labelled axum →
hyper uses). A binding whose target is not found no longer hides a later
one; the lookup still gives up when no binding of the name is found.

Bench with the branch binary (Rust arm only; other languages carried):

- **Cross-repository edges** (RFC 0006 §8): 12 pairs 98.3 / 74.9 → 99.4 /
  94.7; without Rust 99.8 / 96.2 → 100.0 / 96.2 (five wrong carbon edges
  gone: edges written in a dependency's file); Rust alone 94.5 / 53.2 →
  98.3 / 93.1. Per pair, P / R: axum → tokio 92.9 / 14.1 → 98.0 / 95.6,
  axum → hyper 100 / 54.4 → 90.4 / 94.7, axum → tower 100 / 82.7 → 100 /
  89.8, tokio → mio 81.0 / 87.5 → 99.4 / 93.8. The 19 Rust edges counted
  wrong were read one by one: methods of values a library call returned
  (`Server::bind(..).serve(..)`, `Client::builder().build(..)`,
  `tokio::spawn(..).abort()`, `rt.block_on`), all right, which the labeler
  cannot see. Of the 36 Rust uses still missed, 20 are methods of tower's
  `ServiceExt` (blanket `impl<T: Service> ServiceExt for T`).
- **S3 Rust** (serde_json + ripgrep): callers / callees precision 98.5 /
  98.0 unchanged, recall 87.8 / 87.1 → 86.8 / 87.1 (inside the interval;
  `rueca-bench check` passes). Every lost edge (44, ripgrep) touches
  `grep-searcher`'s `testutil.rs`, declared `#[cfg(test)] mod testutil;`:
  now test code, so the default `trace` leaves it out, while the S3 gold
  only skips test paths and inline `#[cfg(test)]` items. Rust micro F1
  99.2 unchanged; S1 Rust File Acc@5 62.8, Function Acc@10 39.0
  unchanged.
- **tokio-rs demo** (8 repositories, published binary → branch): link
  edges 6,199 → 9,550 (847 `DECORATES` from `tokio_macros`); `get_symbol`
  finds `TcpListener`, `UdpSocket`, `spawn`, `AsyncReadExt` (none before);
  callers of `sleep` / `timeout` / `interval` 0 / 0 / 0 → 32 / 12 / 4,
  `tokio::spawn` 0 → 92 (514 with tests), `TcpListener.accept` 0 → 34;
  hyper's `Bytes.from` as a call target 29 → 0; tokio → mio 200 → 158
  edges, its 24 `type_resolved` calls all right (`self.registry`,
  `self.poll`, `mio::event::Event`, `SourceFd`); edges written in a
  dependency's file 19 (13 wrong) → 13 `IMPLEMENTS`, all `impl LocalTrait
  for ForeignType`. Index 7.4 s → 8.0 s, store 134 → 138 MB. Re-indexing
  the unchanged organization still replaces all 8 segments (the segment id
  holds the time and a random suffix, `rueca-segment/src/writer.rs`): left
  open.

## 2026-11-17 — Rust: value and type namespaces; types bind in scope, another crate's by name only as a guess; test doubles never bind

The same tokio-rs demo, after the pack fix above, still showed resolver
gaps. `tokio::time::sleep`, `timeout`, `interval`, `tokio::spawn` and
`tokio::signal::ctrl_c` had no callers in the organization: tokio writes
`mod sleep; pub use sleep::sleep;`, and a lookup of `sleep` in
`tokio::time` bound the module, so every call was external. And in the
joint resolve of a consumer with its dependencies, types were found by name
in other crates: `impl AsRawHandle for CompletionPort` in mio (the trait
comes from a glob of `std`) bound tokio's documentation stub of the trait,
and `Bytes::from(…)` in tonic and axum called hyper's `impl
From<ReasonPhrase> for Bytes`, the first impl attached to `Bytes`.

- **Two namespaces.** A Rust lookup is for the type namespace (modules,
  types: path segments, type references) or the value namespace
  (functions, constants, statics, tuple / unit structs, variants: calls and
  reads). Among a module's bindings, in their existing order, it takes the
  first of its namespace, else the first of the other one, so nothing that
  resolved before stops resolving. A `use` leaf binds both, so the
  resolver keeps a second binding environment for values where they
  differ; a bare name prefers an import of its namespace, then an item of
  the module of its namespace (`mod x;` + `fn x`: `x()` is the function,
  `x::Y` the module). `IMPORTS` of a leaf go to its type, else its value
  when the type namespace has only a module. Rejected: a namespace per
  definition in the pack (the resolver already knows labels; a tuple
  struct is both).
- **Types bind in scope.** By name, a Rust type is first looked for in the
  file's own crate; another crate's type of that name is used only when the
  own crate has none, and then it is a guess: members found through it are
  `heuristic` 0.50 (not `type_resolved`), so the link layer drops them as
  it drops every name match, and a guessed trait or self type never gets an
  `impl`'s items, `IMPLEMENTS` or `OVERRIDES`. A call result typed from a
  callee of another crate matched by name is a guess the same way.
  Rejected: no cross-crate match at all — inside one repository, test and
  bench crates whose import of the package's library does not resolve
  (bytes' bench crate `bytes` shadows the library) lost 30–40 calls each.
- **Own impls first.** When several crates' impls give a type members of
  one name, the type's own crate's come first (`bytes`'s `Bytes::from`,
  tokio's `TcpListener::local_addr` over axum's `impl Listener for
  TcpListener`).
- **Test doubles never bind.** A module file declared by `#[cfg(test)] mod
  x;` (or `all(test, …)`; not `any(test, …)`, also built without tests) is
  test code: the pack adds a `$test` marker import after the `mod`
  import (the file alone cannot know), and the resolver marks the file's
  definitions, and those of the files below it, `is_test` (out of by-name
  matching, never link targets). A module lookup takes a test binding only
  when nothing else binds the name (`#[cfg(test)] use mocks::spawn_blocking;`
  next to the real import). One definition twice (`#[cfg]` variants, same
  label and qualified name) counts once in by-name matches.
- **Globs deeper than the environment.** A bare name that nothing binds
  is looked up through the file's globs (`use crate::helpers::*;` over
  `pub use self::client::*;`), which the binding environment holds one
  level deep.
- **Link layer**: an edge whose evidence is in a dependency's file is
  dropped (a dependency cannot name its consumer). `LINKER_VERSION` 3.

Measured on the tokio-rs demo (8 repositories, `checks`): callers of
`tokio::time::sleep` 0 → 32, `timeout` 0 → 12, `interval` 0 → 4 (tests
excluded, default budget); with tests and the whole budget, 80 of the 87
`time::sleep(` lines of axum, hyper, tonic, tower and tracing lie in a
resolved caller (the rest: doc comments, `select!` arms), 77 of 77
`time::timeout(`, 4 of 4 `time::interval(`; `tokio::spawn` 339 of 355 (0
before), `signal::ctrl_c` 2 of 2. The four mio `CompletionPort` edges into
tokio's documentation stubs and the 29 calls of hyper's `Bytes.from` are
gone (the 29 now reach `bytes`'s own `Bytes.from`). tokio → mio stays at
158 edges, all right (the pack fix had removed the wrong ones: 200 with
the published binary). Link edges 8,540 → 9,550. Single repositories:
tokio references resolved 48.3 % → 50.2 %, tracing 49.5 % → 53.8 %;
callers that now are test code (tonic's `#[cfg(test)] mod xds_e2e;`)
leave the non-test counts (`watch::channel` 18 → 17).

## 2026-11-17 — Rust: items inside macro invocation bodies; attribute macros are decorators

A demo over the tokio-rs repositories showed `tokio::spawn`,
`TcpListener`, `UdpSocket` and `AsyncReadExt` missing: tokio declares
them inside pass-through macros (`cfg_net! { pub mod tcp; pub use
tcp::listener::TcpListener; }`, `cfg_rt! { … }`, `cfg_if! { if #[cfg(…)]
{ … } else { … } }`), which tree-sitter keeps as opaque token trees.

- **Opened in place, same length.** An invocation in item or statement
  position whose body parses on its own as items (a `cfg_if!`-shaped body:
  every branch) has its delimiters blanked and keeps an empty body
  (`cfg_net!{}`), before the file is walked; newlines stay, so offsets,
  lines and texts are the file's, as in the C/C++ pack's same-length
  preprocessor. Its definitions, `use` / `pub use` and `mod` declarations
  are then those of the enclosing file, inline module or function, with
  every existing rule (prepass, local scopes, receiver typing) unchanged,
  and the invocation is still a call of the macro. Rounds repeat for nested
  macros (≤ 4); a round that adds parse errors is dropped. Rejected: a
  separate parse of each body mapped back by an offset, like macro
  arguments: every item rule, the prepass and type inference would have had
  to read a second tree. `#[cfg]` stays ignored, so `cfg_if!` branches may
  define one name twice, as `#[cfg]` items already did.
- **`mod x;` inside inline modules** (`mod net { mod tcp; }`) is the file
  below their directories (`net/tcp.rs`), with or without a file for them.
- **Attribute macros are decorators.** An attribute that is not built in
  (`#[tokio::main]`, `#[tracing::instrument]`, `#[rstest]`) is a
  `Decorator` reference of the item (path qualifier normalized like code
  paths) and its text a `decorators` entry; the resolver's `DECORATES`
  goes from the `#[proc_macro_attribute] pub fn` to the item, through the
  crate's re-exports. Built-in and tool attributes and derive helpers
  (`#[serde(…)]` next to a `#[derive]`) are not references. The micro
  answer keys are `CALLS`-only, so no micro snippet.
- Extractor version 3: cached Rust facts are extracted again.

Measured on tokio 1.54.1 (`tokio/src`, 380 files): definitions 6,245 →
7,002, exported 3,921 → 4,459, imports 3,657 → 4,267; `pub` item lines
with no definition or import at their line 622 → 52 of 3,349 (the rest are
`macro_rules!` bodies, comments and bodies in a macro's own syntax);
parse errors 0 → 0. Whole repository: 15,475 → 16,443 nodes, 44,723 →
54,835 edges, references resolved 39.3 % → 48.3 % (26,290 / 66,823 →
32,764 / 67,775). `get_symbol` finds `TcpListener`, `UdpSocket`, `spawn`,
`AsyncReadExt` and `AsyncReadExt.read`, and `TcpListener.accept` gets its
callers through `use tokio::net::TcpListener` (`type_resolved`). In the
tokio-rs organization, 847 link edges are `DECORATES` from
`tokio_macros::main` / `test` into hyper, tonic, axum, tower and tracing.
Still open for the resolver: a module and a function with one name
(`mod spawn; pub use spawn::spawn;`) bind the module, so `tokio::spawn(…)`
calls stay unresolved.

## 2026-11-18 — Indexer role: leased job objects, child processes per ref, webhooks as hints

RFC 0008 phase 2. Indexing is level-triggered: a reconciler holds the
desired commit of every configured ref from webhooks, adaptive
`ls-remote` polling and admin requests, and writes one job object per
repository in the bucket; indexers lease jobs by compare-and-swap (15 min
leases with epochs, stealable when expired), fetch exact commits into a
bare mirror with `--depth=1 --filter=blob:limit`, export from the mirror
and index each ref in a `rueca index-worker` child under `RLIMIT_AS`,
`RLIMIT_CPU` and a wall-clock kill, so a parser that loops or explodes
fails only its ref; the job fails as a whole only when no ref reached its
desired commit. Webhooks (GitHub, GitLab incl. Standard Webhooks,
Bitbucket) are verified, deduplicated by delivery id and confirmed with
`ls-remote` before they move the desired state; polling keeps
correctness. One updater, elected by a lease object, publishes
generations, collects garbage and expires pull requests by close and by
idle reads that replicas record hourly. Credentials reach `git` only
through `GIT_CONFIG_*` environment variables. `file://` and `git://`
hosts are accepted by debug builds only, for tests.

## 2026-11-17 — Service query role: Rueca's own HTTP transport and auth layer, governor over tower_governor

RFC 0008 phase 1. `rueca serve` answers MCP over a stateless `POST /mcp`
on `axum`, calling the own MCP layer's `handle` once per request with a
fresh or a legacy `Session` (no sessions, no SSE, JSON only), so any
replica answers any request. The resource-server side of MCP
authorization is Rueca's: static API tokens hashed in the file and OIDC
bearer JWTs validated against the issuer's JWKS (`jsonwebtoken` with the
pure-Rust `rust_crypto` backend; no aws-lc-sys), protected resource
metadata, 401 / 403 challenges; a principal maps to the existing
`Visibility` by configured rules, deny by default, and the engine runs
every call with that visibility and a cooperative deadline
(`Engine::respond_with`, checked inside the search, trace, scan and
collapse loops). `governor` is used directly rather than
`tower_governor`, whose IP key extractor trusts `X-Forwarded-For` from
any peer; forwarding headers count only from `[http].trusted_proxies`.
Prometheus text is rendered from the app's own `metrics` registry;
OpenTelemetry is an optional feature. The image is distroless with `git`
copied in; it has no shell or ssh. Verified: byte-equal answers against
local mode, Claude Code over HTTP, sub-3 ms server p95 on the small
organization in a debug build.

## 2026-11-17 — Service store: rusty-s3 and own backends instead of object_store

RFC 0008 phase 0. `object_store` 0.14.2, the crate RFC 0008 §5.10 chose,
fails the required `cargo deny` check: `chrono` 0.4.45
(RUSTSEC-2026-0543) and `humantime` 2.4.0 (RUSTSEC-2026-0542), both
unmaintained with no patched release, are unconditional dependencies
even of its in-memory backend, and its cloud features pull
`webpki-root-certs` (CDLA-Permissive-2.0, not an allowed license)
through `reqwest/rustls`. Ignoring the advisories was rejected, as for
`rmcp`; OpenDAL needs MSRV 1.91 (Rueca: 1.90). Chosen: `rueca-storage`
keeps the RFC's `Storage` trait and compare-and-swap semantics, with
in-memory and `file://` backends of its own (the file backend locks
`<root>/.lock` and uses BLAKE3 e-tags) and an S3 backend assembled from
`rusty-s3` 0.11 (sans-IO SigV4 signing and request building, dates from
`jiff`) over `ureq` 3.5 with `rustls` and the platform's native roots
(`rustls-native-certs`). Request signing is not written in-house. GCS is
served through its S3-compatible XML API when the generation headers can
be signed; Azure Blob is deferred. The RFC's §5.10 carries the amendment.

## 2026-11-17 — Own MCP layer instead of rmcp

The required `cargo deny` check failed on main with RUSTSEC-2026-0543:
`chrono` is unmaintained and no patched release exists. `chrono` reached the
dependency graph only through `rmcp` 3.10, directly and through `rmcp`'s
`schemars` dependency with feature `chrono04`; upstream
(modelcontextprotocol/rust-sdk#1469) had no plan to drop it.

Rejected:

- Ignoring the advisory in `deny.toml`: it keeps an unmaintained crate in
  the graph with no exit.
- Patching or forking `rmcp`: a fork of a fast-moving SDK (releases every 1–3
  weeks) to maintain, for a server that uses a small part of it.

Built: Rueca's own MCP layer in `rueca-mcp`, about 1.3k lines (without
tests), standard library only:

- JSON-RPC 2.0 framing, one message per line (`parse_line`, `Outgoing`);
- lifecycle A, legacy `initialize` for 2024-11-05, 2025-03-26, 2025-06-18
  and 2025-11-25; lifecycle B, the stateless 2026-07-28 one with
  `server/discover` and per-request `_meta`;
- `tools/list`, `tools/call`, `ping`, cancellation;
- a transport-agnostic seam, `RuecaServer::handle(msg, &mut Session) ->
  Step`, which RFC 0008's HTTP transport calls with one message per request.

Equivalence was proven with golden transcripts recorded from the `rmcp`
server (9 files, 20 sessions, 131 steps) and replayed against the new layer
byte for byte. Five differences were accepted on purpose:

1. A notification, a client response or EOF before any lifecycle is ignored;
   `rmcp` exited 1.
2. Malformed `tools/call` params get -32602; `rmcp` answered -32601, an
   artifact of its fallback.
3. A second `initialize` keeps the negotiated version; `rmcp` stored the raw
   requested one, which could turn a legacy session modern.
4. A panic in a tool call is reported as `internal error in <tool>`, without
   `rmcp`'s per-run tokio task id.
5. Past 512 concurrent tool calls the reader waits for a slot; `rmcp` queued
   them on tokio's blocking pool. Only latency changes.

Cost: Rueca owns the protocol, so new spec revisions are ours to implement.
Saved: no async runtime in the server (`tokio`, `futures`, `sha2`, `chrono`,
`rmcp` and 20 other packages left `Cargo.lock`, 387 → 362), 302 → 280
distinct crates in `rueca`'s tree, and a release binary 1.67 MB smaller.
The behaviour reference is [`docs/mcp-protocol.md`](mcp-protocol.md).

## 2026-11-17 — Contracts in the organization link layer (RFC 0006 phase 4)

RFC 0006 phase 4. Contracts are **facts**, as RFC 0001 §6.4 planned: each
pack emits `ContractHint`s (provides / consumes, HTTP / RPC / topic, a
canonical key, the raw text, the owner definition, the framework), and
`FACTS_FORMAT` goes to 3. Keys are canonical so matching is string work:
HTTP `METHOD /path` with parameter segments as `{}`, the base URL
stripped and `*` for an unknown method; RPC `pkg.Service/Method`, with
package-less and `*/Method` keys from code that knows less. A code route
is a row on its handler, not a route node: the handler is what an agent
reads, one hop from the client. `.proto` files get a pack over
`protox-parse` 0.10 (pure Rust, no `protoc` to install; MIT OR
Apache-2.0, in the `deny.toml` allow list); OpenAPI operations, already
`Route` nodes, provide their keys. Spec and code providers are both `CALLS` targets and
are matched separately, so a call into a specified service is not
ambiguous between the handler and the operation it implements; the
handler gets `IMPLEMENTS` to the operation. Matching requires equal
segment counts, with no prefix- or suffix-tolerant fallback: it would
recover mount prefixes composed in other files, at the price of turning
short routes into providers for many calls; the gap is recorded and
measured as recall. Providers that are tests or in test files are never
match candidates, inside one snapshot or across repositories, as package
edges never point into a dependency's tests.
Across repositories, contract keys go into each repository's `RepoDeps`,
a match makes a dependency, and the edges come from the segments' contract
tables alone (no joint resolve) into the same link sets as package edges,
so refs, versioning, recomputation and visibility needed nothing new
(`LINKER_VERSION` 2). S2 (*Cross-service edges*) is a gated suite with the
edge floors, precision ≥ 90 / recall ≥ 75, targets 97 / 90, per kind, on
ollama, qdrant, dapr and weaviate (openai diagnostic: its specs exceed the
2 MiB extraction budget). `list_contracts` (CLI `rueca contracts`) is the
eighth tool: RFC 0001 §11.4 lists it (provided / consumed / dangling), and
dangling calls — the question "what calls an endpoint nobody provides" —
cannot be asked of `trace`, which only follows edges that exist.
## 2026-11-17 — Service mode: object storage as the store of record, compare-and-swap instead of a lock

RFC 0008 (design; nothing implemented). Service mode is the same crates
run as one image with roles (`rueca serve --role query|indexer|all`):
stateless query replicas that download segments into a disk cache, and
indexers that lease jobs. The store of record is object storage through
`object_store` 0.14 (S3, GCS, Azure, local; OpenDAL rejected: MSRV 1.91
and no GCS generation compare-and-swap), with the local layout plus two
changes: a catalog per repository and packs per repository, so a
snapshot lists the packs it reads and `index.rcpi` has no bucket form.
Immutable objects are written with `PutMode::Create`, the few mutable
ones (catalogs, the generation pointer, job and lease objects) with
`PutMode::Update` in a retry loop; no lock and no database (RFC 0006
§12 open question 9 closed; Postgres kept as a phase 4 option), and
local mode gets an advisory file lock. Garbage collection is
mark-and-sweep with a 24 h grace and per-kind retention (pull requests
expire after close + 7 days or 14 days unread). MCP is served over
Streamable HTTP by Rueca's own transport on `axum`, stateless and
JSON-only — not `rmcp`'s server transport: `cargo deny` flagged `chrono`
(RUSTSEC-2026-0543), reached only through `rmcp`, and the owner chose a
minimal MCP layer of Rueca's own over ignoring the advisory;
authentication is a Rueca tower layer (OAuth 2.1 resource server for
external OIDC issuers, plus static API tokens; no embedded authorization
server), and a principal maps to the existing `Visibility` by configured
rules, deny by default, the seam Org mode's mirrored grants will fill.
Indexing is level-triggered after Zoekt: webhooks are hints, adaptive
`ls-remote` polling (45 s – 8 h) is the safety net, one coalesced job
object per repository, refs fetched with `--depth=1
--filter=blob:limit=…` from a bare mirror and read under
`GIT_NO_LAZY_FETCH=1`, each ref indexed in a child process with resource
limits. Three corrections to RFC 0001 §14 / RFC 0002 §7.1 were found on
the way: Zoekt uses `blob:limit`, not `blob:none`; `GIT_NO_LAZY_FETCH`
needs git 2.45, not 2.49; Bitbucket Cloud has supported clone filters
since 2024-08-08. One organization per deployment. Hotcache and delta
segments are deferred to a measured need.

## 2026-11-16 — Refs are snapshots of their own, exported from the object database

RFC 0006 phase 3. A branch, pull request, tag or commit indexed with
`rueca index [PATH] --ref <name>` (or a member's `refs = [...]`) is one
store root, one segment and one catalog entry under the repository's id,
exactly like the working tree: its tree is written blob by blob from the
object database with `gix` into `<store>/refs/<repo>/<ref>/` and indexed
there. Not `git worktree` (it writes into the user's repository, locks the
branch and leaves state behind), not `git archive` (`export-ignore`,
`export-subst` and content filters change files and their blob ids, which
loses facts-cache hits and makes the snapshot lie about the commit), and
not delta segments over the base (RFC 0001's plan, which needs blob
visibility in every reader; left to service mode). Measured on
`pallets/click` (debug build): the second ref re-extracts 52 of 156 files,
13 for the `stable` branch; the segment write is not shared, so its index
time stays 0.9–1.05× the first's. `pr/<n>` is fetched into
`refs/rueca/pr/<n>`, the only kind of ref Rueca writes.

## 2026-11-16 — A ref view is a selection of segments of the one organization index

RFC 0006 phase 3. The organization index lists every repository's default
**and** ref segments; a call selects one (or, with a `ref` list, several)
per repository and BM25 statistics, routing and visibility are computed
over the selection, which the phase 1 collector already did. RFC 0006 §4's
"organization statistics with A filtered out plus A's ref scored through a
second `Bm25StatisticsProvider`" is that selection; a separate overlay
would have been a second scoring path to keep equal to the first.
`repo: A, ref: X` reads A at X and every other repository at its default;
`ref: X` without `repo` reads every repository that has X at it and the
others at their default (the organization "as of" a release branch); with
`repo`, a selected repository without the ref is an error listing its
indexed refs. Measured: reading a ref costs what reading the default does
(99 vs 97 ms for an organization search, debug CLI).

## 2026-11-16 — Link sets per ref against the defaults, remapped on read

RFC 0006 phase 3. A ref's outgoing link set is computed when it is indexed,
its imports resolved jointly against its dependencies' **default**
entries, and versioned like a default's; a ref never re-links another
repository. Edges into a repository read at a ref are remapped on read to
the ref's nodes by (label, qualified name, path), through a
correspondence built per (repository, ref) on first use, and dropped when
the ref no longer has the target (click `7.1.2` drops flask's call to
`Context.with_resource`, added in click 8.0). Resolving imports on the fly
at query time (§4's plan) was rejected: it runs the joint resolver inside a
query. Measured: the organization layer per added ref is 0.18–0.63× the
ref's own indexing (floor ≤ 2×), linking being its largest part.

## 2026-11-16 — Collapse across refs by file blob

RFC 0005 §6 over refs: a symbol is matched across the snapshots of one
repository by (label, qualified name, path) and its versions are compared
by file blob. Facts are extracted per blob, so an equal blob is an equal
symbol for certain and costs no comparison; the price is that a symbol of
a changed file is `changed` even when its own text is the same. Hashing
each symbol's span was left open (RFC 0006 §12). `search_symbols`
collapses before the `limit`; `get_symbol` and `get_file_outline` list
other versions in `versions`; `trace` returns the union graph with `in`
on nodes and a trailing `{in, changed}` on edges. Measured (10 queries,
`limit` 50): 473 items for 648 per-ref items on click `main` / `8.5.1`,
500 for 678 on flask `main` / `pr/5918`.

## 2026-11-16 — Organization suite: pinned members, gold at the pin, BM25 as the floor arm

The organization scenario had 21 instances, one per repository: one
instance was 4.8 points and, with the discordance measured today, a paired
File Acc@5 comparison could not detect less than 29.0 points. It is rebuilt to be big enough to trust. Members are
every gated S1 repository (42), each **pinned** at the base commit of its
first gated S1 instance and indexed one at a time into one organization;
the calibration organization adds the 8 calibration members pinned the
same way, the gated members as distractors. **Issue** instances: per
member the first 3 (calibration: 5) S1 instances whose gold — files and
functions of the real fix patch — is present at the pin, the gold
**carried to the pin** (functions re-resolved there), the issue's keyword
query unchanged. An instance whose fix is already at the pin
(`patch_at_pin == reverse`) is left out: its question has no answer to
find. Gated: 120 issue instances over 42 members (`patch_at_pin` base 42 /
applies 22 / neither 56); calibration: 29, never gated.

A second kind, **impact**: an issue of a library member (tokio for axum;
carbon, monolog, commonmark for laravel/framework; gson for dubbo and
apollo) scored, besides its own gold, on the consumer files that use a
symbol its fix changes, as labelled at the pins by the independent
cross-repository labeler — 16 gated (carbon 6, monolog 4, gson 3, tokio
3, commonmark 0); consumer hit@10 / hit@20 / recall@20. The opposite
direction, **spanning** (a consumer's issue whose answer includes the
library symbols named in its patch), was built and measured: it yields
only 2 instances, both trivial (`tokio_macros::test`), so it is not used;
kept as a documented negative result (`spanning.py`).

Arms: Rueca natively (`--org`, one ranking) and per repository merged by
rank fusion; **BM25 over the union of the members** (one ranking with the
union's statistics, and per repository + rank fusion) as the plain-text
floor arm, as BM25 is for S1; cbm per project + rank fusion. *Single* is
each arm's own-repository answer in the same run. Every rate carries a
Wilson interval; paired comparisons (Rueca − BM25 and the others) a
paired bootstrap (10,000 resamples, the `[scorecard]` seed), the
discordance d and the minimum detectable effect at 80% power, MDE ≈ (1.96
+ 0.84) · √(d / n), also stated for the old n = 21. The 21-instance
numbers are another population: dropped, not carried, not compared.
Cross-repository edges grow to 12 labelled pairs (Flask → Werkzeug, Axum →
Tokio / Hyper / Tower, Tokio → Mio added; a Rust labeler over Cargo
packages and module trees) and are pooled over all pairs, without Rust and
Rust alone, so the Rust gaps do not hide in an average.

Measured (gated, `docs/bench/alpha-results.md`): File Acc@5 56.7 vs 66.7
single (0.85×, 0.74–0.96: at the floor, interval crossing it); Rueca −
BM25 +4.2 (−4.2…+12.5, MDE 12.1) on File Acc@5 and +11.3 (+4.3…+19.1, MDE
11.2) on Function Acc@10; tokens per correct answer 1.29× single;
calibration 0.78×. Impact: consumer hit@10 12.5% vs 37.5% for BM25 —
the organization ranking does not surface consumers of a changed library
symbol. Cross-repository edges: 98.3 / 74.9 all, 99.8 / 96.2 without
Rust, 94.5 / 53.2 Rust alone; the Rust gaps (attribute macros, items
inside macro invocation bodies, method resolution by type name across
crates, Cargo dependencies matched by name only) are findings about
Rueca, listed in RFC 0006 §9.2 and §12. cbm (per project + rank fusion, no
cross-project search): File Acc@5 11.7 against 56.7 alone; Rueca − cbm
+45.0 (+35.8…+54.2).

## 2026-11-16 — Semantic search: top-tier embeddings in presets, vectors by content hash, convex fusion, reranker gated by measurement

[RFC 0007](rfcs/0007-semantic-search.md): semantic retrieval is a mode of
`search_symbols` (and of the organization ranking), not a new tool;
`search_code` is untouched. Static embeddings (`model2vec-rs`, RFC 0002
§6.2) are dropped: embedding quality must be top-tier. Models come as
presets — Qwen3-Embedding 0.6B / 4B / 8B in several quantizations,
CodeRankEmbed, opt-in `remote:` APIs — downloaded on first use and
sha256-pinned, never in the binary; Rueca hosts the conversions Qwen does
not publish. Runtime: llama.cpp through `llama-cpp-2` behind
`Embedder` / `Reranker` traits in a new `rueca-embed` crate, `fastembed`
(candle) as fallback, `ort` rejected. One vector per symbol, keyed by a
hash of the model and the embedded text, cached in the store like facts
and packs, so branches, worktrees and repositories share embeddings;
segments carry int8 vectors (optional components, no format bump),
rescored from f16. Stage 1 fuses BM25, dense and graph-neighbour
candidates by a convex combination of normalized scores (beats RRF, whose
flat head RFC 0006 measured), times today's priors; stage 2 is an optional
reranker. Every default is set by a stated rule on the calibration split
or CoIR; the gate is pooled S1 File Acc@5 on the issue text at least 5
points above BM25 with the interval above zero. Two owner decisions: the
default quantization (int4 chosen; the owner's own rule gives Q8 on Metal
and x86, so `qwen3-0.6b-q8` is the default) and the reranker
(Qwen3-Reranker-0.6B chosen; it scores below the embedder alone on code in
Qwen's table and costs seconds per query locally, so it ships off by
default locally, on only if it adds ≥ 3 points of Function Acc@10 on the
calibration split). Confirmed by the owner on 2026-11-17: Q8 default
decided in phase 0 by the rule; reranker off by default locally, 4B and
mxbai measured first; the remaining open questions were closed
provisionally (RFC §15).

## 2026-11-16 — Rust: call results typed from signatures; methods by name only when unique

Dogfooding showed name-matched edges where the receiver's type was
knowable: `let store = Store::open(p).context(…)?; store.register(…)` and
`Index::open(dir).map_err(…)?.tokenizers().register(…)` bound the only
project trait method named `register`. The pack now writes such receivers
as `$ret:<receiver>#<name>` (`?` when unwrapped by `?`, `unwrap()`,
`expect()`; `.context()`, `.map_err()`, `.clone()` seen through), also for
`let` bindings, and the resolver resolves the callee like a call, reads the
return type of its signature (`Self` → its impl's type or trait,
`Result<T, _>` / `Option<T>` → `T` when unwrapped, references and smart
pointers seen through), resolves it in the callee's file and looks the
member up there (`type_resolved` 0.85); a callee or return type outside
the project (tantivy's `Index::open`) makes the call external. Same-file
inference also learned `HashMap` / `BTreeMap` `get` / `values` / `keys`.

By-name method calls on receivers of unknown type now bind a **unique**
candidate only (a trait method's implementations still count as it), as
the Go and C/C++ unique-method rung does; the fan-out to two or three
candidates at × 0.8 of the first Rust entry is gone. An impl on `&mut T` /
`&T` / `Box<T>` forwards to `T`'s impl and counts as the trait's item
instead of attracting every call of its names (ripgrep's `impl Sink for
&mut S`), and `context` / `with_context` (anyhow) joined the
standard-library method names that never match by name.

Measured with the Rust S3 suite (macro precision / recall): ripgrep
callers 95.5 / 89.6 → 99.5 / 89.9, callees 93.3 / 93.4 → 99.4 / 91.4;
serde_json callers 94.8 / 83.6 → 95.3 / 81.1, callees 92.6 / 81.7 →
95.4 / 80.5; micro-benchmark unchanged (callers 99.6 / 97.5). Most
remaining false edges on serde_json point into code behind
`#[cfg(feature = …)]` features off by default, which rust-analyzer does
not analyze and Rueca indexes (`#[cfg]` is not evaluated); a type with
several `impl From<X>` binds the first `from` it sees.

## 2026-11-16 — Rust pack: module paths, `Type.method`, impl items outside their type

Rust (`rueca-lang-rust`, `tree-sitter-rust` 0.24.2, vendored
nvim-treesitter `locals.scm`) names definitions the way the bench
labeler does: a file's module name is its **module path**, crate name
first (`rueca_index::link`, from `Cargo.toml` targets and the file
layout; separator `::`), and local names join items with `.` — `f`,
`outer.inner` for a nested function, `m.f` for an item of an inline
module (a `Module` definition that owns its items, like a TS namespace),
`Tr.m` for a trait item, `Type.m` for an `impl` item (the last segment of
the self type, generics and references stripped). File modules are not
part of local names, as Go packages are not. Traits are `Interface`s,
structs and unions `Struct`s, variants `Constant`s, `macro_rules!` a
callable `Function` so that invocations of the repository's macros are
`CALLS`.

`impl` items are **not children of their type**: an `impl` block may sit
in another file (often another module) than the type, and only the
resolver can find the type through imports. They are top level (or
children of their inline module) and carry the self type as written in
`bases` and the trait in `implements`; the new `rust` module of
`rueca-resolve` attaches them to the type wherever it is declared, adds
`IMPLEMENTS` type → trait (also from the type's own `implements` when an
`impl Tr for T {}` without items is in the type's file) and `OVERRIDES`
item → trait item. Rejected: an `Impl` label or definition per block (a
new label for every tool and the bench to learn, and impl nodes competing
with the type in name lookups) and a facts-format change (every pack
touched).

Module lookups follow Rust, not the Python / ECMAScript defaults: a name
in a module is an item, else what `mod`, `use` (leaves, re-exports, globs,
private ones too) and `extern crate` bind; a `use` path is walked from its
anchor (`crate`, `self` / `super`, a name in scope, a crate of the
snapshot) through module files, inline modules, types and re-exports, so
the pack's `resolve_import` only maps module paths to files. `IMPORTS`
go to the file each `use` leaf is found in (not to the crate root of
`crate::…`). Path anchors written in code (`crate::a::f()`, `super::g()`,
`serde_json::to_string`) get synthetic imports (`type_only`), which make
no `IMPORTS`.

Measured: the fixture workspace resolves as labelled (`rueca-resolve`
`tests/rust.rs`); the Rust micro-benchmarks of `bench/micro/rust` (58
cases on this date) give 132 of their 133 call edges with 1 extra (a
method on an `impl Trait` return value resolves to the trait, not the
concrete type); 0 files with parse errors on rueca (251 files), tokio
(816) and ripgrep (102), 1 of 72 on serde_json (a deliberately invalid
`trybuild` UI test). Resolution (references resolved / external /
unresolved): ripgrep 59.1 / 35.5 / 4.5 %, serde_json 60.4 / 37.4 / 1.9 %,
the rueca workspace itself 45.2 / 52.7 / 0.3 % (`rueca_reads_itself`, a
test), tokio 39.9 / 45.1 / 14.6 % — tokio declares many items inside
`cfg_*!` macros, which are not expanded (a known gap; a child module file
that only a macro declares is still found).

## 2026-11-16 — Rust: macro arguments are code, macro bodies are not

tree-sitter keeps a macro's arguments as a flat token tree, yet most of
them are expressions that rust-analyzer resolves at their real positions
(`println!("{}", total())`, `assert_eq!(v.len(), 3)`, `vec![make()]`,
`write!(f, "{}", self.name())`), and its SCIP index is the Rust S3 gold.
The pack parses each argument list again on its own, wrapped so that it is
valid Rust (`(…)` as a tuple, `[…]` as an array, `{…}` as a block), the
text copied byte for byte so a node maps back to the file by one fixed
offset; the re-parsed text gets its own local scopes (closures inside
`json!` / `assert!`), nesting is bounded (4 levels, four times the file's
bytes plus 64 KiB per file) and its parse errors do not count as the
file's. Not re-parsed: `stringify!`, `concat!`, `include*!` and
`quote!`-style token builders (their tokens are data, not references), and
the bodies of `macro_rules!` rules: they are token patterns, rust-analyzer
records no references in them, and the bench's micro-benchmark agrees
(an invocation calls the macro, not what its body calls; unlike the C/C++
pack, whose macro expansion follows `#define` bodies). Standard macros are
not references unless the file defines a `macro_rules!` of the same name;
a bare invocation no rung resolves binds the only `macro_rules!` of that
name in the snapshot (`#[macro_use]` needs no import), `unique_name` 0.60.

## 2026-11-16 — Rust: trait dispatch and methods called by name

A call on a receiver typed `T: Trait`, `impl Trait` or `dyn Trait` binds
the trait's method (`type_resolved` 0.85), as rust-analyzer does;
`trace` then reaches the implementations through `OVERRIDES`. With an
unknown receiver, the implementations of one trait method count as that
method: a name that only one trait declares binds the trait's method
(`heuristic` 0.50) however many types implement it; two or three distinct
candidates are all linked at × 0.8, as C++ overloads the arguments cannot
tell apart; more is no edge (amended the same day: a unique candidate
only, see the entry on call results above). A blanket impl (`impl<T: Bound> Tr for T`,
items named `T.m`) applies to every type, so its items are the by-name
targets. Rust types are open: a member missing from a project type comes
from a derive or a blanket implementation outside the project
(`Clone::clone`, `ToString::to_string`) and is external, never a guess;
so is a method call on an unknown receiver when no project method has the
name, or when the name is a common standard-library method (`unwrap`,
`iter`, `insert`, `len`, …: such calls are overwhelmingly on standard
types).

## 2026-11-16 — Cargo packages in the organization layer

A Cargo package with a library publishes `rust:<crate name>` (`[lib]
name`, else the package name with `-` → `_`); its dependency tables
(`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, their
`[target.*]` forms and `[workspace.dependencies]`) are `cargo:<package>`
requirements. An import a repository's own pack cannot resolve is keyed
by its first path segment (`serde::de::Visitor` → `rust:serde`; `crate`,
`self`, `super` and the standard library never), so `use acme_kit::Retry`
in one repository links to the repository whose package builds the
`acme_kit` crate, resolved by its own module model. Inside one snapshot a
crate is found by name (a `path` / `workspace = true` dependency of the
file's package first, renames followed; else any library crate with that
name, the same workspace first).

## 2026-10-27 — An ambiguous overload call never targets its caller

The Java benchmark showed self-loop `CALLS` edges on overloads that
delegate to a sibling of the same name: gson `Gson.fromJson(String,
Class)` calls `fromJson(json, TypeToken.get(classOfT))`, javapoet
`ArrayTypeName.of(Type)` calls `of(TypeName.get(componentType))`. The
argument built by a call has no static type, so several overloads fit
and the fan-out (× 0.8) linked all of them — the caller included.
`Resolver::expand_overloads` now leaves the caller out of a fan-out when
another candidate remains; a call whose arity or argument types pick the
caller alone is still a recursive self-loop. Measured on gson and
javapoet: 11 + 8 wrong self-loops go, the other overloads keep their
edges; 2 true recursions through an untyped argument (javapoet
`TypeName.get(Type, Map)`, `ClassName.get(Class)`) lose their self-loop
— telling them apart needs the argument's type (the return type of the
call that builds it), which the packs do not report.

## 2026-10-27 — Quality gate: ephemeral SonarQube Community, for merges and releases

Static analysis gates both merging to `main` (required `SonarQube` check) and
publishing (the release job needs it). It runs on a SonarQube Community
server started as a service container in the job and thrown away after it,
configured by `scripts/sonar-gate.sh` through the Web API. Rejected:
SonarCloud / a hosted SonarQube, which need an account and a token secret
(and secrets are not available to PRs from forks, where the gate matters
most), and keeping a server running, which is infrastructure to maintain for
a volunteer project. Consequences of the ephemeral server: no history and no
"new code" period, so the gate applies to the overall code and asks for
zero issues rather than "no new issues"; findings are waived per line with
`NOSONAR` and a reason, never by loosening the profile. The server and
scanner images are pinned by digest so the rule set changes only in a PR.
Rust gets every rule the analyzer has, cognitive complexity at 15 per
function. When introduced the gate failed only on that rule (204
functions), fixed by the follow-up refactor PRs; Python (bench) passed after
small fixes (private runtime directory, linear-time parsers, `math.isclose`,
a waived seeded PRNG). A cold run (server start, configuration, scan,
report) takes about a minute locally.

## 2026-10-26 — Repository routing as a prior; package links by joint resolution

RFC 0006 phase 2. A query in organization mode also ranks the
repositories — CORI over the organization index (where its terms occur,
one dictionary lookup per term and repository segment) plus BM25 over a
per-repository profile (names, packages, README head, most referenced
symbols, path vocabulary) — and multiplies every symbol score by
`((0.25 + r) / 1.25)²` inside the top-k. A prior rather than a filter, and
organization-wide statistics rather than per-repository ones: both
alternatives were measured on the calibration split and lost or tied.
Calibration: organization File Acc@5 0.64× → 0.92× of the
single-repository value, tokens per correct answer 1.69× → 1.25×; gated:
0.77× → 0.69× (21 instances; the right repository is now routed for fmt
and code-server but their files still rank lower under organization
statistics; micropython is lost to fmt). Cross-repository edges reuse the
whole resolver: a consumer and the repositories publishing what it imports
(package registry from manifests) are resolved together, each pack
wrapped so a dependency keeps its own module model and the consumer's
package imports resolve with the publishing repository's pack and
project. Edges keep their ladder strategy and confidence; name matching
never crosses repositories. Link sets are stored per (consumer segment,
dependency segments) and recomputed only for changed consumers and the
reverse dependencies of changed repositories. Precision / recall on seven
labelled consumer–library pairs: 99.7 / 95.3.

## 2026-10-26 — Organization index from the repositories' own segments

RFC 0006 phase 1. The organization symbol index is a tantivy index whose
segments are the repository segments' embedded symbol-index segments,
copied unchanged: no second document format, no re-tokenization, and an
update of one repository is a file copy plus a generation file and an atomic
pointer swap (≈ 0.1 s on the benchmark's 21-repository organization, 0.05×
the repository's own index time). The repository of a document is an
attribute of its tantivy segment, so the `repo` filter and the visibility
filter are segment filters inside tantivy (before top-k), and BM25
statistics are summed over the visible segments only, so permissions never
change scores. Rank fusion is gone everywhere: roots ranked together
without an organization use summed statistics, which give the same scores.
Measured: organization File Acc@5 47.6% vs 61.9% on the right repository
alone (0.77×, floor 0.85×; rank fusion gave 9.5%); tokens per correct
answer 1.67× single-repository. Repository routing is the next lever.

## 2026-10-26 — C/C++: test heads as blocks, errors searched inside, the remaining C++ precision gap is the gold's

Toward the last C++ floors of [RFC 0004](rfcs/0004-quality-and-benchmark.md)
(`rueca-lang-cpp` extractor version 4; crate docs, `repair.rs` and
[how-it-works](how-it-works.md) have the details):

- **Catch2 / doctest heads are blocks before parsing.** `TEST_CASE("t",
  "[x]") {` at declaration level becomes `if       ("t", "[x]") {`, as C
  test heads already did; the template variants' type lists are blanked.
  The C++ grammar's recovery from a head without `;` (counted as benign
  before) was unstable: a head on its own lines, a type list or a later
  construct could turn the whole file into one error (nlohmann/json's
  `unit-iterators2.cpp`: 280 error nodes from line 1).
- **An error over several lines is searched inside.** The repair rounds
  took such an ERROR node as one site, where no targeted rule applies; its
  inner errors are now sites too, and a `;` missing after a block is looked
  for in the block's head. Thirteen same-length repairs are new (unnamed
  pointer parameters with a default, `new T *[n]`, declarations and
  dangling operators as macro arguments, trailing macros after template-ids
  and before initializer lists, `__declspec` with a calling convention,
  qualified explicit instantiations, member pointer types as template
  arguments, declaration macros without `;`, …).
- **Dead code for both grammars**: Objective-C (`__OBJC__`) and C++/CLI
  (`_MANAGED`, `__cplusplus_cli`) branches, and every branch after one
  known to be taken whatever the target (the `#else` of `#ifndef
  __OBJC__`).
- **Fewer wrong edges from C++ syntax**: `T x(a, b);` with names as
  arguments (the most vexing parse: the grammar reads a function
  declaration) is a variable and a construction, so `x.m()` is typed;
  `using std::get;` in a block makes `get` local there (fmt's `for_each`
  no longer calls `compile.h`'s `get`); a known class argument never picks
  a pointer overload (`Load(stream)` with a `std::stringstream` is not
  `Load(const char*)`).
- **Measured** (this branch's base → after, same bench): C++ files with
  parse errors, gated 5.0 → 0.5% (47 → 5 of 946: Catch2 11 → 1, fmt at
  the S1 commit 9 → 2, nlohmann/json 19 → 2, simdjson 4 → 0, fmt 12.1.0
  1 → 0, yaml-cpp 3 → 0; raw 7.0 → 3.2%); callers / callees recall 85.3 /
  82.8 → 85.5 / 83.6, callers precision 95.3 → 95.4, callees precision
  89.0 → 89.0, definition found 98.9, micro callee F1 98.8 (outputs
  identical); C files with parse errors 1.3 → 1.1% (micropython 27 → 21
  files), C S3 and micro outputs identical. File Acc@5 81.6 → 78.9: one
  Catch2 instance (1609) where `Misc.tests.cpp`, now parsed, takes rank 5
  from `include/catch.hpp`.
- **C++ callees precision stays below 90, and the gap is the gold's.**
  Every false positive was classified against the scip-clang index itself
  (`.cache/scip/fmt.scip`, `yaml-cpp.scip`): the SCIP symbols of the
  predicted callable's definition against those of the gold neighbours and
  the references inside the caller. Of 631 false positives over the 1,019
  questions with an answer (macro precision 89.0):
  - **262 (3.6 points) share their SCIP symbol with a gold neighbour.**
    scip-clang gives same-signature overloads and function template
    specializations one symbol, and the gold binds it to one definition
    (the first bound), so the others are indistinguishable from it in the
    gold yet count as wrong. fmt's three SFINAE `copy(InputIt, InputIt,
    OutputIt)` (`base.h` 2083 / 2095 / 2108) are all
    `fmt/v12/detail/copy(5151da2f42c51758).`, bound to 2083 (60 false
    positives); `num_bits()` (430) and its explicit specializations
    `num_bits<int128_opt>` … `num_bits<uint128_fallback>` (434–436) are all
    `num_bits(b126dc7c1de90089).`, bound to 436, so the correct edge from
    `num_bits<unsigned short>()` to the primary template is a false
    positive (11); the two-argument `write(OutputIt, T)` overloads
    (`format.h` 3503 / 3570 / 3599 / …) share `write(131888e4bbdcf483).`
    (125 of the `write` false positives). Excluding these undecidable edges
    the precision is 92.2 (92.6 counting them as matches).
  - **25 (0.5 points)**: scip-clang records the reference inside the
    caller, but the bench's tree-sitter call-site filter (only
    `call_expression.function` and `new_expression.type`) drops it:
    constructions by declaration (`Token token(Token::DIRECTIVE,
    INPUT.mark());`, `Parser parser(input);`), where scip-clang puts the
    constructor on the variable name.
  - **48 (1.3 points)**: constructions in template code the gold cannot
    see — scip-clang references the class inside the caller but no
    constructor of it (`basic_memory_buffer<Char> buffer;` in a function
    template, a dependent construction clang leaves unresolved).
  - **296 (5.6 points)** remain: mostly overload sets called with
    template-dependent arguments (fmt's `write` / `count_digits` where only
    the instantiation knows the type; the gold lists whatever clang's
    unresolved lookup kept), bodies of `#ifdef _WIN32` branches the gold
    (built on macOS / Linux) never compiles (`os.cc`), and real limits:
    const / non-const overloads are not told apart by the receiver's
    constness (yaml-cpp `Node::begin()` → `node::begin() const`), and
    argument types do not follow calls into other files
    (`Write(SecondaryTag("binary"))`).
  The bench gold is unchanged; C++ stays `preview` on this one floor.

## 2026-10-26 — Organization graph on top of repository graphs

[RFC 0006](rfcs/0006-org-graph.md): the graph of one repository and ref stays
the unit of work (indexed, versioned, branches and pull requests as deltas).
When repositories are configured as an organization, Rueca also builds one
organization graph on top: a single symbol index (one tantivy segment per
repository, so BM25 statistics and scores are organization-wide), the union
of repository graphs and a link layer of cross-repository edges (packages
first, contracts later), updated one repository at a time. Federated search
with rank fusion (RFC 0005 §5) is dropped: it measured 9.5% File Acc@5
organization-wide against 61.9% on the right repository.

## 2026-10-25 — Tool responses designed for the org mode

[RFC 0005](rfcs/0005-agent-responses.md): token efficiency must not cost
identity. Ids keep repository and commit (abbreviated to 12 hex digits);
responses open with an envelope of the snapshots read (repository, ref,
commit, overlay); every tool takes `ref` and a multi-repository `repo`, with
rank fusion across snapshots; symbols identical across refs are returned
once with the refs they appear in. Savings come from grouping, short fields,
index-based `trace` edges, smaller defaults, a token budget and a compact
text rendering. Efficiency is measured as tokens per correct answer.

## 2026-10-25 — Ranking: bodies over declarations, sources over copies

C++ function localization failed its RFC 0004 floor (Function Acc@10 24.2
vs BM25 42.4, Δ −18.2, CI −33.3 to −6.1) and C file localization was
borderline (File Acc@5 Δ −16.7, CI −33.3 to 0.0). Prototypes in headers
outscored the bodies they declare (a one-line text wins on BM25 length
normalization), and amalgamated single headers (`single_include/`,
simdjson's `singleheader/`, Catch2's `extras/catch_amalgamated.*`)
doubled every hit. Three query-independent priors of 0.5, computed at open
(`rueca-segment` `priors`), no segment format bump:

- **Bodiless C/C++ callables** — `Function` / `Method` nodes whose source
  has no `{` (prototypes, in-class method declarations, `= default` /
  `= delete`, function-like macros), marked at write time in a new optional
  component `symbols.bodiless`. Older segments lack it and rank as before
  until re-indexed.
- **Copy files** — detected from the node table: ≥ 50 callables / types
  with a body and ≥ 4 lines, ≥ 60% of them fingerprinted (family, label,
  name, signature, line count) in another file, no single other file
  holding > 70% of those. On the benchmark corpora this flags exactly the
  amalgamations (partner share 0.08–0.30, one early nlohmann header 0.65)
  and `monaco.d.ts`; per-platform siblings, generated stubs and
  test-file families (0.79–1.0) are not flagged. The qualified name is not
  part of the fingerprint because amalgamation scripts rename namespaces.
- **Vendored directories** — `third_party`, `thirdparty`, `third-party`,
  `3rdparty`, `vendored`, `deps`, `dependencies` (no gold file of the 305 S1
  instances lives under one).

Result (S1 `kw`, gated, Rueca vs BM25 paired): C++ Function Acc@10 24.2 →
36.4 (Δ −6.1, CI −21.2 to +9.1, floor ⚠ instead of ❌), File Acc@5 81.6
unchanged; C File Acc@5 63.3 → 73.3 (Δ −6.7, CI −23.3 to +10.0), Function
Acc@10 30.0 unchanged; every other language identical. S3 symbol lookup
Acc@1: C 81 → 99, C++ 45 → 61. Calibration split: nng Func Acc@10 40 → 60,
botan 60 → 80. The diagnosis read the gated C/C++ misses (as the task
asked); the thresholds were set from the copy statistics of every corpus
file and the 0.5 strength from a flat region (0.2–0.7 give the same
calibration and gated Func Acc@10), not fitted per instance.

Tried and dropped: collapsing hits that share a qualified name (keeps the
best copy only; lost File Acc@5 on C++ and Func Acc@10 elsewhere), a 0.5
prior on Markdown sections (no effect), body boost 12 or name / qualified
name boosts 2 / 1.5 (Python / Go file accuracy up, C / C++ / PHP / Java
function accuracy down).

## 2026-10-25 — C/C++: macros expand to their calls, overloads by argument type, more repairs

Toward the RFC 0004 C and C++ floors (`rueca-lang-cpp` extractor version
3, `rueca-resolve` C-family rules; crate docs and
[how-it-works](how-it-works.md) have the details):

- **Macro expansion in the resolver, body calls in the pack.** The calls a
  macro's replacement list makes are read lexically and reported as calls
  of the macro's definition; a definition that uses the macro (`CALLS` or
  `REFERENCES` edge) also calls them, through nested macros. The macro is
  usually defined in a header and used in another file, so expanding at
  extraction (one file) could not see it; expanding edges after
  resolution keeps extraction per file and resolves each body name in the
  macro's own file, as the preprocessor's output would. A macro defined
  once per `#if` branch expands as one configuration: its first
  definition that is not for release builds (`NDEBUG`), since a
  compilation database describes a development build. The macro use
  stays a `REFERENCES` edge (macros are still uses, not callables).
- **Same-parameter overloads are distinct definitions.** cv-qualified
  members (`get()` / `get() const`), SFINAE overloads and one body per
  `#if` branch used to merge into one node (the later ones vanished). They
  are told apart by a comment at the end of the overload suffix
  (`get(/*const*/)`, `write(OutputIt, T/*#2*/)`), which keeps the suffix
  ending in `)` for every consumer of qualified names and is ignored by
  arities and lookups. Function template specializations are named with
  their arguments, like class template specializations.
- **Argument types for C++ calls** from literals, casts, constructions and
  declared locals, through the existing `with_arg_types` mechanism; a
  reference parameter binds its type, a string literal prefers a string
  parameter and never a primitive, and default arguments come from the
  declaration of an out-of-line body.
- **Parenthesized prototype names** (`int (lua_gettop) (lua_State *L);`,
  Lua's whole API) are functions, so calls reach their bodies.
- **Repairs stay error-driven and same-length**, now covering storage
  classes before declaration macros, macros wrapping declarations or
  statements, operators and empty statements as arguments, attribute
  macros around declarations, `asm` macros, access macros, and a few
  C++ shapes (the table in `repair.rs`). Two masking rules were too eager
  (a subscripted macro call in a body, a `const NAME` after a pointer).
- **Measured** (RFC 0004 scorecard, S3 scip-clang gold and health
  corpus; before → after): C callers / callees recall 81.0 / 65.8 →
  96.3 / 98.5 (Lua callees 62.0 → 99.0) at precision 98.1 / 98.9;
  C++ callers / callees recall 73.9 / 71.1 → 85.6 / 83.0, callers /
  callees precision 91.7 / 86.3 → 95.4 / 89.4, definition found 94.4 →
  98.9; files with parse errors C 12.0 → 3.1%, C++ 11.3 → 7.0%. The C
  micro-benchmark's `macros/macro_body_calls` key still says a macro's
  body calls are not the caller's (scip-clang says they are): C micro
  callee F1 100 → 99.0. What remains
  below the floors: C++ callee precision is held down by the gold on
  dependent template calls (scip-clang lists one representative per
  same-signature overload group, or none through a dependent base), and
  the parse errors are a long tail (vendored GoogleTest, Objective-C
  headers, non-C `qstrdefs.h`, token pasting).

## 2026-10-25 — Structure floors: members by kind, overloads by type, receiver types from assignments

Work toward the structure floors of [RFC 0004](rfcs/0004-quality-and-benchmark.md)
§6 (Java precision and definition found, TS/JS callee recall, PHP micro
callers). All of it is in the resolver's generic rules or in the packs;
no model, facts-format or tool change (extractor versions: Java 2, TS/JS
4, Python 3, PHP 3).

- **Members by kind.** A type keeps one member per name and kind
  (callable, nested type, value); calls look for callables first, reads
  and writes for values, type lookups and receiver paths for types, each
  through the whole hierarchy before the next kind. A constructor (a
  callable named like its class, Java and C/C++) is reached by calls
  only. This replaces the Java pack's workarounds (fully qualified
  `$type:` receivers, `super` for the superclass, dropped self-references,
  fields emitted last, unreported reads of fields sharing a method's name).
  C, C++ and Go graphs of their fixtures and micro-benchmarks are
  byte-identical before and after.
- **Overloads by argument type.** Packs may write the static types of a
  call's arguments into the reference name (`value$args:long,?`,
  `rueca_extract::with_arg_types`), which keeps the facts format and every
  other pack untouched; the resolver drops overloads that cannot accept a
  known argument, then prefers an exact match, then the narrowest
  primitives. Class-to-class compatibility is assumed (subtyping is not
  known), so precision never depends on a guessed hierarchy.
  `OVERRIDES` is paired by signature (parameter count, then types, from
  the overload suffix or the definition's signature).
- **Java anonymous classes are definitions** (`TypeAdapter$1`, child of
  the enclosing definition, base = the instantiated type): their methods
  get callers, `this` inside them finds their own and inherited members,
  and they override their base's methods. Named after the base rather
  than javac's `Outer$1` so a trace reads as what it is.
- **Receiver types from declarations and assignments** ("strategy 5") for
  TS/JS and Python, per file: annotations, `new T()` / `x as T` /
  `x = Foo()`, return annotations of the same file's functions and
  methods, class fields and `self.x` attributes. A name is typed only when
  its annotations agree, or, without one, when every binding and
  assignment of it constructs the same type; anything ambiguous stays
  untyped. Contextual typing (callback parameters) and types that only
  another file knows are out of reach of a per-file pass and stay gaps.
- **Measured** (S3, scip / PHPStan gold, 50 questions per repository, the
  alpha bench): Java callers / callees precision 88.4 / 86.4 → 97.3 /
  96.3, definition found 88 → 100, java-micro callee F1 88.1 → 93.7;
  TS/JS callee recall 69.8 → 74.6; Python callers / callees recall
  82.6 / 77.5 → 88.3 / 80.8 at unchanged precision; PHP micro callers F1
  89.0 → 91.2. TS/JS stays below its recall floor: the remaining misses on
  the gold are contextually typed callback parameters (hono), recursion
  (the bench's Rueca arm reads `trace` `nodes`, which never repeat the
  root, so self-calls never count) and gold edges attributed by line to a
  function expression passed as an argument (express `defineGetter`).

## 2026-10-25 — C/C++: a same-length preprocessor in the pack, resolver rules for C++ names

Toward the RFC 0004 floors (`rueca-lang-cpp`, `rueca-resolve`; crate docs
and [how-it-works](how-it-works.md) have the details):

- **Assemble before build, but nothing fit.** The Rust C preprocessors on
  crates.io (`ancpp`, `includium`) are MPL-2.0, and every real preprocessor
  needs the headers and the compiler flags and emits an expanded token
  stream without a byte map back to the source. Extraction sees one file
  (facts are cached per blob), so the pack prepares the parsed text itself.
- **Same-length rewrites only.** Masking, conditional compilation and
  repairs replace bytes with bytes (spaces, a keyword, `;`) and never touch
  newlines: offsets, lines and the text read for names and signatures are
  the original's, so the "source map" is the identity and no span needs
  translating. The one expansion that would grow the text, a namespace
  macro, is a qualified-name segment instead (below).
- **`#if` groups: keep when the grammar can, collapse when it cannot.** A
  group stays with all its branches (the old behaviour, and what keeps
  platform-specific code in the index) unless a branch is unbalanced, ends
  mid-statement or the group sits where the grammar takes none
  (expressions, initializer lists, after `if (…)`); then it collapses to
  one branch, chosen in three-valued logic with a fixed define set
  (`__cplusplus` per language, GCC / Clang defined, Windows undefined) and
  one configuration per file. Only dead code (`#if 0`, C++-only code in C)
  is dropped otherwise. Project `-D` flags are not used: they would need a
  project context at extraction time, which the contract does not give.
- **Error-driven repair, never worse.** Rewrites that would be risky
  before parsing (a lower-case specifier macro, a statement macro without
  `;`, a type as a call argument) run only where tree-sitter reports an
  error, and a round is kept only when the error count drops. Files with
  parse errors: hiredis 17.0 → 0.0%, jq 26.7 → 1.3%, cJSON 31.3 → 0.0%,
  redis `src/` 15.3 → 0.5%, fmt 37.1 → 5.7% (1.5% without vendored
  GoogleTest).
- **Macro invocations are uses, not calls** (`REFERENCES`): the code a
  macro expands to runs at the call site, and both SCIP and the micro
  answer keys treat macros as non-callables. `static` and
  anonymous-namespace definitions are exported, because an include is
  textual. Class template specializations are their own classes, named
  with their arguments, so the members of `formatter<A>` and
  `formatter<B>` no longer merge.
- **Namespace macros are expanded by the resolver.** A masked
  `FMT_BEGIN_NAMESPACE` … `FMT_END_NAMESPACE` region gives its definitions
  the segment `${FMT_BEGIN_NAMESPACE}`; before nodes are built the
  resolver replaces it with the namespaces the macro's definition opens
  anywhere in the snapshot (`fmt::v12`), keeping extraction per file and
  names consistent across files. Exact lookups see through the inline
  namespaces it opens.
- **Resolver, keyed on the C family or on texts only the C/C++ pack
  emits:** a declaration and its body in one file are one node with the
  body's span; `$ns:` receivers and rung 1c (enclosing namespaces);
  `$member:` receivers typed from members declared in another file; C++
  overload sets across files with arity from signatures (default
  arguments), limited to the declarations the calling file includes;
  `OVERRIDES` between bodies. Golden unchanged (C/C++ are not in it).

## 2026-10-25 — Quality bar: deterministic metrics, absolute floors, cbm as reference

[RFC 0004](rfcs/0004-quality-and-benchmark.md) replaces the adoption gate
"match or beat codebase-memory-mcp on every metric" with absolute floors and
aspirational targets per metric family (localization, structure,
micro-benchmarks, configuration, health), checked with confidence
intervals. A language is `stable` when it meets every floor; the first
public release requires every language at `stable`. cbm and BM25 are
published as reference; localization must not be significantly below BM25.
Agent runs, a vector retrieval arm and hybrid retrieval stay on the roadmap:
deterministic metrics are the north star until a representative agent run
is affordable.

## 2026-10-25 — Structured data and the code ↔ config bridge

Waves 1, 2a and 2b of [RFC 0003](rfcs/0003-structured-data-and-languages.md)
landed: `rueca-lang-data` serves JSON/JSONC, YAML, TOML, `.env`, INI,
`.properties`, XML kinds, HCL/Terraform and Dockerfile with the parsers the
RFC chose (`jsonc-parser`, `saphyr-parser` events, `toml_edit`, `roxmltree`,
`hcl-edit`; own line parsers), and the Python, TS/JS and PHP packs emit env
var and config key reads. Decisions taken while building, all documented in
the crate docs and the RFC's *Implemented* notes:

- **One generic walk, kinds as a refinement.** Every mapping-shaped format
  is converted to one spanned tree; the generic walk emits keys, previews,
  docs, YAML anchors / merges and `$ref` pointers for every file, and a
  kind only relabels, re-signs and adds facts. An unrecognized variant of a
  kind degrades to plain keys, as §5 asks.
- **Shape rules are for files of no kind.** Kubernetes bundles and OpenAPI
  specs are capped (`partial`) instead of skipped; the "mostly array
  elements" test needs at least 20 definitions. The 5,000-key cap counts
  keys as the walk emits them (before kinds relabel some).
- **Laravel config keys live in the PHP pack.** `config/<file>.php` is PHP,
  so the PHP pack turns the returned array into `Key`s and makes `env()`
  calls in an entry reads by that key; it sets `facts_depend_on_path`
  (one cache key per path for PHP files). PHP has no module name, so these
  keys' global names are bare key paths (`name`); ids carry the file.
- **Evidence over names for code reads.** Python needs `import os`; TS/JS
  needs `process` not to be local (and `config` imported from `config` for
  node-config); PHP matches the global `getenv` / `env` / `config` helpers
  and the `Config` facade by name (PHP falls back to globals), which a
  project-defined namespaced `env()` would fool.
- **Secrets.** Provider values and secret-looking keys never reach
  signatures or docs. The segment's `body` field (search only, never
  returned) still tokenizes each symbol's source, as it does for code; the
  raw file is in the content packs either way.
- **Golden.** The fixture golden now includes the data fixtures (a small
  polyglot service) and changes only where JSON files gained a language
  (`php/laravel/composer.json`); no existing code fixture reads env vars.

## 2026-10-24 — Go pack (preview): file-local receiver types, heuristic interfaces

Go landed as `rueca-lang-go` (RFC 0003 §10.2) on the wave R hooks, without
changes to the model, the contract or the generic resolver beyond wiring
one Go-only post-pass (`rueca-resolve` `go` module). Choices worth
recording:

- **Module name = import path**, from every `go.mod` (and local
  `replace` of `go.mod` / `go.work`); an external test package is marked by
  a `Package` definition so `module_name_for` can append `_test`.
- **Import binding names are inferred in the file.** A pack cannot read the
  target's `package` clause, so the conventional name is used and, because
  Go rejects unused imports, an import whose conventional name is never used
  is re-bound to the qualifier the file does use.
- **Receivers are typed from the file's declarations only** (`$type:T`),
  including struct-field chains; chains that reach a package-qualified type
  or an import are written relative to it so third-party calls are
  external, not 0.5 name guesses. This is what keeps unresolved references
  at 3–7 % on gin and urfave/cli.
- **Implicit interfaces are a heuristic** (`IMPLEMENTS` and `OVERRIDES` at
  0.5), names only, capped at 64 implementers per interface.
- Status `preview` until the S1/S3 benchmark rows exist (RFC 0003 §3).

## 2026-10-25 — Java: packages from the package clause, constructors vs implicit `this`

Java support (`rueca-lang-java`, `tree-sitter-java` 0.23.5, status
`preview` until its benchmark entry) is the first pack on the wave R hooks:
package scope, implicit `this`, `$type:` receivers, overloads by arity and
`OVERRIDES`. No resolver, model or query code changed. Choices:

- **The package clause is an import without names.** `module_name_for`
  reads the package from the facts, which carry no other place for it; a
  `Package` definition would have added one node per file. Source roots
  (Maven / Gradle conventions at any depth, declared `sourceDirectory` /
  `srcDirs`, then a package-path suffix match) only map imports to files
  and name files without facts.
- **Synthetic imports.** Fully qualified types in code and names under a
  static on-demand import are recorded as single imports, so the generic
  ladder resolves them like explicit imports (34 jackson-core test classes
  extend a fully qualified base; static on-demand imports took its
  unresolved share from 8.9% to 3.1%).
- **Constructors are members named like their class (wave R)**, so with
  implicit `this` a bare type name equal to an enclosing class or its
  superclass reaches the constructor. The pack writes types of the file
  fully qualified in `$type:` receivers, uses `super` for the direct
  superclass and drops self-references; a field and a method with one
  name share one member slot, so fields are emitted last. A resolver rule
  that ignores constructors and fields in type and call lookups would make
  both workarounds unnecessary (follow-up).
- **Bridge facts** (`System.getenv`, Spring `@Value` / `Environment`) need
  file-local evidence (the Spring import, an unshadowed `System`); relaxed
  binding stays off.
- **JDK names are builtins** for the unique-name and unique-method rungs:
  `java.lang` and common `java.util` / `java.io` / … types, JDK package
  heads, and `Object` / collection / stream / string method names, so an
  untyped `list.stream()` never links to the only project `stream()`.

## 2026-10-25 — C and C++: one pack, macro masking, all overloads suffixed

C and C++ land as one pack, `rueca-lang-cpp` (`CppPack` for
`Language::C` and `Language::Cpp`; RFC 0003 §10.3 called it
`rueca-lang-c`), status `preview` until the bench entry and the parity row
exist. Choices beyond the RFC, all documented in the crate docs:

- **No preprocessor, but a masking pass.** Macro tokens that expand to
  declaration syntax (`FMT_API`, `FMT_BEGIN_NAMESPACE`, `FMT_ENABLE_IF(…)`
  in a template head, `extern "C" {` split by `#ifdef __cplusplus`, a
  function header written once per `#if` branch) are replaced by spaces
  before parsing when their position allows no other reading; `#if`
  conditions become `1`. Offsets are preserved and text is read from the
  original source. On hiredis, jq and fmt it cut the files with parse
  errors from 34 / 45 / 53% to 17 / 35 / 40% (and fmt gained 20% more
  definitions); the remaining rate is a grammar limit without a
  preprocessor and is reported, not hidden.
- **Overloads: every overload is suffixed** (RFC 0003 open question 4),
  per file, so a declaration and its body share the suffix; lookups treat
  the suffix as optional.
- **Declaration ↔ body: references prefer the body** (open question 3); no
  new edge.
- **Namespaces are not nodes**: they qualify names; a namespace-qualified
  call has no receiver and resolves through includes. Out-of-line method
  bodies are top-level, unexported `Method`s (the class declaration
  carries the visibility).
- Includes are read lexically (they survive parse errors) and resolved
  through `compile_commands.json` / `compile_flags.txt` include
  directories; the unique path-suffix fallback produces a 1.0 `IMPORTS`
  edge because the generic resolver fixes that confidence.

## 2026-10-24 — Wave R: shared model, contract and resolver for RFC 0003

The groundwork every RFC 0003 pack needs landed before the packs, so the
data / bridge, Go, C/C++ and Java work can proceed in parallel without
touching `rueca-model`, `rueca-extract` or the generic resolver and query
code ([RFC 0003](rfcs/0003-structured-data-and-languages.md) §9.2, §9.3,
§10.1; details in the *Implemented* notes there and in the crate docs).

- **Additive on disk.** `Language`, `Label`, `RefKind` and `Strategy`
  variants are appended (enum indexes are pinned by a test). Segment format
  stays 2: a store written before the change is committed as a fixture and
  must keep opening with identical tables; the new optional component
  `files.modules` (module name per file, for symbol ids of Go and Java) is
  ignored by older readers. A segment that uses a new variant is rejected by
  an older build as corrupt rather than misread.
- **Facts format 2.** `Definition::is_declaration` and `Reference::arity`
  (needed to choose overloads, which §10.1 requires and §9.3 did not list)
  change the positional postcard encoding, so `FACTS_FORMAT` is part of the
  facts cache key: old entries are re-extracted once, never misdecoded.
- **Detection without a pack is not a language.** `Language::from_path`
  recognizes every RFC 0003 file, but the indexer records a language only
  when a pack for it is registered; until then such files stay plain text,
  exactly as before. `.h` is C++ when the snapshot has C++ sources.
- **Open question 2 decided**: `module_name_for(path, facts, project)` is an
  additive hook defaulting to `module_name`. Other refinements, all
  defaulted: `module_scope(language)` with a `Directory` variant
  (Terraform), `overloads()`, `overrides()`, `facts_depend_on_path()` (data
  kinds are chosen by file name; the cache key then includes the path),
  `ExtractError::Skipped(reason)`.
- **`$self` is opt-in.** It is a valid identifier in JavaScript, PHP and
  Java, so it is honoured only for packs that list it in
  `self_receivers()`; `$type:` cannot collide and is reserved for all
  non-PHP packs.
- **Behaviour of the shipped languages is unchanged**: every rule is keyed
  on a hook or on facts only the new packs emit. Evidence: a golden graph of
  every language fixture (`rueca-index/tests/golden`) committed before the
  change, and byte-identical graphs before and after on four real
  repositories (Python, TypeScript, PHP with Markdown; ~280k graph lines).
- The RFC's lockfile additions (§4.5) are excluded by the walk now; the
  `Key` / `Document` default-label rule (§11) and `readers_count` /
  `writers_count` (§7.4) are in the tools.

## 2026-10-24 — Language order for the first release

Before the first public release, languages land in this order: PHP,
Markdown, JSON, YAML, the other structured config formats (TOML, `.env`,
INI, `.properties`, XML kinds, HCL/Terraform, Dockerfile), Go, C++, C, Java.
Structured data is indexed as `Key` nodes with qualified names
`path:key.path`, file kinds (compose, Kubernetes, GitHub Actions, OpenAPI,
Terraform, Dockerfile) add `Resource` / `Image` / `Route` / `EnvVar` nodes,
and code that reads env vars or config keys is linked to them with `READS`
(providers with `WRITES`). A language is "supported" only when it meets the
release gate (extraction, import resolution, tests, bench entry, published
parity row vs cbm v0.12.0, docs). The design, parsers, phasing and the
release point are in
[RFC 0003](rfcs/0003-structured-data-and-languages.md). **The first public
release ships after Java**, with every language above `supported` (none as
`preview`).

## 2026-10-24 — Alpha 2 quality pass: ranking over code, segment format 2

The alpha's symbol index looked only at names, signatures, docstrings and
paths; a BM25 baseline over function bodies beat it on localization. Changes,
each kept only because the benchmark moved (`docs/bench/alpha-results.md`):

- **Segment format 2.** The tantivy symbol index gains a `body` field (the
  source text of each symbol's span, nested definitions included, ≤ 32 KiB,
  read from the content packs at write time) and a `qname_suffix` field
  (qualified-name suffixes for dotted queries such as `Session.request`).
  Version-1 segments are rejected with `SegmentError::Version` and must be
  re-indexed; `rueca mcp` detects an old segment from its header and
  re-indexes on start. Segments grow ~20–25 %; index time is unchanged.
- **Ranking.** Boosts body 8, name 4 (was 8), qualified name 3;
  query term frequency `1 + ln n`; score × 0.5 for fields / variables /
  constants and × 0.5 for tests (flag, `Test` label or test path); one-chunk
  queries are name lookups (body × 0.25). The region around these values is
  flat on the benchmark (±3 points); they were not tuned further to avoid
  fitting 81 instances.
- **`trace` leaves tests out by default** (`include_tests`, `labels`,
  `tests_excluded` are additive). Module-level and initializer callers stay
  and are identified by their label.
- **TypeScript extractor 2**: named function expressions are definitions;
  type parameters of call / construct signatures and function types, and
  inside parse-error fragments, are not type references (hono: 11.9k → 5.6k
  `Type` references); web/runtime globals (`Request`, `Response`, …) are
  builtins for the resolver only; `export default x`, `module.exports = x`
  and `export { a as b }` are recorded as self re-exports, and the resolver
  now prefers an explicit `default` re-export over the default-export
  heuristic.

Tried and dropped: excluding nested definitions from a symbol's body (worse
function ranking), a `.d.ts` penalty and body caps of 8 / 128 KiB (no
measurable effect), an in-degree "popularity" prior to break same-name ties
(no gain on Python lookups, worse on raw issue text). BM25 `k1` / `b` were
not tuned: tantivy 0.27 fixes them unless a custom scorer is written.

## 2026-10-24 — Markdown: sections and links, parsed with pulldown-cmark

Markdown documents join the code graph (`rueca-lang-markdown`): headings are
`Section` nodes nested by `DEFINES` (local name = heading chain joined by
` > `, duplicates suffixed ` (2)`), module name = path with extension, and
links / images to repository paths are imports that resolve to `IMPORTS`
File→File edges. Parser: `pulldown-cmark` 0.13 (MIT, pure Rust, offset
iterator) rather than `tree-sitter-md`, following RFC 0002 §3.3 (pure-Rust
parsers for data kinds on untrusted input); the pack does not need a syntax
tree, only headings, paragraphs and link destinations with byte ranges.
Inline code spans are deliberately not emitted as references: the resolver's
unique-name fallback would turn prose into 0.6-confidence noise edges.
Anchor → section references, front-matter titles and MDX are future work.

## 2026-10-24 — PHP: names qualified at extraction, no file modules

PHP support (`rueca-lang-php`, `tree-sitter-php` 0.25) departs from the
Python / ECMAScript shape in three documented ways:

- **Qualified names are PHP names.** A PHP file may declare several
  namespaces, or one that differs from its Composer path, and code never
  refers to files. Local qualified names therefore carry the declared
  namespace (`App\Models\User::save`, properties `::$email`) and
  `module_name` is empty, so global names, symbol ids and suffix lookups
  (`User::save`) all use the real PHP name. PSR-4 / PSR-0 is used where it
  belongs: mapping an imported class to its file (`resolve_import`).
- **Name resolution happens in the pack.** PHP resolves names at compile
  time from the file's `namespace` and `use` statements only, so the pack
  reports names fully qualified (receiver encoding in the crate's `names`
  module) and the resolver looks them up in an index by fully qualified
  name instead of building per-file binding environments. Python and
  ECMAScript resolution is unchanged; PHP is dispatched by language family.
- **Receiver typing from declarations.** The pack types `$x->m()` when every
  binding of `$x` declares or constructs one class (typed or docblock
  parameters, `catch`, `new`), and `$this->prop->m()` from the property's
  declared type, using a vendored `locals.scm` for variable scopes. These
  edges are `type_resolved` (0.85); the generic "strategy 5" (typing from
  arbitrary assignments) is still open for all languages.

Traits are recorded as bases (`INHERITS`) so their members are found
through the using class; `parent::` reuses the `super` lookup. scip-php
remains non-viable as a precision layer (RFC 0002 §4.2).

## 2026-10-22 — Project renamed from Telar to Rueca

`telar` is taken on crates.io by an unrelated Rust UI framework with its own
`telar-*` crate family, and several GitHub projects share the name. Rueca
(Spanish for spinning wheel, a sibling of the loom) is free on crates.io,
PyPI and npm. Crates are `rueca-*`, the binary and the installable package are
`rueca`. On-disk magic and extensions changed accordingly (`RUECASEG`,
`.rcseg`, `.rcpack`); nothing had been released, so no migration is needed.

## 2026-10-22 — Alpha 1 (local mode) shipped; deviations from RFC 0001/0002

Alpha scope: local mode, one binary, MCP over stdio, Python + TS/JS, six tools.
Built by assembling the RFC 0002 components; deviations, all documented in the
crate docs:

- Node/file tables are postcard, not Arrow IPC (behind the reader; swap later).
- Trigram content index is Rueca's own (sorted keys + roaring postings) rather
  than a tantivy n-gram field; `fst` not used (keys are fixed width).
- tantivy symbol index is embedded in the bundle and served through a
  read-only bundle directory (Quickwit pattern); doc id = node id for
  deterministic tie-breaks.
- Local catalog is `catalog.json`, not SQLite.
- `oxc_resolver` pinned to 11.23 (11.24 needs Rust 1.95; MSRV is 1.90), run
  over a virtual file system built from the snapshot's paths and configs.
- Local-binding detection uses vendored locals queries (nvim-treesitter,
  bloop; see NOTICE). scip-syntax Python locals is empty upstream.
- Git reads via gix; `dirty` is computed by comparing indexed blob SHAs with
  the HEAD tree instead of gix status.
- Benchmark Python call gold: scip-python + PyCG, not pyrefly. Function
  localization gold reuses LocAgent and SWE-PolyBench labels where available.

Results in `docs/bench/alpha-results.md`: parity gate not met. Rueca leads
cbm on TS/JS callers/callees F1 and on query latency (~10 ms vs ~440 ms per
call), trails on S1 localization ranking (and both graph tools trail BM25), on
Python callees, and on JS named function expressions passed as arguments
(express definition-found 56%).

## 2026-10-20 — Scope: code only

Rueca indexes code and repository-resident docs. External document stores
(Notion, Drive) are out of scope: the industry converged on per-source MCP
servers that handle their own permissions and freshness. Rueca composes with
them instead of absorbing them.

## 2026-10-20 — Zero capability loss vs cbm (superseded 2026-10-21)

~~Migration uses a cbm-backed extractor adapter so every release keeps full
capability.~~ Replaced by the entry below.

## 2026-10-21 — Built from scratch; cbm is a baseline, not a dependency

Rueca does not embed, wrap, or call codebase-memory-mcp. cbm v0.12.0 is the
reference baseline that the benchmark measures against. "No capability loss"
is an adoption gate, not a development constraint: Rueca may be partial while
it is built, and an adopter switches only when Rueca matches or beats cbm on
the benchmark for the languages and capabilities that adopter needs. Parity is
declared per language (see RFC 0001, supported-language tiers).

## 2026-10-22 — RFC 0002 amendments applied to RFC 0001

Segments become a bundle file (tantivy + mmap CSR + Arrow IPC + hotcache)
instead of SQLite; branch/PR deltas use blob-visibility bitmaps; precision
layer is SCIP plus pyrefly (Python calls), a PHPStan extension (PHP) and
index-store import (Swift), uploaded from adopter CI; MCP targets the
2026-07-28 spec with Rueca as OAuth resource server; git network operations
use the `git` CLI and all reads use gix; data file kinds use pure-Rust
parsers.

## 2026-10-20 — Storage model (segment format superseded 2026-10-22)

Indexing unit is the git blob (content-addressed), so identical files across
branches and PRs are parsed once. Output is immutable segments (graph, full
text, trigram, vectors, file contents) stored in a local directory or object
storage, plus a small catalog (SQLite locally, Postgres as a service). Postgres
as the primary graph store was rejected: it breaks parity on BM25 ranking,
recursive traversal semantics, and per-node query latency.

## 2026-10-20 — Core language

Rust core (tree-sitter bindings, single static binary for local mode).

## 2026-10-20 — Open source, vendor-neutral

Rueca is an independent open-source project. No company-specific code,
names, or data in this repository; adopters deploy it with their own
configuration.
