# How Rueca works: building the graph and answering with it

This document explains the method: what Rueca extracts from source code, how
it turns that into a graph, what the graph contains, and exactly which parts
of it each tool reads. It describes the alpha as implemented. The *why* lives
in the [RFCs](rfcs/) and the [decisions log](decisions.md); the exact tool
arguments and result shapes live in [tools.md](tools.md); byte-level formats
live in the crate docs (`rueca-segment`). Structured data (config keys,
env vars, the code ↔ config bridge) and the extension to Go, C/C++ and Java
are designed in [RFC 0003](rfcs/0003-structured-data-and-languages.md); this
document changes as each part lands. Structured data and the bridge are
implemented (§4.2–§4.4, §5); Go, Java and C/C++ are built on the shared
resolver rules.

Contents:

1. [The pipeline in one picture](#1-the-pipeline-in-one-picture)
2. [Vocabulary](#2-vocabulary)
3. [A worked example](#3-a-worked-example)
4. [Building the graph](#4-building-the-graph)
5. [The graph schema](#5-the-graph-schema)
6. [How each tool uses the graph](#6-how-each-tool-uses-the-graph)
7. [Confidence: how to read an edge](#7-confidence-how-to-read-an-edge)
8. [Determinism, incrementality, limits](#8-determinism-incrementality-limits)
9. [Where the code lives](#9-where-the-code-lives)

---

## 1. The pipeline in one picture

```
                 per file (parallel, cached by blob)              per snapshot                   query time
              ┌──────────────────────────────────────┐   ┌──────────────────────────┐   ┌─────────────────────────┐
working tree ─┤ walk → read → blob SHA → tree-sitter ├──▶│ resolve: facts → graph   ├──▶│ segment (one file)      │◀── six tools
              │        → FileFacts                   │   │ nodes + edges + evidence │   │ + content packs (zstd)  │
              └──────────────────────────────────────┘   └──────────────────────────┘   └─────────────────────────┘
                 rueca-index + rueca-lang-*                 rueca-resolve                   rueca-segment, rueca-query
```

Two ideas carry the whole design:

- **Extraction is local, resolution is global.** A language pack looks at
  *one file* and reports what it sees (definitions, references by name,
  imports by specifier). It never guesses what a name points to. Linking
  names across files happens once, in the resolver, with the whole snapshot
  in view. This is what makes per-file caching possible and keeps every
  guess in one place.
- **Every edge says how it was made.** Edges carry a *strategy* and a
  *confidence*. The graph does not pretend to be a compiler; it tells the
  agent which links are certain and which are educated guesses.

## 2. Vocabulary

| Term | Meaning |
|---|---|
| **Snapshot** | One indexed state of a working tree: repo id, commit (or `worktree`), dirty flag. |
| **Blob** | A file's content, identified by its git blob SHA-1. The unit of caching and content storage. |
| **Facts** (`FileFacts`) | What one file says about itself: definitions, references, imports, parse errors, line count. Output of a language pack. |
| **Definition** | A named thing declared in a file (class, function, method, field, variable, constant, interface, type alias, enum, test…), with its span, signature, docstring and parent. |
| **Reference** | A use of a name: a call, `new`, type annotation, decorator, `raise`/`throw`, read or write. Has a name, an optional receiver (`self`, `app`, `a.b`) and the definition that encloses it. |
| **Import** | A module specifier plus imported names (`from .http import Session`). |
| **Module name** | How other files refer to a file: Python `shop.http`; ECMAScript `src/util/index` (path without extension); Markdown and data files `docs/guide.md` (path with extension); Go the package import path; Java the package. PHP and C/C++ files have none: code refers to classes and functions by namespace, never to files. |
| **Qualified name** | Global name of a definition: module name + separator + local name. Python `shop.http.Session.send`; ECMAScript `src/util/index:formatAmount`; Markdown `docs/guide.md:Install > Linux`; PHP the fully qualified name, members after `::` (`App\Models\User::save`); data `config/app.yaml:database.host` (the local name is a **key path**); Go `github.com/gin-gonic/gin.Engine.ServeHTTP`; Java `com.example.Owner.getPets`; C/C++ `fmt::v11::format_to`. |
| **Package** | Files that share one module namespace without imports: Go and Java packages (same module name), Terraform modules (same directory). Declared by the pack (`module_scope`). |
| **Canonical env var** | One file-less `EnvVar` node per environment variable name (`$env:DATABASE_URL`): code that reads it has a `READS` edge to it, manifests that set it a `WRITES` edge. |
| **Node / edge** | The resolved graph. Nodes are definitions plus `Repository` / `Folder` / `File`; edges are typed relations between them. |
| **Strategy / confidence** | How a reference edge was resolved, and how much to trust it (0–1). |
| **Segment** | The immutable file that stores one snapshot's graph and indexes. |
| **Content pack** | Compressed file contents, keyed by blob SHA, shared across segments. |
| **Symbol id** | Stable, human-readable id: `repo@commit:path#Local.name`, e.g. `local/shop@worktree:shop/http.py#Session.send`. |

## 3. A worked example

A three-file Python package, `shop/`:

```python
# shop/__init__.py   (empty)

# shop/http.py
class Session:
    """HTTP session with connection reuse."""

    def request(self, method, url):
        return self.send(method)

    def send(self, req):
        return req

# shop/api.py
from .http import Session


def get(url):
    return Session().request("GET", url)
```

`rueca index` produces **9 nodes and 12 edges** (actual output):

```
Repository local/shop
└─CONTAINS─ Folder shop
   ├─CONTAINS─ File shop/__init__.py
   ├─CONTAINS─ File shop/http.py ─DEFINES─ Class Session ─DEFINES─┬─ Method request
   │                                                               └─ Method send
   └─CONTAINS─ File shop/api.py  ─DEFINES─ Function get
                                 ─IMPORTS─▶ File shop/http.py

Method request ─CALLS─▶ Method send     0.90  type_resolved   (self.send → member of the enclosing class)
Function get   ─CALLS─▶ Class Session   0.95  import_resolved (Session bound by `from .http import Session`)
Function get   ─CALLS─▶ Method request  0.85  type_resolved   (receiver `Session()` constructs a `Session`)
```

The last edge is the interesting one. A human sees that `Session()` returns a
`Session`, so `.request` is `Session.request`. The pack sees it too: calling
a class-like name constructs an instance, so it reports the receiver as
`$type:Session` (§4.2), and the resolver finds `request` in that class
(strategy 5, 0.85). Without that evidence — `make().request()` with `make`
defined in another file — the resolver would fall back to "the only method
called `request` in the project" and mark the edge 0.5 / `heuristic`; an
agent that needs certainty can ask `trace` with `min_confidence: 0.6` and
such edges disappear. The sections below reference this example.

## 4. Building the graph

### 4.1 Walk and snapshot identity (`rueca-index`)

1. Walk the root with the `ignore` crate: `.gitignore`, `.ignore`, global git
   excludes, plus hard excludes for dependency, build and cache directories
   (`node_modules`, `.venv`, `dist`, `__pycache__`, …), minified bundles,
   source maps and lockfiles. Hidden files are kept (`.github/` matters).
2. Read each file **once**, compute its git blob SHA. Files over 2 MiB or
   binary are listed but not extracted. Every listed file gets a `File` node
   and is content-searchable if it is text, even without a language pack.
3. Identity from git via `gix`: repo id from `remote.origin.url`
   (`github.com/owner/name`) or `local/<dir>`; commit = `HEAD`; *dirty* when
   any indexed blob differs from the `HEAD` tree.

### 4.2 Extract facts per file (`rueca-lang-python`, `rueca-lang-typescript`, `rueca-lang-php`, `rueca-lang-go`, `rueca-lang-java`, `rueca-lang-cpp`, `rueca-lang-markdown`, `rueca-lang-data`)

Each supported code file is parsed with tree-sitter under a budget (size, time,
depth). The pack walks the syntax tree and emits facts. In the example,
`shop/http.py` yields:

```
definitions:
  0 Class    Session           parent: —   span 1–8  signature "class Session"  doc "HTTP session…"
  1 Method   Session.request   parent: 0   span 4–5  signature "def request(self, method, url)"
  2 Method   Session.send      parent: 0   span 7–8  signature "def send(self, req)"
references:
  Call  name "send"  receiver "self"  enclosing: 1 (Session.request)   line 5
```

and `shop/api.py` yields the import `from .http import Session` (level 1),
the function `get`, and two calls inside it: `Session` (no receiver) and
`request` (receiver `$type:Session`, see *receiver types* below).

What packs deliberately do and don't do:

- **Containment and local qualified names** (`Session.request`) come from the
  nesting of definitions. The parent link becomes a `DEFINES` edge later.
- **Local variables are filtered at the source.** `url`, `method` and `req`
  are parameters, so reads of them are not reported. The decision "is this
  identifier local?" is driven by vendored tree-sitter `locals.scm` queries
  (adapted from nvim-treesitter and bloop, see `NOTICE`), plus the scope rules
  queries cannot express (Python closures, `global`/`nonlocal`, class-body
  scope; JS hoisting). This removes most noise before resolution.
- **No cross-file knowledge.** `Session` in `api.py` is just a name with no
  receiver. The pack does not know it is a class.
- **PHP names are qualified at extraction.** PHP resolves every class,
  function and constant name at compile time from two things in the file
  itself: the current `namespace` and its `use` imports. The PHP pack applies
  those rules, so `new User` under `use App\Models\User` is reported as the
  name `User` in namespace `\App\Models\`, `strlen()` inside
  `namespace App` as "`App\strlen`, else global `strlen`", and `User::find()`
  as member `find` of class `\App\Models\User`. This is still one-file
  knowledge; which file defines `App\Models\User` is the resolver's job. The
  pack also types receivers from declarations in the same file: `$user` in
  `$user->save()` holds a `User` when every binding of `$user` in its scope
  says so (typed parameter, `catch (User $e)`, `$user = new User`, docblock
  `@param User $user`), and `$this->repo` has the declared type of property
  `$repo` (typed, promoted or `@var`). Variable scopes come from a vendored
  `locals.scm`; variables themselves are never references.
- **Go receivers are typed from the file's declarations.** A Go method is
  declared outside its type (`func (s *Server) Start()`, possibly in
  another file of the package), so the pack names it `Server.Start` at top
  level and rewrites its receiver variable to the reserved `$self`. Calls on
  other variables carry what the file declares about them as `$type:T`: a
  typed parameter (`c *gin.Context`, also in function literals), `var x
  T`, `x := T{}` / `&T{}` / `new(T)`, the named result of a function of the
  same file, the constructor convention `x := NewT()` (a heuristic), a type
  assertion, a type-switch `case`, a struct field of a type declared in the
  file (`s.store.Get()` with `store *store.Store`). Chains that reach a
  package-qualified type (`c.Request.Header.Set()` with `Request
  *http.Request`) are written relative to that type, so a third-party type
  makes the call external rather than a name guess. Type parameters are
  scoped by the locals query and never become references.
- **Java receivers are typed from declarations.** Parameters, locals
  (`var x = new T()` included), pattern variables, `catch` parameters and
  fields of the enclosing classes are declared with a type, so `x.m()`
  is reported with the receiver `$type:T` (§4.4, typed receiver), written
  as in the source; a bounded type parameter (`F extends Figure`) stands
  for its bound, and a call on the result of a method of the file's own
  types (`getAsSingleElement().getAsBoolean()`) for its declared return
  type. Locals come from a vendored `locals.scm` and are never references.
  Overloaded methods and constructors get a parameter suffix in their
  qualified name (`Parser.parse(String)`, `Parser.parse(String,int)`; a
  method that is not overloaded stays bare), calls carry their argument
  count and, in the name, the static types of the arguments the file
  knows (`value$args:long`: literals, typed locals, parameters and fields,
  `new T()`, casts), and constructors are members named like their class.
  Anonymous classes (`new TypeAdapter<Foo>() {…}`) and enum constant
  bodies are classes named after their base (`TypeAdapter$1`, children of
  the enclosing definition), so their methods are definitions with their
  own callers and `OVERRIDES`. Fully qualified type names
  (`com.acme.Owner`) and static on-demand imports are recorded as extra
  single imports so they resolve like explicit ones.
- **Receiver types from declarations and assignments** (TS/JS, Python;
  "strategy 5"). A per-file pre-pass records what every binding of a name
  says about its type, per function (and for module-level names and class
  fields): an annotation (`c: Context`, `let x: Foo = …`,
  `opt?: Foo | undefined`, Python `req: Request`, `Optional[Foo]`,
  `"Foo"`), or a value that constructs one (`new T()`, `v as T`, `<T>v`;
  Python `x = Foo(…)`, or a call of a function or method of the same file
  with a return annotation, `ctx = self.request_context(env)`). A name is
  typed when its annotations agree, or, without one, when every binding
  and assignment of it constructs the same type; anything else — a
  reassignment to another or an unknown value, destructuring, `for` /
  `catch` / `with` targets, a parameter without annotation — leaves it
  untyped. Class fields work the same way (`#node: Node`,
  `private cache = new Cache()`, TS parameter properties,
  `this.x = new T()`; Python `self.x = Foo()`, class-body annotations), and
  so do module-level bindings used in functions
  (`const app = new Hono(); … app.get()`). TS/JS also types a name by the
  bound of its type parameter (`d: D` with `D extends Dog`), the element
  of an annotated array for an array-method callback parameter and a
  `for … of` variable (`xs: T[]; xs.map((x) => x.m())`), and a call of a
  module-level function or of a method of the file's classes by its
  declared return type (`b.map(f).get()`, `Registry.create().m()`); a
  function whose every `return` is an object literal types its result as
  itself, its object-literal methods being its members
  (`const c = makeCounter(); c.next()`). The call is then reported with
  the receiver `$type:T`. Types are file-local: a callback parameter typed
  only by context (`app.use((c, next) => …)`) or a value returned by a
  function of another file stays untyped. PHP does the same from its own
  declarations (`(new Foo())->m()`, inline `/** @var Foo $x */` included).
- **C and C++ names carry their namespace and class path**
  (`geo::Circle::area`), like PHP's: code never refers to files, so the
  module name is empty. Prototypes and in-class method declarations are
  definitions marked `is_declaration`; an out-of-line body
  (`double Circle::area() const {…}` inside `namespace geo`) is a top-level
  `Method` with the same qualified name, which is how the resolver pairs
  the two. When a file declares several callables with one name and
  different parameters, all of them get the parameter types as a suffix
  (`geo::scale(double)`, `geo::scale(double, double)`). Macros are
  definitions (`#define GEO_PI 3.14` → `Constant`, `#define GEO_SQ(x) …` →
  `Function`), and their invocations are **uses** (`Read`, so `REFERENCES`
  edges), not calls: the code a macro expands to runs at the call site.
  A class template specialization is its own class, named with its
  arguments (`formatter<std::error_code>`).
  There is no C preprocessor; the pack prepares the text the grammar parses
  with **same-length rewrites** (offsets, lines and the text read for names
  stay the original's): macro tokens whose position allows no other reading
  are blanked (`FMT_API`, `FMT_ENABLE_IF(…)` in a template head); an `#if`
  group keeps all its branches when the grammar can take them together and
  otherwise collapses to one branch, as a preprocessor with one
  configuration would (a function header written once per branch, a group
  inside an expression); and at each remaining parse error a targeted
  repair is tried (`va_arg(ap, char*)`, a loop macro before a block, a
  statement macro without `;`, an unknown specifier macro such as
  `static redisAtomic size_t n`, …), kept only when the file's error count
  drops (`rueca-lang-cpp` crate docs, *Macros and conditional
  compilation*). A namespace macro (`FMT_BEGIN_NAMESPACE` …
  `FMT_END_NAMESPACE`) leaves the segment `${FMT_BEGIN_NAMESPACE}` in the
  qualified names inside, which the resolver expands from the macro's
  definition anywhere in the snapshot (`fmt::v12::format_to`). Receivers
  are typed from declarations in the file: `obj.m()` / `p->m()` is reported
  with the receiver `$type:Foo` when `obj` is a parameter, local, field of
  the enclosing class or global declared `Foo`, `Foo*`, `const Foo&`, or
  `auto obj = new Foo`, or the result of a callable of the file declared
  to return `Foo` (`Conn::open()->close()`). A member chain whose type the
  file does not know (`member_.m()` in an out-of-line body whose class is
  declared in a header) is `$member:member_`, typed by the resolver; a
  namespace-qualified name `ns::f()` is `$ns:ns`.
- **Tests are definitions too**: `test_*` functions and `Test*` classes in
  Python; `describe` / `it` / `test` blocks in JS/TS and Pest become `Test`
  nodes whose qualified name is the describe chain; PHPUnit `test*`,
  `@test` and `#[Test]` methods of `*Test` / `*TestCase` subclasses are
  tests; Go `Test…`, `Benchmark…`, `Fuzz…` and `Example…` functions of
  `_test.go` files are tests;
  in Java, JUnit 4 / 5 and TestNG `@Test` (and parameterized,
  repeated, factory) methods, their fixture methods, JUnit 3 `test*`
  methods, and classes named `*Test` / `*Tests` / `*IT` or declaring tests
  (`Method` / `Class` with `is_test: true`);
  GoogleTest `TEST(Suite, Name)` / `TEST_F` / `TEST_P`
  (`Suite::Name`) and Catch2 / doctest `TEST_CASE("title")` blocks.
- **Named function expressions are definitions** in JS/TS wherever they
  appear (`app.use(function handler() {})`, `defineGetter(req, "query",
  function query() {})`, `return function timing() {}`): a `Function` child
  of the enclosing definition. Anonymous callbacks are not.
- **Exports the resolver needs exactly are recorded as facts**: in JS/TS,
  `export default x`, `module.exports = x` and `export { a as b }` become
  re-exports of the file's own names, so `import x from "./m"` binds the
  right definition instead of a guess.

**Documents** (`.md`, `.markdown`) are parsed with the pure-Rust
[`pulldown-cmark`](https://github.com/pulldown-cmark/pulldown-cmark) instead
of tree-sitter, and become structure in the same graph:

- Every top-level heading (ATX or setext) is a `Section` definition whose
  parent is the nearest preceding heading of a lower level, so `DEFINES`
  gives the document tree (`# Install` contains `## Linux`). The local
  qualified name is the heading chain joined by ` > ` (`Install > Linux`);
  a repeated chain gets a ` (2)`, ` (3)`, … suffix. The span runs from the
  heading to the last non-blank line before the next heading of the same or
  a higher level, the signature is the heading line (`## Linux`) and the doc
  is the section's first prose paragraph (badges skipped, ≤ 500 chars).
- Every link, image and raw-HTML `href` / `src` whose destination is a path
  (not a URL, `mailto:` or a same-page `#anchor`) is an import of that path.
- Nothing else: inline code that looks like an identifier is **not** a
  reference (it would turn prose into 0.6-confidence name guesses), and code
  blocks record nothing in the graph (they stay searchable with
  `search_code`). Headings inside block quotes and lists, front matter and
  MDX are not extracted.

**Structured data** (`rueca-lang-data`) is parsed with pure-Rust,
span-preserving parsers — `jsonc-parser` (JSON, JSONC), `saphyr-parser`
(YAML, at the event level), `toml_edit` (TOML), `roxmltree` (XML),
`hcl-edit` (HCL / Terraform) — and small parsers built here for `.env`,
INI, `.properties` and Dockerfile:

- **Keys.** Every mapping entry is a `Key` whose local name is a **key
  path**: `database.host`; keys that need it are quoted
  (`paths."/v1/a.{b}".get`); mapping elements of arrays are `[N]`
  (`jobs.test.steps[2]`) or keyed by an identity field where the file kind
  has one (`containers["web"].env["DATABASE_URL"]`); documents of a
  multi-document YAML stream are `{N}`. The module name is the path with
  extension, so the global name is `k8s/web.yaml:Deployment/web.spec.replicas`.
  TOML tables and arrays of tables, INI sections, `.properties` keys
  (`server.port` split into a key path) and XML elements of known kinds
  (`pom.xml`, `*.csproj`, `AndroidManifest.xml`) map to the same shape, so
  `database.host` looks the same in YAML, TOML, INI and `.properties`.
- **Signatures and docs.** `key: <preview>` — scalars as written (≤ 80
  chars), maps `{a, b, c, … 2 more}`, arrays `[3 items]` — and the comment
  above a key (plus a trailing one) as its doc. Values of env var providers
  and of secret-looking keys (`password`, `token`, `api_key`, …) are never
  copied into signatures: `DATABASE_URL=•••`.
- **File kinds** add meaning chosen by path and shape: `package.json`
  entry points and tsconfig `extends` → imports; `pyproject.toml` scripts →
  references to the Python function; compose services → `Resource`,
  `environment` → `EnvVar` providers, `build` → `Image` plus imports of the
  context and Dockerfile, `depends_on` / `extends` → references / bases;
  Kubernetes documents → `Resource` `Kind/name`, container `env` →
  providers, `configMapKeyRef` / `secretKeyRef` / `envFrom` → references to
  the ConfigMap or Secret in any manifest; GitHub Actions jobs (with their
  `runs-on`), steps keyed by `id`, `needs` → references, local `uses: ./…`
  → imports from the repository root, `env` → providers; OpenAPI operations
  → `Route` `GET /users/{id}`; Terraform `resource` / `data` / `module` →
  `Resource` (`aws_s3_bucket.logs`), `variable` / `locals` → `Variable`
  (`var.x`, `local.x`), module `source` → an import of the folder, and
  `var.x` / `aws_s3_bucket.logs.arn` traversals → references within the
  module directory; Dockerfile stages → `Image`, `FROM <stage>` → base,
  `COPY --from` → reference, `ENV` → providers.
- **Pointers** are exact: `$ref` (JSON Pointer), YAML aliases and merge
  keys (never expanded: an alias is one reference, so a "billion laughs"
  file costs nothing), and the kind pointers above.
- **Not indexed** (the file stays listed and searchable, status
  `skipped:<reason>`): lockfiles (by the walk), minified / notebook / HAR /
  GeoJSON files and data under `fixtures/`, `testdata/`, `__snapshots__/`
  (`data`), generated files (`generated`), Helm templates (`template`),
  data files over 256 KiB that are not a known kind (`data-size`), and
  data dumps — long root arrays, more than 5,000 keys, mostly array
  elements (`data-shape`). Inside a file, keys stop at depth 32, arrays at
  64 mapping elements and files at 5,000 keys (`partial`).

**Env var and config reads in code** (the code side of the bridge) are
facts of the code packs, recognized only with file-local evidence and
literal names: Python `os.environ["X"]`, `os.environ.get("X")`,
`os.getenv("X")` (with `import os` / `from os import …`); TS/JS
`process.env.X`, `process.env["X"]`, `const { X } = process.env`,
`import.meta.env.X`, `Deno.env.get("X")`, and `config.get("a.b")` with
`config` imported from the `config` package (node-config); PHP
`getenv('X')`, `$_ENV['X']`, `$_SERVER['X']`, Laravel `env('X')`,
`config('app.name')` and `Config::get(…)`. Laravel config files are PHP:
the array a `config/<file>.php` returns becomes `Key`s (`name`,
`providers.cache.driver`), and `env()` calls inside an entry are reads by
that key.

Facts are cached at
`<store>/facts/<sha>-<lang>-v<extractor version>-f<facts format>.bin`.
Re-indexing an unchanged file is a cache hit; bumping a pack's
`extractor_version` invalidates its entries, and changing the facts structs
(`rueca_model::FACTS_FORMAT`) invalidates every entry once. Packs whose
facts depend on the path as well as the bytes (data file kinds are chosen by
name) add a hash of the path to the key. A pack may also decline a file with
a reason (`skipped:data-shape`): the file stays listed and searchable.

**Detection** is by extension, then by file name (`Dockerfile`,
`Dockerfile.*`, `Containerfile`, `.env`, `.env.*`, `pom.xml`,
`AndroidManifest.xml`; other XML stays text); a C header
(`.h`) is C++ when the snapshot has C++ sources. A file gets a language in
the graph only when a pack for it is registered; otherwise it is plain text
(listed, content-searchable).

### 4.3 Module names and import targets

Two per-language hooks, still without guessing symbols:

- `module_name(path)` (the resolver calls `module_name_for(path, facts)`,
  which defaults to it, so a pack can read the module from the file itself —
  a Java `package` clause, a Go `package x_test`): Python walks up `__init__.py` packages and source
  roots (`src/`, `pyproject` package dirs) → `shop.http`. ECMAScript strips
  the extension → `src/util/index`. PHP returns nothing (definitions already
  carry their declared namespace, which may differ from the path). Go
  returns the package's **import path**: the `module` of the nearest
  `go.mod` plus the directory (`github.com/gin-gonic/gin/binding`), with
  `_test` appended for an external test package (`package gin_test`, read
  from the file's facts); every file of a directory with one module name is
  one package (`module_scope` = `Package`).
  Java
  reads the `package` clause (`com.acme.core`; the default package is
  empty), and all files of one package form one module.
- `resolve_import(from, import)`: maps a specifier to a project file or to
  *nothing* (third-party, builtin). Python follows relative levels and source
  roots (`from .http` in `shop/api.py` → `shop/http.py`). TS/JS uses
  [`oxc_resolver`](https://github.com/oxc-project/oxc-resolver) over a virtual
  file system built from the snapshot: extension probing, `index` files,
  `.js` → `.ts` aliasing, `tsconfig` `paths` / `baseUrl` / `extends`,
  `package.json` `exports`, and workspace packages mounted by name.
  Markdown keeps the path as the module name (`README.md:Usage` never
  collides with `README.py`) and resolves a link relative to the document
  (`/x` from the repository root): `#anchor` and `?query` are dropped, the
  path is percent-decoded and tried as written, with `.md`, `x.html` →
  `x.md`, and as a directory (`README.md`, `index.md`, …); a bare path that
  misses is retried from the root.
  PHP maps
  a `use App\Models\User` to its file through Composer `autoload` /
  `autoload-dev` (`psr-4`, `psr-0`, `classmap`) of every `composer.json`
  (monorepos included), falling back to the unique `User.php` whose
  directories match the namespace; `require` / `include` with static paths
  (`__DIR__ . '/x.php'`) resolve relative to the file. Go maps an import
  path to the package **directory** through every `go.mod` (longest module
  path wins) and the local `replace` directives of `go.mod` / `go.work`;
  standard library and third-party paths resolve to nothing. An unaliased
  import binds the conventional name (last path element without `/vN`,
  `go-`, `.vN`); since Go rejects unused imports, an import whose
  conventional name is never used is re-bound to the qualifier the file
  does use (`jsoniter` for `github.com/json-iterator/go`).
  Java maps `import com.acme.Owner` (and `Owner.Pet`, static
  `Owner.find`) to `Owner.java` in the package directory under a source
  root — Maven / Gradle `…/src/<set>/java` at any depth (every module of a
  multi-module build), `<sourceDirectory>` of a `pom.xml`, literal
  `srcDirs` of a `build.gradle` — else under any directory whose path ends
  with the package path; `import com.acme.*` resolves to the package
  directory. JDK and dependency classes are not in the snapshot: external.
  C and C++ files return nothing as module name (definitions carry their
  namespace). An `#include "x.h"` resolves relative to the including file,
  then through the `-iquote` / `-I` directories of every
  `compile_commands.json` and `compile_flags.txt` in the snapshot
  (`-isystem` and directories outside the repository ignored), then
  `include/`, `src/` and the repository root, then the only project file
  whose path ends with `/x.h`; `#include <x.h>` is a system header unless a
  project include directory (or `include/`) has it.

Every resolved import becomes an `IMPORTS` File→File edge (confidence 1.0);
for documents that means "links to" (docs → docs, code files, images).
`resolve_import` may also return a **directory** (a Go package, a Java
on-demand import, a Terraform module source, a compose build context): the
edge is then `IMPORTS` File→Folder, and for package-scoped languages the
import binds the package in that directory.
An import that resolves to nothing marks its names **external**: later
references through them are never fuzzy-matched to a project symbol.

### 4.4 Resolve references (`rueca-resolve`)

**Nodes first.** One `Repository`, one `Folder` per directory, one `File` per
file, one node per definition with its global qualified name
(`shop.http` + `.` + `Session.request`). Structural edges are added with
confidence 1.0: `CONTAINS` (repository → folders → files) and `DEFINES`
(file → top-level definitions, parent → child).

**Binding environments.** For every file, the resolver computes which names
its imports bind: `from .http import Session` binds `Session` to the node
`shop.http.Session`; `import a.b` binds `a`; `import * as ns` binds a
module; star imports bind every exported name. Looking up a name in a module
checks its own definitions, then its re-exports (`export { x } from`,
`export *`), then the names its own imports bind (barrel files, package
`__init__.py`), transitively and cycle-safe up to depth 10. An ECMAScript
`default` import binds a definition named `default`, else the module's
explicit `default` re-export (`export default x`), else a heuristic (the
exported class or function named after the file, else the only export). PHP files have
no binding environment: their names arrive qualified (§4.2), and the
resolver looks them up in an index of PHP classes, functions and constants
by fully qualified name (case-insensitive where PHP is).

**The strategy ladder.** Each reference is resolved by trying strategies in
order; the first one that succeeds wins and stamps the edge:

| Order | Strategy (edge `strategy`) | Applies when | Confidence |
|---|---|---|---|
| 1 | local scope (`same_module`) | no receiver; name defined in an enclosing function | 0.95 |
| 2 | self receiver (`type_resolved`) | receiver is `self` / `cls` / `this`: member of the enclosing class | 0.90 |
| 2 | …inherited / `super` (`type_resolved`) | member found in a base class (depth-first, left-to-right) | 0.85 |
| 3 | imported name (`import_resolved`) | no receiver; name bound by an import | 0.95 |
| 3 | imported module (`import_resolved`) | receiver is an imported module (`pkg.mod.f`, `ns.f`) | 0.90 |
| 3 | imported class (`import_resolved`) | receiver is an imported class (static member) | 0.85 |
| 4 | same module (`same_module`) | no receiver; top-level definition of the same file | 0.90 |
| 4 | same-module class (`same_module`) | receiver is a top-level class of the same file | 0.85 |
| 6 | unique name (`unique_name`) | no receiver; exactly one compatible non-test definition with that name in the project | 0.60 |
| 5 | typed receiver (`type_resolved`) | receiver `$type:T` (§4.2: declared, constructed or returned type known in the file): `T` resolves like a type reference, the member through its bases; a missing member of a type with a base outside the project is external | 0.85 |
| 6 | unique method (`heuristic`) | call with an unknown receiver; exactly one method with that name (≥ 4 chars, not a common builtin method like `keys`, `then`) | 0.50 |

**Members by kind.** A type's members are kept in separate slots by kind:
callables, nested types and values (fields, properties, constants). A
call looks for a callable first (then a nested type, then a value), a
read or write for a value first (then a callable that is not a
constructor, then a type), a type reference or a receiver path for a type
first; each slot is searched through the whole base hierarchy before the
next. So a Java field `code` and method `code()` do not hide each other,
and a constructor — a Java or C++ method named like its class — is reached
by calls only: a type named like an enclosing class or its superclass is
that type, never its constructor. An ECMAScript function that owns
methods (`View.prototype.render = function render() {}`) counts as a type
for `this` and `$type:View` receivers, and so does a function or variable
owning object-literal methods (`const api = { get() {} }`,
`return { next() {} }`, `export default { start() {} }`, which the pack
records as a `default` variable): `api.get()`, also through an import,
finds the member, and a bare call inside such a method never finds a
sibling method by local scope. A name bound to a whole module
(`const Queue = require("./queue")`) that is called, constructed or used
as a type is the module's default export (`module.exports = Queue`).

Rungs and rules that the RFC 0003 packs opt into (Python, ECMAScript, PHP
and Markdown resolve exactly as above; Terraform uses directory scope; Go
uses package scope, directory imports, `$self` and `$type:`; Java uses
package scope, implicit self, typed receivers, overloads and overrides;
C and C++ use implicit self, typed receivers, declarations, overloads,
overrides and includes):

| Rule | Hook | Effect | Confidence |
|---|---|---|---|
| package scope | `module_scope` = `Package` / `Directory` | rung 4 and module lookups cover every file of the package; a top-level `T.m` (Go method) is a member of `T` | as rung 4 (0.90) |
| implicit self (rung 1b) | `implicit_self` | a receiver-less name inside a method is first a member of the enclosing class, then of outer classes (C++, Java) | `type_resolved` 0.90 / 0.85 inherited |
| namespace scope (rung 1c) | C/C++ family | a receiver-less name inside `a::b::f` is `a::b::name`, else `a::name` (exact qualified name) | `same_module` 0.90 / `import_resolved` 0.95 |
| namespace-qualified name | receiver `$ns:a` | `a::name` from the innermost enclosing namespace outwards, then global; else the bare name through includes — never an implicit member | as namespace scope |
| member chain | receiver `$member:a.b` | member `a` of the enclosing type, typed by its declaration (its first `Type` reference, in its own file), then `b`, then the name | `type_resolved` 0.85 |
| namespace macros | C/C++ family | `${M}` segments become the namespaces `M`'s definition opens (`fmt::v12`); exact lookups see through its inline namespaces | — |
| typed receiver (strategy 5) | receiver text `$type:T` | `T` resolves like a type reference (or as an exact qualified name), the member through its bases (also for Python, ECMAScript and PHP, whose packs type receivers from declarations and assignments) | `type_resolved` 0.85 |
| normalized self | receiver `$self` in `self_receivers` | the enclosing type, or the type named by the method's qualified-name prefix (Go) | `type_resolved` 0.90 |
| declarations | `Definition::is_declaration` | a C/C++ prototype or in-class declaration is replaced by its unique body; in the same file the two are one node with the body's span | as the rung |
| overloads | `overloads` | `parse(String)` / `parse(Reader)` chosen by the call's argument count, then by the static argument types the pack reports (`value$args:long`): overloads that cannot accept a known argument (a primitive that does not widen, a non-`String` for `String`) are dropped, and of the rest the one whose parameter types equal the arguments', else the one whose primitive parameters are the narrowest, wins; else all remaining linked | as the rung, × 0.8 when several; C/C++ arity comes from the signature (default arguments optional), a C++ set spans files and only overloads visible through includes are candidates |
| overrides | `overrides` | `OVERRIDES` from a method to the same-named method of each base, paired by signature: same parameter count (overload suffix, else the signature's parameter list) and, among several, the same parameter types ; between C/C++ bodies when they exist | `type_resolved` 0.90 |
| includes | C/C++ family | `#include` is a transitive star import | as rung 3 |
| implicit interfaces | Go family | a type whose method names (own and promoted through embedding) cover an interface's gets `IMPLEMENTS` to it and `OVERRIDES` from each covering method; ≤ 64 implementers per interface | `heuristic` 0.50 |

A C++ example: in `double Circle::area() const { return GEO_PI * GEO_SQ(r_); }`
(in `src/shape.cpp`, which includes `geo/shape.h`), `r_` is a member of
`geo::Circle` found through the body's qualified name (implicit self,
0.90), and `GEO_SQ` / `GEO_PI` are the header's macros bound by the include
(`REFERENCES`, 0.95: a macro is used, not called). In `geo::Circle c(2.0);
c.area();`, the receiver `$type:geo::Circle` finds the in-class
declaration of `area`, which is replaced by its unique body in
`shape.cpp` (0.85); the construction calls the class and its constructor
(× 0.95). `geo::scale(a, 2.0)` has the receiver `$ns:geo` (namespaces are
not nodes): `geo::scale` is found by its exact qualified name, and the two
arguments pick `geo::scale(double, double)`. In `double
geo::Circle::reach() const { return center_.norm(); }` (`center_` is a
`Point` field declared in the header), `$member:center_` types the call
through the field's declaration and reaches `geo::Point::norm`.

A reference to a builtin, a name bound to an external module, or a
`self` member that does not exist **stops** resolution: it is counted as an
expected miss, never fuzzy-matched. For JS/TS, "builtin" includes web and
runtime globals that a project could also define (`Request`, `Response`,
`Headers`, `Event`, `crypto`, …): an imported or same-module `Request` still
resolves through rungs 1–4, but an unbound one is never matched to a project
type by unique name. Calls to a class also call its
`__init__` / `constructor` / `__construct` (confidence × 0.95).

PHP references use the same rungs with PHP receivers:

| Strategy (edge `strategy`) | Applies when | Confidence |
|---|---|---|
| name (`import_resolved`) | class, function or constant by fully qualified name (`new User`, `format_money()`, `User::class`) | 0.95 |
| global fallback (`import_resolved`) | unqualified function / constant not in the current namespace (`helper()` → global `helper`) | 0.90 |
| autoload file (`import_resolved`) | class not indexed under that name, found in the file Composer maps it to | 0.80 |
| static member (`import_resolved`) | `User::find()`, `Status::Active`, `Config::$cache` (through parents, traits, interfaces) | 0.85 |
| typed receiver (`type_resolved`) | `$user->save()` / `$this->repo->find()` with a declared class (§4.2) | 0.85 |
| `$this` / `self` / `static` / `parent` (`type_resolved`) | as `self` / `super` above | 0.90 / 0.85 |
| unique method (`heuristic`) | any other receiver | 0.50 |

A PHP name outside the project (a vendor namespace such as `Illuminate\…`
in an application, `\DateTime`) is external or builtin; a member missing
from a class that extends, uses or implements vendor code (an Eloquent
model's `where`, a test's `assertSame`) is external too.

Java references go through the same ladder with the RFC 0003 rules on:

| Java reference | Rule (edge `strategy`) | Confidence |
|---|---|---|
| `m()` inside a class: own, outer or inherited member | implicit self (`type_resolved`) | 0.90 / 0.85 inherited |
| `m()` bound by `import static` (single, on demand) | imported binding (`import_resolved`) | 0.95 |
| `Owner`, `new Owner(…)`: imported, or in the same package (any file) | imported binding / same module | 0.95 / 0.90 (constructor × 0.95) |
| `Owner.find()`, `Color.RED` | imported / same-package class (`import_resolved` / `same_module`) | 0.85 |
| `x.m()` with `x` declared as `T`, `new T().m()`, `((T) e).m()`, `own().m()` with `own` declared `T own()` in the file | typed receiver (`type_resolved`) | 0.85 |
| `this.m()`, `super.m()`, `this(…)`, `super(…)`; inside an anonymous class, its own and inherited members first | self / `super` (`type_resolved`) | 0.90 / 0.85 |
| several overloads with the call's argument count, not told apart by the argument types | as the rule, × 0.8 each | |
| anything else (`a.b().c()`) | unique method (`heuristic`) | 0.50 |

Overriding methods get `OVERRIDES` to the same-named method of each
resolved superclass and interface, paired by signature when the base is
overloaded (`type_resolved` 0.90), so `trace` on an interface method with
`edge_types: ["CALLS", "OVERRIDES"]` reaches the implementations,
anonymous classes included. JDK and dependency types are external; `java.lang`
names are builtins.

In the example: `self.send` matches rung 2 → 0.90; `Session` matches rung 3
→ 0.95; `Session().request` has the receiver `$type:Session`, so rung 5
applies → 0.85.

**From reference kind to edge kind.**

| Reference | Edge |
|---|---|
| `Call`, `New` | `CALLS` (+ `TESTS` when a test calls non-test code) |
| `Decorator` | `DECORATES` (decorator → decorated definition) |
| `Type` | `REFERENCES`, or `INHERITS` / `IMPLEMENTS` for a class's own base list |
| `Throw` | `THROWS` |
| `Read` | `REFERENCES` |
| `Write` | `WRITES` |
| `EnvVar`, `ConfigKey` | `READS` (the bridge, below) |
| `DataRef` | `REFERENCES` (data pointers, below) |

**The code ↔ config bridge and data pointers** (RFC 0003 §6–§7; Go emits
env var reads — `os.Getenv("X")`, `os.LookupEnv` — and viper config key
reads; Java emits env var and Spring config key reads; C and C++ emit `getenv("X")`
reads). These
references never go through the
ladder: they match exact names only, so an env var or a key path can never
be fuzzy-matched to a code symbol.

- An `EnvVar` read (`os.getenv("DATABASE_URL")`, `process.env.X`) gets a
  `READS` edge to the **canonical env var** `$env:DATABASE_URL`, a
  file-less `EnvVar` node created when anything reads or sets the name
  (`contract`, 0.95). Every `EnvVar` *definition* — a `.env` line, a
  compose / Kubernetes / GitHub Actions `env` entry, a Dockerfile `ENV` —
  is a provider with a `WRITES` edge to it (1.0). Config that reads env
  (Spring `${DB_URL}`) reads from its `Key`, so chains compose: function
  → `READS` → key → `READS` → env var ← `WRITES` ← manifest.
- A `ConfigKey` read (`config('app.name')`, `@Value("${server.port}")`)
  carries a namespace; it gets `READS` to every `Key` with that key path in
  the namespace's config files (`spring`: `application*` / `bootstrap*`,
  every profile; `laravel`: `config/<file>.php`; `dotnet`:
  `appsettings*.json`; `node-config`: `config/*`; `viper`: `config.*` or
  the files `SetConfigName` names) at 0.90; without a scope match, to the
  only `Key` in the snapshot with that path (`unique_name`, 0.60);
  several candidates → nothing.
- A `DataRef` (`$ref`, a YAML alias, `COPY --from`) points at a key path:
  in the same file (1.0), in its package (Terraform directory, 0.95), in an
  imported file (`./user.yaml#/User`, 0.95), at a global qualified name
  (`pyproject` scripts → a Python function, 0.95) or in any data file
  (unique, 0.60). Bases of data definitions (YAML merge keys, compose
  `extends`, Dockerfile `FROM <stage>`) resolve the same way into
  `INHERITS`.

Duplicate edges `(src, dst, kind)` are merged keeping the best strategy, the
highest confidence and the first evidence. **Evidence** is the file and span
of the reference: this is what `trace` returns as `call_site`.

### 4.5 Write the segment (`rueca-segment`)

The resolved graph is frozen into one segment file plus content packs. Each
component exists because a tool needs it:

| Component | What it is | Used by |
|---|---|---|
| node table, file table | every node / file with its metadata | all tools |
| CSR adjacency, out and in | edges grouped by source and by destination, sorted by kind | `trace`, `get_symbol` |
| edge metadata | confidence, strategy, evidence span per edge | `trace` |
| name indexes | node ids sorted by qualified name and by name | symbol lookup (`get_symbol`, `get_snippet`, `trace`) |
| file → symbols | symbol nodes per file, sorted by position | `get_file_outline`, `search_code` (enclosing symbol) |
| tantivy symbol index | BM25 over name, qualified name, signature, path, docstring and the symbol's source (`body`, from the content packs) | `search_symbols` |
| trigram index | which files contain each 3-byte sequence | `search_code` |
| content packs | zstd-compressed file contents by blob SHA | `search_code`, `get_snippet` |

The segment is written to a temp file and atomically renamed; the catalog
then points the indexed root at it. Query time never reads the working tree.

## 5. The graph schema

**Node labels** produced by the alpha:

| Label | Produced from |
|---|---|
| `Repository`, `Folder`, `File` | the walk (every listed file, any language) |
| `Module` | TS `namespace` / `declare module` |
| `Class`, `Interface`, `Trait`, `Struct`, `Enum`, `TypeAlias` | class / interface / PHP trait / Go struct / C and C++ `struct` and `union` / enum / type declarations (`typedef`, `using`) |
| `Function`, `Method` | functions, methods, arrow functions assigned to names, getters/setters; Go methods (`Server.Start`, attached to their type through the package) and interface methods; C/C++ prototypes and out-of-line bodies, function-like macros |
| `Package` | Go external test packages (`package x_test`), marking the module |
| `Field`, `Variable`, `Constant` | class fields, `self.x =` in `__init__`, PHP properties (`$email`, also promoted constructor parameters), module-level assignments (`UPPER_CASE` → `Constant`), PHP `const` / `define()` / enum cases |
| `Test` | JS/TS `describe` / `it` / `test` blocks, Pest tests, GoogleTest `TEST*` and Catch2 `TEST_CASE` blocks (Python and PHPUnit tests are `Function` / `Method` / `Class` with `is_test: true`) |
| `Section` | Markdown headings, nested by `DEFINES` |
| `EnvVar` | canonical env vars `$env:NAME` (file-less) and env var providers (`.env` lines, compose / Kubernetes / GitHub Actions `env` entries, Dockerfile `ENV`) |
| `Key`, `Document` | structured-data keys at any depth (key path = local name), Laravel `config/*.php` keys, and documents of multi-document YAML streams |
| `Resource`, `Image` | deployable / infrastructure objects (Kubernetes objects, compose services, Terraform resources, data sources and modules) and container images (Dockerfile stages, compose `build`) |
| `Route`, `Variable` (data) | OpenAPI operations (`GET /users/{id}`); Terraform `var.x` / `local.x` and Dockerfile `ARG` |

Every symbol node carries: name, qualified name, file, span, language,
signature, docstring, `exported`, `is_test`. Canonical env vars have no
file, span or language.

**Edge kinds**: `CONTAINS`, `DEFINES`, `IMPORTS` (structural, 1.0; `IMPORTS`
also File → Folder for directory imports); `CALLS`, `REFERENCES`,
`INHERITS`, `IMPLEMENTS`, `DECORATES`, `THROWS`, `WRITES`, `TESTS`
(resolved, with strategy and confidence). `READS` (env var and config key
reads by code and by config that reads env) and `WRITES` from env var
providers form the code ↔ config bridge; `REFERENCES` also carries data
pointers (`$ref`, aliases, `depends_on`, Terraform traversals), and
`INHERITS` YAML merges, compose `extends` and Dockerfile `FROM <stage>`.
`OVERRIDES` come from Java and C++ overrides and Go's implicit interfaces
(heuristic). Go embedding (struct fields and
interfaces) and type aliases are `INHERITS`; Go `IMPLEMENTS` edges are
inferred (heuristic 0.5).
Python, ECMAScript, PHP and Java reads of ordinary names stay `REFERENCES`.

**Edge metadata**: `confidence` (0–1), `strategy`, and `evidence` (file +
span of the reference that produced it; absent for structural edges).
Strategies: `syntactic`, `scip`, `type_resolved`, `import_resolved`,
`same_module`, `unique_name`, `heuristic`, and `contract` (the code ↔
config bridge).

## 6. How each tool uses the graph

All six tools first pick the segment (the `repo` argument, the server's
`--root`, or the only indexed root) and, if they take a symbol, run the same
**symbol lookup**:

1. `id` → parse `repo@commit:path#local.name` and find that definition in
   that file (the commit is ignored, so ids survive re-indexing);
2. else `qualified_name` → exact match in the qualified-name index, then
   suffix match on a `.` / `:` / `/` / `\` boundary (`Session.send` finds
   `shop.http.Session.send`, `User::save` finds `App\Models\User::save`,
   `server.port` finds `config/app.yaml:server.port`), or after a key-path
   accessor (`["DATABASE_URL"]`); for Java and C/C++, an overload's
   parameter suffix is optional (`Parser.parse` finds every
   `Parser.parse(…)`);
3. else `name` → name index, optionally narrowed by `path`.

When several nodes match, the pick is deterministic (non-test, exported,
symbol before container, shorter qualified name, lower id) and the others
are returned as `candidates`.

| Tool | Reads | Graph traversal |
|---|---|---|
| `search_symbols` | tantivy symbol index → node table | none |
| `search_code` | trigram index → content packs → file → symbols | none (positional) |
| `get_symbol` | lookup → node table → adjacency | one hop: `DEFINES`, `INHERITS`/`IMPLEMENTS`, `CALLS` counts |
| `get_snippet` | lookup → node span → content packs | none |
| `trace` | lookup → adjacency + edge metadata | breadth-first, N hops |
| `get_file_outline` | file → symbols → `DEFINES` | containment tree |

### `search_symbols` — "where is the thing called…?" / "where is the code that…?"

Ranks definitions, not lines. The query is split with the same code-aware
tokenizer used at index time (`user by id` → `user`, `by`, `id`, and
`getUserById` → `get`, `user`, `by`, `id`, plus the whole identifier), then
BM25 runs over six fields with boosts: the symbol's **source text** (`body`)
×8, name ×4, qualified name ×3, signature ×1.5, path ×1, docstring ×0.5.
On top of that:

- **Exact and qualified matches**: the whole query equal to a name (×20) or
  qualified name (×10); a dotted part of the query (`Session.request`,
  `util:formatAmount`) equal to a qualified-name suffix (×20); two adjacent
  words (`session request`) equal to one (×10). So `Session.request` ranks
  that method above `session` and `Session`.
- **Name lookups vs descriptions**: a one-chunk query (`getUserById`) is a
  name lookup and the body only breaks ties (×0.25), so a definition is not
  outranked by its callers. A multi-word query (keywords, an error message,
  an issue) is matched against the code itself: a function whose body uses
  `redirect`, `location` and `header` is found even if its name says none of
  that. Words repeated in the query weigh more (`1 + ln count`).
- **Priors**: the score is multiplied by 0.5 for fields, variables and
  constants, and by 0.5 for tests (test functions, `describe`/`it` blocks,
  files under `test`/`tests`/`__tests__`/`spec` or named `test_*.py`,
  `*.spec.ts`, …), so code an agent navigates to comes first.

Filters (`labels`, `language`) are exact terms; `path_glob` filters after
ranking. Default labels exclude `File`, `Folder`, `Module` and `Repository`, so
document sections are found alongside code (`install linux` →
`README.md:Install > Linux`); they also exclude structured-data `Key` and
`Document` nodes unless `language` is a data language, because a repository
has far more config keys than functions. Ties break by node id so pages are
stable.

### `search_code` — "where does this text appear?"

The only tool that reads file contents for matching. The pattern (literal or
regex) is parsed; required literals are extracted and turned into an AND/OR
query over trigram postings, which yields candidate files. Candidates are
decompressed from the content packs in path order and verified with the real
regex, stopping at `limit` or the scan budget (`truncated: true`). Patterns
with no usable literal scan every text file. Each match is then **placed in
the graph**: the file's symbols (sorted by position) give the innermost
definition containing that line, returned as `symbol`. In the example,
searching `self.send` returns line 5 of `shop/http.py` with symbol
`Session.request`; a match in a document is placed in its section.

### `get_symbol` — "tell me about this symbol and its neighbourhood"

One hop around the node:

- `parent`: incoming `DEFINES` (the class or file that declares it);
- `children`: outgoing `DEFINES` (methods of a class);
- `bases`: outgoing `INHERITS` + `IMPLEMENTS`;
- `subtypes`: incoming `INHERITS` + `IMPLEMENTS`;
- `callers_count` / `callees_count`: distinct nodes across incoming /
  outgoing `CALLS`, so the agent can decide whether a `trace` is worth it;
- `readers_count` / `writers_count` (env vars and config keys only):
  distinct nodes across incoming `READS` / `WRITES` — "who reads it, does
  anything set it?".

### `get_snippet` — "show me the code"

Resolves the symbol to its file and span (or takes `path` + lines), fetches
the blob from the content packs, and returns those lines plus
`context_lines`, capped by `max_lines`. It serves the indexed version of the
file, which matches what the graph describes. For a Markdown `Section` the
span is the whole section, subsections included.

### `trace` — "who calls this, and what does it call?"

Breadth-first search from the root node over the in-adjacency (`callers`)
and/or out-adjacency (`callees`), level by level up to `depth`, following
only `edge_types` (default `CALLS`) and skipping edges below
`min_confidence`. Each direction keeps its own visited set, so a node
reachable both ways appears once per direction. Every traversed edge is
returned with its confidence, strategy and `call_site` (the evidence span).
`limit` caps nodes and sets `truncated`.

Which neighbours count:

- **Tests are left out by default.** In a well-tested project most callers
  of a function are tests (over half of them in a sample of `vuejs/core`
  functions), which would
  bury the code callers and eat the `limit`. They are counted in
  `tests_excluded`; `include_tests: true` brings them back, and following
  `TESTS` edges includes them automatically.
- **Code outside functions is reported where it lives.** A call at module
  level comes from the `File` node; one in a class body or a variable / field
  initializer comes from that `Class`, `Variable`, `Constant` or `Field`.
  These are real call sites (the code runs on import or construction), so
  they are kept and the label says what they are.
- **Calling a class is a constructor call**: the callee is the `Class`, and
  its `__init__` / `constructor` is listed as a callee too (confidence
  × 0.95).
- `labels` restricts reported *and traversed* neighbours, e.g.
  `["Function", "Method"]` for a callables-only call graph. The root is never
  filtered. Other edge kinds make `trace` a
general traversal: `["INHERITS"]` walks a class hierarchy, `["IMPORTS"]`
from a `File` node walks module dependencies (and, from a document, the
files it links to; `callers` gives the documents that link to a file),
`["TESTS"]` finds the tests that exercise a function.

### `get_file_outline` — "what is in this file?"

Takes the file's symbols (already sorted by position) and nests them by
`DEFINES` parent, falling back to span containment, children ordered by
line. No content is read; signatures come from the node table. For a
Markdown file the outline is its heading tree.

### Putting them together

The recommended agent loop, and why it is cheap:

1. `search_symbols` (with a data `language` for config keys; `search_code`
   for literals and error messages) to find candidates. Milliseconds, a few hundred tokens.
2. `get_symbol` to confirm the right one and see its neighbourhood size.
3. `get_snippet` to read only that code.
4. `trace` to follow callers / callees, filtering by `min_confidence`.

Each step returns ids that the next step accepts, so the agent navigates the
graph instead of reading whole files.

## 7. Confidence: how to read an edge

| Confidence | Strategies | Meaning |
|---|---|---|
| 1.0 | `syntactic`, `import_resolved` (IMPORTS) | Structure or a resolved import. Certain. |
| 0.90–0.95 | `contract` | An env var or config key read resolved through a known namespace (the code ↔ config bridge). |
| 0.85–0.95 | `type_resolved`, `import_resolved`, `same_module` | Resolved through scope, `self`, or an import. Wrong only with dynamic tricks (monkey-patching, `getattr`, re-assigned names). |
| 0.60 | `unique_name` | A bare name that matched the only compatible definition in the project. Usually right in small/medium code bases; can be wrong when a third-party symbol shadows it. |
| 0.50 | `heuristic` | Method call on an untyped receiver matched by name alone. Useful for exploration; verify before relying on it. |

Practical guidance for agents: use the default (all edges) to explore;
use `min_confidence: 0.6` to drop pure name guesses, `0.85` for
"resolved through code structure only".

Known gaps behind low-confidence or missing edges, in rough order of impact:

- **Receiver typing is file-local** (strategy 5): `x = Session();
  x.request()`, `const app = new Hono(); app.get()` and annotated
  parameters resolve, but a receiver whose type only another file knows —
  the result of a function or method declared elsewhere
  (`assertThat(x).isEqualTo(y)`, builder chains), a callback parameter
  typed by context (`app.use((c, next) => c.header())`), an element of a
  collection — resolves by name only (0.50) or not at all. This is now the
  largest source of misses.
- No call-graph knowledge of third-party code; calls into dependencies stop
  at the import.
- Java: receivers are not typed from fields inherited from a class in
  another file; overloads that differ only by class parameters (subtyping
  is not known) are linked together (× 0.8) unless an argument type matches
  one exactly; method references (`Text::upper`) are calls without arity.
- Python MRO is approximated depth-first (differs from C3 only for diamond
  inheritance).
- Dynamic dispatch, decorators that replace functions, `getattr`,
  dependency injection containers. In PHP: magic methods (`__call`,
  `__callStatic`, `__get`), Laravel facades and the service container,
  dynamic properties, `$this->$name()`, and properties declared only in a
  parent class in another file (no receiver type).
- Same-name definitions: a bare-name `search_symbols` lookup of `get` or
  `add_url_rule` ranks them by text only (shorter, denser matches first);
  nothing structural (base class, call count) breaks the tie yet.
- `search_symbols` ranks symbols, not files: a file is found through its
  best-matching definition. Module-level code that is not inside a
  definition is only reachable through `search_code`.
- C and C++ without a preprocessor: code generated by macros is invisible
  (a macro invocation is a use of the macro, not the calls in its body),
  `#if` groups the grammar can take keep all their branches, and files the
  grammars still cannot fully parse lose the definitions and references
  inside the broken region (0–1.3% of the files of the C smoke
  repositories, 5.7% of fmt's with its vendored GoogleTest). Conditions
  are not evaluated with the project's `-D` flags. Overloads are chosen by
  argument count only; ADL, calls through function pointers and virtual
  dispatch are not followed.

The benchmark ([alpha results](bench/alpha-results.md)) measures how these
gaps show up on real repositories.

## 8. Determinism, incrementality, limits

- **Deterministic**: the same files produce the same graph, byte for byte
  in node/edge order (inputs are sorted; ties break by path and id). Tests
  shuffle inputs and compare serialized output.
- **Incremental where it matters**: extraction, the expensive part, is
  cached per blob; unchanged files are never re-parsed. Resolution and the
  segment are rebuilt per snapshot (tens to hundreds of milliseconds on
  medium repositories). File contents are stored once per blob across
  segments.
- **Bounded**: per-file budgets (2 MiB, 5 s parse, depth 512, 20 000
  references); files over budget are listed with a `skipped:*` status
  instead of failing the index.
- **Immutable**: a segment is never modified after it is written; re-indexing
  writes a new one and swaps the catalog entry atomically, so queries running
  during a re-index keep working.

## 9. Where the code lives

| Step | Crate | Start reading at |
|---|---|---|
| Data model (labels, edge kinds, facts, ids) | `rueca-model` | `src/lib.rs` |
| Language-pack contract, budgeted parsing | `rueca-extract` | `LanguagePack` (crate docs list the RFC 0003 hooks) |
| Python extraction, module names, imports | `rueca-lang-python` | crate docs, `queries/locals.scm` |
| TS/TSX/JS extraction, `oxc_resolver` | `rueca-lang-typescript` | crate docs, `src/resolve.rs` |
| Markdown sections and links (`pulldown-cmark`) | `rueca-lang-markdown` | crate docs, `src/links.rs` |
| Structured data, file kinds, data pointers | `rueca-lang-data` | crate docs (mapping tables), `src/emit.rs`, `src/kinds/` |
| PHP extraction, name resolution, Composer autoload | `rueca-lang-php` | crate docs, `src/names.rs`, `src/composer.rs` |
| Go extraction, receivers, `go.mod` module model | `rueca-lang-go` | crate docs, `src/locals.rs`, `src/modules.rs` |
| Java extraction, packages, Maven / Gradle source roots | `rueca-lang-java` | crate docs, `src/extract.rs`, `src/project.rs`, `queries/locals.scm` |
| C/C++ extraction, preprocessing (masking, conditionals, repairs), include directories | `rueca-lang-cpp` | crate docs, `src/extract.rs`, `src/preprocess.rs`, `src/mask.rs`, `src/cond.rs`, `src/repair.rs`, `src/includes.rs` |
| Walk, cache, snapshot identity | `rueca-index` | `index_worktree` |
| Resolution | `rueca-resolve` | crate docs (strategy table); `src/hooks.rs` (RFC 0003 hooks), `src/bridge.rs` (env vars, config keys, data pointers), `src/go.rs` (implicit interfaces) |
| Segment, search, packs, catalog | `rueca-segment` | crate docs (format spec) |
| Tools | `rueca-query` | `src/tools.rs` |
| MCP / CLI | `rueca-mcp`, `rueca` | `serve_stdio`, `src/main.rs` |
