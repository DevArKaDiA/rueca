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
10. [Embeddings (RFC 0007)](#10-embeddings-rfc-0007)

---

## 1. The pipeline in one picture

```
                 per file (parallel, cached by blob)              per snapshot                   query time
              ┌──────────────────────────────────────┐   ┌──────────────────────────┐   ┌─────────────────────────┐
working tree ─┤ walk → read → blob SHA → tree-sitter ├──▶│ resolve: facts → graph   ├──▶│ segment (one file)      │◀── the tools
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

### 4.2 Extract facts per file (`rueca-lang-python`, `rueca-lang-typescript`, `rueca-lang-php`, `rueca-lang-go`, `rueca-lang-java`, `rueca-lang-cpp`, `rueca-lang-rust`, `rueca-lang-markdown`, `rueca-lang-data`, `rueca-lang-proto`)

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
- **Rust names follow the module tree, items join with `.`.** The module
  name of a file is its module path, crate name first
  (`rueca_index::link`, from `Cargo.toml` and the file layout, §4.3); the
  local name of an item joins its containers with `.`: a function `f`, a
  nested function `outer.inner`, an item of an inline module `m.f` (the
  module is a `Module` definition owning its items), a trait item `Tr.m`.
  Items of an `impl` block are named after the last segment of the self
  type (`impl<T> Vec<T>` → `Vec.push`, `impl Display for &Foo<'a>` →
  `Foo.fmt`) and are top level (or children of their inline module), not
  children of the type: the block may be in another file than the type.
  They carry the self type as written in `bases` and the trait in
  `implements`, which the resolver turns into membership, `IMPLEMENTS` and
  `OVERRIDES` (§4.4). Structs and unions are `Struct` (named fields are
  `Field`s), enums `Enum` (variants `Constant`s), traits `Interface`,
  `macro_rules!` a callable `Function` (an invocation of a repository macro
  is a call). Receivers are typed from the file (`self` → `$self`,
  annotations, `T { … }` / `T::new()` / `T(…)`, return types of the file's
  functions and methods, `?` / `unwrap()`, `for` elements, `Some(x)`
  patterns, struct fields, a generic parameter's bound, `impl Tr` / `dyn
  Tr`, smart pointers seen through); `self.a.b.m()` with `a` declared in
  another file is `$member:a.b`, and the result of a call the file cannot
  type (a function or method of another file) is `$ret:<receiver>#<name>`
  (`?` when unwrapped), which the resolver types from the callee's
  signature. **Macro arguments are code**: a macro's
  argument list is a flat token tree to tree-sitter, so the pack parses it
  again on its own — wrapped as a tuple, an array or a block, the text copied
  byte for byte so every node maps back to the file by one offset — and
  visits it like the file's own code (`println!("{}", total())`,
  `assert_eq!(v.len(), 3)`, `vec![make()]`, `write!(f, …)`), with its own
  local scopes for closures inside the arguments. Standard macros are not
  references; `stringify!`, `concat!`, `include*!` and `quote!`-style token
  builders are not re-parsed, and `macro_rules!` bodies are token patterns,
  not code (rust-analyzer records no references in them either).
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
  (`geo::scale(double)`, `geo::scale(double, double)`); callables with the
  same parameters are told apart by a comment the lookups ignore: their
  qualifiers (`get()` and `get(/*const*/)`) or, for another body (a SFINAE
  overload, one body per `#if` branch), its ordinal (`write(OutputIt,
  T/*#2*/)`). A function template's explicit specialization is named with
  its arguments (`num_bits<int128_opt>`), so calls bind the primary
  template. Macros are definitions (`#define GEO_PI 3.14` → `Constant`,
  `#define GEO_SQ(x) …` → `Function`), and their invocations are **uses**
  (`Read`, so `REFERENCES` edges), not calls; the calls a macro's body
  makes (read lexically: `#define lua_pop(L,n) lua_settop(L, -(n)-1)`)
  are calls of the macro's definition, which the resolver passes on to
  every user of the macro (*macro expansion* below), as the code after
  preprocessing does. C++ calls carry the static types of literal, cast
  and local-variable arguments (`Write$args:char`) for overload choice.
  A class template specialization is its own class, named with its
  arguments (`formatter<std::error_code>`).
  There is no C preprocessor; the pack prepares the text the grammar parses
  with **same-length rewrites** (offsets, lines and the text read for names
  stay the original's): macro tokens whose position allows no other reading
  are blanked (`FMT_API`, `FMT_ENABLE_IF(…)` in a template head); an `#if`
  group keeps all its branches when the grammar can take them together and
  otherwise collapses to one branch, as a preprocessor with one
  configuration would (a function header written once per branch, a group
  inside an expression), with Objective-C and C++/CLI branches dropped
  (no grammar parses them); test heads become blocks (`TEST(Suite, Name) {`
  in C, Catch2 / doctest `TEST_CASE("title") {` in C++, as `if (…)`); and
  at each remaining parse error a targeted repair is tried (`va_arg(ap,
  char*)`, a loop macro before a block, a statement macro without `;`, an
  unknown specifier macro such as `static redisAtomic size_t n`, …), kept
  only when the file's error count drops (`rueca-lang-cpp` crate docs,
  *Macros and conditional compilation*). A namespace macro (`FMT_BEGIN_NAMESPACE` …
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
  (`Suite::Name`) and Catch2 / doctest `TEST_CASE("title")` blocks; in
  Rust, functions with a test attribute (`#[test]`, `#[tokio::test]`,
  `#[bench]`, `#[rstest]`, …), everything in a `#[cfg(test)]` module and
  every definition under `tests/` or `benches/`.
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
  64 mapping elements and files at 5,000 keys (`partial`). An OpenAPI /
  Swagger document is a known kind wherever its root `openapi` / `swagger`
  key sits (JSON emitters that sort keys write it last), and its operations
  stay `Route`s past the key cap.

**Env var and config reads in code** (the code side of the bridge) are
facts of the code packs, recognized only with file-local evidence and
literal names: Python `os.environ["X"]`, `os.environ.get("X")`,
`os.getenv("X")` (with `import os` / `from os import …`); TS/JS
`process.env.X`, `process.env["X"]`, `const { X } = process.env`,
`import.meta.env.X`, `Deno.env.get("X")`, and `config.get("a.b")` with
`config` imported from the `config` package (node-config); PHP
`getenv('X')`, `$_ENV['X']`, `$_SERVER['X']`, Laravel `env('X')`,
`config('app.name')` and `Config::get(…)`; Rust `std::env::var("X")`,
`env::var("X")` / `var("X")` with `std::env` / `std::env::var` imported (and
`var_os`), `env!("X")` and `option_env!("X")`. Laravel config files are PHP:
the array a `config/<file>.php` returns becomes `Key`s (`name`,
`providers.cache.driver`), and `env()` calls inside an entry are reads by
that key.

**Contract hints in code** (`FileFacts::contracts`, §4.4 *Contracts*) are
found by each code pack in one pass after its definitions, so a provider
names its handler and a consumer its enclosing definition. A URL is
rendered from literals, interpolations and concatenations (a gap is a
hole; a leading interpolated base URL is dropped by the key), and from a
local assigned once in the function. **Python**: Flask / FastAPI /
aiohttp route decorators (composed with a `Blueprint` / `APIRouter`
prefix of the file), `add_url_rule`, Django `path()` / `re_path()`
(method `*`), aiohttp and Starlette route tables provide; calls whose
callee is an HTTP verb (`get`, `_post`, `get_api_list`) or `request` /
`_request` / `urlopen` with a path-like argument consume (method from the
callee, a literal method argument or `method=self._conn.get`); `x.Method`
on a name the file binds to a `<mod>.XStub` (assignment, annotation,
parameter, return type) consumes `X/Method` (`*/Method` on an unbound
holder named `…stub`, a stub handed in as a parameter), and the PascalCase
methods of a `…XServicer` subclass provide it; kafka producers' `send("t")` consume
and consumers' topics provide a topic. **TS / JS**: `app.get("/x", h)`,
`app.route("/x").get(h)`, `fastify.route({…})` (in a file importing a
server framework, or on `app` / `router` / `server` with an inline
handler) and NestJS `@Controller` + `@Get` (composed) provide; `fetch`,
`axios`, `got`, verb wrappers and `request` with a path-like argument
consume, and a wrapper whose URL interpolates its own parameter
(`` `${host}/api/${endpoint}` ``) adds a consumer at each caller passing a
literal there (one hop); `new GreeterClient(…)` from generated gRPC code
and `server.addService(GreeterService, {…})` give `Greeter/SayHello`.
**PHP**: Laravel `Route::get(…)` (also chained, `match`, `any`,
`resource`, inside `Route::prefix(…)->group(…)` closures), Symfony
`#[Route]` attributes (class prefix composed) and Slim / Lumen
`$app->get('/x', handler)` provide; Guzzle-style `->get('/x')` /
`->request('POST', …)`, the `Http::` facade and `CURLOPT_URL` consume.
**Go** (router or client imported in the file; generated files skipped):
`net/http` `HandleFunc` / `Handle("GET /x")`, gin / echo `r.GET`, chi /
fiber `r.Get`, gorilla `.HandleFunc(…).Methods(…)` provide (group,
sub-router and chi `Route` prefixes of the file composed; the handler is
the last function value among the arguments, looked for inside wrapping
calls), `http.Get` / `http.NewRequest`, resty and SDK wrappers called with
a method then a path (`c.do(ctx, http.MethodPost, "/api/generate")`)
consume; a type embedding `pb.Unimplemented<S>Server` or the server
interface `pb.<S>Server` (a delegating wrapper), or registered with
`pb.Register<S>Server(s, x)` (`x` a `&T{}`, the method's receiver or a
local assigned `&T{}`), provides `S/Method` for its handler-shaped methods
(a context and a pointer, or a stream) — in the file, and through a
service marker `S/*` on the type that the resolver expands over its
methods in the other files of the package — a stream parameter
`pb.<S>_<M>Server` gives `S/M`, and a unary server signature taking
`*pb.<M>Request` (a version suffix may be dropped or follow `Request`)
gives `*/M` (service unknown in the file), while
calls on names bound to `pb.<S>Client` consume `S/M` (`*/M` through a
`protoClient` field typed in another file). **Java**: Spring
`@GetMapping` / `@RequestMapping`, JAX-RS `@GET` + `@Path` and Micronaut
`@Get` provide (class prefix composed) — consume on a `@FeignClient` /
`@RegisterRestClient` / Micronaut `@Client` type, for `@GetExchange` and
Feign `@RequestLine` — and `RestTemplate`, `WebClient` / `RestClient`,
`HttpRequest.newBuilder` and OkHttp `Request.Builder` consume; a class
extending `XGrpc.XImplBase` provides `X/Method` for its public
`StreamObserver` methods and calls on `XGrpc` stubs consume it.
**Rust**: actix-web / rocket `#[get("/x")]` attributes (prefixed by the
file's `web::scope` registering the function), actix `.route("/x",
web::get().to(h))` / `web::resource`, and axum `.route("/x",
get(h).post(g))` (with `.nest` of a router built in the same expression)
provide, `reqwest` calls consume; the methods of a tonic service trait
impl (`impl Points for S`, `Request<…>` → `Response<…>`) provide
`Points/Upsert` (snake_case back to the proto's PascalCase) and calls on
names bound to a `PointsClient` consume it.

Facts are cached at
`<store>/facts/<sha>-<lang>-v<extractor version>-f<facts format>.bin`.
Re-indexing an unchanged file is a cache hit; bumping a pack's
`extractor_version` invalidates its entries, and changing the facts structs
(`rueca_model::FACTS_FORMAT`) invalidates every entry once. Packs whose
facts depend on the path as well as the bytes (data file kinds are chosen by
name) add a hash of the path to the key. A pack may also decline a file with
a reason (`skipped:data-shape`): the file stays listed and searchable.

**Detection** is by extension (`.rs` is Rust), then by file name (`Dockerfile`,
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
  empty), and all files of one package form one module. Rust returns the
  file's **module path**: every `Cargo.toml` with a `[package]` is a
  package whose targets are crates (the library, `[lib] path` or
  `src/lib.rs`, named by `[lib] name` or the package name with `-` → `_`;
  binaries `src/main.rs`, `src/bin/*`, `[[bin]]`; `tests/`, `examples/`,
  `benches/` crates; `build.rs`), a file belongs to the crate whose root
  it is or whose directory holds it (else the library), and its module path
  is the crate name plus the directories and stem below the crate root
  (`rueca_index::link`; `mod.rs` names its directory). Every file is its
  own module.
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
  Rust imports are module paths normalized at their site (`self::` and
  `super::` inside inline modules folded into the file's module, see the
  crate docs, *Imports*): `mod x;` is `<dir>/x.rs` or `<dir>/x/mod.rs` of
  the declaring file's module directory (`#[path]` honoured); `crate::a::b`
  starts at the crate root, `super::` at the parent module, a first segment
  that is a module the file declares or a **crate of the snapshot** (a
  dependency of the file's package — renames, `path` and `workspace = true`
  entries followed — else any crate with that name, same workspace first)
  continues from there, and each further segment is a module file; the
  walk returns the deepest module file. `std`, `core`, `alloc` and
  third-party crates resolve to nothing. `resolve_import` maps module
  paths to files; the resolver walks the rest of a `use` path itself
  (inline modules, re-exports, types, §4.4).

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
overrides and includes; Rust uses `$self`, `$type:` and `$member:` with its
own module rules, below):

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
| overloads | `overloads` | `parse(String)` / `parse(Reader)` chosen by the call's argument count, then by the static argument types the pack reports (`value$args:long`): overloads that cannot accept a known argument (a primitive that does not widen, a non-`String` for `String`) are dropped (also a C/C++ class for a pointer parameter), and of the rest the one whose parameter types equal the arguments', else the one whose primitive parameters are the narrowest, wins; else all remaining linked | as the rung, × 0.8 when several; C/C++ arity comes from the signature (default arguments optional), a C++ set spans files and only overloads visible through includes are candidates |
| overrides | `overrides` | `OVERRIDES` from a method to the same-named method of each base, paired by signature: same parameter count (overload suffix, else the signature's parameter list) and, among several, the same parameter types ; between C/C++ bodies when they exist | `type_resolved` 0.90 |
| includes | C/C++ family | `#include` is a transitive star import | as rung 3 |
| macro expansion | C/C++ family | a definition that uses a macro (a `CALLS` or `REFERENCES` edge to a `#define`) also calls what the macro's body calls, through nested macros (depth ≤ 8); a macro defined once per `#if` branch expands as its first non-release (`NDEBUG`) definition | the use's strategy, product of both edges × 0.95 |
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

Rust references go through the same ladder with Rust's module semantics
(`rueca-resolve` crate docs, *Rust*): a name in a module is one of its
items, else what its `mod`, `use` (leaves, re-exports, globs) and
`extern crate` declarations bind, and a `use` path is walked from its
anchor (`crate`, `self` / `super`, a name in scope, a crate) through module
files, inline modules, types and re-export chains. Path qualifiers used in
code (`crate::a::f()`, `super::g()`, `serde_json::to_string(…)`) are bound
by synthetic imports the pack adds for them.

| Rust reference | Rule (edge `strategy`) | Confidence |
|---|---|---|
| `f()` in the same module; a nested function or an item of the enclosing inline module | same module / local scope | 0.90 / 0.95 |
| `f()`, `T` bound by `use` (also through `pub use` chains and globs) | imported binding (`import_resolved`) | 0.95 |
| `m::f()`, `crate::a::f()`, `super::f()`, `other_crate::f()` | imported module (`import_resolved`) | 0.90 |
| `T::new()`, `E::V` through an import / a type of the file | imported / same-module type (`import_resolved` / `same_module`) | 0.85 |
| `self.m()`, `Self::f()`, `self.field` in an `impl` (the type may be declared in another file) | self receiver (`type_resolved`) | 0.90 |
| `x.m()` with `x` typed from the file (`$type:T`), `self.a.m()` through declared field types (`$member:a`); trait default methods through the type | typed receiver (`type_resolved`) | 0.85 |
| `x.m()` on a generic `T: Tr`, `impl Tr` or `dyn Tr` | the trait's method (`type_resolved`) | 0.85 |
| `x.m()` where `x` holds the result of a call the file cannot type (`let s = Store::open(p)?;`, `make().build()`): the callee's declared return type, `Self` and `Result<Self>` / `Option<Self>` through `?` / `unwrap()` included (`$ret:`) | typed receiver (`type_resolved`) | 0.85 |
| `x.m()` with an unknown receiver: one trait method (its implementations count as it), or one method, of that name | `heuristic` | 0.50; several candidates: no edge |
| `my_macro!()` with no import (`#[macro_use]`): the only `macro_rules!` of that name | `unique_name` | 0.60 |

Items of an `impl` block are attached to the type their self type names
(resolved in the item's scope, wherever the type is declared); `impl Tr for
T` adds `IMPLEMENTS` `T` → `Tr`, makes `Tr` a base of `T` (its default
methods are found through `T`) and `OVERRIDES` from each item to the
trait's (`type_resolved` 0.90); a blanket impl (`impl<T: B> Tr for T`)
overrides the trait's items and its items are the by-name targets. Rust
types are open: a member missing from one is external (a derive, a
blanket implementation outside the project), and so is a method call on
an unknown receiver whose name no project method has or that is a common
standard-library method (`unwrap`, `iter`, `insert`, …).

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
reads; Rust emits `std::env::var("X")`, `env!("X")` and `option_env!("X")`
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

**Contracts** ([RFC 0006](rfcs/0006-org-graph.md) phase 4). Packs also
emit contract hints (`FileFacts::contracts`): a route or spec operation a
file *provides* (an OpenAPI `Route`, a `.proto` `rpc`), an endpoint it
*consumes* (an HTTP or RPC call), each with a canonical key
(`rueca_model::contract`: `GET /users/{}` — every parameter segment `{}`,
host, query and trailing `/` dropped — or `pkg.Service/Method`). Every hint
becomes a row of the segment's contract table on its owner's node (the
handler, `Route` or `RpcMethod`; else the file), and keys that match within
the snapshot become `contract` edges: consumer → provider `CALLS`, code
provider → spec provider `IMPLEMENTS`, at 0.9 (same key), 0.85 (a provider
`{}` segment stands for a literal one, or an RPC key without package) or
0.7 (a `*` method, or an RPC key `*/Method` whose service the code does not
name), 0.5 each when several providers tie. Code-to-code and
spec-to-spec providers never link; unmatched consumers stay in the table
(dangling). Providers that are tests or in test files (a test path, or a
Go `_test.go` file: `rueca_model::contract::is_test_source`) keep their
rows but are never match candidates, here and across repositories, so a
fake sidecar does not take a client's calls from the real server. A
`*/M` provider row of a node that also provides `S/M` is dropped.

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
| contract table | every contract hint on its node: side, kind, canonical key, raw text, span, framework (optional component `contracts`) | the organization linker, `list_contracts` |
| tantivy symbol index | BM25 over name, qualified name, signature, path, docstring and the symbol's source (`body`, from the content packs) | `search_symbols` |
| bodiless set | C/C++ functions and methods whose source has no body (prototypes, in-class declarations), for the declaration prior | `search_symbols` |
| trigram index | which files contain each 3-byte sequence | `search_code` |
| content packs | zstd-compressed file contents by blob SHA | `search_code`, `get_snippet` |
| symbol vectors | with `rueca index --embed`: one int8 vector per callable, type and Markdown section, its node, its vector key and a per-row scale, plus the model record (optional components `vectors.meta`, `vectors.nodes`, `vectors.keys`, `vectors.scale`, `vectors.data`; §10) | `search_symbols` in `hybrid` / `semantic` mode |

The segment is written to a temp file and atomically renamed; the catalog
then points the indexed root at it. Query time never reads the working tree.

Every component is BLAKE3-checksummed when a segment opens, except the
vector bulk (`vectors.keys`, `vectors.scale`, `vectors.data`): their bounds
and lengths are checked at open and their checksums on the first dense use
in a process, so lexical queries never pay for them. On this repository
(9,868 vectors of 1,024 dimensions, 19.0 MB segment, 29.5 MB with vectors)
opening takes the same time with or without vectors, about 21 ms, and the
first dense use adds about 7 ms of checksum (debug build with BLAKE3 and
`rueca-segment` optimized). Readers ignore components they do not know, so
a segment with vectors opens in an older build of format 2.

Besides `segments/`, `packs/` and `facts/`, the store holds a **vector
cache** (`<store>/vectors/<embed key>/`, or `$RUECA_VECTOR_DIR`): the f16
vector of every text ever embedded under one embed key, at the model's full
width, keyed by its vector key (BLAKE3 of the embed key and the text), so
worktrees, branches and repositories of one store embed a text once (§10).

**Ref snapshots** ([RFC 0006 §9.3](rfcs/0006-org-graph.md#93-phase-3--implemented)).
`rueca index [PATH] --ref <name>` indexes a branch, pull request (`pr/<n>`),
tag or commit of PATH's repository without touching its working tree: the
name is resolved with `gix` (a branch or pull request missing locally is
fetched from `origin`; a tag or commit too), the commit's tree is written blob by blob to
`<store>/refs/<repo-slug>[--<path-slug>]-<hash>/<ref-slug>/` (the hash of the
repository id and the root's path inside its repository, so neither ids with
one slug nor two subdirectory roots of one repository share an export), and that export is indexed like any
root, its snapshot pinned to the ref (`ref` = the name, `commit`, never
dirty) under the repository's own id. The catalog entry carries `ref_name`,
so one repository id groups its working tree and its ref snapshots; tools
read the working tree unless `ref` names another snapshot, and a `ref` list
(`["main", "feature/x"]`) reads several side by side, collapsing what is
identical (§6, *Responses*). The facts cache
is keyed by blob, so a second ref only re-extracts the files that differ
(its walk, resolution and segment write are paid in full: in a debug
build a second ref of a small repository costs about as much as the
first, RFC 0006 §9.3); re-indexing a ref at the same commit does nothing, and `--ref <name>
--remove` drops the snapshot and its export.

### 4.6 The organization layer (`rueca-segment` `org`)

When repositories are configured as an organization (`rueca-org.toml`,
[RFC 0006](rfcs/0006-org-graph.md)), Rueca keeps one more structure on top
of the repository segments, which do not change: an **organization symbol
index**, so a question asked of the whole organization gets one ranking.

- **Segments side by side.** Every segment already embeds a tantivy index
  of exactly one tantivy segment (doc id = node id). The organization index
  is a tantivy index whose segments are those, copied unchanged out of the
  bundles into `<store>/orgs/<name>/index/` — same schema, tokenizer, body
  text and priors, nothing re-tokenized. tantivy computes BM25 statistics
  over all segments of an index, so a document scores the same way whichever
  repository it comes from: scores are comparable by construction. A hit is
  (repository, node id), mapped back to the repository's segment for ids,
  paths and the graph.
- **Incremental.** `rueca index --org <file> <root>` (or a plain `rueca
  index <root>` of a member) re-indexes that repository with the normal
  pipeline, then replaces only its part: the new segment's tantivy files are
  copied in, the old ones dropped from the list. Other repositories are not
  touched. On the benchmark's first, 21-repository organization this took
  70–140 ms (mostly fsyncs), against 0.2–4.7 s for indexing the repository
  itself; on today's 42-member organization, with routing profiles and link
  sets included, re-indexing micropython updates the organization layer in
  504 ms against 2,681 ms (`docs/bench/alpha-results.md`).
- **Generations.** The list of repositories (id, root, snapshot, the store
  segment and tantivy segment of each) plus the index schema is an immutable
  **generation** file; `orgs/<name>/org.json` points at the current one and
  is swapped atomically after the new generation is written. A query reads
  the pointer once and keeps that generation (and its files) for its whole
  duration; the previous generation is kept so running readers finish, older
  ones are garbage-collected with the files and store segments only they
  used.
- **Scope and visibility inside the index.** A query names the repositories
  the caller may read (local mode: all) and the ones it selects (`repo`).
  Both are applied inside tantivy: BM25 statistics are summed over the
  visible repositories only and the collector skips unselected segments
  before scoring, so results, totals and scores never depend on a repository
  the caller cannot see, and narrowing `repo` keeps organization-wide scores.

- **Repository routing.** Each repository of a generation has a
  **profile** (`orgs/<name>/profiles/<segment>.json`): its id and the
  names of the packages it publishes, manifest descriptions and the head of
  its README, the names of its 64 most referenced symbols (distinct callers,
  referrers, subclasses) and its 64 most frequent directory names and file
  stems. A query in organization mode also scores the visible repositories
  with two signals, each min-max normalized over them and mixed half and
  half: where the query's terms occur (CORI, the classic resource-selection
  score, read off the organization index: the number of a repository's
  symbols holding a term in their name, path or source is one dictionary
  lookup per term and repository segment) and BM25 over the profiles
  (name × 3, about × 1, symbols × 1, paths × 0.5). The best repository gets
  relevance 1; every symbol score of a repository is multiplied, inside
  the top-k, by `((0.25 + r) / 1.25)²` — a prior, so a strong match in
  another repository still surfaces. `list_repositories` with `query`
  returns that ranking. On the benchmark's earlier 35-instance calibration
  split this lifted organization File Acc@5 from 0.64× to 0.92× the
  single-repository value; on today's organization suite the same default
  gives 0.85× on the 120 gated issues and 0.78× on the 29 calibration ones
  (`bench/README.md`, *Org scenarios*; RFC 0006 §9.2).
- **The link layer: packages.** `rueca index --org` also builds a
  **package registry** from each repository's manifests (`package.json`
  names, Python distributions and their top-level packages, `go.mod`
  modules, Composer PSR-4 namespaces, Maven / Gradle artifacts with their
  Java packages, C / C++ `include/` directories, Cargo packages with their
  library crate name) and records the imports
  each repository's own pack could not resolve inside it. A repository
  that imports (or, by its manifests, requires) a package another one
  publishes depends on it; the consumer and its dependencies are then
  resolved **together** by the normal resolver (`rueca-index` `link`): the
  dependencies' files are mounted under a private prefix, each keeps its
  own module names and resolves its own imports with its repository's
  configuration, and an import the consumer could not resolve is resolved
  by the **publishing repository's pack against that repository's
  project** (its workspaces and `exports`, source roots, `go.mod`, PSR-4
  map). The strategy ladder does the rest, so `import { renderButton } from
  "@acme/ui"`, `acmelib.charge(…)`, `kit.Retry(3)`, `Money::of(3)` and
  `Strings.shout("x")` become `IMPORTS` and `CALLS` / `INHERITS` /
  `REFERENCES` / `DECORATES` edges into the other repository, with the
  strategy, confidence and evidence they would have inside one. Kept are
  edges between a consumer node and a dependency node, never through the
  name-matching rungs (`unique_name`, `heuristic`), never into a
  dependency's tests or into a name the consumer defines itself.
- **The link layer: contracts** (RFC 0006 phase 4). The analysis also
  records the distinct keys of each repository's **contract table** (the
  routes, OpenAPI operations and `.proto` RPC methods it provides, the HTTP
  and RPC calls it makes; §4.4, *Contracts*). A repository depends on
  another when one of its consumed keys matches a key the other provides,
  or its server code provides a key matching a spec the other publishes;
  such a dependency is not mounted for the joint resolve (no packages are
  involved): its edges come from the contract tables alone, with the same
  matcher as inside one snapshot (`rueca_model::contract::best_matches`).
  The calling definition gets `CALLS` to the handler and to the spec
  operation, a server handler `IMPLEMENTS` to the spec operation of another
  repository, all `strategy: contract` with the call site as evidence, in
  the **same link set** as the package edges — so versioning, recompute on
  change, refs, the remap of incoming edges at a ref and visibility apply
  unchanged. Ambiguity is decided over the caller's own providers and every
  dependency's: two handlers matching equally well, one in the caller's
  repository and one in another, give the cross-repository edge 0.5; a
  better match in its own repository leaves none. Calls matched by no
  visible provider are **dangling**: `list_contracts` computes them per
  query, nothing is stored.
- **Link sets are versioned with the generation.** A consumer's edges are
  stored as one file (`orgs/<name>/links/<segment>-<hash>.bin`), keyed by
  its segment and its dependencies' segments; the generation records the
  dependencies each set was computed against. An update recomputes exactly
  the consumers whose own segment, one of whose dependencies, or whose set
  of dependencies changed — re-indexing a library re-links its reverse
  dependencies, nothing else.
- **Refs in the organization** ([RFC 0006 §9.3](rfcs/0006-org-graph.md#93-phase-3--implemented)).
  A member's ref snapshots — the `refs = [...]` of its `[[repo]]` in
  `rueca-org.toml`, and any `rueca index <member> --ref <name>` — are
  **entries of its repository** in the generation, next to the default
  entry (the member root): an export belongs to the member whose
  repository id it carries. Each entry has its own store segment, tantivy
  segment (the organization index lists the default **and** the ref
  segments), profile (`profiles/<segment>.json`) and link set
  (`links/<segment>-<hash>.bin`); a generation written before refs reads
  unchanged. An update reuses every entry by (root, segment) on its own, so
  refreshing a ref copies, profiles and links that ref only. A ref's link
  set comes from its own imports and points into its dependencies'
  **default** entries, so a ref never re-links another repository; a view
  that reads a library at a ref sees the consumers' edges remapped to the
  ref's nodes by (label, qualified name, path), and loses those whose
  target the ref no longer has. `rueca index --org <file>` syncs the file's
  refs and refreshes the ones indexed earlier (a `url` member's branches
  are fetched from the remote first), `rueca index --org <file> <member>
  --ref <name> [--remove]` handles one, a plain `rueca index <path> --ref
  <name>` updates the organizations holding the repository, `rueca status`
  lists `main@… + feature/x@…` per repository and `rueca links --org <org>
  --repo <id> --ref <name>` prints one ref's link set. At query time `ref`
  picks the entries a call reads — `repo: A, ref: pr/7` is the organization
  with A at `pr/7` and every other repository at its default — and search
  statistics, routing and `trace`'s link edges are computed over that view.
- **Vectors in the organization** (RFC 0007 §10, phase 3). Vectors travel
  with each repository segment; nothing is copied into the organization
  layer. `rueca index --org <file> --embed[=PRESET]` (or a configured
  preset: `RUECA_EMBED_PRESET`, the file's `[retrieval] preset`, the
  store's) embeds every member and ref with one preset through the store
  vector cache, so a member embedded before embeds nothing again and a ref
  embeds only the text it changed. The generation records the
  organization's **model** (preset, embed key, model key, width) and its
  `allow_remote`. At query time the members whose vectors carry that model
  key are fused; the others rank lexically and are listed in
  `retrieval.lexical` with their reason in `retrieval.lexical_reasons` (no
  vectors, or vectors of another model). A generation without a recorded
  model (built lexically, or before phase 3) falls back to the embed key
  most selected snapshots share. `rueca index --org`, `rueca status` and
  `rueca links --org` print each entry's coverage (`fused`, `other model`,
  `no vectors`).
- **Centroid routing.** A routing profile (format 2) also holds the entry's
  **embedding centroid**: the mean of its dequantized symbol vectors,
  re-normalized, at the segment's width. When a hybrid query has a vector,
  the cosine between it and each centroid of the same embed key is a third
  routing signal, min-max normalized over the visible repositories; a
  repository with a centroid mixes the three signals as `(2(1 − mu)·terms
  + 2mu·doc + w·centroid) / (2 + w)` (`w` = `RUECA_ORG_ROUTE_CENTROID`, or
  `[retrieval] route_centroid`, or `centroid=` in `RUECA_ORG_ROUTE`;
  default 1, equal weights at `mu` = 0.5), one without keeps the
  two-signal mix, and the prior's shape is unchanged. Fewer than two
  centroids give no signal. The weight is the bench's to sweep.
- **Remote presets are opt-in per organization.** A `remote:` preset
  (§10, *Remote presets*) embeds or reranks an organization's code only
  when its file sets `[retrieval] allow_remote = true` (default `false`):
  `rueca index --org` refuses it otherwise, a plain `rueca index` of a
  member root with a remote preset is refused while an organization that
  does not allow them holds that root, and the query engine never sends a
  query or reranker candidates of such an organization to a provider (it
  ranks lexically with a warning, or errors when the call asked for the
  mode). A repository outside any organization opts in by naming the
  preset.
- **Dense scan at organization scale.** The exhaustive int8 scan stays:
  over 21 segments × 50,000 vectors × 1,024 dimensions (1.05M vectors) it
  takes 74 ms on one thread (70 ns per row) and 8.3 ms with the views of
  a call scanned in parallel, which `search_symbols` now does (at most one
  thread per core); no ANN index is built (`docs/decisions.md`, phase 3).
- **Across repositories at query time.** `trace` follows link edges in
  both directions: callers of a library function include its callers in
  other repositories, each node carrying its snapshot (`s`). A link edge is
  returned only when both repositories are visible to the caller.
  `list_repositories` names each repository's packages and the
  repositories it depends on; `list_contracts` lists the contract tables
  with each row's counterparts in the visible repositories (and the
  dangling calls); `rueca links --org <org>` lists every edge, contract
  edges marked `via: contract`.

The repository graph stays the unit of work: `get_symbol`, `get_snippet`
and outlines read the repository segment of the id they are given; only
`trace`, `list_contracts` (counterparts) and routing look across
repositories.

## 5. The graph schema

**Node labels** produced by the alpha:

| Label | Produced from |
|---|---|
| `Repository`, `Folder`, `File` | the walk (every listed file, any language) |
| `Module` | TS `namespace` / `declare module`; Rust inline `mod m { … }` |
| `Class`, `Interface`, `Trait`, `Struct`, `Enum`, `TypeAlias` | class / interface / PHP trait / Go struct / C and C++ `struct` and `union` / enum / type declarations (`typedef`, `using`); Rust `struct` / `union` (`Struct`), `enum`, `trait` (`Interface`), `type` |
| `Function`, `Method` | functions, methods, arrow functions assigned to names, getters/setters; Go methods (`Server.Start`, attached to their type through the package) and interface methods; C/C++ prototypes and out-of-line bodies, function-like macros; Rust `impl` and trait items (`Type.method`, `Trait.method`), `macro_rules!` |
| `Package` | Go external test packages (`package x_test`), marking the module |
| `Field`, `Variable`, `Constant` | class fields, `self.x =` in `__init__`, PHP properties (`$email`, also promoted constructor parameters), module-level assignments (`UPPER_CASE` → `Constant`), PHP `const` / `define()` / enum cases |
| `Test` | JS/TS `describe` / `it` / `test` blocks, Pest tests, GoogleTest `TEST*` and Catch2 `TEST_CASE` blocks (Python and PHPUnit tests are `Function` / `Method` / `Class` with `is_test: true`) |
| `Section` | Markdown headings, nested by `DEFINES` |
| `EnvVar` | canonical env vars `$env:NAME` (file-less) and env var providers (`.env` lines, compose / Kubernetes / GitHub Actions `env` entries, Dockerfile `ENV`) |
| `Key`, `Document` | structured-data keys at any depth (key path = local name), Laravel `config/*.php` keys, and documents of multi-document YAML streams |
| `Resource`, `Image` | deployable / infrastructure objects (Kubernetes objects, compose services, Terraform resources, data sources and modules) and container images (Dockerfile stages, compose `build`) |
| `Route`, `Variable` (data) | OpenAPI operations (`GET /users/{id}`); Terraform `var.x` / `local.x` and Dockerfile `ARG` |
| `Interface`, `RpcMethod`, `Struct`, `Field`, `Enum`, `Constant` (proto) | `.proto` services (`helloworld.Greeter`), their `rpc`s (`helloworld.Greeter.SayHello`), messages and fields, enums and values |

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
`OVERRIDES` come from Java and C++ overrides, Rust trait impls and Go's
implicit interfaces (heuristic). Go embedding (struct fields and
interfaces) and type aliases are `INHERITS`; Go `IMPLEMENTS` edges are
inferred (heuristic 0.5).
Python, ECMAScript, PHP and Java reads of ordinary names stay `REFERENCES`.

**Edge metadata**: `confidence` (0–1), `strategy`, and `evidence` (file +
span of the reference that produced it; absent for structural edges).
Strategies: `syntactic`, `scip`, `type_resolved`, `import_resolved`,
`same_module`, `unique_name`, `heuristic`, and `contract` (the code ↔
config bridge, and the HTTP / RPC contract edges `CALLS` and `IMPLEMENTS`
of §4.4 and §7).

## 6. How each tool uses the graph

All tools first pick the snapshots to read — one segment per indexed
root: the `repo` argument (one repository, a list, or `"*"`), else the
repository of an `id` argument, else (organization mode) every repository of
the organization's current generation the caller may see, the server's
`--root`, or the only indexed root — keep those the `ref` argument names (local mode: the working tree,
named by its branch), and, if they take a symbol, run the same **symbol
lookup**:

1. `id` → parse `repo@commit:path#local.name`, pick the snapshot of that
   repository whose commit (full, ≥ 7-digit prefix, 12-digit form with the
   overlay `+`) or ref the id names, and find that definition in that file;
   a commit that is no longer indexed resolves against the current snapshot,
   so ids survive re-indexing;
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
are returned as `candidates`. A name looked up in several snapshots keeps
the best match over all of them; the best match of each other snapshot is a
candidate.

| Tool | Reads | Graph traversal |
|---|---|---|
| `search_symbols` | tantivy symbol index → node table | none |
| `search_code` | trigram index → content packs → file → symbols | none (positional) |
| `get_symbol` | lookup → node table → adjacency | one hop: `DEFINES`, `INHERITS`/`IMPLEMENTS`, `CALLS` counts |
| `get_snippet` | lookup → node span → content packs | none |
| `trace` | lookup → adjacency + edge metadata | breadth-first, N hops |
| `get_file_outline` | file → symbols → `DEFINES` | containment tree |
| `list_contracts` | contract tables of the selected and visible snapshots | none (keys matched by bucket) |

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
  `*.spec.ts`, …), so code an agent navigates to comes first. Three more
  factors of 0.5 keep copies of code from crowding the code itself:
  C/C++ **declarations without a body** (a prototype in `config.h` ranks
  below `parse_config`'s body in `config.c`; the segment marks them at index
  time), definitions in an **amalgamated copy** (a single header such as
  `single_include/nlohmann/json.hpp`, recognised because most of its
  definitions reappear, same name, signature and length, spread over many
  other files), and **vendored** directories (`third_party`, `deps`, …).
  Demoted symbols are still listed — an exact name match still finds a
  prototype — they just come after the code they declare or copy.

Filters (`labels`, `language`) are exact terms; `path_glob` filters after
ranking. Over several repositories there is **one ranking**: in an
organization the organization symbol index scores every repository with
organization-wide statistics (§4.6); locally indexed roots without an
organization are scored with BM25 statistics summed over the selected
segments, which gives the same scores. (RFC 0005 merged per-repository
rankings by reciprocal rank fusion; every repository's first hit then tied,
and the right repository's answer was lost among 21 — RFC 0006 §2.) Default labels exclude `File`, `Folder`, `Module` and `Repository`, so
document sections are found alongside code (`install linux` →
`README.md:Install > Linux`); they also exclude structured-data `Key` and
`Document` nodes unless `language` is a data language, because a repository
has far more config keys than functions. Ties break by node id so pages are
stable.

**Hybrid and semantic modes** (RFC 0007, §10). When the repository was
indexed with `rueca index --embed`, the default `mode` is `hybrid`: the
BM25 top 100 above and the 100 symbols whose vectors are closest to the
query's (an exact scan under the same filters) are fused by a convex
combination of their normalized scores, times the same priors and routing
boost. A question in plain words (`where is the configuration file
parsed`) then finds `load_settings` though it shares none of them.
`semantic` ranks by the embedding alone, `lexical` is the ranking above,
unchanged.

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

### `list_contracts` — "which endpoints exist, who calls them, what calls nothing?"

Reads the contract table of every selected snapshot (one row per route,
spec operation, RPC method or call, on its definition's node) and, to count
counterparts, the tables of every visible repository at the entry the call
reads. Rows are bucketed by kind and shape (HTTP: segment count; RPC: method
name) and matched with the key matcher of §4.4, so `matches` is the number
of providers a call reaches (code and spec) or of calls a route receives;
`side: "dangling"` keeps the calls with none. A flat list sorted by key,
paged by entry offset; nothing walks the graph. From an entry, `trace` (by
its `id`) follows the contract edges themselves.

### Responses: the envelope, grouping and the budget

The tools compute their results as before (the `format: "verbose"` alpha
shapes), and a last step (`rueca_query::compact`, RFC 0005) shapes them for
an agent:

- **Envelope.** `snapshots` lists the snapshots the answer touched (`repo`,
  `ref`, full `commit`, `overlay` when uncommitted changes were indexed, a
  host `url` template) — over an organization, only the repositories whose
  items are in the answer; items refer to a snapshot by index `s` but keep a
  fully pinned id `repo@commit12[+]:path#name`, valid on its own.
- **Structure instead of repetition.** Search results and matches are
  grouped by snapshot and file, so the path is written once per file; field
  names are short (`qn`, `sig`, `lines`); scores are left out unless asked
  for. `trace` lists each node once and edges as
  `[src, dst, kind, confidence, strategy, line]` over node indexes instead
  of two full ids per edge.
- **Text.** MCP text content (and `rueca query --text`) is a rendering of
  that JSON with one line per item and the local name of each symbol, which
  together with the header and the group path spells the id.
- **Several refs, once.** With a `ref` list, a symbol whose file blob is the
  same in two refs is one item `in` both (facts are per blob, so it is
  identical); versions that differ, or exist in one ref only, are marked
  `changed` — the unchanged code once, the difference highlighted. `trace`
  returns the union of the walks from every version of the root.
- **Budget.** `budget_tokens` (default 2,000) bounds the text: the builder
  binary-searches the longest prefix of items whose rendering fits
  (`ceil(bytes / 4)` tokens), marks the response `truncated` and returns a
  `next_cursor` (an item offset, or the next line of a snippet).
  Smaller defaults (`search_symbols` 10, `search_code` 20, `trace` depth 1
  and 50 nodes) keep the first answer short; cursors fetch the rest.

Measured on `psf/requests`, `gin-gonic/gin` and `sindresorhus/ky` (text
content vs the alpha JSON, same arguments): `search_symbols` −64 to −70% at
the same limit (−83 to −85% with the new default limit), `trace` depth 1
−79 to −81%, `get_file_outline` −70 to −73%, `get_symbol` −20% (a long
docstring dominates) to −83% (a class with many children).

### Putting them together

The recommended agent loop, and why it is cheap:

1. `search_symbols` (with a data `language` for config keys; `search_code`
   for literals and error messages) to find candidates. Milliseconds, a few hundred tokens.
2. `get_symbol` to confirm the right one and see its neighbourhood size.
3. `get_snippet` to read only that code.
4. `trace` to follow callers / callees, filtering by `min_confidence`.

Each step returns ids that the next step accepts, so the agent navigates the
graph instead of reading whole files.

### Serving the tools over MCP

`rueca mcp` serves these tools through Rueca's own MCP layer in
`rueca-mcp`, written with the standard library only (no SDK, no async
runtime): JSON-RPC framing on stdio, the `initialize` lifecycle for
protocol versions 2024-11-05 to 2025-11-25, and the stateless 2026-07-28
lifecycle (`server/discover`, per-request `_meta`). Each `tools/call` makes
the same engine call as `rueca query`, on its own thread. The behaviour, the
versions and the transport seam for HTTP are specified in
[`docs/mcp-protocol.md`](mcp-protocol.md).

## 7. Confidence: how to read an edge

| Confidence | Strategies | Meaning |
|---|---|---|
| 1.0 | `syntactic`, `import_resolved` (IMPORTS) | Structure or a resolved import. Certain. |
| 0.90–0.95 | `contract` | An env var or config key read resolved through a known namespace (the code ↔ config bridge). |
| 0.5–0.9 | `contract` | An HTTP / RPC call or handler matched to a route or spec operation by canonical key: 0.9 same key, 0.85 template, 0.7 method (or RPC service) wildcard, 0.5 ambiguous (§4.4, *Contracts*). |
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
- C and C++ without a preprocessor: definitions generated by macros are
  invisible (only the calls in a macro's body are passed on to its users),
  `#if` groups the grammar can take keep all their branches, and files the
  grammars still cannot fully parse lose the definitions and references
  inside the broken region (about 3% of the C and 0.5% of the C++ files of
  the benchmark corpus, 3.4% of the C++ files with vendored GoogleTest,
  amalgamations and Objective-C headers included). Conditions are not
  evaluated with the project's `-D` flags. Overloads are chosen by
  argument count and by the types of literal, cast and local-variable
  arguments; overloads that differ only by template constraints are all
  linked; ADL, calls through function pointers and virtual dispatch are
  not followed.

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
  during a re-index keep working. Adding vectors (§10) is the same move: a
  copy of the lexical segment plus the `vectors.*` components, under a new
  id, swapped in. An organization does the same with
  generations: one repository's part is replaced and a pointer swapped.

## 9. Where the code lives

| Step | Crate | Start reading at |
|---|---|---|
| Data model (labels, edge kinds, facts, ids) | `rueca-model` | `src/lib.rs`; `src/contract.rs` (contract keys and matching) |
| Language-pack contract, budgeted parsing | `rueca-extract` | `LanguagePack` (crate docs list the RFC 0003 hooks) |
| Python extraction, module names, imports | `rueca-lang-python` | crate docs, `queries/locals.scm` |
| TS/TSX/JS extraction, `oxc_resolver` | `rueca-lang-typescript` | crate docs, `src/resolve.rs` |
| Markdown sections and links (`pulldown-cmark`) | `rueca-lang-markdown` | crate docs, `src/links.rs` |
| Structured data, file kinds, data pointers | `rueca-lang-data` | crate docs (mapping tables), `src/emit.rs`, `src/kinds/` |
| PHP extraction, name resolution, Composer autoload | `rueca-lang-php` | crate docs, `src/names.rs`, `src/composer.rs` |
| Go extraction, receivers, `go.mod` module model | `rueca-lang-go` | crate docs, `src/locals.rs`, `src/modules.rs` |
| Java extraction, packages, Maven / Gradle source roots | `rueca-lang-java` | crate docs, `src/extract.rs`, `src/project.rs`, `queries/locals.scm` |
| C/C++ extraction, preprocessing (masking, conditionals, repairs), include directories | `rueca-lang-cpp` | crate docs, `src/extract.rs`, `src/preprocess.rs`, `src/mask.rs`, `src/cond.rs`, `src/repair.rs`, `src/includes.rs` |
| Rust extraction, receivers, macro arguments, Cargo module model | `rueca-lang-rust` | crate docs, `src/extract.rs`, `src/receivers.rs`, `src/macros.rs`, `src/modules.rs` |
| `.proto` services, RPC methods, messages (`protox-parse`) | `rueca-lang-proto` | crate docs, `src/extract.rs` |
| Walk, cache, snapshot identity | `rueca-index` | `index_worktree` |
| Resolution | `rueca-resolve` | crate docs (strategy table); `src/hooks.rs` (RFC 0003 hooks), `src/bridge.rs` (env vars, config keys, data pointers), `src/go.rs` (implicit interfaces), `src/rust.rs` (Rust modules, `impl` blocks, trait methods), `src/contracts.rs` (contract table and edges) |
| Segment, search, packs, catalog | `rueca-segment` | crate docs (format spec) |
| Organization index, generations, visibility | `rueca-segment` | `src/org.rs` (module docs) |
| Organization file, `index --org` | `rueca-index` | `src/org.rs` |
| Tools | `rueca-query` | `src/tools.rs` |
| MCP protocol layer, stdio / CLI | `rueca-mcp`, `rueca` | [`docs/mcp-protocol.md`](mcp-protocol.md), `RuecaServer::handle` (`src/protocol.rs`), `serve_stdio`, `src/main.rs` |
| Embedding presets, weights cache, llama.cpp runtime | `rueca-embed` | crate docs, `src/presets.rs`, `src/backend/llama.rs` |

## 10. Embeddings (RFC 0007)

Semantic search ([RFC 0007](rfcs/0007-semantic-search.md)) is being built
in phases. Phase 0 added the runtime and a batch command; phase 1 adds
symbol vectors to the index (`rueca index --embed`) and the `hybrid` and
`semantic` modes of `search_symbols` (*Indexing with vectors* and *Hybrid
search* below). Phase 2 adds graph expansion, a centrality prior and a
cross-encoder reranker after that first stage, all off by default until
the bench's sweep decides (*Graph signals* and *Reranker* below). Phase 3
adds the organization scope (one model per organization, centroid
routing: §4.6), remote presets, the service profile and `rueca query
--batch` (*Remote presets*, *Configuration* and *Commands* below).

**Crate.** `rueca-embed` owns presets, weights and runtimes behind two
traits: `Embedder` (`embed` of a batch of documents and queries into
L2-normalized vectors, `model_key`, `dims`, `meta`) and `Reranker`
(`rerank(query, docs)`: `NoReranker`, and the llama.cpp Qwen3-Reranker of
*Reranker* below). The runtime is llama.cpp through
`llama-cpp-2` (pinned exactly), behind the `embeddings` feature, on by
default in `rueca-embed` and `rueca`: it needs CMake and a C/C++ compiler,
uses Metal on Apple silicon, and `--no-default-features` builds a
lexical-only binary. `cuda` is an opt-in feature for service builds.

**Presets.** A manifest compiled into the binary (`rueca models list`):

| Preset | File | Status |
|---|---|---|
| `qwen3-0.6b-q8` | Qwen3-Embedding-0.6B, official `Q8_0` GGUF | default (provisional) |
| `qwen3-0.6b-f16` | same, official `f16` | reference for rank overlap |
| `qwen3-0.6b-q4` | `Q4_K_M` converted by Rueca from the official `f16` | unavailable until published; a local file with the pinned sha256 loads |
| `qwen3-4b-q4`, `qwen3-4b-q8`, `qwen3-8b-q4`, `qwen3-8b-q8` | official `Q4_K_M` / `Q8_0` | available (GPU) |
| `coderank-137m`, `coderank-137m-q8` | CodeRankEmbed (NomicBERT encoder, CLS pooling, query prefix), `f16` / `Q8_0` converted by Rueca | unavailable until published; `rueca models import` registers a local conversion (*Encoder presets* below) |
| `gte-modernbert-149m` | gte-modernbert-base (ModernBERT encoder, CLS pooling, no prefix), `f16` converted by Rueca | same |
| `remote:voyage/…`, `remote:openai/…`, `remote:gemini/…` | provider APIs (*Remote presets* below) | remote, opt-in |
| `remote:voyage/rerank-2.5`, `remote:cohere/rerank-v3.5` (rerankers) | provider APIs | remote, opt-in |
| `remote:bedrock/…` | Amazon Bedrock | later (SigV4) |
| `qwen3-reranker-0.6b-q8` (reranker) | Qwen3-Reranker-0.6B, `Q8_0` converted by Rueca with its classifier head | unavailable until published; `rueca models import` registers a local conversion (*Reranker* below) |
| `qwen3-reranker-4b-q4`, `mxbai-rerank-large-v2` (rerankers) | to convert | planned |

Each local preset pins the file's URL at an upstream revision, its sha256
and size, the quantization, the width (with its Matryoshka range), the
context, the pooling (last token for Qwen3, the first token `[CLS]` for
the BERT-family encoders, mean available) and the query format. Qwen3
queries are `Instruct: {instruction}\nQuery:{query}` with a code-search
instruction by default (`--instruction` overrides it for the phase 0
sweep); encoder queries get a fixed prefix (CodeRankEmbed: `Represent
this query for searching relevant code: `; gte-modernbert: none), which
`--instruction` replaces verbatim; documents get no prefix.

The `qwen3-0.6b-q4` file is made with llama.cpp's own tool at the commit
`llama-cpp-2` vendors, and is byte-for-byte reproducible:

```sh
llama-quantize Qwen3-Embedding-0.6B-f16.gguf Qwen3-Embedding-0.6B-Q4_K_M.gguf Q4_K_M
# llama.cpp d81235049384 → sha256 44f67b5875def0e90ae67247b141a5a861538efb8d8b0f783eef3d94211d9dcd
```

**Encoder presets.** CodeRankEmbed (`coderank-137m`, MIT; NomicBERT,
the Arctic-embed-m-long encoder: 12 layers, rotary positions, 768 dims,
8,192 tokens) and gte-modernbert-base (`gte-modernbert-149m`,
Apache-2.0; ModernBERT: 22 layers, alternating local and global
attention, 768 dims, 8,192 tokens) run through the same llama.cpp
backend. Both read the `[CLS]` state (llama.cpp `CLS` pooling, set from
the preset, and also the pooling the converter records in the GGUF), have
no Matryoshka range (768 only), wrap every input in the tokenizer's
`[CLS]` … `[SEP]` (WordPiece, lower-cased, for CodeRankEmbed; BPE for
ModernBERT; both embedded in the GGUF by the converter) and cut the body
so the budget holds both. Attention is bidirectional, so llama.cpp runs
them without a KV cache, every sequence of a batch in one micro-batch, as
for Qwen3. The converter records a 2,048-token training context for
NomicBERT (its rule for nomic-embed v1 / v1.5, whose config has the same
`n_positions` 8192 / `max_trained_positions` 2048); CodeRankEmbed's card
and `sentence-transformers` use 8,192 and llama.cpp runs a longer context
with a warning: a 5,481-token input matched the reference at cosine
0.999999.

The files are Rueca conversions, `unavailable` until their release
assets are published; convert them locally and register them (the same
converter checkout as the reranker's, *Reranker* below):

```sh
# nomic-ai/CodeRankEmbed at revision 3c4b60807d71f79b43f3c4363786d9493691f8b1
mkdir -p CodeRankEmbed/1_Pooling && cd CodeRankEmbed
for f in README.md config.json config_sentence_transformers.json modules.json sentence_bert_config.json \
    special_tokens_map.json tokenizer.json tokenizer_config.json vocab.txt 1_Pooling/config.json model.safetensors; do
  curl -sSfL -o $f https://huggingface.co/nomic-ai/CodeRankEmbed/resolve/3c4b60807d71f79b43f3c4363786d9493691f8b1/$f
done
cd ..
uv run --no-project --with torch --with transformers --with numpy --with sentencepiece --with safetensors \
  python llama.cpp/convert_hf_to_gguf.py CodeRankEmbed --outtype f16 --outfile CodeRankEmbed-F16.gguf
# 274,181,632 bytes, sha256 8017b5bca09c111b5264cd686274b361aa54213a10cdf307eced3a3a0940444c
# --outtype q8_0 --outfile CodeRankEmbed-Q8_0.gguf:
# 146,033,152 bytes, sha256 829cc983d0c54aec5705ee6a3cdf12f6f87db145300d88ec2145bc235d94be9a
rueca models import coderank-137m CodeRankEmbed-F16.gguf
rueca models import coderank-137m-q8 CodeRankEmbed-Q8_0.gguf

# Alibaba-NLP/gte-modernbert-base at revision e7f32e3c00f91d699e8c43b53106206bcc72bb22
# (README.md config.json config_sentence_transformers.json modules.json special_tokens_map.json
#  tokenizer.json tokenizer_config.json 1_Pooling/config.json model.safetensors), then
uv run --no-project --with torch --with transformers --with numpy --with sentencepiece --with safetensors \
  python llama.cpp/convert_hf_to_gguf.py gte-modernbert-base --outtype f16 --outfile gte-modernbert-base-F16.gguf
# 299,877,088 bytes, sha256 a17fb60a5a2d8b6f171f846c834876fdaac3626e78a0b199c3e335adfff9eafe
rueca models import gte-modernbert-149m gte-modernbert-base-F16.gguf
```

Every conversion is reproducible (two runs, same file). The upstream
gte-modernbert weights are `float16`, so its `f16` file loses nothing.
Validated against `sentence-transformers` on 2 queries and 3 code
documents (CodeRankEmbed: float32, `trust_remote_code`, transformers
4.45.1 as its config records, the model card's `query` prompt;
gte-modernbert: its `float16` weights): cosine to the reference

| Preset | Metal | `--cpu` | ranking of the 3 documents per query |
|---|---|---|---|
| `coderank-137m` (`f16`) | ≥ 0.99999 | ≥ 0.99999 | equal |
| `coderank-137m-q8` | ≥ 0.9984 | ≥ 0.9976 | equal |
| `gte-modernbert-149m` (`f16`) | ≥ 0.99999 | ≥ 0.99999 | equal |

Throughput (`rueca embed --preset P --bench --runs 3`, 2,000 inputs of
about 210 tokens, batch 512, median of 3; M-series GPU and 10 performance
cores, both shared with a running benchmark, so read them as lower
bounds): `coderank-137m` 7,011 tokens/s on Metal (runs 72.0, 49.9,
59.9 s), 4,562 on `--cpu` (runs 80.8, 92.0, 102.5 s); `gte-modernbert-149m`
(22 layers) 4,223 on Metal and 4,160 on `--cpu`.

**Weights cache.** Never in the binary:

```text
<models>/<preset>/<file>          the GGUF file
<models>/<preset>/<file>.sha256   its verified sha256
```

`<models>` is `$RUECA_MODEL_DIR` when set, else `<store>/models`
(`~/.cache/rueca/models` by default). `rueca models pull [PRESET]`
downloads to a temporary file, checks size and sha256, renames it into
place and writes the sidecar; a hand-placed file is hashed once on first
use. A missing file is an error naming the preset, the URL,
`rueca models pull` and `RUECA_MODEL_DIR`.

**Embedding.** Inputs are tokenized with the tokenizer inside the GGUF
file, cut to the token budget (512 by default, special tokens included;
Qwen3 ends every input with `<|endoftext|>`, which last-token pooling
reads; encoders begin with `[CLS]`, which CLS pooling reads, and end with
`[SEP]`), packed into batches of 512 tokens by default (`--batch`; every
input fits one micro-batch: 512 tokens keep the default model near 1.0 GB
of memory where 2,048 took 2.2 GB, for about 4% less throughput, measured
in phase 0), pooled, L2-normalized and, with `--dims`, truncated and
re-normalized. The batch order is fixed by content: inputs are sorted by
token length, then BLAKE3 of the text, and results are returned in input
order. Threads default to the performance cores (on Apple silicon the
efficiency cores slow a CPU run down by half).

**Identity and audit** (RFC 0007 §5.4, §9). The *embed key* is BLAKE3 of
the model file's sha256, the pooling, the document prefix, the text
builder version and the token budget; the *model key* adds the output
width and the vector encoding. The query instruction is in neither key
(documents carry none), nor are the runtime, its version, the backend or
the CPU level: those go to the audit record that `rueca embed --info`
prints (`model_sha256`, `quantization`, `dims`, `pooling`, `runtime`,
`runtime_version`, `backend` = `metal` | `cpu` | `cuda` | `remote`, `cpu_level` =
`neon` or the x86-64 psABI level, batch and micro-batch sizes, threads).

**Commands.**

```sh
rueca index . --embed                 # index with symbol vectors (default preset)
rueca index . --embed=remote:voyage/voyage-code-3   # through a provider (VOYAGE_API_KEY)
rueca index --org rueca-org.toml --embed            # every member and ref with the organization's model
rueca index . --ref feature/x --embed               # a ref: only the text it changed is embedded
rueca query --batch requests.jsonl    # {"tool","args"} per line → one answer per line, one process
rueca models list                     # presets, status, what the cache holds
rueca models pull qwen3-0.6b-q8       # download and verify
rueca models path qwen3-0.6b-q8       # where the file lives
rueca models rm qwen3-0.6b-f16
rueca models import qwen3-reranker-0.6b-q8 Qwen3-Reranker-0.6B-Q8_0.gguf   # register a local conversion (size and sha256 checked)
rueca rerank --query "where is login handled?" --input docs.jsonl         # {"id","text"} → {"id","score"}, input order
rueca embed --info                    # the audit record of the default preset (no model load; load_ms: null)
rueca embed --text "where is login handled?" --query
rueca embed --input in.jsonl --output out.jsonl          # {"id","text","query"} → {"id","vector"}
rueca embed --input in.jsonl --output out.f32 --format f32le --dims 256   # raw rows + out.f32.ids
rueca embed --bench --n 2000 --tokens 200 --runs 3 [--cpu]               # tokens/s and texts/s as JSON
```

`--preset` (or `RUECA_EMBED_PRESET`), `--max-tokens`, `--batch`,
`--threads` and `--cpu` apply to every form. Output keeps input order.
Errors print `rueca: …` on stderr and exit with status 1.

**Indexing with vectors.** `rueca index --embed[=PRESET]` (or a configured
preset, below) runs a vectors pass after resolution:

1. **Units.** Every `Function`, `Method`, `Test`, `Class`, `Struct`,
   `Interface`, `Trait`, `Enum` and Markdown `Section` with a file gets one
   vector; fields, variables, constants, type aliases, configuration keys,
   contracts and containers do not (RFC 0007 §5.1).
2. **Canonical text** (text builder version 1, the bench vector arm's
   format):

   ```text
   <language> <label> <qualified name>
   path: <repository-relative path>
   <signature>
   <doc comment>
   <body head>
   ```

   The signature is the declaration header scanned from the source (to the
   `:` closing a Python header, else to the first `{` outside brackets,
   lines joined by one space); the doc is the extracted docstring, else the
   comment lines right above the definition; the body is the rest of the
   span, dedented, without blank lines or the closing `}`. Types list their
   members' signatures instead of their body, so a method edit does not
   re-embed its class; sections are their text. Empty parts are left out
   and the text is cut to `max_tokens × 4` characters (512 tokens → 2,048
   characters; the model then cuts at 512 real tokens — a tokenizer-based
   cut in the builder is a follow-up).
3. **Cache lookup.** Each text's *vector key* is BLAKE3(embed key ‖ text);
   the store vector cache answers which are already embedded.
4. **Publication.** If every key hits (a re-index after a small change, a
   second worktree, another repository with the same files), the segment is
   written once with its vectors and the model is never loaded. Otherwise
   the lexical segment is published and registered first (queries are
   lexical meanwhile), the misses are embedded in batches of 256 texts in
   key order — each batch appended to the cache as it finishes, so an
   interrupted run resumes — and the snapshot is published again: the
   bundle's components copied byte for byte, `meta` with a new segment id,
   plus the `vectors.*` components; the catalog entry is swapped to it and
   the lexical file deleted. Copying beats appending to the file: a
   published segment is never modified (readers map it), and the copy of a
   20 MB bundle costs tens of milliseconds next to minutes of embedding.

`IndexReport.vectors` (and the `vectors:` line of `rueca index`) gives the
units, how many were cached and embedded, tokens, time and the number of
publications; progress (`embedded n of m`, tokens/s) is logged at `info`.
`--embed` is an explicit request: missing weights are an error naming
`rueca models pull`, raised before anything is published. A preset that
comes from configuration only warns and indexes lexically.

**Segment vectors.** `vectors.meta` (JSON: the audit record above plus
`dims`, `rows`, `encoding: "i8"`, the segment's model key and the embedded
labels), `vectors.nodes` (`[u32]`, ascending), `vectors.keys` (`[[u8; 32]]`),
`vectors.scale` (`[f32]`) and `vectors.data` (`[i8; rows × dims]`). The
cache keeps the model's full width; the segment stores `dims` components
(`[retrieval] dims`, default the full width): truncated, re-normalized
(Matryoshka) and quantized with one scale per row (`max |x| / 127`), so a
new width re-quantizes from the cache without embedding anything.

**Store vector cache.** `<store>/vectors/<embed key>/` (or
`$RUECA_VECTOR_DIR`, shared across stores): `meta.json` (width, `f16`) and
256 shards by the first byte of the key, each a pair of append-only files,
`<xx>.keys` (32-byte keys) and `<xx>.f16` (rows). Writers lock the shard's
`.keys` file (`flock`), skip keys already present, append and fsync the
rows, then the keys; readers take no lock and count the complete entries
(`min(keys, rows)`), so indexers of several repositories can share one
store. A shard whose lengths disagree (a crash between the two appends) is
truncated to its complete entries by the next writer. Entries are never
removed yet (like content packs).

**Hybrid search.** `search_symbols` gains `mode`:

| Snapshot has vectors | Model loads | `mode` | Ranking |
|---|---|---|---|
| yes | yes | absent or `hybrid` | hybrid (`retrieval.mode: "hybrid"`) |
| yes | yes | `semantic` | dense only |
| yes | no (weights missing, model differs) | absent | lexical, with `retrieval.warnings` |
| yes | no | `hybrid` / `semantic` | error naming `rueca models pull` |
| no | — | absent | lexical |
| no | — | `hybrid` / `semantic` | error naming `rueca index --embed` |
| any | — | `lexical` | lexical, unchanged |

The query model is the one recorded in the segment's `vectors.meta`
(preset, token budget, text builder version; its embed key is checked), not
the configured preset; it is loaded once per process (about 0.2 s and
1.0 GB resident for the default model: a `rueca query` process pays it on
every call, the MCP server once). The query is embedded with the preset's
instruction, truncated to each segment's width. Candidates are the BM25 top
N (today's ranking; its raw score is recovered by dividing out the prior and
routing boost) and the dense top N of an exhaustive int8 scan of each
selected snapshot under the same label, language, `path_glob` and scope
filters (N = 100, at least the requested page; about 1.4 ms for this
repository's 9,868 vectors). Every candidate with a vector gets its cosine,
rescored from the cache's f16 rows when the cache is there. Then

```text
s = (α · b̂ + (1 − α) · d̂) · prior · route
```

with `b̂`, `d̂` min-max normalized over each list's top N (a list of equal
scores normalizes to 1; BM25 is 0 outside its top N, a cosine below the
dense list's range 0) and α = 0.5 until the phase 1 tuning. A snapshot
without vectors for the model (an organization member indexed without
`--embed`) is scored `b̂ · prior · route` and listed in `retrieval.lexical`;
`semantic` leaves it out with a warning. Ties break by (snapshot, node),
so pages are exact. Knobs: `RUECA_FUSION_ALPHA`, `RUECA_FUSION_N`,
`RUECA_FUSION_NORM` (`minmax`, or `theoretical`: RFC 0007 §7.2's division
by the theoretical minimum), over `[retrieval]` in the store's
`rueca.toml`.

**Graph signals** (RFC 0007 §7.3, §7.4). When either is on,
`search_symbols` takes a pool of at least N first-stage candidates
(lexical or fused, priors and routing applied) and, in this order:

- **Centrality prior.** Each score is multiplied by `((0.25 + p) /
  1.25)^β`, `p` the candidate's centrality percentile: the share of its
  snapshot's symbols with a smaller distinct in-degree over `CALLS` and
  `IMPORTS` (files, folders, modules and the repository get 1, so the prior
  never moves them). β = 0 (off) by default. A multiplication commutes with
  fusion, so in hybrid mode this is exactly the prior of §7.4; in lexical
  mode it re-scores the BM25 top N (the other priors are applied inside the
  index, before top-k). The percentiles come from the incoming CSR the
  first time a query asks for them and stay with the opened segment: 7.9 ms
  for 100k nodes and 400k edges with the crate optimized (32 ms
  unoptimized; `cargo test -p rueca-segment --test scale centrality --
  --ignored`), so they are not computed at open (cost 0 while β = 0) and
  not written at index time.
- **Graph expansion.** The 1-hop neighbours of the top 10 candidates (the
  seeds) join the pool. A neighbour's score is its seed's score with the
  seed's prior, routing boost and centrality taken out, times the decay
  (0.5), times the neighbour's own multipliers; a candidate already in the
  pool keeps the higher of its score and the inherited one. Edge sets:
  `calls` (callers and callees), `calls+contains` (also the RFC's
  containment, which is Rueca's `DEFINES`: a type's members and a member's
  container; `CONTAINS` links folders and files, which search does not
  return), `calls+contains+imports` (also `IMPORTS` both ways; they link
  files, so with the default labels they add nothing). In organization mode
  `CALLS` and `IMPORTS` also follow the link layer into other repositories
  (the edges `trace` follows, through the call's view and visibility) when
  the other repository is selected: a search that finds a library symbol
  brings its consumers (the impact question). Neighbours pass the call's
  `labels`, `language` and `path_glob`; at most 20 are added per seed and
  100 per query, seeds in rank order, then callers, callees, members,
  container, importers, imported. One hop only. On this repository (34,757
  nodes) a `rueca query` process takes 0.30 s with and without expansion.

`retrieval` gains `expansion` (`{"edges": "calls", "added": 3}`) and
`centrality` (β) when they are on.

**Reranker** (RFC 0007 §7.5). `search_symbols` with `rerank: true` (or
`[retrieval] rerank = true`; off by default) scores the top `rerank_k`
(default 50) of the pool above with a cross-encoder and returns the
reranker's order for them, the first-stage order below. A candidate's text
is its canonical text (the text its vector embeds, header, path,
signature, doc, body head or member signatures) cut to 256 tokens (1,024
characters), rebuilt from the content packs at query time, so a snapshot
without vectors reranks too. Scores stay monotone with the order: a
reranked result's `score` is the reranker's relevance in `[0, 1]`, a result
below K keeps its first-stage score scaled under half the lowest reranked
one. `retrieval.reranker` is `{"preset", "model_key", "k", "ms"}`. An
explicit `rerank: true` whose weights are missing is an error naming
`rueca models import` / `rueca models pull`; a reranker turned on by
configuration that cannot load leaves the first-stage order and a warning.
The model loads once per process.

The runtime is llama.cpp. Qwen3-Reranker is a causal LM asked whether a
document meets a query; its score is `softmax(logit(yes), logit(no))[yes]`
after the model card's chat prompt:

```text
<|im_start|>system
Judge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be "yes" or "no".<|im_end|>
<|im_start|>user
<Instruct>: {instruction}
<Query>: {query}
<Document>: {document}<|im_end|>
<|im_start|>assistant
<think>

</think>

```

llama.cpp's `convert_hf_to_gguf.py` recognises the reranker (class
`Qwen3Model`, `conversion/qwen.py`: a README titled `# Qwen3-Reranker` or
the name) and writes the `yes` and `no` rows of the LM head as a two-way
classifier (`cls.output.weight`, labels `["yes", "no"]`) with rank pooling;
llama.cpp then reads the last token of each sequence through it and
applies the softmax in the graph (`build_pooling`, rank pooling with
`LLM_ARCH_QWEN3`), so a pooled read is `[p(yes), p(no)]`. Rueca refuses a
file without that head. Per call, the shared prefix (system turn,
instruction, query, `<Document>:`) is decoded once as sequence 0 and copied
into each chunk's sequences in the unified KV cache; documents (`
{document}` cut to 256 tokens, then the assistant suffix) are decoded in
chunks of about 2,048 tokens, in input order. The instruction is the code
retrieval one of the embedder; documents and query are tokenized as plain
text (a special-token string in code stays text).

The preset `qwen3-reranker-0.6b-q8` is a Rueca conversion (Qwen publishes
no reranker GGUF; most community files score near 4.5e-23, llama.cpp issue
#16407, because they were converted without the head). It is
`unavailable` until its release asset is published; meanwhile convert it
locally and register it:

```sh
# llama.cpp at the commit llama-cpp-2 0.1.161 vendors (converter only)
git init llama.cpp && cd llama.cpp
git remote add origin https://github.com/ggml-org/llama.cpp.git
git sparse-checkout set --no-cone /convert_hf_to_gguf.py /conversion/ /gguf-py/
git fetch --depth 1 --filter=blob:none origin d81235049384534c167caea52b85a694f6103d14 && git checkout FETCH_HEAD
cd ..
# Qwen/Qwen3-Reranker-0.6B at revision e61197ed45024b0ed8a2d74b80b4d909f1255473 (README.md included)
mkdir Qwen3-Reranker-0.6B && cd Qwen3-Reranker-0.6B
for f in README.md config.json generation_config.json merges.txt tokenizer.json tokenizer_config.json vocab.json model.safetensors; do
  curl -sSfLO https://huggingface.co/Qwen/Qwen3-Reranker-0.6B/resolve/e61197ed45024b0ed8a2d74b80b4d909f1255473/$f
done
cd ..
uv run --no-project --with torch --with transformers --with numpy --with sentencepiece --with safetensors \
  python llama.cpp/convert_hf_to_gguf.py Qwen3-Reranker-0.6B --outtype q8_0 --outfile Qwen3-Reranker-0.6B-Q8_0.gguf
# 639,153,376 bytes, sha256 def2f55d8729597c9456a441f9cc7781bbbbc1ac5ce8592a404a7575e30dd81e (two runs, same file)
rueca models import qwen3-reranker-0.6b-q8 Qwen3-Reranker-0.6B-Q8_0.gguf
```

Validated against `transformers` (float32, the model card's recipe) on 6
queries × 5 code documents (`cargo test -p rueca-embed --test rerank`,
`RUECA_EMBED_TESTS=1`): every query's ranking is equal, Pearson r 0.9999
on log-odds and 0.9997 on probabilities, largest probability difference
0.046 (`Q8_0`). Latency, documents of about 256 tokens, M-series GPU
(Metal) while another process embedded on the same GPU: K = 20 2.6 s,
K = 50 6.0 s; `--cpu` 4.5 s and 15.6 s (`cargo test -p rueca-embed --test
rerank latency -- --ignored`); 512 tokens per decode call is 1.6× slower,
4,096 and 8,192 no faster than 2,048. Loading takes about 0.2 s when the
Metal kernels are cached (`rueca rerank --timing`); a load that compiles
them took 9.4 s here.
Other presets: `qwen3-reranker-4b-q4` (a Rueca conversion to come, split
under the 2 GiB release-asset limit) and `mxbai-rerank-large-v2` are
planned; the remote rerankers are below.

**Remote presets** (RFC 0007 §3.5; `rueca-embed` `remote`, feature
`remote`, part of `embeddings`). `remote:<provider>/<model>` names an
embedder or a reranker (a model starting with `rerank`) behind an API; any
model of the provider works, the manifest lists the compared ones.

| Provider | Embeddings | Rerank | Key | Batch per request |
|---|---|---|---|---|
| `voyage` | `POST /v1/embeddings`, `input_type` `query` / `document`, `output_dimension` | `POST /v1/rerank` | `VOYAGE_API_KEY` | 256 texts, ~100K tokens |
| `openai` | `POST /v1/embeddings`, `dimensions` | — | `OPENAI_API_KEY` | 512 texts, ~250K tokens |
| `gemini` | `POST /v1beta/models/<model>:batchEmbedContents`, `taskType` `CODE_RETRIEVAL_QUERY` / `RETRIEVAL_DOCUMENT`, `outputDimensionality` | — | `GEMINI_API_KEY` (`x-goog-api-key`) | 100 texts |
| `cohere` | — | `POST /v2/rerank` | `COHERE_API_KEY` | — |
| `bedrock` | later: AWS SigV4 signing and credential resolution need the AWS SDK or a hand-written signer | — | — | — |

Tokens are estimated at 4 characters each (no tokenizer); the width is
`dims` (`--dims`, `[retrieval] dims`), else the model's default (Voyage
1,024, OpenAI `text-embedding-3-large` 3,072, Gemini
`gemini-embedding-001` 3,072), sent only when it differs; every vector is
L2-normalized on arrival. A 429, a 5xx or a transport error is retried
up to 5 times with exponential backoff from 0.5 s (capped at 30 s), or
after the answer's `Retry-After` seconds; other errors fail at once with
the provider's message. `RUECA_VOYAGE_BASE_URL`, `RUECA_OPENAI_BASE_URL`,
`RUECA_GEMINI_BASE_URL` and `RUECA_COHERE_BASE_URL` point a provider at a
proxy or a compatible server. The *embed key* of a remote preset hashes
provider, model, API version, width, text builder version and token
budget; providers publish no weights version, so a model changed behind
its name is not detected. The vectors pass, the store vector cache and
hybrid search treat remote vectors like local ones (`backend: remote` in
the audit record). Privacy: an organization's code goes to a provider
only when its `rueca-org.toml` sets `[retrieval] allow_remote = true`
(§4.6); a repository outside any organization opts in by naming the
preset. The tests run against a local mock server; no real API is
called.

**Configuration.** `<store>/rueca.toml`:

```toml
[retrieval]
preset = "qwen3-0.6b-q8"   # rueca index embeds with it ("off": never)
dims = 1024                # width stored in segments
alpha = 0.5                # BM25 weight of the hybrid fusion
fusion_n = 100             # candidates per list
normalization = "minmax"   # or "theoretical"
expand = "off"             # calls | calls+contains | calls+contains+imports
expand_decay = 0.5         # expand_seeds = 10, expand_per_seed = 20, expand_total = 100
centrality_beta = 0.0      # centrality prior exponent (0: off)
rerank = false             # rerank when a call does not say
rerank_k = 50
rerank_preset = "qwen3-reranker-0.6b-q8"
route_centroid = 1.0       # weight of the centroid routing signal (§4.6)
```

The same `[retrieval]` table in `rueca-org.toml` configures an
organization (its `preset` and `dims` are the organization's model) and,
only there, `allow_remote = true` lets a `remote:` preset see its code
(default `false`; the store file rejects the key).

**Profiles.** Unset keys take the defaults of the profile in effect,
`RUECA_PROFILE`: `local` (the default) keeps the built-in defaults below;
`service` (RFC 0008 service mode, RFC 0007 §8.3) embeds with
`qwen3-4b-q4` when the build runs models on a GPU (`cuda`, or Metal on
Apple silicon) and stays lexical on a CPU-only build, with `rerank =
false` and `rerank_k = 50`:

```toml
# what RUECA_PROFILE=service means on a GPU build
[retrieval]
preset = "qwen3-4b-q4"
rerank = false   # turned on only once the reranker's gate (RFC 0007 §7.5, §11.2) holds on the bench
rerank_k = 50
```

Precedence: the argument (`--embed=PRESET`, `--no-embed`; `rerank`,
`rerank_k`), the environment (`RUECA_EMBED_PRESET`, `RUECA_FUSION_*`,
`RUECA_EXPAND`, `RUECA_EXPAND_DECAY`, `RUECA_EXPAND_SEEDS`,
`RUECA_EXPAND_PER_SEED`, `RUECA_EXPAND_TOTAL`, `RUECA_CENTRALITY_BETA`,
`RUECA_RERANK` (`1` / `0`), `RUECA_RERANK_K`, `RUECA_RERANK_PRESET`,
`RUECA_ORG_ROUTE_CENTROID`), the organization file (indexing), the store
file, the profile's defaults, the built-in default (no embedding; α 0.5,
N 100, min-max; no expansion, β 0, no reranking; centroid weight 1).
Without `--embed`, `rueca index` (and the MCP server's first-start index)
embeds only when a preset is configured: embedding stays opt-in. `rueca status` shows the preset that embedded each
snapshot (`VECTORS`, `-` for a lexical one).

**Fallback runtime.** RFC 0007 §4.2 names `fastembed` (candle) as the
fallback should the llama.cpp build break cargo-dist or the size budget.
Phase 0 measured `fastembed` 7.7.1 with only `qwen3-text` on the same
`Q8_0` file: it builds in pure Rust (plus Oniguruma for the tokenizer) and
its vectors match the reference (cosine ≥ 0.999), but it needs the
model's `tokenizer.json` beside the GGUF file, runs on the CPU at about
0.7× llama.cpp's speed with about twice the memory, and its Metal path
fails on Qwen3 (`no metal implementation for qk-norm-rope`). It is not in
the tree; llama.cpp builds, links statically and adds about 11 MB to the
release binary.
