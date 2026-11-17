# RFC 0008 — Service mode

- Status: Draft; phase 0 (§10) in implementation
- Created: 2026-11-17
- Depends on: RFC 0001 (§8 storage, §13 security, §14 deployment modes),
  RFC 0004 (latency floor), RFC 0005 (envelope, pinned ids, `repo` /
  `ref`), RFC 0006 (organization layer, refs, generations), RFC 0007
  (embeddings, §5.5 vector cache, §8.3 service mode)
- Amends: RFC 0001 §5, §8.3, §8.4, §13, §14, §19 open questions 1, 5, 7,
  8; RFC 0002 §7.1, §7.3, §9; RFC 0006 §9.3, §12 open questions 5, 6, 9; RFC 0007
  §8.3, §15 open question 8
- Research date: 2026-11-17. Crate versions, specifications, host
  documentation and comparable systems were checked that day (§15). Code
  references are to commit `1568e47` (`main` after #60, #63 and #64)
  unless a reference names `c82029013c6d`, the commit the draft was
  written against. "Measured on this repository"
  means a release build of that commit; *(estimate)* marks arithmetic on
  stated inputs; *(derived)* arithmetic on published figures;
  *(unverified)* a claim no primary source confirmed; *(default)* a
  configuration value chosen here, to be tuned by measurement.

## 1. Summary

Rueca gets a server form: the same crates, segments and eight tools, run
as a container that indexes every repository of an organization (default
branches, chosen branches and tags, pull requests) and answers over MCP
on HTTP.

1. **One image, one binary, roles.** `rueca serve --config
   rueca-service.toml [--role query|indexer|all]`; `all` (default) runs
   everything; `query` is a stateless replica; `indexer` leases jobs,
   fetches, indexes, publishes, and one indexer runs the organization
   updater. Two image variants, CPU and CUDA.
2. **Object storage is the store of record**, with the local layout plus
   two changes: per-repository catalogs and per-repository packs.
   Immutable objects use `PutMode::Create`, mutable ones `PutMode::Update`
   in a compare-and-swap (CAS) retry loop. **No lock in the service
   store**; local mode gets a file lock (closes RFC 0006 §12 open
   question 9).
3. **Query replicas download whole segments** into an LRU disk cache and
   open them as today; they poll the generation pointer with a
   conditional GET every 5 s. Hotcache and range reads are phase 4,
   gated on measured time to ready.
4. **MCP over Streamable HTTP, stateless**: Rueca's own transport on
   `axum` 0.8 at `/mcp` (owner decision, §6.1: no `rmcp`), JSON responses,
   no sessions, no SSE; a per-request deadline inside the engine, where
   none exists today.
5. **Authentication** is a Rueca tower layer: an OAuth 2.1 resource
   server (MCP 2026-07-28) validating JWTs of external OIDC issuers, plus
   static API tokens; no embedded authorization server. A principal maps
   to the existing `Visibility` by configured rules; **deny by default**.
6. **Level-triggered indexing**: a reconciler holds the desired state
   (repository → ref → commit); webhooks are hints, adaptive polling
   (45 s – 8 h) the safety net; one coalesced job object per repository,
   leased by CAS.
7. **Git and isolation**: a bare mirror per repository, depth-1
   size-filtered fetches of exact commits, `GIT_NO_LAZY_FETCH=1`; today's
   `refs::export` and `index_worktree` reused; each job runs in child
   processes (`rueca index-worker`, one per ref) with resource limits and
   a timeout.
8. **Refs and retention**: default branch always, branches and tags by
   glob, pull request heads from events; per-kind retention; mark-and-sweep
   garbage collection with a 24 h grace (closes RFC 0006 §12 open
   question 6).
9. **Embeddings optional**: an `embed` job kind on GPU indexers or an
   endpoint the deployment runs; CPU-only deployments serve lexical search
   fully; `[retrieval].allow_remote = false` by default.
10. **One organization per deployment**, measured by a service harness
    whose differential test requires answers byte-equal to local mode.

## 2. Motivation

**North star.** A service that indexes every repository of an
organization, including branches and pull requests. The repository graph
per ref stays the unit (RFC 0006 §1) and the organization graph merges on
top; an agent searches one repository, several or all, and every id keeps
its repository and commit (RFC 0005).

Local mode and `--org` (RFC 0006 §7) cannot be that service:

| Limit today | Where |
|---|---|
| **One writer per store**: `write_segment` and `Store::register` "are not coordinated across threads or processes … can lose one of two simultaneous catalog updates" | `crates/rueca-segment/src/lib.rs:28-37` |
| No lock anywhere (no `flock`, `fs2`, `fd-lock`); tantivy's `acquire_lock` is a no-op | `crates/rueca-segment/src/org/mod.rs:1122-1124` |
| The catalog is one JSON array rewritten by read-modify-write | `Store::register_impl`, `crates/rueca-segment/src/store.rs:126-141` |
| One machine: `File::open` + `mmap`, no storage trait, "No hotcache and no object-storage reads" | `bundle.rs:175-181`, `lib.rs:240-241` |
| The read path creates directories (`Store::open` → `ensure_dirs`): a read-only mount fails | `crates/rueca-query/src/lib.rs:486`, `store.rs:12-17` |
| MCP over stdio only; no HTTP crate and no async runtime in `Cargo.lock` | `serve_stdio`, `crates/rueca-mcp/src/lib.rs:359`; `docs/mcp-protocol.md` §8 |
| `Visibility` exists, but nothing outside tests sets it, and local `select` ignores it | `crates/rueca-query/src/lib.rs:346-361, 482-508` |
| No per-query time budget | `Engine::respond`, `crates/rueca-query/src/lib.rs:450-465` |
| Extraction in process (rayon + `catch_unwind`), no memory limit | `crates/rueca-index/src/lib.rs:264-278, 525-544`; `crates/rueca-extract/src/lib.rs:61-72` |
| No jobs: indexing runs in the CLI or on the `rueca-auto-index` thread | `spawn_gate`, `crates/rueca-mcp/src/lib.rs:327` |
| "packs only grow" | `crates/rueca-segment/src/lib.rs:213-216` |
| `url` members are `git clone --depth 1` checkouts under the store | `clone_dir` / `sync_clone`, `crates/rueca-index/src/org.rs:169-211` |

Agents in an organization need one URL that answers for every repository
the caller may read, fresh minutes after a push, at any indexed ref, with
local mode's answers, and without a checkout or an index per developer.
The README roadmap row: "**Service mode** — One container, segments in
object storage, MCP over HTTP, organization-wide indexing with GPU for
embeddings".

## 3. Goals and non-goals

**Goals.** Index every configured repository of one organization
(default branch, branch and tag globs, pull request heads) from URLs,
with no operator action after configuration. Serve the eight tools
(`search_symbols`, `search_code`, `get_symbol`, `get_snippet`, `trace`,
`get_file_outline`, `list_repositories`, `list_contracts`) with `docs/tools.md`'s arguments
(`repo`, `ref`, `budget_tokens`, `cursor`, `format`) unchanged. Answers
equal to local mode for the same snapshots (§8.3). N stateless replicas
and N indexers with no coordinator but object storage. Per-caller
visibility, deny by default, with the seam for Org mode's mirrored
permissions. CPU-only deployments complete; GPUs only speed up
embeddings. Local mode unchanged in behaviour, numbers and store.

**Non-goals.**

- **Permissions mirroring** and the **inventory of every host** (users,
  teams, grants): Org mode. This RFC designs the seams (`Authenticator`,
  the rule source of `Visibility`) and adds only a one-call repository
  listing (§7.1, phase 3).
- **Multi-tenancy**: one organization per deployment (§6.9).
- **A REST query API**: MCP is the query API; other endpoints are
  operational (§6.8).
- **Delta segments** and shared segment writes between refs: phase 4
  (§7.10).
- An embedded authorization server (§6.3); running adopter code (RFC 0001
  §13).

## 4. Architecture

### 4.1 Picture

```
   code hosts: GitHub · GitLab · Bitbucket           MCP clients       OIDC issuer
     │ webhooks (HMAC)    ▲ ls-remote / fetch           │ POST /mcp      │ JWKS
     ▼                    │                             ▼                ▼
 ┌─────────── indexer role (1..N) ────────────┐  ┌──── query role (1..N, stateless) ───┐
 │ /webhooks/* → desired state (job objects)  │  │ auth · rate limit · deadline        │
 │ lease job (CAS) → `rueca index-worker`     │  │ Streamable HTTP (Rueca's own)       │
 │ bare git mirrors, facts cache (local disk) │  │ Engine (organization mode)          │
 │ org updater (one leader): reconcile,       │  │ disk cache: segments, packs, index  │
 │   generations, garbage collection          │  │ /healthz /readyz /metrics           │
 └──────────────┬─────────────────────────────┘  └───────────────┬─────────────────────┘
                │ Create / Update (CAS) / Delete                  │ GET, conditional GET
                ▼                                                 ▼
 ┌───────────────────────── object storage: the store of record ─────────────────────────┐
 │ store.json · repos/<repo-dir>/{catalog.json, segments/, packs/} · facts/ · vectors/   │
 │ orgs/<name>/{org.json, generations/, index/, profiles/, links/} · jobs/ · leases/     │
 └────────────────────────────────────────────────────────────────────────────────────────┘
```

No database, no queue, no service discovery: processes talk only to
object storage, the code hosts and their clients. `all` is both boxes in
one process.

### 4.2 What runs where

| Component | `query` | `indexer` | `all` |
|---|---|---|---|
| `/mcp` (Streamable HTTP, auth, visibility, deadline) | yes | no | yes |
| `/healthz`, `/readyz`, `/metrics`, `/status` | yes | yes | yes |
| `/webhooks/*`, `/admin/*` | no (404) | yes | yes |
| Generation watcher (`org.json`, 5 s) and disk cache | yes | no | yes |
| Job leasing, git mirrors, `rueca index-worker` children, facts cache | no | yes | yes |
| Organization updater (reconcile, polling, generations, GC) | no | while holding `leases/org-updater.json` | yes |
| `embed` jobs (§7.11) | no | if `gpu = true` or a remote endpoint | same |
| Code-host credentials | never | yes | yes |
| Object storage writes | `reads/*` only (§5.8) | §9.4 | §9.4 |

**Recorded:** one binary; the role decides which loops start. RFC 0001
§14's `rueca worker` / `rueca query` commands become `--role`; `rueca
index`, `rueca query` and `rueca mcp` keep their local behaviour.

### 4.3 Images

| Variant | Base | Binary | Size |
|---|---|---|---|
| CPU | `gcr.io/distroless/cc-debian13` (glibc + libgcc) + `git` | `--features service` with the default `embeddings` feature (llama.cpp CPU backend, RFC 0007 §4.1: query-side embedding and the small CPU presets work; `--no-default-features` gives a lexical-only variant) | tens of MB + `git` ([distroless](https://github.com/GoogleContainerTools/distroless)); not measured |
| CUDA | `nvidia/cuda:13.5.0-runtime-ubuntu24.04` + `git` | `--features service,cuda` (`llama-cpp-2`, RFC 0007 §4) | base ≈ 1,953 MB compressed, amd64 ([Docker Hub](https://hub.docker.com/r/nvidia/cuda/tags)) |

**`git` ≥ 2.45** is required (§7.6). distroless has no package manager
and `git-remote-https` links libcurl, TLS and zlib, copied from a Debian
build stage; if that proves brittle the CPU base becomes
`debian:trixie-slim` (≈ 30 MB compressed, Docker Hub). Ubuntu 24.04
ships git 2.43 ([packages](https://packages.ubuntu.com/noble/git)), so one `git` build stage feeds both images
and each build asserts the version. A new container workflow builds from
the release binaries; cargo-dist 0.33.1 keeps archives and installers (no
images *(unverified: from its README)*). Images run non-root on a
read-only root filesystem, with volumes for `cache_dir`, `mirror_dir` and
`facts_dir`.

### 4.4 Configuration

TOML, `deny_unknown_fields` like `rueca-org.toml`
(`crates/rueca-index/src/org.rs:55-69`). Secrets are environment
references (`"env:NAME"`); a literal where a secret is expected is a
configuration error.

```toml
# rueca-service.toml
[store]
url = "s3://acme-rueca/prod"        # s3:// gs:// az:// file:/// memory://
options = { region = "eu-west-1" }  # object_store keys (aws_*, google_*, azure_*)
cache_dir = "/var/cache/rueca"
cache_bytes = "40GiB"
ready_when = "index"                # or "warm" (§6.7)

[org]
name = "acme"
# phase 3 (§7.1):
# inventory = { host = "github", owner = "acme", token = "env:GITHUB_TOKEN",
#               include = ["*"], exclude = ["sandbox-*"], archived = false }

[[repo]]
url = "https://github.com/acme/billing.git"
default_branch = "main"             # optional: the remote's HEAD
branches = ["release/*"]
tags = "v*"
prs = true

[http]
listen = "0.0.0.0:8080"
ops_listen = "0.0.0.0:9090"         # /healthz /readyz /metrics
canonical_url = "https://rueca.acme.internal/mcp"
allowed_hosts = ["rueca.acme.internal"]
allowed_origins = []                # the canonical origin is always allowed
max_body_bytes = "1MiB"
request_timeout = "12s"
query_deadline = "10s"
rate_limit = { per_second = 10, burst = 50 }

[auth]
admins = ["token:ops", "group:platform"]

[[auth.token]]
name = "ci"
hash = "sha256:6f1ed002ab5595859014ebf0951522d9…"
scopes = ["rueca:read"]
repos = ["github.com/acme/*"]

[[auth.oidc]]
issuer = "https://login.acme.com/realms/eng"
audience = "https://rueca.acme.internal/mcp"   # default: canonical_url
algorithms = ["RS256", "ES256"]
groups_claim = "groups"
rules = [
  { group = "eng", repos = ["github.com/acme/*"] },
  { group = "contractors-billing", repos = ["github.com/acme/billing"] },
]

[indexing]
jobs = 1
threads = 0                          # 0 = all cores
gpu = false
org_updater = true
budget = { max_bytes = "2MiB", max_parse_time = "5s", max_depth = 512 }
worker = { address_space = "24GiB", cpu_time = "4h", wall_time = "30m" }
mirror_dir = "/var/lib/rueca/mirrors"
mirror_max_age = "7d"
facts_dir = "/var/lib/rueca/facts"
facts_write_through = false
poll = { min = "45s", max = "8h", pr_heads = false }
updater_tick = "5s"
hosts = ["github.com", "gitlab.com", "bitbucket.org"]
allow_private_networks = false
credentials = { "github.com" = "env:GITHUB_TOKEN" }
webhooks = { github = "env:RUECA_GITHUB_WEBHOOK_SECRET" }

[retention]
default_snapshots = 2
branch_snapshots = 1
tag_snapshots = 1
pr_after_close = "7d"
pr_idle = "14d"
grace = "24h"

[retrieval]                          # RFC 0007 §8.3
preset = "qwen3-4b-q4"               # "off", or "remote:openai-compatible/<url>"
allow_remote = false

[telemetry]
log_format = "json"
otlp_endpoint = "http://otel-collector:4318"
prometheus = true
```

**Precedence** (RFC 0007 §8.3's rule): argument, environment, file,
built-in default. Environment: `RUECA_CONFIG`, `RUECA_ROLE`,
`RUECA_STORE_URL`, `RUECA_LOG` (today's `EnvFilter`,
`crates/rueca-cli/src/main.rs:226-239`) and the variables `env:`
references name. `rueca serve --check-config` validates the file, the
references and the store, then exits.

## 5. Storage

### 5.1 Layout

```
<bucket>/<prefix>/
  store.json                               format, created_at, org          (CAS, once)
  repos/<repo-dir>/catalog.json            refs → snapshots                  (CAS)
  repos/<repo-dir>/segments/<id>.rcseg     segment bundle, format 2          (Create)
  repos/<repo-dir>/packs/<id>.rcpack       blobs of this repository          (Create)
  facts/<sha[0..2]>/<key>.bin              facts cache, optional             (Create)
  vectors/<embed key>/<file>               RFC 0007 §5.5 vector cache        (Create)
  orgs/<name>/org.json                     generation pointer                (CAS)
  orgs/<name>/generations/<n>.json         OrgGeneration                     (Create)
  orgs/<name>/index/<uuid>.<ext>           tantivy files copied from segments (Create)
  orgs/<name>/profiles/<segment>.json      RepoProfile                       (Create)
  orgs/<name>/links/<segment>-<hash>.bin   link set of one consumer entry    (Create)
  jobs/<repo-dir>.json                     index job: desired state, lease   (CAS)
  jobs/<repo-dir>.embed.json               embed job (§7.11)                 (CAS)
  leases/org-updater.json                  updater leader lease              (CAS)
  reads/<yyyy-mm-dd>/<hh>-<replica>.json   refs read, per replica and hour   (Create)
```

`<repo-dir>` is RFC 0006 §9.3's slug-and-hash name. `orgs/` keeps today's
names and formats (`crates/rueca-segment/src/org/mod.rs:13-21`,
`ORG_FORMAT = 1`). Against the local store (`docs/how-it-works.md`
§4.5–§4.6):

| Local store | Service store | Why |
|---|---|---|
| `catalog.json`: one array sorted by root (`lib.rs:218-227`) | `repos/<repo-dir>/catalog.json` per repository | CAS contention per repository; there are no roots |
| shared `packs/` + `packs/index.rcpi` | `repos/<repo-dir>/packs/`, no index file | a snapshot lists its packs (§5.9) |
| `refs/<repo-dir>/<ref>/` exports, `orgs/<n>/clones/` | none; scratch and bare mirrors on the indexer | the bucket holds only what readers read |
| `tmp/` | none | `Create` is atomic per object |

### 5.2 Immutable and mutable objects

| Class | Objects | Write | On conflict |
|---|---|---|---|
| Immutable | segments, packs, facts, vectors, generations, `index/`, `profiles/`, `links/`, `reads/` | `PutMode::Create` | `AlreadyExists`: facts and vectors are content-addressed (ignore); a generation means another publisher won (§5.4); otherwise an error |
| Mutable | `store.json`, catalogs, `org.json`, `jobs/*`, `leases/*` | read, then `PutMode::Update(UpdateVersion { e_tag, version })`; `Create` for the first write | `Precondition`: re-read, re-apply, retry (8 attempts, jittered *(default)*) |

A mutation is a function re-applied to the fresh value at every retry,
so a retry never overwrites another writer's change. Both `e_tag` and
`version` are kept: GCS compares generations and fails with
`MissingVersion` without one (`src/gcp/client.rs`).

**Ids.** A segment id is `<repo slug>-<commit12>-<unix seconds>-<16 random
bits>` (`crates/rueca-segment/src/writer.rs:36-40`) and a pack id
`<unix seconds>-<32 random bits>` (`writer.rs:79`): two indexers writing
the same commit in the same second collide on a segment with probability
1/65,536 *(derived)*. **Recorded:** service mode widens both random parts
to 64 bits (ids are strings; no format change), so a `Create` collision
on a segment or pack is a bug and an error.

### 5.3 Per-repository catalog

```json
{"format": 1, "repo": "github.com/acme/billing",
 "url": "https://github.com/acme/billing.git", "default_ref": "main",
 "refs": {
   "main": {"kind": "default", "snapshots": [
     {"commit": "9f1c2e7a4b0d5c6e8f9a1b2c3d4e5f60718293a4",
      "segment": "github-com-acme-billing-9f1c2e7a4b0d-1794929130-5e1d7a90c3b24f18",
      "indexed_at": 1794929130, "rueca_version": "0.2.0", "bytes": 14199972,
      "packs": ["1794929120-3f6c09a1d2e4b5c6", "1794012345-a1b20c3d4e5f6a7b"],
      "vectors": {"model_key": "3f9a1c2b…", "segment": "…"}}]},
   "release/2.4": {"kind": "branch", "snapshots": […]},
   "v2.4.1": {"kind": "tag", "snapshots": […]},
   "pr/42": {"kind": "pr", "snapshots": […], "closed_at": null}},
 "removed": [{"ref": "pr/17", "at": 1794900000}]}
```

`snapshots` is newest first, capped by retention (§5.8); the first is
what queries read, the others stay readable by pinned id until
collected. An entry is idempotent by `(ref, commit)`: a writer whose CAS
retry finds that pair stops and leaves its segment to garbage collection
(§7.5). `vectors` is set by the `embed` job (§7.11). One catalog with a
few dozen refs is a few kilobytes *(estimate)*.

### 5.4 Generation pointer and updater lease

`orgs/<name>/org.json` keeps today's `{name, generation}` (`OrgPointer`,
`org/mod.rs:236-239`). Today `publish` writes `generations/<next>.json`,
renames `org.json` over the old one, then collects garbage
(`org/mod.rs:844-877`). In service mode: (1) `generations/<next>.json`
with `Create` (`AlreadyExists`: another updater published `next`; stop,
re-read, rebuild on top of it); (2) `org.json` with `Update` against the
e-tag read when the build started (`Precondition`: the same); (3) no
inline garbage collection (§5.8).

The updater is elected by `leases/org-updater.json`, `{"owner":
"indexer-2/7f3a91", "until": 1794929190, "epoch": 412}`: taken with
`Create` when absent or `Update` once `until` is 30 s *(default, clock
skew)* in the past; 60 s lease renewed every 20 s *(defaults)*; `epoch`
increments on take-over. Leader election by conditional writes is "only
ever … eventually correct"
([Morling](https://www.morling.dev/blog/leader-election-with-s3-conditional-writes/);
single-object `If-Match` variant,
[quanttype](https://quanttype.net/posts/2025-02-25-leader-election-with-s3-and-if-match.html)).
That suffices because each publication is itself a CAS: a second updater
fails steps 1–2 and rebuilds; a duplicated build costs compute only.

Replicas GET `org.json` with `If-None-Match: <e-tag>` every 5 s
*(default)*; `304` (`object_store` `NotModified`) is one request; a
change reopens the generation with today's logic (`Engine::org_index`,
`crates/rueca-query/src/lib.rs:657-674`; `current_generation` retries
when a generation file is gone, `org/mod.rs:360-376`).

### 5.5 Local filesystem: a file lock

`object_store`'s local backend implements `Create` but not `Update`
(`NotImplemented`, `src/local.rs`). Local mode keeps its atomic-rename
publication (`atomic_write`, `store.rs:21-30`) and adds an **advisory
exclusive lock** on `<store>/.lock` around every read-modify-write of
`catalog.json`, `packs/index.rcpi` and `orgs/<name>/org.json`, and around
organization garbage collection. Readers take no lock; a writer waiting
more than 60 s *(default)* fails naming the holder's pid. Mechanism:
`std::fs::File::lock` (stable since Rust 1.89,
[docs](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock); MSRV 1.90),
else `fs4` 0.13.1, already in `Cargo.lock` through tantivy (MIT OR
Apache-2.0, MSRV 1.75): no new dependency. This answers RFC 0006 §12 open
question 9 for local mode as well; the `file://` service backend uses the
same lock and is single-host.

### 5.6 Replica disk cache

A replica downloads the segments, packs and organization index files it
needs into `cache_dir` (temporary name, BLAKE3 trailer check, rename)
and opens them as today, `mmap` + BLAKE3 over every component
(`Bundle::open`, `bundle.rs:175-181`). Eviction is LRU by bytes within
`cache_bytes`, never touching the current generation's open set, and
drops the segment from the engine's map (`evict_stale`,
`crates/rueca-query/src/lib.rs:808-812`). Each new generation prefetches
the organization index files (every organization-scope query needs
them), then default segments, most recently read first; ref segments
load on first use.

This repository's segment is 14,199,972 bytes for 1,599 files (measured
on this repository at `c82029013c6d`; of 30 components, `nodes` 3.25 MB, tantivy 4.4 MB,
n-grams 2.9 MB). Fifty such repositories hold ≈ 710 MB of default
segments *(estimate)*: ≈ 7 s to download at 100 MB/s, ≈ 28 s at 25 MB/s
*(estimate: assumed bandwidth)*, ≈ 0.9 s to open *(estimate: 50 × RFC
0007 §6.5's ~18 ms)*.

**Recorded:** whole-object caching in v1. Hotcache and footer-first range
reads (RFC 0001 §8.1, §19 open question 1) start in phase 4 only if the
bench organization's measured time to ready exceeds 60 s.
`Bundle::from_mmap` and its per-component offsets and checksums
(`bundle.rs:195-226`) are where a range-read bundle would sit.

### 5.7 Facts and vectors

- **Facts**: each indexer keeps today's local cache
  (`<sha>-<lang>-v<ver>-f<FACTS_FORMAT>[-p<hash>].bin`,
  `crates/rueca-index/src/lib.rs:546-572`). `facts_write_through = true`
  also writes misses to `facts/` (`Create`) and reads it on a local miss.
  Off by default: a GET per missed file is slow next to a local read, and
  a repository's refs already run in sequence on one indexer (§7.5),
  where RFC 0006 §9.3 measured the reuse (a second ref of `pallets/click`
  re-extracts 52 of 156 files, `docs/decisions.md`). Phase 2 measures the
  cross-indexer hit rate.
- **Vectors**: RFC 0007 §5.5's content-addressed cache moves to
  `vectors/<embed key>/`, format unchanged (f16, 204.8 MB per 100k
  vectors at 1024 dimensions); embedding indexers mirror it locally.

### 5.8 Garbage collection

Mark-and-sweep by the updater, hourly *(default)*. **Live set**: every
snapshot in a repository catalog (segment, `vectors` segment, packs); the
current and previous generations, as today's `collect_garbage` keeps `n`
and `n − 1` (`org/mod.rs:1024-1056`), with every `index/`, `profiles/`,
`links/` object and segment they name; vector files named by a live
segment's `vectors.meta`. **Sweep**: list each immutable prefix and
delete what is outside the live set and older than the **grace period,
24 h** *(default)*, so a publish in flight (objects written, catalog CAS pending) and
a replica downloading a just-replaced generation are safe. Facts are a
cache: deleted 90 days after their last write *(default)*; `reads/` after
the idle period.

**Retention** (answers RFC 0006 §12 open question 6; *(defaults)*):

| Ref kind | Snapshots kept | Expiry |
|---|---|---|
| Default branch | 2 | never |
| Branch (glob) | 1 | the host stops listing it, or it leaves the glob |
| Tag | 1 | never; only `DELETE /admin/refs` |
| `pr/<n>` | 1 | closed or merged + 7 days, or 14 days unread |
| On demand (`POST /admin/index`) | 1 | 14 days unread |

"Unread" needs read evidence, and the decision makes query replicas
read-only. **Recorded, with that caveat:** a replica writes one `reads/`
object per hour (`Create`, a list of `(repo, ref)` pairs it served), the
only write a query credential has; the updater lists the idle window.
`pr_idle = "0"` disables idle expiry and keeps replicas strictly
read-only (§13, question 12).

Expiry is a catalog CAS by the updater (the ref moves to `removed`); its
objects go at the next sweep past the grace. A pack is deleted when no
live snapshot of its repository lists it; repacking is phase 4. **Pinned
ids** resolve while their snapshot is live; afterwards today's contracts
apply (`docs/tools.md`): an id whose commit is no longer indexed resolves
against the repository's current default, and a `ref` not indexed is an
error listing the indexed refs.

### 5.9 Segment meta and the store lock question

`SegmentMeta` (`crates/rueca-segment/src/lib.rs:323-336`) gains
`#[serde(default, skip_serializing_if = "Vec::is_empty")] pub packs:
Vec<String>`, the packs holding the snapshot's blobs; local stores leave
it empty and keep `index.rcpi`. Format 2 takes it without a bump
("Readers ignore components they do not know", `lib.rs:61-62`;
precedents `files.modules`, `symbols.bodiless`). Readers build the blob
map from those packs' footers (44-byte entries, `packs.rs:15-25`) instead
of `index.rcpi`, a derived file "rebuilt whenever the set of packs …
differs" (`lib.rs:34-37`) that has no meaning over a bucket; the catalog
repeats the list so garbage collection opens no segment.

**RFC 0006 §12 open question 9 — closed. Recorded:** neither a lock nor
a transactional catalog. Mutable objects are small and CAS-written,
immutable objects never conflict, the pointer is CAS, and the one
singleton (the updater) holds a lease whose failure mode is a wasted
build, never a lost write. Local mode gets §5.5's file lock.

### 5.10 Crate

| | `object_store` 0.14.2 | OpenDAL 0.59.5 |
|---|---|---|
| License, MSRV | MIT/Apache-2.0, 1.85 | Apache-2.0, **1.91** (Rueca: 1.90) |
| GCS update CAS | `if-generation-match` | only `if_not_exists` |
| Verdict | ~~chosen~~ rejected at implementation (below) | rejected (§12) |

**Amended 2026-11-17 (phase 0, owner decision).** Adding `object_store`
0.14.2 fails `cargo deny`: it depends unconditionally, even with no
features, on `chrono` 0.4.45 (RUSTSEC-2026-0543, unmaintained) and
`humantime` 2.4.0 (RUSTSEC-2026-0542, unmaintained), and its `aws` /
`gcp` / `azure` features pull `webpki-root-certs` (CDLA-Permissive-2.0,
not in `deny.toml`) through `reqwest/rustls`. Advisory ignores are
rejected (the same rule that replaced `rmcp`, §6.1), and OpenDAL needs
MSRV 1.91. **Recorded:** `rueca-storage` keeps the `Storage` trait of
§5.11 and assembles the S3 backend from
[`rusty-s3`](https://crates.io/crates/rusty-s3) 0.11 (sans-IO request
builder and SigV4 signer, BSD-2-Clause, `jiff` for dates) and `ureq`
3.5 (`rustls-no-provider` + `ring`) with the platform's roots through
`rustls-native-certs` (no CDLA bundle); the in-memory and `file://`
backends are Rueca's own. Signing is never written in-house. Covered:
AWS S3 and S3-compatible stores (MinIO verified in phase 0, R2); GCS
through its XML API with HMAC keys, the generation preconditions
signed, verified against a fake server only; Azure Blob is deferred (an
S3-compatible gateway in front of it works today). Credentials come
from options or `AWS_*` environment variables; IMDS / IRSA providers
are phase 2 (`CredentialsProvider` seam). The rest of this section stays as
the behaviour reference for conditional writes.

The first draft chose `object_store = { version = "0.14", default-features = false, features =
["aws", "gcp", "azure", "fs"] }`; the in-memory backend backs every test.
Conditional writes ([`PutMode`](https://docs.rs/object_store/0.14.2/object_store/enum.PutMode.html);
backend sources in the [repository](https://github.com/apache/arrow-rs-object-store)):

| Backend | `Create` | `Update` |
|---|---|---|
| S3 | `If-None-Match: *` (304/412 → `AlreadyExists`) | `If-Match` e-tag; 409 retried; 404 → `Precondition` |
| GCS | `x-goog-if-generation-match: 0` | `if-generation-match` with `UpdateVersion.version` |
| Azure | `If-None-Match: *` | `If-Match` e-tag |
| Local fs | yes | **NotImplemented** → §5.5 |
| In-memory | yes | yes |
| HTTP | no | no (read-only) |

S3 added `If-None-Match` on 2024-08-20, then `If-Match` and bucket
policies requiring conditional writes on 2024-11-25
([guide](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html),
[announcement](https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes),
[enforcement](https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-enforcement-conditional-write-operations-general-purpose-buckets/));
concurrent conditional requests can return 409, after which a multipart
upload is redone. `S3ConditionalPut::ETagMatch` is the default since
0.12.0 ([docs](https://docs.rs/object_store/0.14.2/object_store/aws/enum.S3ConditionalPut.html));
DynamoDB locking was removed in 0.13.0
([changelog](https://github.com/apache/arrow-rs-object-store/blob/main/CHANGELOG-old.md)),
so stores without conditional writes are unsupported. `RetryConfig`
defaults to 10 retries within 3 min, without renewing credentials; no
`~/.aws/config` profile reader *(unverified: from the source)*. R2
documents the conditional headers
([R2](https://developers.cloudflare.com/r2/api/s3/extensions/)); MinIO
accepts `If-None-Match: *` since 2024
([#19682](https://github.com/minio/minio/pull/19682),
[#20346](https://github.com/minio/minio/issues/20346)) but is archived
(§13, question 2).

**Reference pattern: Quickwit** — immutable UUID-named splits in object
storage, each with a hotcache so a searcher can "open a split in less than
60ms", stateless searchers, and a metastore that is PostgreSQL in a
cluster and a file (local or S3) on one node
([architecture](https://quickwit.io/docs/overview/architecture)). Rueca's
segments are its splits, catalogs and the pointer its file metastore; the
hotcache is phase 4.

### 5.11 The storage seam

`atomic_write` / `sync_dir` (`store.rs:21-41`) is today's only
publication primitive; `atomic_write`'s six direct callers (`Store.save`,
`BlobIndex.save`, `OrgBuild.publish`, `OrgBuild.compute_profile`,
`copy_tantivy_files`, `write_links`) and `sync_dir`'s two others
(`write_segment`, `PackWriter.finish`, which rename their own temporary
files) are what moves behind:

```rust
pub trait Storage: Send + Sync + 'static {
    async fn get(&self, path: &str) -> Result<Versioned<Bytes>>;
    async fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> Result<Option<Versioned<Bytes>>>;
    async fn get_range(&self, path: &str, range: Range<u64>) -> Result<Bytes>;
    async fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError>;      // Create
    async fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>)
        -> Result<ObjectVersion, PutError>;                                                    // Update / Create
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>>;                             // with e-tags
    async fn delete(&self, path: &str) -> Result<()>;
    fn local_path(&self, path: &str) -> Option<PathBuf>;   // filesystem backends: mmap in place
}
pub struct ObjectVersion { pub e_tag: Option<String>, pub version: Option<String> }
pub enum PutError { AlreadyExists, Precondition, Other(anyhow::Error) }
```

`object_store` is async; `rueca-segment` and `rueca-index` are not.
**Recorded:** the trait is async (boxed futures for `dyn Storage`); local
commands keep their synchronous code over the local implementation;
service roles bring `tokio` for `axum` and `object_store` in the service
crate, while `rueca-mcp`'s core stays synchronous and runtime-free
(`docs/mcp-protocol.md` §8). RFC 0001 §8.1's `SegmentReader` / `SegmentWriter` stay unbuilt:
a segment is still a local file; only how it arrives changes.

## 6. MCP over HTTP

### 6.1 Transport

**Owner decision (2026-11-17): no `rmcp`.** On the research date `cargo
deny` failed on `main` with RUSTSEC-2026-0543, `chrono` unmaintained
([advisory](https://rustsec.org/advisories/RUSTSEC-2026-0543)); `chrono`
reaches Rueca only through `rmcp` 3.10.0 (directly, and through its
`schemars` dependency, `cargo tree -i chrono` at `c82029013c6d`). The
owner rejected ignoring the advisory and chose a minimal MCP layer of
Rueca's own (branch `refactor/mcp-without-rmcp`: JSON-RPC `initialize`,
`tools/list`, `tools/call`, `ping`, stdio first, a transport abstraction
for HTTP) over a dependency tree Rueca does not control. The first draft
of this section ran rmcp's `transport-streamable-http-server` under
`axum`; the behaviour chosen then is kept, the implementation is Rueca's.

| Item | Choice | Source |
|---|---|---|
| MCP layer | `rueca-mcp`'s own, landed in #64: `RuecaServer::handle(msg, &mut Session) -> Step` (`crates/rueca-mcp/src/protocol.rs:134-142`), `Step { Reply, Call, Silent, Cancel }` (`protocol.rs:105-116`), `ToolCall::run` (`tools.rs:195`), `parse_line` (`jsonrpc.rs`); synchronous, no I/O, no runtime | `docs/mcp-protocol.md` §8 |
| HTTP | `axum` 0.8.9 (MIT, MSRV 1.80), `hyper` 1.12.0, `tower-http` 0.7.1 (`trace`, `limit`, `timeout`) | [tower-http](https://crates.io/crates/tower-http) |
| Mount | `Router::new().route("/mcp", post(mcp_handler))`; no `GET`, no `DELETE` | §6.2 |
| Protocol versions | `SUPPORTED_VERSIONS` (`protocol.rs:20`): 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25 with `initialize`, 2026-07-28 stateless with `server/discover`; every one served statelessly over HTTP | `docs/mcp-protocol.md` §7; [changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog) |

Transport behaviour, the same choices made against rmcp's
`StreamableHttpServerConfig` and now Rueca's own (`[http]`, §4.4):

| Behaviour | Rueca | Why |
|---|---|---|
| Sessions | none: `Mcp-Session-Id` ignored, never minted. Per request: `initialize`, or a body whose `_meta` carries a protocol version → `Session::new()`; anything else → `Session::legacy(<MCP-Protocol-Version header, "2025-03-26" when absent>)` (`protocol.rs:83-95`; `docs/mcp-protocol.md` §8) | stateless for every version (SEP-2567 already makes 2026-07-28 requests stateless) |
| Response body | always `application/json`, one JSON-RPC response per `POST`; no SSE | tool calls are short: one body, no stream, no load-balancer affinity (§13, question 11) |
| `allowed_hosts` | `[http].allowed_hosts` + the canonical host; loopback when unset | public deployments must set it |
| `allowed_origins` | `[http].allowed_origins` + the canonical origin | `Origin` MUST be validated (403) |
| `max_body_bytes` | 1 MiB *(default)* | 413 above |
| Required headers | `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` checked against `_meta` by the handler itself | §6.2; tested in phase 1 |
| Shutdown | the server stops accepting, drains in-flight calls up to `request_timeout`, exits | graceful drain |

The handler is one message per `POST`: `parse_line(body)` → `handle` →
`Step::Reply` is `200` with `Outgoing::to_json()`; `Step::Call` runs
`call.run(&server)` under `spawn_blocking` (it ends in
`RuecaServer::call_blocking` → `Engine::respond`,
`crates/rueca-mcp/src/lib.rs:133-144`) and answers `200`; `Step::Silent`
and `Step::Cancel` answer `202`; `Incoming::Unparsable` is `400` with
-32700 (`docs/mcp-protocol.md` §8). The validated principal travels as an
`axum` request extension into the handler and selects that call's
`Visibility`. `IndexGate` and `INDEX_WAIT` (120 s, `lib.rs:164`) are
local only; `tokio` enters only in the HTTP layer, as that document
asks.

### 6.2 Specification behaviours (MCP 2026-07-28)

From the [transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
and the [changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog):

| Request | Response |
|---|---|
| `POST /mcp` with `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` (for `tools/call`) matching `_meta` | `200`, `application/json` |
| Header and body disagree | `400`, JSON-RPC `-32020` `HeaderMismatch` |
| Unsupported `MCP-Protocol-Version` | `400` |
| `GET` / `DELETE /mcp` | `405` |
| `Origin` or `Host` not allowed | `403` |
| Body over the limit | `413` |
| `Mcp-Session-Id`, `Last-Event-ID` | ignored, never minted |
| `server/discover` | answered (MUST, SEP-2575; `handle` already does, advertising `SUPPORTED_VERSIONS`, `protocol.rs:20`) |

Rueca sends no server-initiated messages, so the removed GET stream costs
nothing; `tools/list` keeps a 1 h public cache TTL (`ttlMs` /
`cacheScope`, SEP-2549; `TOOLS_TTL_MS`, `protocol.rs:31`;
the list does not depend on the principal). The `Mcp-Method` / `Mcp-Name`
/ body match is Rueca's own check, tested in phase 1. 2025-11-25 clients
([transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports))
that `initialize` and send `MCP-Session-Id` are served statelessly.

### 6.3 Authentication

No SDK supplies the server side: the official Rust SDK's auth is
client-side only
([OAUTH_SUPPORT.md](https://github.com/modelcontextprotocol/rust-sdk/blob/main/docs/OAUTH_SUPPORT.md))
and its server examples check tokens themselves
([examples](https://github.com/modelcontextprotocol/rust-sdk/tree/main/examples/servers/src)),
so dropping it (§6.1) removes nothing here. The resource-server layer is
Rueca's (RFC 0001 §13), per the
[2026-07-28 authorization spec](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization).

**Protected Resource Metadata** ([RFC 9728](https://www.rfc-editor.org/rfc/rfc9728)),
unauthenticated, at `/.well-known/oauth-protected-resource/mcp` and
`/.well-known/oauth-protected-resource` (2025-11-25's fallback order,
SEP-985):

```json
{"resource": "https://rueca.acme.internal/mcp",
 "authorization_servers": ["https://login.acme.com/realms/eng"],
 "scopes_supported": ["rueca:read", "rueca:admin"],
 "bearer_methods_supported": ["header"],
 "resource_name": "Rueca (acme)"}
```

`authorization_servers` lists the configured issuers (the spec requires
at least one; a deployment with static tokens only serves no PRM and
omits `resource_metadata`). Challenges (`<prm>` = the PRM URL):

```http
HTTP/1.1 401 Unauthorized
WWW-Authenticate: Bearer resource_metadata="<prm>", scope="rueca:read"

HTTP/1.1 401 Unauthorized
WWW-Authenticate: Bearer error="invalid_token", error_description="token expired", resource_metadata="<prm>"

HTTP/1.1 403 Forbidden
WWW-Authenticate: Bearer error="insufficient_scope", scope="rueca:admin", resource_metadata="<prm>"
```

Tokens only in `Authorization: Bearer`; `access_token=` in the query
string is refused with `400 invalid_request` and never logged.

**JWT validation** with `jsonwebtoken` 12.0.0 (MIT, MSRV 1.88,
`default-features = false, features = ["aws_lc_rs"]`, the backend
`object_store` already pulls; 12.0 rejects unknown `crit` and fixes
`sub`/`iss`/`aud` type confusion,
[changelog](https://github.com/Keats/jsonwebtoken/blob/master/CHANGELOG.md)):

1. `alg` in the issuer's `algorithms` (never `none` or `HS*`); the
   unverified `iss` selects a configured issuer, else 401.
2. `kid` in that issuer's JWKS, fetched from its discovery document and
   cached (`Cache-Control`, ≤ 24 h *(default)*); an unknown `kid` refreshes
   it at most once per 60 s *(default)*. `jsonwebtoken` parses `JwkSet`
   but fetches nothing; the fetcher is Rueca's (`reqwest`, already pulled
   by `object_store`).
3. Signature; `exp`, `nbf` (60 s leeway *(default)*); `iss` exact; `aud`
   contains `audience` (RFC 8707 binding, MUST).
4. `scope` / `scp` holds `rueca:read` for `/mcp`, `rueca:admin` for
   `/admin/*`, else 403; principal `oidc:<iss>#<sub>` with the groups of
   `groups_claim`.

**Static API tokens** for CI, scripts and deployments without an
identity provider: random, prefixed `rueca_` (a JWT has two dots, so the
two never collide), stored as `sha256:<hex>`, compared in constant time
against every configured hash; principal `token:<name>` with the entry's
scopes and repositories. **Code-host tokens are never accepted** and no
token is forwarded ("MCP servers MUST NOT accept or transit any other
tokens").

**The seam** for Org mode's host-federated login:

```rust
pub trait Authenticator: Send + Sync + 'static {
    fn authenticate<'a>(&'a self, parts: &'a http::request::Parts)
        -> BoxFuture<'a, Result<Principal, Challenge>>;
}
pub struct Principal { pub id: String, pub scopes: BTreeSet<String>, pub groups: Vec<String> }
pub enum Challenge { Missing, Invalid(String), InsufficientScope(String) }
```

The decision fixed `fn authenticate(&self, parts: &Parts) ->
Result<Principal, Challenge>`; it returns a future because step 2 may
fetch a JWKS (§13, question 13). The chain is static tokens, then OIDC.
`Principal.id` is `token:<name>` or `oidc:<iss>#<sub>`.
**Recorded:** external OIDC only, no embedded authorization server
(answers RFC 0001 §19 open question 7).

### 6.4 Principal → visibility

The engine already filters by `Visibility::Repos(set)` inside the
organization index before top-k, counts and BM25 statistics
(`crates/rueca-query/src/lib.rs:346-361`, `org_entries` from 590), so RFC
0006 §5's contract holds: results, totals, truncation markers and link
edges never reveal an unreadable repository. Service mode supplies the
set:

- a static token's `repos` globs; for an OIDC principal, the union of the
  `repos` of every rule whose `group` it has (or whose `claim` / `value`
  matches, for issuers without groups);
- globs matched against the current generation's repository ids, cached
  per (rule set, generation);
- **deny by default**: no matching rule → `Visibility::Repos(∅)`; empty
  `list_repositories`, no results, no repository named in errors;
- `admins` (`token:<name>`, `group:<name>`) grants the `admin` role for
  §6.8 and no visibility;
- local `select` (`lib.rs:482-508`) applies `Visibility` too, with
  `Visibility::All` its default, so local users see no change;
- Org mode swaps the rule source for mirrored grants (RFC 0001 §13); the
  type stays.

### 6.5 Deadlines and rate limits

- **Deadline.** Today the only bounds are `budget_tokens` (default
  2,000), `search_code`'s `scan_budget_bytes` (256 MiB,
  `crates/rueca-segment/src/lib.rs:493-505`) and `limit`s.
  `Engine::respond` gains a `Deadline` (default 10 s) checked in the
  search collectors, `trace`'s traversal and `search_code`'s scan; on
  expiry the tool returns an error that names the deadline and suggests a
  narrower `repo` or `limit`. It must be cooperative: `spawn_blocking`
  work cannot be cancelled from outside. `tower-http`'s timeout (12 s *(default)*) is
  the outer bound.
- **Rate limits.** `tower_governor` 0.8.0 (MIT OR Apache-2.0,
  [docs](https://docs.rs/tower_governor/latest/tower_governor/)) on
  `governor` 0.10.4: a `KeyExtractor` keyed by principal after
  authentication (10/s, burst 50 *(default, not measured)*) and a peer-IP
  limiter before it, against token guessing (`SmartIpKeyExtractor` behind
  a trusted proxy). Per process; a global limit belongs to the gateway.
  Over the limit: `429` with `Retry-After`.
- Layer order: trace → body limit → IP limiter → auth → principal limiter
  → timeout → the MCP handler.

### 6.6 Observability

- **Logs**: `tracing` JSON on stdout; fields named `token`, `secret`,
  `authorization`, `password` redacted.
- **Traces**: `opentelemetry` 0.33.1, `opentelemetry-otlp`,
  `tracing-opentelemetry` 0.34.0 over OTLP/HTTP. OpenTelemetry Rust's
  traces API, SDK and OTLP exporter are **Beta**; logs and metrics are
  Stable ([status](https://github.com/open-telemetry/opentelemetry-rust#project-status)).
  `traceparent` from the request's `_meta` (SEP-414) parents the tool
  span.
- **Metrics**: `metrics` 0.24.6 + `metrics-exporter-prometheus` 0.18.3
  (`default-features = false, features = ["http-listener"]`; the
  defaults pull a push gateway), or OTLP; `/metrics` on `ops_listen`. No
  repository names in labels (cardinality; leaks through a shared
  Prometheus).

| Metric | Type | Labels |
|---|---|---|
| `rueca_mcp_requests_total` | counter | `tool`, `outcome` (`ok`, `error`, `deadline`, `denied`) |
| `rueca_mcp_request_seconds` | histogram (p50, p95, p99) | `tool`, `scope` (`repo`, `repos`, `org`) |
| `rueca_auth_failures_total` | counter | `reason` (`missing`, `invalid`, `scope`, `issuer`) |
| `rueca_rate_limited_total` | counter | `stage` (`ip`, `principal`) |
| `rueca_jobs` | gauge | `kind`, `state` |
| `rueca_queue_depth` | gauge | `kind`, `priority` |
| `rueca_job_seconds` | histogram | `kind`, `result` |
| `rueca_worker_exits_total` | counter | `reason` (`ok`, `error`, `timeout`, `cpu`, `memory`, `signal`) |
| `rueca_webhook_deliveries_total` | counter | `host`, `result` (`accepted`, `duplicate`, `bad_signature`, `ignored`, `malformed`) |
| `rueca_publish_seconds` | histogram | `stage` (`fetch`, `export`, `index`, `upload`, `catalog_cas`, `updater_wait`, `generation`, `replica_refresh`) |
| `rueca_push_to_queryable_seconds` | histogram | `host` |
| `rueca_cas_conflicts_total` | counter | `object` (`catalog`, `job`, `pointer`, `lease`) |
| `rueca_generation`, `rueca_generation_lag_seconds` | gauge | — |
| `rueca_gc_deleted_bytes_total` | counter | `class` |
| `rueca_cache_requests_total` | counter | `cache` (`segment`, `pack`, `org_index`, `facts`, `vectors`), `result` |
| `rueca_cache_bytes` | gauge | `cache` |
| `rueca_segment_open_seconds` | histogram | — |
| `rueca_store_requests_total` | counter | `op` (`get`, `get_range`, `put`, `list`, `delete`), `result` |

### 6.7 Health and readiness

Liveness decides "when to restart a container", readiness "when a
container is ready to accept traffic"
([Kubernetes](https://kubernetes.io/docs/concepts/configuration/liveness-readiness-startup-probes/)).

| Endpoint | 200 when | Body |
|---|---|---|
| `GET /healthz` | HTTP served, runtime responsive | `{"ok": true}` |
| `GET /readyz` (query, all) | `org.json` read, the organization index downloaded and opened | `{"generation": 412, "warm": "37/50"}` |
| `GET /readyz` (indexer) | `store.json` readable, `git` ≥ 2.45 found | `{"store": "ok"}` |

**Recorded:** ready when the organization index is open; segments open
lazily, `warm: n/m` counts downloaded default segments, and a query on a
cold segment downloads it within its deadline. `ready_when = "warm"`
waits for every default segment instead.

### 6.8 Operational endpoints

RFC 0001 §5's "HTTP API" shrinks to these; none is a query API.

| Method and path | Roles | Auth | Effect |
|---|---|---|---|
| `GET /.well-known/oauth-protected-resource[/mcp]` | query, all | none | PRM |
| `POST /mcp` | query, all | `rueca:read` | MCP |
| `POST /webhooks/github` · `/gitlab` · `/bitbucket` | indexer, all | host signature (§7.3) | bump desired state, `202` |
| `GET /status` | all | `rueca:read`, filtered by visibility | generation, repositories, refs, jobs by state, lag |
| `POST /admin/index` `{"repo", "ref"}` | indexer, all | `admin` | add or refresh a ref, `202` |
| `DELETE /admin/refs` `{"repo", "ref"}` | indexer, all | `admin` | remove a ref from the catalog, `202` |
| `GET /admin/jobs` | indexer, all | `admin` | job objects with state, attempts, last error |
| `GET /healthz` · `/readyz` · `/metrics` | all | none, `ops_listen` | §6.6–§6.7 |

### 6.9 One organization per deployment

**Recorded.** A second organization is a second deployment or a second
`[store].url` prefix. Multi-tenancy would put a tenant id into every
object path, visibility rule and metric, and make a cross-tenant leak
out of every bug; RFC 0006's organization is already the unit of ranking
and linking.

## 7. Indexing pipeline

### 7.1 Inventory

v1: the `[[repo]]` entries (`url`, `default_branch`, `branches`, `tags`,
`prs`), like `rueca-org.toml`'s `url` members. **Phase 3:**
`[org].inventory` lists every repository of a GitHub organization, GitLab
group or Bitbucket workspace on each reconcile (paginated), filtered by
`include` / `exclude` / `archived`; new repositories get a job, vanished
ones leave the catalog after the grace. Users, teams and grants stay Org
mode. A GitHub App installation gets 5,000 REST requests/h, +50/h per
repository or user above 20, capped at 12,500/h; secondary limits 100
concurrent and 900 points/min
([GitHub](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api));
listing 1,000 repositories at 100 per page is 10 requests *(derived)*.

### 7.2 Reconciler and desired state

Level-triggered, after Zoekt's indexserver, which "pulls, it is not
pushed to": one queue item per repository, a new desired state replaces
the old, never-indexed first, failed last, then FIFO, failures backing
off 10 min linearly to 120 min
([`queue.go`](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/queue.go),
[`main.go`](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/main.go)).
The reconciler runs in the updater. Four inputs write one thing, the
`desired` map of `jobs/<repo-dir>.json`: webhook hints (§7.3, applied by
the receiving indexer), adaptive polling (§7.4), inventory and
configuration (§7.1), admin requests (§6.8). Each tick (5 s *(default)*) it compares
desired commits with each ref's newest snapshot and sets `pending` with a
priority where they differ: **0** never indexed, **1** default branch
behind, **2** pull request head behind, **3** branch behind, **4** tag
missing, **last** `failed` after `not_before`.

### 7.3 Webhooks

Hints only: no host guarantees delivery, and payloads truncate. GitHub
"does not automatically redeliver failed deliveries"
([GitHub](https://docs.github.com/en/webhooks/using-webhooks/handling-failed-webhook-deliveries));
Stripe and Shopify advise the same pattern: verify, acknowledge before
complex logic, process asynchronously, dedupe by event id, and "use
reconciliation jobs" ([Stripe](https://docs.stripe.com/webhooks),
[Shopify](https://shopify.dev/docs/apps/build/webhooks/best-practices)).

| | GitHub | GitLab | Bitbucket Cloud |
|---|---|---|---|
| Events | `push`; `pull_request` `opened`, `synchronize` (`before`/`after`), `reopened`, `closed` (`merged`); `create`/`delete`; `installation_repositories` (phase 3) | Push, Tag push, Merge request | `repo:push`; `pullrequest:created`, `updated`, `fulfilled`, `rejected` |
| Signature | `X-Hub-Signature-256`: `sha256=` HMAC-SHA256 of the body | `webhook-signature`: Standard Webhooks HMAC-SHA256 over `id.timestamp.body` (19.0+); legacy `X-Gitlab-Token`, the plain secret | `X-Hub-Signature`: HMAC-SHA256 hex |
| Ack budget | 2XX within 10 s | 10 s | 10 s |
| Delivery id | `X-GitHub-Delivery`, same on redelivery | `Idempotency-Key` / `webhook-id`, same across retries | `X-Request-UUID` (+ `X-Hook-UUID`, `X-Attempt-Number`) |
| Retries | none automatic; manual or API redelivery within 3 days | hook auto-disabled after 4 consecutive failures (1 min to 24 h back-off), permanently after 40 (17.11) | up to two more on 5xx |
| Truncation | `push` ≤ 2,048 commits; no event when > 5,000 branches are pushed at once; no tag events beyond 3 tags; 25 MB cap | a push of > 3 branches or tags sends **no webhook** (`push_event_hooks_limit`); 25 MB | `repo:push` ≤ 5 commits, else `truncated` |
| Sources | [events](https://docs.github.com/en/webhooks/webhook-events-and-payloads), [practices](https://docs.github.com/en/webhooks/using-webhooks/best-practices-for-using-webhooks), [redeliver](https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/redelivering-webhooks) | [webhooks](https://docs.gitlab.com/user/project/integrations/webhooks/), [GitLab.com](https://docs.gitlab.com/user/gitlab_com/) | [manage](https://support.atlassian.com/bitbucket-cloud/docs/manage-webhooks/), [payloads](https://support.atlassian.com/bitbucket-cloud/docs/event-payloads/), [troubleshoot](https://support.atlassian.com/bitbucket-cloud/kb/troubleshoot-webhooks/) |

Whether `X-Request-UUID` survives Bitbucket's retries is *(unverified)*;
a duplicate costs one coalesced bump. **Handler**, never blocking on the
network:

1. body over 25 MB → `413`;
2. signature with `hmac` + `sha2` (`Mac::verify_slice`, constant time;
   GitLab's legacy token compared in constant time, flagged as weaker at
   startup) → `401` on failure;
3. delivery id in a bounded LRU (24 h, 100k ids *(default)*) → `200`
   `duplicate`; duplicates across replicas coalesce anyway;
4. repository and ref names parsed; unknown repository → `202` `ignored`;
5. `(repo, refs, event)` queued in process → `202`;
6. a background task runs `git ls-remote` for those refs (the host's
   state is the truth; the payload, Stripe's "thin event", only names
   it) and CAS-merges the commits into `desired`; close events set the
   catalog's `closed_at`. The indexer then tries to lease that job itself.

A crash between 5 and 6 is recovered by polling. Sourcegraph calls
webhooks "usually not necessary" given polling
([update frequency](https://sourcegraph.com/docs/admin/repo/update_frequency)):
here they cut latency and polling keeps correctness.

### 7.4 Adaptive polling

Per repository, `git ls-remote <url> HEAD 'refs/heads/*' 'refs/tags/*'`
(filtered by the globs) every half of the time since the repository last
changed, clamped to **[45 s, 8 h]**: Sourcegraph polls "never more
frequently than 45 seconds, and no less frequently than every 8 hours",
and a last commit 8 h ago gives a next sync in 4 h (same source). The
schedule lives in the job object (`poll`), so a new updater continues it.
Pull request heads (`refs/pull/*/head`, `refs/merge-requests/*/head`) are
polled only with `pr_heads = true`: on an active repository the list is
long and moves with every push to any pull request; webhooks are the
intended path.

### 7.5 Jobs

```json
{"format": 1, "kind": "index",
 "repo": "github.com/acme/billing", "url": "https://github.com/acme/billing.git",
 "desired": {"main":  {"commit": "9f1c2e7a…", "kind": "default", "seen_at": 1794929130},
             "pr/42": {"commit": "4be0d913…", "kind": "pr", "seen_at": 1794929101}},
 "state": "leased", "priority": 1,
 "lease": {"owner": "indexer-2/7f3a91", "until": 1794930030, "epoch": 3},
 "attempts": 0, "not_before": null, "last_error": null,
 "poll": {"next_at": 1794943530, "interval_s": 14400, "changed_at": 1794914730},
 "updated_at": 1794929131}
```

```
            desired ≠ catalog                 lease (CAS)              all desired refs indexed
  done ──────────────────────────▶ pending ───────────────▶ leased ──────────────────────────▶ done
                                    ▲  ▲                     │  │
                                    │  └─ desired moved ─────┘  │ error · crash · timeout · limit
                                    │                           ▼
                                    └──── not_before ──── failed (backoff)
  leased with lease.until < now − 30 s is taken as if pending (epoch + 1)
```

**Lease rules:**

1. **Discover**: every 5 s *(default)* each indexer LISTs `jobs/` (S3
   returns up to 1,000 keys per page, each with its e-tag,
   [ListObjectsV2](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html))
   and re-reads only changed jobs.
2. **Take** the lowest priority, then oldest `updated_at`, among
   `pending` with `not_before` ≤ now and `leased` whose `until` is 30 s
   past; `Update` to `leased`, owner, `until = now + 15 min` *(default)*, `epoch + 1`.
   `Precondition` → taken by another; try the next.
3. **Renew** every 5 min *(default)* by CAS on the same epoch. A failed renewal means
   the lease was taken: kill the child, discard the output, stop.
4. **Finish** by CAS: refs whose indexed commit equals `desired` are
   satisfied; all satisfied → `done` (`attempts = 0`), else `pending`.
5. **Fail**: `attempts + 1`, `failed`, `not_before = now + min(10 min ×
   attempts, 120 min)` (Zoekt), `last_error = {stage, reason, at}`. A ref
   that fails alone is recorded and skipped; the job fails only when
   every ref failed.
6. **Idempotent and content-addressed**: a duplicated job (taken lease,
   split brain, clock skew) costs compute only; the second publisher's
   catalog CAS finds the `(ref, commit)` entry and stops, and its segment
   is collected after the grace.

One job per repository at a time; its refs run **sequentially**, default
branch first, so the facts cache is warm for the rest (RFC 0006 §9.3).
`jobs = 1` per indexer: a job is already parallel (rayon,
`crates/rueca-index/src/lib.rs:264-267`; writer threads
`min(available_parallelism, 16)`, `writer.rs:188-191`). Zoekt pins
repositories to indexers by consistent hashing only because its shards
live on local disk ([Sourcegraph](https://sourcegraph.com/docs/admin/architecture));
here any indexer takes any job, preferring the last holder
(`lease.owner` survives `done`) to keep mirror and facts warm.

### 7.6 Git

A bare mirror per repository (`mirror_dir/<repo-dir>.git`, `git init
--bare`), fetched with:

```sh
GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= GIT_CONFIG_NOSYSTEM=1 \
git -c protocol.version=2 -c protocol.allow=never \
    -c protocol.https.allow=always -c protocol.ssh.allow=always \
    -C <mirror> fetch --quiet --depth=1 --no-tags --atomic --no-auto-maintenance \
    --no-write-fetch-head --filter=blob:limit=<max_bytes + 1> \
    <url> +<commit>:refs/rueca/commit/<commit>
```

- **Filter**: Zoekt's fetch, exact commits at depth 1 with
  `--filter=blob:limit=<MaxFileSize+1>`
  ([`git_cache.go`](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/git_cache.go));
  with `Budget.max_bytes` = 2 MiB, `blob:limit=2097153`. Larger blobs are
  absent and become coverage gaps (`skipped.size`), Zoekt's "NOT-INDEXED".
  Wanting a commit by id needs the server's `uploadpack.allowReachableSHA1InWant`
  (or `allowAnySHA1InWant`, which Zoekt requires of its own gitserver,
  same source); the commit is a tip `ls-remote` just listed, so it is
  reachable. Which of the three hosts accept a reachable commit want with
  a filter is *(unverified)*; a host that refuses gets the ref name
  fetched instead and the tip compared with the desired commit.
- **`GIT_NO_LAZY_FETCH=1`** on every read (Git 2.45,
  [notes](https://github.com/git/git/blob/master/Documentation/RelNotes/2.45.0.adoc)),
  set by Zoekt because "cat-file's size filter does not prevent fetching
  them" ([`index.go`](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/index.go)):
  a missing blob is an error, never a fetch. 2.49 adds only `git
  backfill` ([notes](https://github.com/git/git/blob/master/Documentation/RelNotes/2.49.0.adoc)).
- **Hosts**: Bitbucket Cloud has supported clone filters since 2024-08-08
  ([BCLOUD-19847](https://jira.atlassian.com/browse/BCLOUD-19847);
  `blob:limit` with commit wants *(unverified)*). GitHub calls shallow
  fetches "computationally more expensive compared to a full fetch"
  ([GitHub](https://github.blog/open-source/git/get-up-to-speed-with-partial-clone-and-shallow-clone/));
  Zoekt uses them regardless.
- **Credentials**: `-c http.extraHeader` from an environment variable,
  never on the command line or in the mirror's config; SSH through
  `GIT_SSH_COMMAND` with `IdentitiesOnly=yes` and a pinned `known_hosts`.
- **Expiry**: never repacked ("repacking partial clones retains
  unreachable objects in promisor packs"), deleted after 7 days (Zoekt's
  `SRC_GIT_REPO_CACHE_MAX_AGE`).
- **gix** has no partial clone
  ([crate-status](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md);
  [#2375](https://github.com/GitoxideLabs/gitoxide/pull/2375) open,
  [#3018](https://github.com/GitoxideLabs/gitoxide/pull/3018) draft) but
  recognises `extensions.partialClone`; libgit2 cannot open such
  repositories ([#5564](https://github.com/libgit2/libgit2/issues/5564)).
  `git` does the network, `gix` reads.
- Local mode keeps today's fetch (`refs::fetch`,
  `crates/rueca-index/src/refs.rs:237-245`; `git`,
  `crates/rueca-index/src/org.rs:183-196`: `GIT_TERMINAL_PROMPT=0`, empty
  `GIT_ASKPASS`, no filter, no `GIT_NO_LAZY_FETCH`).

### 7.7 Export and index

Each ref reuses the local pipeline:

1. **Export** with `refs::export(repo_dir, commit, dest)` (`refs.rs:449-468`;
   gix tree walk, blobs written in parallel) into the job's scratch
   directory. Two changes: a blob the filter left out is skipped and
   reported (today a missing object fails the export), and the export
   opens a bare mirror (`git::discover` on a bare repository is
   *(unverified)*; phase 2 tests it).
2. **Index** with `index_worktree` (`crates/rueca-index/src/lib.rs:243-377`)
   and `IndexOptions { root, store, repo, ref_name, commit, threads }`
   (`lib.rs:82-105`), pinned to the commit and ref name with `dirty =
   false`, as `index_ref` does (`refs.rs:579-599`). `store` is a per-job
   staging store whose `packs/` holds the footers of the repository's
   packs (the writer stores only new blobs) and whose `facts/` is the
   indexer's cache.
3. **Publish** the new segment and pack (§7.9).

Measured on this repository at `c82029013c6d` (release build, 1,599 files): cold 1,982 ms
(walk 29, extract 409, resolve 870, write 672); warm, 1,457 of 1,457
cache hits, 864 ms (27, 117, 100, 619). The segment write is ≈ 72% of a
warm re-index *(derived)*.

### 7.8 Failure isolation

Today extraction runs in process: each file under `catch_unwind`
(`lib.rs:525-544`), the resolver too (`lib.rs:324-325`), per-file
`Budget { max_bytes: 2 MiB, max_parse_time: 5 s, max_depth: 512 }`
enforced through tree-sitter progress callbacks and match limits
(`crates/rueca-extract/src/lib.rs:61-72`); no memory limit, no process
isolation. A stack overflow, an abort in C code, an allocation storm or
a loop that ignores its callback still kills the process.

**Recorded:** the indexer runs each job in child processes of the same
binary, `rueca index-worker`, one per ref of the job, so a ref that
crashes fails alone (§7.5, rule 5) while the facts cache, on disk, stays
shared:

- **stdin**: one JSON object. `budget` travels explicitly because
  `IndexOptions.budget` is `#[serde(skip)]` (`lib.rs:90-91`) and `Budget`
  is not `Serialize` (`crates/rueca-extract/src/lib.rs:60-66`):
  `{"version": 1, "options": {"root": "/work/7f3a/export", "store":
  "/work/7f3a/store", "repo": "github.com/acme/billing", "ref_name":
  "main", "commit": "9f1c2e7a…", "threads": null}, "budget":
  {"max_bytes": 2097152, "max_parse_time_ms": 5000, "max_depth": 512}}`.
- **stdout**: `{"ok": <IndexReport>}` (`lib.rs:123-146`, with `timings`)
  or `{"error": {"stage": "resolve", "message": "…"}}`; **stderr**: JSON
  log lines, forwarded under the job's span.
- **Limits**, `setrlimit` before `exec`: `RLIMIT_AS` (24 GiB
  *(default)*), `RLIMIT_CPU` (summed over threads), `RLIMIT_CORE = 0`; a
  wall-clock `SIGKILL` from the parent after 30 min, GitLab Zoekt's
  per-project timeout ([GitLab](https://docs.gitlab.com/integration/zoekt/)).
  `RLIMIT_AS` counts mapped files and thread stacks, so it is coarse; in
  a container the cgroup memory limit is the precise cap, and its OOM
  kill shows as `SIGKILL`.
- **Outcome**: exit 0 with `ok` → publish; `error`, non-zero exit,
  signal or timeout → the ref fails with its reason and the job backs
  off. The parent never dies with a job.

This is RFC 0001 §13's "isolated worker processes" at job granularity,
not per blob, which would spawn once per file (1,599 for this
repository). Inside the child, per-file budgets and `catch_unwind` still
turn ordinary failures into coverage gaps.

### 7.9 Publish

1. Upload the pack, then the segment, with `Create` (`put_multipart`
   above 8 MiB *(default)*; S3 checks `If-None-Match` on
   `CompleteMultipartUpload`).
2. CAS the repository catalog: the snapshot at the head of its ref, with
   `packs`; trim to retention.
3. CAS the job (§7.5, rule 4).
4. The updater's next tick (5 s) lists `jobs/`, re-reads the catalogs of
   jobs that became `done`, and runs today's `update_org_with(store,
   name, members, linker)` (`org/mod.rs:459-511`) with `PackageLinker`
   (`crates/rueca-index/src/link.rs:125-134`), which reads facts from the
   cache or re-extracts from packs (`link.rs:169-214`): the organization
   layer is rebuildable from the bucket alone. Profiles, link sets and
   tantivy files go up with `Create`, then §5.4's two CAS steps.
5. Replicas see the pointer within 5 s and download what changed.

Push → queryable for a one-commit push to a repository of this one's size:

| Stage | Estimate | Basis |
|---|---|---|
| Webhook ack, `ls-remote`, lease | 1–3 s | *(estimate: three round trips)* |
| Fetch, depth 1, filtered | 1–10 s | *(estimate)* |
| Export + index, warm facts | ≈ 1–2 s | 864 ms warm index (measured on this repository at `c82029013c6d`) + export *(estimate)* |
| Upload 14.2 MB + catalog CAS | 0.5–2 s | *(estimate: 10–30 MB/s)* |
| Updater wait | 0–5 s | tick |
| Generation build | ≤ 2× the repository's index time | RFC 0006 §8 floor |
| Replica tick + download + open | 0–5 s + 0.5–1.5 s + ~18 ms | tick, *(estimate)*, RFC 0007 §6.5 |
| **Total** | **≈ 10–30 s** | *(estimate)* |

### 7.10 Refs lifecycle

| Kind | Trigger | Ref name | Retention |
|---|---|---|---|
| Default branch | always; push, polling | `main` | 2; never expires |
| Branch | `branches` glob; push, `create`/`delete`, polling | `release/2.4` | 1; expires when unlisted |
| Tag | `tags` glob or `POST /admin/index` | `v2.4.1` | 1; never expires; re-indexed if moved |
| Pull request head | `opened`/`synchronize`/`reopened`; polling only with `pr_heads` | `pr/<n>` (`refs/pull/<n>/head`, then `refs/merge-requests/<n>/head`, RFC 0006 §9.3) | 1; closed + 7 days or 14 days unread |
| Commit or other ref | `POST /admin/index` | the ref or the 12-digit commit | 1; 14 days unread |

Every ref is a full snapshot, as in local mode. **Delta segments**, or a
segment write shared between refs (RFC 0006 §12 open question 5, RFC 0001
§7), are phase 4: the facts cache already removes extraction, the write
is the remaining 72% of a warm re-index, and Zoekt's delta builds need a
shard-count fallback its source calls a "HACK … interim compaction
strategy" ([`gitindex/index.go`](https://github.com/sourcegraph/zoekt/blob/main/gitindex/index.go)).
Glean measured stacked increments at ≈ 7% storage and < 10% typical query
overhead ([Glean](https://glean.software/blog/incremental/)), the
reference if phase 4 builds them.

### 7.11 Embeddings

RFC 0007 §5.6 publishes lexically first, then embeds. Here:

- after an `index` job publishes a snapshot without vectors for the
  organization's model key, the updater writes
  `jobs/<repo-dir>.embed.json` (same schema, `kind = "embed"`);
- only indexers with `gpu = true` or a `remote:openai-compatible/<url>`
  preset the deployment runs (TEI, llama-server) lease it; the job builds
  canonical texts from segment and packs (RFC 0007 §5.2), embeds the
  vector-cache misses, republishes the segment with `vectors.*` (new id)
  and sets the catalog entry's `vectors`;
- GPU defaults are RFC 0007 §8.3's (`qwen3-4b-q4`; reranker only past
  RFC 0007 §7.5's gate): a cold embed of 100k symbols with the 4B takes
  13–26 min on an RTX 4090 against 10.6–21 h on an M1 Pro GPU (RFC 0007
  §5.6, *(estimate)*);
- replicas load the embedder for queries only; the reranker runs on GPU
  replicas only;
- CPU-only deployments serve lexical search, the envelope's
  `retrieval.lexical` naming snapshots without vectors (RFC 0007 §8.2); a
  small CPU preset is opt-in;
- **Recorded:** `[retrieval].allow_remote = false` by default; code leaves
  the deployment only when the operator sets it with a `remote:` preset
  outside it (RFC 0007 §15 open question 8, for service mode).

### 7.12 Backpressure

Indexers lease only when free; the backlog is the pending job objects
(`rueca_queue_depth`, `/status`). The reconciler changes a repository's
desired state at most once per tick, so webhook bursts coalesce. `git
ls-remote` runs at most 8 per host at once *(default)*, and a host's `429`
or secondary-limit answer pauses its polling for `Retry-After`. The
updater publishes at most one generation per tick. Replicas download at
most 4 objects at once *(default)*.

## 8. Performance targets and measurement

### 8.1 Targets

| Metric | Today / reference | Floor | Target | How measured |
|---|---|---|---|---|
| Cold index of the organization suite (21 repositories, RFC 0006 §2; 50 members — 42 gated and 8 calibration — with bench PR #60's scenario) | sum of local `rueca index` times on the same hardware | service wall time ≤ 1.5× that sum | ≤ 1.2× | `bench/service`, 1, 2, 4 indexers, absolute times |
| Push → queryable, one commit, webhook delivered | goals only: Blackbird "within a few minutes", Sourcegraph "usually a few minutes" after a sync, Augment "within a few seconds" (IDE) | p50 ≤ 60 s, p95 ≤ 5 min | p50 ≤ 20 s | synthetic signed webhook; §7.9 stages apart |
| Lexical `search_symbols` p95, organization scope, warm replica, server side | RFC 0004 health floor (local) | ≤ 100 ms | ≤ 20 ms | `rueca_mcp_request_seconds` |
| Same, end to end over HTTP | — | reported | — | harness client |
| Hybrid `search_symbols` p95 | RFC 0007 §11.2 | ≤ 300 ms | ≤ 100 ms | phase 3 |
| Time to ready, fresh replica | ≈ 8 s for 50 × 14.2 MB at 100 MB/s *(estimate, §5.6)* | reported | ≤ 60 s, else phase 4 hotcache | `/readyz`, `ready_when = "warm"` |
| Organization layer after re-indexing one repository | RFC 0006 §8 | ≤ 2× the repository's index time | ≤ 1× | updater span |
| Cost per repository per month | — | reported | — | §8.4 |

No system publishes a measured push-to-searchable p95; the figures above
are goals ([Blackbird](https://github.blog/engineering/architecture-optimization/a-brief-history-of-code-search-at-github/),
[Sourcegraph](https://sourcegraph.com/docs/admin/repo/update_frequency),
[Augment](https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable)).

### 8.2 Harness

`bench/src/rueca_bench/service.py` (`--suite service`) with fixtures in
`bench/service/` (signed webhook templates for the three hosts, a compose
file for the nightly S3-compatible store):
it starts `rueca serve --role all` (scale rows: 1 query + N indexers) on
the in-memory or `file://` store, nightly on an S3-compatible one; serves
the organization suite's repositories from a local git HTTP server, so
fetch time excludes the internet; pushes commits and posts signed `POST
/webhooks/github` deliveries, polling an MCP query until each commit is
served; runs the suite's queries through an MCP client over HTTP,
recording server and client latency; and injects faults: a
looping-parser fixture (a test-only language pack flag), a worker killed
mid-job, a lease taken mid-job, duplicate and reordered webhooks, a
bucket answering 409 and 503.

### 8.3 Correctness floors

| Test | Floor |
|---|---|
| **Differential**: every bench query against the service and local mode on the same snapshots | byte-equal responses, modulo envelope fields naming the deployment |
| 2–8 concurrent indexers, in-memory store, injected CAS failures | no lost registration; no duplicate `(ref, commit)` publication |
| GC under concurrent publishes | never deletes a live object (property test over interleavings) |
| Lease taken mid-job | the catalog never regresses to an older commit |
| Duplicate webhooks | one bump per delivery id |
| PR expiry, injected clock | a closed `pr/<n>` leaves after `pr_after_close` |
| Deny by default | a principal without rules gets an empty `list_repositories` and no results |
| Local mode | existing tests and bench numbers unchanged |

### 8.4 Cost report

Per run, per repository and in total: bucket bytes by class (segments,
packs, facts, vectors, organization) and object counts; requests by
operation (GET, range GET, PUT, LIST, DELETE) and role; egress; indexer
CPU-minutes (worker `rusage`), GPU-minutes; git bytes fetched; host API
calls. The reader applies prices.

## 9. Security

### 9.1 Threats

| Threat | Control | Where |
|---|---|---|
| Hostile content crashes or exhausts the indexer | child process, `RLIMIT_AS` / `RLIMIT_CPU`, wall-clock kill; per-file budgets; fuzzed parsers; no adopter code | §7.8, RFC 0001 §13 |
| Network access during reads | `GIT_NO_LAZY_FETCH=1`; extractors have no network | §7.6 |
| Git configuration or hooks | bare mirror, no checkout; `GIT_CONFIG_NOSYSTEM=1`; only https and ssh protocols | §7.6 |
| SSRF through repository, issuer, JWKS or embedding URLs | §9.2's allowlist, checked after DNS resolution and on every redirect | §9.2 |
| Forged or flooded webhooks | HMAC mandatory, constant-time; body limit; dedupe; IP limiter; `202` before work | §7.3, §6.5 |
| Token theft, replay, passthrough | header only; audience bound; static tokens hashed; nothing forwarded; host tokens never accepted | §6.3 |
| DNS rebinding, cross-site requests | `allowed_hosts`, `Origin` (403) | §6.1–§6.2 |
| Cross-principal disclosure | visibility before top-k, counts and statistics; deny by default; errors name no hidden repository | §6.4, RFC 0006 §5 |
| Expensive queries | deadline, `budget_tokens`, scan budget, rate limits | §6.5 |
| Bucket tampering | per-role credentials; BLAKE3 on every component at open; immutable objects | §9.4, §5.6 |
| Secret leakage | `env:` references; redaction; never in the bucket or on a command line | §9.3 |
| Supply chain | `cargo deny`; pinned images; SBOM and provenance | §9.5 |

### 9.2 SSRF allowlist

`[indexing].hosts` lists the code hosts; a repository or inventory URL on
another host is a configuration error. Only `https` and `ssh`. OIDC
issuers, their `jwks_uri` hosts and `remote:` endpoints follow the same
rules. Addresses in loopback, link-local (169.254.0.0/16, fe80::/10),
private (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, fc00::/7) and
metadata ranges are refused unless `allow_private_networks = true` (an
on-premises GitLab, a self-hosted TEI), which is logged at startup.

### 9.3 Secrets

Read at startup from `env:` references (a missing variable is an error
naming the key, never the value); redacted in logs; never in metric
labels, the bucket or `git`'s command line. Code-host credentials exist
only in indexer roles. A webhook route without its secret refuses to
start.

### 9.4 Least-privilege credentials

| Prefix | query | indexer | indexer running the updater |
|---|---|---|---|
| `store.json` | read | read | read, write |
| `repos/*/catalog.json` | read | read, write | read, write |
| `repos/*/segments/*`, `repos/*/packs/*` | read | read, write | read, write, delete |
| `facts/*`, `vectors/*` | read | read, write | read, write, delete |
| `orgs/*` | read | read | read, write, delete |
| `jobs/*` | — | read, write | read, write, delete |
| `leases/*` | — | read | read, write |
| `reads/*` | create (§5.8) | — | read, delete |

`org_updater = false` keeps delete rights off all but one credential. S3
bucket policies can require conditional writes (§5.10); server-side
encryption on; no public access.

### 9.5 Supply chain

`cargo deny` stays (`deny.toml`: permissive licenses only; yanked
crates, unknown registries, git sources and wildcards denied), with an
`--all-features` job (RFC 0007 §4.3); every crate this RFC adds is MIT,
Apache-2.0 or both. Base images pinned by digest; SBOM and provenance
attestations from the container workflow; `cargo auditable` embeds the
dependency list in the binary (MIT OR Apache-2.0,
[crates.io](https://crates.io/crates/cargo-auditable)). `git` is a
separate GPLv2 process (RFC 0002 §7.1).

### 9.6 What never leaves the deployment

Source, segments, packs, facts and vectors stay in the deployment's
bucket and volumes. Rueca has no telemetry (RFC 0001 §13); OTLP and
Prometheus go where the operator points them and carry no content and no
repository names. Code reaches an embedding provider only with
`allow_remote = true` and a `remote:` preset (§7.11).

## 10. Phasing

| Phase | Delivers | Acceptance |
|---|---|---|
| 0. Storage abstraction, CAS catalog | `Storage` trait (local, `object_store`); per-repository catalogs; CAS pointer; local file lock; mark-and-sweep GC with grace; `packs` in segment meta; 64-bit id suffixes | Local-mode tests and bench numbers unchanged; the store round-trips through the in-memory `object_store`; two concurrent indexers never lose a registration (test); GC never deletes a referenced object (test); `rueca status` works against a bucket |
| 1. `rueca serve`, query path | MCP over HTTP, stateless; static tokens + OIDC; visibility rules; deadline; rate limits; health, readiness, metrics, traces, `/status`; configuration file; Dockerfiles and image workflow; disk cache | Three MCP clients (Claude Code, VS Code, Cursor) complete `search_symbols` → `get_symbol` → `trace` over HTTP; differential test service == local; unauthenticated → 401 with PRM; a principal without rules sees an empty organization; lexical p95 ≤ 100 ms on the organization suite; time to ready reported |
| 2. Indexer role | Jobs and leases; reconciler; mirrors and filtered fetch; webhooks for three hosts; adaptive polling; refs lifecycle and expiry; `rueca index-worker`; updater lease | Organization suite indexed from URLs by 1 and 4 indexers with equal results; push → queryable p95 ≤ 5 min; a looping-parser fixture fails only its job; duplicate webhooks coalesce (test); a taken lease never corrupts the catalog (test); a PR ref expires by policy (test) |
| 3. Embeddings and inventory | `embed` jobs on GPU indexers or a self-run endpoint; vector cache in the bucket; inventory discovery for the three hosts; cost report | Organization suite embedded on one GPU within RFC 0007 §5.6's estimate band; a CPU-only deployment serves lexical with the envelope's `lexical` list; inventory adds and removes repositories on reconcile (test); cost per repository reported |
| 4. Scale, on measured need | Hotcache / range reads; shared segment writes or delta segments; pack compaction; a Postgres catalog if CAS contention is measured; ANN threshold (RFC 0007 phase 3) | Each starts on its trigger: time to ready > 60 s; ref re-index cost; pack live fraction; `rueca_cas_conflicts_total`; scope above RFC 0007 §6.4's threshold |

## 11. Amendments

- **RFC 0001 §5** — the "HTTP API" becomes §6.8's operational endpoints;
  MCP is the only query API; the catalog box is object storage.
- **RFC 0001 §8.3** — the catalog is per-repository JSON objects written
  by CAS, with job objects and leases beside them; Postgres is an
  optional phase 4 backend (§10, §12). Local mode keeps `catalog.json` under a
  file lock.
- **RFC 0001 §8.4** — mark-and-sweep with a 24 h grace and per-kind
  retention (§5.8); service packs are per repository, deleted when
  unlisted.
- **RFC 0001 §13** — worker isolation is a child process per job with
  resource limits, not per blob (§7.8); tokens come from external OIDC
  issuers or static API tokens, with no embedded authorization server
  (§6.3); visibility comes from configured rules until Org mode mirrors
  grants (§6.4).
- **RFC 0001 §14** — one binary with roles and two images (§4).
  **Git transport, three corrections:**
  1. Zoekt does not use `--filter=blob:none` and does not fetch changed
     blob ids one by one; it fetches exact commits with `--depth=1
     --filter=blob:limit=<max file size + 1>`
     ([`git_cache.go`](https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/git_cache.go)).
  2. `GIT_NO_LAZY_FETCH` needs Git **2.45**; 2.49 only adds `git
     backfill` ([2.45](https://github.com/git/git/blob/master/Documentation/RelNotes/2.45.0.adoc),
     [2.49](https://github.com/git/git/blob/master/Documentation/RelNotes/2.49.0.adoc)).
  3. Bitbucket Cloud has supported clone filters since 2024-08-08
     ([BCLOUD-19847](https://jira.atlassian.com/browse/BCLOUD-19847)); its
     fallback is unnecessary.
- **RFC 0001 §19** — open question 1 (hotcache): phase 4, only if time to
  ready exceeds 60 s (§5.6). Open question 5 (polling cost at scale):
  adaptive `ls-remote` polling plus one-call inventory listing (§7.1,
  §7.4); permission polling stays Org mode. Open question 7: answered,
  external OIDC only (§6.3). Open question 8: answered, neither `apalis`
  nor a `SKIP LOCKED` table; job objects leased by CAS (§7.5).
- **RFC 0002 §7.1** — the fetch pattern and its Bitbucket sentence take
  the same three corrections.
- **RFC 0002 §7.3 and §9** — `rmcp` leaves the dependency table: the MCP
  layer is Rueca's own (§6.1, owner decision of 2026-11-17); the
  resource-server auth row stays Rueca's as it already was.
- **RFC 0006 §9.3** — "service mode will materialize refs from the object
  store": as built, refs are exported from a bare mirror on the indexer
  and indexed as `index_ref` does (§7.7); snapshot, ref naming and error
  contracts unchanged.
- **RFC 0006 §12** — open question 5: delta segments and shared writes are
  phase 4 (§7.10). Open question 6: answered by §5.8's retention. Open
  question 9: answered, no lock: CAS in service stores, a file lock in
  local ones (§5.5, §5.9).
- **RFC 0007 §8.3** — the service-mode line is realised by `embed` jobs on
  GPU indexers or a self-run `remote:openai-compatible` endpoint (§7.11).
  **§15 open question 8** — `allow_remote = false` is also the service
  default, set in `rueca-service.toml`.

## 12. Alternatives considered

- **Postgres catalog and jobs** (RFC 0001 §8.3; `FOR UPDATE SKIP LOCKED`
  "can be used to avoid lock contention with multiple consumers accessing
  a queue-like table", [PostgreSQL](https://www.postgresql.org/docs/current/sql-select.html)).
  Rejected for v1: a second stateful system to run, back up and upgrade,
  while CAS on per-repository objects covers an organization's write
  rate (one catalog write per indexed ref). Kept for phase 4 if
  `rueca_cas_conflicts_total` shows contention.
- **OpenDAL** 0.59.5: MSRV 1.91 > 1.90, no GCS update CAS
  ([crates.io](https://crates.io/crates/opendal)). Quickwit uses the AWS
  and Azure SDKs plus OpenDAL for GCS, not `object_store`
  ([Cargo.toml](https://github.com/quickwit-oss/quickwit/blob/main/quickwit/quickwit-storage/Cargo.toml)).
- **One global catalog object**: every indexer contends on it and retries
  grow with the indexer count; per-repository objects contend only within
  a repository, which one lease already serialises.
- **Global packs with cross-repository blob dedup**: Blackbird shards by
  blob id and dedups 115 TB to 28 TB of unique content
  ([Blackbird](https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/)),
  but needs global reference counting and a mutable index; Rueca's unit
  is the repository, and refs of a repository still share packs.
- **`rmcp`'s `transport-streamable-http-server`** (the first draft of
  §6.1): a maintained, spec-tracking server transport, but its dependency
  tree carries `chrono` (RUSTSEC-2026-0543) and the owner would not
  ignore the advisory; Rueca's MCP surface is four methods, small enough
  to own (§6.1).
- **Stateful sessions with a session store**: affinity or shared state
  for no benefit; tools are request/response, and 2026-07-28 removed
  sessions (SEP-2567).
- **Embedded authorization server** federating code-host login (RFC 0001
  §13): a security surface to own, while organizations wanting a shared
  service already run an identity provider; Org mode can revisit it
  behind `Authenticator`.
- **Multi-tenant deployment**: §6.9.
- **A worker process per blob**: one spawn per file; a per-job child gives
  the indexer the same containment, and per-file budgets keep one bad
  file from failing a job.
- **Consistent hashing of repositories to indexers** (Zoekt): needed only
  when indexes live on local disk (§7.5).
- **Kafka, Redis or a message queue** (Blackbird uses Kafka; Greptile
  Hatchet on Postgres and RabbitMQ,
  [Greptile](https://www.greptile.com/docs/self-hosting/system-architecture.md)):
  another system to operate; a job object per repository already
  coalesces and orders, and polling covers lost messages. `apalis` 1.2.0
  adds a backend and a license clarification (`apalis-postgres` reports a
  non-standard license, [apalis](https://github.com/apalis-dev/apalis)).
- **Reading through Gitaly or host APIs** (GitLab's Zoekt reads over
  Gitaly RPC, [GitLab](https://docs.gitlab.com/integration/zoekt/)):
  host-specific; git's protocol serves all three hosts.
- **Delta segments now** (§7.10) and **hotcache now** (§5.6): deferred
  until measured.

## 13. Open questions

All fourteen were decided by the owner on 2026-11-17 with the
recommendation given; the "revisit if" notes stay.

1. ~~**CAS-only catalog or Postgres?**~~ Decided (§5, §10): CAS only;
   revisit if `rueca_cas_conflicts_total` shows retries above a few
   percent of catalog writes on a real organization (phase 4).
2. ~~**S3-compatible store for CI**, MinIO's repository being archived
   (AGPL-3.0).~~ Decided (§8.2): in-memory in CI; nightly against a real
   bucket (S3, GCS or Azure, the targets of §5.10's table) rather than an
   emulator: Garage does not support conditional writes
   ([maintainer](https://news.ycombinator.com/item?id=46326984)) and
   LocalStack implements `If-Match`
   ([#11941](https://github.com/localstack/localstack/pull/11941)) but its
   `If-None-Match` is *(unverified)*; an emulator that passes the phase 0
   CAS tests may replace the bucket later.
3. ~~**PR expiry defaults** (closed + 7 days, 14 days unread).~~ Decided
   (§5.8): as stated; both are configuration keys.
4. ~~**Static tokens long-term, or OIDC only?**~~ Decided (§6.3): kept,
   for CI jobs and agents without an identity provider.
5. ~~**Per-repository packs** give up cross-repository dedup.~~ Decided
   (§5.1): per repository; phase 2 measures duplicated bytes on the
   organization suite and §12's global packs are the fallback.
6. ~~**Child process per job or per blob?**~~ Decided (§7.8): per job, one
   child per ref.
7. ~~**Operational endpoints beyond MCP.**~~ Decided (§6.8): that set only.
8. ~~**Hotcache timing.**~~ Decided (§5.6, §10): phase 4; starts early if
   the bench organization's time to ready exceeds 60 s.
9. ~~**`allow_remote` default.**~~ Decided (§7.11): `false`.
10. ~~**Inventory discovery here (phase 3) or in Org mode?**~~ Decided
    (§7.1): here, one list call per reconcile; permissions in Org mode.
11. ~~**JSON responses only, no streaming.**~~ Decided (§6.1): yes;
    revisit if a tool ever streams.
12. ~~**Read evidence for idle expiry** gives query replicas one
    create-only prefix, against "query = read-only" (§5.8).~~ Decided
    (§5.8): accepted; `pr_idle = "0"` restores strictly read-only
    replicas.
13. ~~**`Authenticator` returns a future**, since JWKS refresh is I/O
    (§6.3).~~ Decided (§6.3): accepted.
14. ~~**`git` version in the images** (Ubuntu 24.04 ships 2.43,
    §4.3).~~ Decided (§4.3): one `git` build stage for both images,
    asserted at build time.

## 14. Dogfood notes: where Rueca failed while writing this RFC

Code claims were located with Rueca's own tools on this repository at
`c82029013c6d` (`rueca query … --text`, 10–130 ms per call with the segment open).
Worked: `get_file_outline` (`org/mod.rs`, 1,658 lines, ~18 ms) replaced
most file reads; `trace` callers of `Store.register`, `update_org_with`
and `atomic_write` gave every writer path with call-site lines (§5.11's
list); `trace` callers of `Engine.with_visibility`, none outside tests,
is §2's "nothing sets it"; `search_code` grouped hits by enclosing symbol
or section; `search_symbols` with `labels: ["EnvVar"]` listed every
`RUECA_*` variable. Failures:

1. `search_code '{"query": …}'` is rejected (`unknown argument query …
   valid arguments: pattern, …`): its argument is `pattern`, unlike
   `search_symbols`.
2. `trace` callers of `RUECA_HOME` returned 0 nodes while `get_symbol`
   reported two readers: callers' default `edge_types` exclude `READS`;
   `"edge_types": ["READS"]` found `Store.default_dir_impl`.
3. `get_symbol "RUECA_LOG"` → no symbol: it is read through a third-party
   API (`EnvFilter::try_from_env("RUECA_LOG")`, `main.rs:191`); grep was
   needed.
4. `trace` gave `run_query → Engine.respond` and `index_worktree →
   ResolvedGraph.validate` as heuristic 0.5 edges (receiver typed through
   a `match` or a field of a call result).
5. **Absence cannot be shown**: `search_symbols "store lock concurrent
   writers"` returned the no-op tantivy `acquire_lock` and `Store`;
   `"per query deadline timeout"` returned the language packs'
   `LocalsQuery` and `run_query` ("query" is a homonym). "No lock" and "no
   deadline" needed grep.
6. `get_symbol IndexOptions` lists fields without attributes or derives:
   that `budget` is `#[serde(skip)]` and `Budget` is not `Serialize`,
   which shapes §7.8's worker protocol, came from reading the file.
7. `get_symbol rueca_segment::Store.register` names the file (`lib.rs
   L1-640`) as the method's parent rather than `impl Store`, and does not
   point at `register_impl` in `store.rs`, where the logic lives.
8. `search_symbols StreamableHttpService` returned unrelated fuzzy hits
   (`compose.rs` service) with no "no exact match" signal: dependency
   symbols are not indexed, and the tool does not say so.
9. `search_symbols "fetch"` ranked bench fixtures above product code; the
   transport code (`refs.rs:237`, `org.rs:183`) was reached by tracing
   `index_ref`.
10. Stale documentation at `c82029013c6d`: the `rueca-mcp` crate doc said
    rmcp 3.7 while `Cargo.lock` resolved 3.10.0; moot since #64 removed
    rmcp.

## 15. Sources

All consulted on 2026-11-17.

- **MCP and rmcp (the first draft's transport, §6.1):** https://rustsec.org/advisories/RUSTSEC-2026-0543 · https://modelcontextprotocol.io/specification/2026-07-28/basic/transports · https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http · https://modelcontextprotocol.io/specification/2026-07-28/changelog · https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization · https://modelcontextprotocol.io/specification/2025-11-25/basic/transports · https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization · https://modelcontextprotocol.io/specification/2025-11-25/changelog · https://modelcontextprotocol.io/specification/2025-06-18/changelog · https://crates.io/api/v1/crates/rmcp/versions · https://crates.io/api/v1/crates/rmcp/3.10.0/dependencies · https://docs.rs/rmcp/3.7.0/rmcp/transport/streamable_http_server/ · https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/CHANGELOG.md · https://github.com/modelcontextprotocol/rust-sdk/blob/main/docs/OAUTH_SUPPORT.md · https://github.com/modelcontextprotocol/rust-sdk/tree/main/examples/servers/src · https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/counter_streamhttp.rs · https://crates.io/crates/tower-http
- **Authorization and JWT:** https://www.rfc-editor.org/rfc/rfc9728 · https://www.rfc-editor.org/rfc/rfc8707 · https://crates.io/crates/jsonwebtoken · https://github.com/Keats/jsonwebtoken/blob/master/CHANGELOG.md · https://crates.io/crates/jwt-simple · https://crates.io/crates/openidconnect
- **Object storage:** https://crates.io/crates/object_store · https://github.com/apache/arrow-rs-object-store · https://docs.rs/object_store/0.14.2/object_store/enum.PutMode.html · https://docs.rs/object_store/0.14.2/object_store/aws/enum.S3ConditionalPut.html · https://github.com/apache/arrow-rs-object-store/blob/main/CHANGELOG.md · https://github.com/apache/arrow-rs-object-store/blob/main/CHANGELOG-old.md · https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html · https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html · https://aws.amazon.com/about-aws/whats-new/2024/08/amazon-s3-conditional-writes · https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes · https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-enforcement-conditional-write-operations-general-purpose-buckets/ · https://developers.cloudflare.com/r2/api/s3/extensions/ · https://github.com/minio/minio/issues/20346 · https://github.com/minio/minio/pull/19682 · https://crates.io/crates/opendal · https://github.com/apache/opendal · https://quickwit.io/docs/overview/architecture · https://github.com/quickwit-oss/quickwit/blob/main/quickwit/quickwit-storage/Cargo.toml · https://news.ycombinator.com/item?id=46326984 · https://github.com/localstack/localstack/pull/11941
- **Leases and queues:** https://www.morling.dev/blog/leader-election-with-s3-conditional-writes/ · https://quanttype.net/posts/2025-02-25-leader-election-with-s3-and-if-match.html · https://github.com/apalis-dev/apalis · https://crates.io/crates/apalis-postgres · https://www.postgresql.org/docs/current/sql-select.html
- **Observability and rate limits:** https://github.com/open-telemetry/opentelemetry-rust · https://github.com/open-telemetry/opentelemetry-rust/tree/main/opentelemetry-prometheus · https://crates.io/crates/tracing-opentelemetry · https://crates.io/crates/metrics-exporter-prometheus · https://kubernetes.io/docs/concepts/configuration/liveness-readiness-startup-probes/ · https://docs.rs/tower_governor/latest/tower_governor/ · https://crates.io/crates/governor
- **Containers:** https://github.com/axodotdev/cargo-dist · https://github.com/GoogleContainerTools/distroless · https://hub.docker.com/r/nvidia/cuda/tags · https://hub.docker.com/_/debian · https://crates.io/crates/llama-cpp-2 · https://github.com/ggml-org/llama.cpp/blob/master/.devops/cuda.Dockerfile · https://crates.io/crates/aws-lc-sys · https://packages.ubuntu.com/noble/git · https://crates.io/crates/cargo-auditable · https://doc.rust-lang.org/std/fs/struct.File.html
- **Zoekt and Sourcegraph:** https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/index.go · https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/git_cache.go · https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/queue.go · https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/main.go · https://github.com/sourcegraph/zoekt/blob/main/cmd/zoekt-sourcegraph-indexserver/sg.go · https://github.com/sourcegraph/zoekt/blob/main/index/shard_builder.go · https://github.com/sourcegraph/zoekt/blob/main/gitindex/index.go · https://github.com/sourcegraph/zoekt/blob/main/doc/design.md · https://github.com/sourcegraph/zoekt/blob/main/doc/indexing.md · https://github.com/sourcegraph/zoekt/blob/main/doc/faq.md · https://sourcegraph.com/docs/admin/architecture · https://sourcegraph.com/docs/admin/repo/update_frequency · https://sourcegraph.com/docs/admin/permissions/syncing · https://sourcegraph.com/docs/admin/config/webhooks/incoming · https://sourcegraph.com/docs/admin/search · https://sourcegraph.com/docs/self-hosted/deploy/instance-size · https://sourcegraph.com/docs/admin/deploy/resource_estimator · https://github.com/sourcegraph/docs/blob/main/docs/self-hosted/deploy/scale.mdx · https://github.com/sourcegraph/resource-estimator/blob/master/internal/scaling/references.go · https://sourcegraph.com/blog/tackling-the-long-tail-of-tiny-repos-with-shard-merging
- **Other systems:** https://github.blog/engineering/architecture-optimization/the-technology-behind-githubs-new-code-search/ · https://github.blog/engineering/architecture-optimization/a-brief-history-of-code-search-at-github/ · https://glean.software/docs/databases/ · https://glean.software/blog/incremental/ · https://engineering.fb.com/2024/12/19/developer-tools/glean-open-source-code-indexing/ · https://kythe.io/docs/kythe-overview.html · https://cursor.com/blog/secure-codebase-indexing · https://www.augmentcode.com/blog/a-real-time-index-for-your-codebase-secure-personal-scalable · https://www.greptile.com/docs/self-hosting/system-architecture.md · https://www.greptile.com/docs/self-hosting/deployment-options.md · https://github.com/DeusData/codebase-memory-mcp · https://docs.gitlab.com/integration/zoekt/
- **Webhooks:** https://docs.github.com/en/webhooks/using-webhooks/best-practices-for-using-webhooks · https://docs.github.com/en/webhooks/using-webhooks/handling-failed-webhook-deliveries · https://docs.github.com/en/webhooks/testing-and-troubleshooting-webhooks/redelivering-webhooks · https://docs.github.com/en/webhooks/webhook-events-and-payloads · https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api · https://docs.gitlab.com/user/project/integrations/webhooks/ · https://docs.gitlab.com/user/gitlab_com/ · https://support.atlassian.com/bitbucket-cloud/docs/manage-webhooks/ · https://support.atlassian.com/bitbucket-cloud/docs/event-payloads/ · https://support.atlassian.com/bitbucket-cloud/kb/troubleshoot-webhooks/ · https://docs.stripe.com/webhooks · https://shopify.dev/docs/apps/build/webhooks/best-practices
- **Git transport:** https://github.com/git/git/blob/master/Documentation/RelNotes/2.45.0.adoc · https://github.com/git/git/blob/master/Documentation/RelNotes/2.49.0.adoc · https://github.com/git/git/blob/master/Documentation/git.adoc · https://github.blog/open-source/git/get-up-to-speed-with-partial-clone-and-shallow-clone/ · https://jira.atlassian.com/browse/BCLOUD-19847 · https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md · https://github.com/GitoxideLabs/gitoxide/pull/2375 · https://github.com/GitoxideLabs/gitoxide/pull/3018 · https://github.com/libgit2/libgit2/issues/5564 · https://github.com/libgit2/libgit2/pull/7364
- **Rueca:** https://github.com/DevArKaDiA/rueca/pull/60 (bench organization suite)
