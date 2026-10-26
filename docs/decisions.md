# Decisions log

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
