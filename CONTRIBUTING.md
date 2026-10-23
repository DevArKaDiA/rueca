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

Read [RFC 0001](docs/rfcs/0001-architecture.md),
[RFC 0002](docs/rfcs/0002-building-blocks.md), [the decisions log](docs/decisions.md)
and [the tool contract](docs/tools.md) before a non-trivial change: they are
short and explain why things are the way they are. In particular, RFC 0002
§9 lists which layers reuse existing crates and which are built here — prefer
assembling a maintained, permissively licensed component over writing one.

## Development setup

Requirements: Rust 1.90+ (`rustup` recommended), git. The benchmark also needs
Python 3.12+, [uv](https://docs.astral.sh/uv/) and Node.js (for SCIP indexers).

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check            # optional locally; runs in CI (cargo install cargo-deny)
```

Some tests are `#[ignore]` because they clone real repositories; each
crate's docs explain the environment variable that enables them.

Try a change end to end:

```sh
cargo run -p rueca -- index path/to/repo
cargo run -p rueca -- query search_symbols '{"query":"parse"}' --root path/to/repo --pretty
```

## Pull requests

- Keep PRs focused; one logical change per PR.
- Add or update tests. Extraction and resolution changes need a fixture that
  shows the new behaviour.
- **Docs ship with code**: update crate docs, `docs/tools.md`, the relevant
  RFC or `docs/decisions.md`, and `CHANGELOG.md` (Unreleased section) in the
  same PR.
- CI must pass: fmt, clippy (`-D warnings`), tests on Linux and macOS, MSRV,
  and `cargo deny`.
- If a change affects extraction or resolution quality, run the benchmark
  (`bench/README.md`) and include before/after numbers. Report regressions
  honestly; they are not automatically blockers.

### Developer Certificate of Origin

Every commit must be signed off, certifying the
[Developer Certificate of Origin](https://developercertificate.org/):

```sh
git commit -s -m "fix(resolve): follow re-exports through namespace imports"
```

This adds `Signed-off-by: Your Name <you@example.com>` to the message. A CI
check rejects PRs with commits that lack it. To fix a branch:
`git rebase --signoff main`.

### Commit messages

[Conventional Commits](https://www.conventionalcommits.org/) style is
preferred: `feat:`, `fix:`, `docs:`, `perf:`, `refactor:`, `test:`,
`chore:`, with the crate as scope when it helps (`fix(segment): …`).

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
benchmark before declaring support.

## Maintainers and decisions

Rueca is currently maintained by [@DevArKaDiA](https://github.com/DevArKaDiA).
Decisions are recorded in [`docs/decisions.md`](docs/decisions.md); significant
design changes are proposed as RFCs via pull request and discussed in the open.
