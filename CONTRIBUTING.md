# Contributing to Rueca

Thanks for your interest! Bug reports, benchmark cases, language support and
documentation fixes are all welcome.

## Before you start

- **Bugs**: open an issue with the bug template. A minimal repository or
  snippet that reproduces the wrong symbol, edge or search result helps most.
- **Features and new languages**: open an issue first so we can agree on the
  shape. Larger changes (new modes, storage format, tool contract) go through
  an RFC in [`docs/rfcs/`](docs/rfcs/).
- **Security issues**: do not open a public issue; see [SECURITY.md](SECURITY.md).

Read [how Rueca works](docs/how-it-works.md), [RFC 0001](docs/rfcs/0001-architecture.md),
[RFC 0002](docs/rfcs/0002-building-blocks.md), [the decisions log](docs/decisions.md)
and [the tool contract](docs/tools.md) before a non-trivial change: they are
short and explain why things are the way they are. In particular, RFC 0002
§9 lists which layers reuse existing crates and which are built here — prefer
assembling a maintained, permissively licensed component over writing one.

## Development setup

Requirements: Rust 1.90+ (`rustup` recommended), git, and for the default
`embeddings` feature (llama.cpp, built from source by `rueca-embed`) CMake, a
C/C++ compiler and libclang (Debian / Ubuntu: `apt install cmake clang
libclang-dev`; macOS: Xcode command-line tools and `brew install cmake`).
`cargo build -p rueca --no-default-features --features service` builds a
lexical-only binary without them. The benchmark also needs
Python 3.12+, [uv](https://docs.astral.sh/uv/) and Node.js (for SCIP indexers).

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check            # optional locally; runs in CI (cargo install cargo-deny)
```

Some tests are `#[ignore]` because they clone real repositories; each
crate's docs explain the environment variable that enables them. No test
downloads model weights: the embedding tests use the hashing test embedder
or no model at all, and the ones against real weights
(`crates/rueca-embed/tests/llama.rs`) run only with `RUECA_EMBED_TESTS=1`
after `rueca models pull qwen3-0.6b-q8`.

Try a change end to end:

```sh
cargo run -p rueca -- index path/to/repo
cargo run -p rueca -- query search_symbols '{"query":"parse"}' --root path/to/repo --pretty
```

## Navigating Rueca with Rueca

Rueca indexes itself; agents and contributors are encouraged to explore the
code through it. From the repository root, with a built binary
(`cargo build --release -p rueca`, then `target/release/rueca`, or an
installed `rueca`):

```sh
rueca index .        # first run extracts every file; re-run after edits
rueca status         # indexed roots: repo, commit, nodes, edges, files, age
```

The index lives in `$RUECA_HOME` (default `~/.cache/rueca`). After editing,
re-run `rueca index .`: unchanged files come from the facts cache, only
changed files are re-extracted.

From an agent: `claude mcp add rueca -- rueca mcp --root "$PWD"`, or for
other clients `{"mcpServers": {"rueca": {"command": "rueca", "args": ["mcp",
"--root", "/absolute/path/to/rueca"]}}}`. The flow is `search_symbols` →
`get_symbol` / `get_snippet` → `trace` (`search_code` for literals).

```sh
rueca query search_symbols '{"query":"register language pack","language":"rust","limit":5}' --root . --text
rueca query get_symbol '{"qualified_name":"rueca_lang_rust::RustPack.extract"}' --root . --text
rueca query trace '{"qualified_name":"rueca_extract::Registry.register","direction":"callers"}' --root . --text
rueca query get_file_outline '{"path":"crates/rueca-index/src/lib.rs"}' --root . --text
```

Rust qualified names are the module path, crate name first, then the local
name joined with `.`: `impl` items are `Type.method`, trait items
`Trait.method`, items of an inline module `m.f`. The bare local name
(`Registry.register`) also works but may pick a same-named symbol in a test
fixture under `crates/*/tests/fixtures` or `bench/`; use the full name, or
`name` + `path` with `get_symbol`. The `trace` above prints:

```
github.com/DevArKaDiA/rueca@117083093bf2 (feat/lang-rust)
0 Method Registry.register  crates/rueca-extract/src/lib.rs L397-399
1 in d1 Function default_registry  crates/rueca-index/src/lib.rs L125-137  →0 CALLS 0.85 type_resolved @127
2 in d1 Function index_worktree  crates/rueca-index/src/lib.rs L207-331  →0 CALLS 0.4 heuristic @294
3 in d1 Method PackageLinker.link  crates/rueca-index/src/link.rs L708-737  →0 CALLS 0.85 type_resolved @730
…
(16 tests excluded; `include_tests: true` lists them)
```

Each edge carries its strategy and confidence; drop the `heuristic` ones
when you need certainty. `get_symbol` gives the signature and neighbourhood:

```
Method RustPack.extract  crates/rueca-lang-rust/src/lib.rs L219-230  rust exported
  qn: rueca_lang_rust::RustPack.extract
  fn extract( &self, path: &str, language: Language, source: &[u8], budget: &Budget, ) -> Result<FileFacts, ExtractError>
  parent: File crates/rueca-lang-rust/src/lib.rs L1-252
  callers 4, callees 1
```

## Branching

Trunk-based development:

- `main` is the only long-lived branch. It is protected (PR + CI required,
  linear history) and always releasable.
- Work on short-lived branches off `main`: `feat/…`, `fix/…`, `docs/…`,
  `perf/…`, `refactor/…`, `ci/…`. Forks work the same way.
- PRs are **squash-merged**; the PR title becomes the commit on `main`.
- There are no `develop` or permanent release branches. Releases are cut from
  `main` by merging the release PR (see [docs/releasing.md](docs/releasing.md)).
  If an older version ever needs a patch, a `release/X.Y` branch is cut from
  its tag and fixes are cherry-picked onto it.

## Pull requests

- Keep PRs focused; one logical change per PR.
- Add or update tests. Extraction and resolution changes need a fixture that
  shows the new behaviour.
- **Docs ship with code**: update crate docs, `docs/tools.md` and the
  relevant RFC or `docs/decisions.md` in the same PR. `CHANGELOG.md` is
  generated from PR titles at release time.
- CI must pass: fmt, clippy (`-D warnings`), tests on Linux and macOS, MSRV,
  rustdoc, `cargo deny`, DCO, the PR title check and the SonarQube quality
  gate (below).
- If a change affects extraction or resolution quality, run the benchmark
  (`bench/README.md`) and include before/after numbers. Report regressions
  honestly; they are not automatically blockers.

### Quality gate (SonarQube)

Every PR, merge-queue entry and push to `main` runs the `SonarQube` check
(`.github/workflows/sonarqube.yml`), and no release is published without it.
It needs no account or secret: the job starts a throwaway SonarQube Community
server as a service container (image pinned by digest) and
`scripts/sonar-gate.sh` configures it through the Web API, runs
`sonar-scanner` (pinned image) with `sonar-project.properties`, prints a
summary (conditions, metrics, issues by rule and file) to the job summary and
fails the job if the gate fails.

- Profiles `rueca-strict`: Rust with **every** rule active, cognitive
  complexity (`rust:S3776`) at 15 per function; Python with the built-in
  default profile.
- Gate `rueca-strict`, on the **overall** code (a fresh server sees every line
  as new): 0 issues of any kind (bugs, vulnerabilities, code smells), every
  security hotspot reviewed (i.e. none open), duplicated lines ≤ 3 %,
  reliability / security / maintainability ratings A.
- A finding that is wrong for a specific line is waived with `# NOSONAR` /
  `// NOSONAR` on that line plus a comment saying why. Do not weaken the
  profile or the gate to get a PR through.
- Merge gate: `SonarQube` is a required status check of the `main` ruleset
  (Settings → Rules → Rulesets → `main`, maintainers). The ruleset is not in
  the repository; when the job name changes, update it there too.

Run it locally (Docker, `curl`, `jq`) against a throwaway server; the script
works against any URL (`SONAR_HOST_URL`, default `http://localhost:9000`):

```sh
docker run -d --name rueca-sonar-ci -p 9100:9000 \
  --tmpfs /opt/sonarqube/data:rw,size=3g,mode=1777 \
  sonarqube:community@sha256:aa0d41212704d1ed35f7ae3b49886b39f639590baf8020cdb191c2a2eb8f9eae
SONAR_HOST_URL=http://localhost:9100 scripts/sonar-gate.sh            # whole repository
SONAR_HOST_URL=http://localhost:9100 scripts/sonar-gate.sh -Dsonar.sources=bench   # one directory
docker rm -f rueca-sonar-ci
```

Extra arguments go to `sonar-scanner`. The script waits for the server, so it
can run right after `docker run` (about a minute end to end). The `--tmpfs`
keeps Elasticsearch working when Docker's disk is nearly full (it refuses to
create indices past 90 % use). The project is then browsable at
`http://localhost:9100` (user `admin`, password `SONAR_ADMIN_PASSWORD`,
default `Rueca-ephemeral-gate-1`).

### Developer Certificate of Origin

Every commit must be signed off, certifying the
[Developer Certificate of Origin](https://developercertificate.org/):

```sh
git commit -s -m "fix(resolve): follow re-exports through namespace imports"
```

This adds `Signed-off-by: Your Name <you@example.com>` to the message. A CI
check rejects PRs with commits that lack it. To fix a branch:
`git rebase --signoff main`.

### PR titles and commit messages

PR titles must follow [Conventional Commits](https://www.conventionalcommits.org/)
(checked in CI): `feat:`, `fix:`, `perf:`, `refactor:`, `docs:`, `test:`,
`build:`, `ci:`, `chore:`, `revert:`, optionally with the crate as scope
(`fix(segment): …`) and `!` for breaking changes (`feat(query)!: …`). They
drive the changelog and the next version. Commit messages inside a PR are
free-form but the same style is appreciated.

## Licensing of contributions

Rueca is licensed under Apache-2.0. Unless you state otherwise, any
contribution you submit is licensed under the same terms (Apache-2.0 §5).

Third-party code and queries must be under a permissive license (MIT,
Apache-2.0, BSD, ISC, Zlib, Unicode). GPL, AGPL, MPL, non-commercial and
source-available licenses cannot be vendored. Vendored files keep an SPDX
header with their origin and pinned commit, and are listed in [NOTICE](NOTICE).
New dependencies must pass `cargo deny check` (policy in `deny.toml`).

## Adding a language

A language is a crate implementing `rueca_extract::LanguagePack`: extraction
(definitions, references, imports), module naming and import resolution. Use
`rueca-lang-python` and `rueca-lang-typescript` as templates, vendor locals
queries rather than hand-writing scope rules, and add the language to the
benchmark before declaring support. Package scope, directory imports, typed
receivers, implicit `this`, overloads, declarations and the env / config
bridge are defaulted hooks of the contract (the `rueca-extract` crate docs
list them); `crates/rueca-resolve/tests/hooks.rs` has a small fake-pack
example for each. A new pack should only need its crate, a line in
`rueca_index::default_registry` and, if its language needs it,
family-specific resolver code. What "supported" requires (the release
gate and the parity row) is defined in
[RFC 0003 §3](docs/rfcs/0003-structured-data-and-languages.md#3-what-supported-means).

## Maintainers and decisions

Rueca is currently maintained by [@DevArKaDiA](https://github.com/DevArKaDiA).
Decisions are recorded in [`docs/decisions.md`](docs/decisions.md); significant
design changes are proposed as RFCs via pull request and discussed in the open.
