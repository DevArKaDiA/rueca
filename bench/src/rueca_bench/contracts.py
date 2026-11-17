"""Cross-service edges (S2, RFC 0004 §4; RFC 0006 phase 4, `contracts.py`):
precision and recall of the organization's **contract edges** — HTTP routes,
OpenAPI operations and protobuf / gRPC services matched across
repositories — on public multi-repository setups pinned to commits.

**Groups** ([`GROUPS`]), each one organization (`rueca index --org`):

- `ollama` — ollama/ollama (Go gin server, `docs/openapi.yaml`) with its
  Python and JavaScript clients: *http-code*, *http-spec*;
- `qdrant` — qdrant/qdrant (Rust: actix-web routes, tonic gRPC services,
  `.proto`) with qdrant-client (Python: generated REST client, gRPC stubs):
  *http-code*, *rpc-spec-client*, *rpc-code*;
- `dapr` — dapr/dapr (`.proto`, Go gRPC runtime and app-callback client)
  with python-sdk and go-sdk (gRPC clients, app-callback servers):
  *rpc-spec-client*, *rpc-spec-server*, *rpc-code*;
- `weaviate` — weaviate/weaviate (Swagger 2 spec, `.proto`, Go gRPC
  server) with weaviate-python-client: *http-spec*, *rpc-spec-client*,
  *rpc-code*;
- `openai` (diagnostic, not gated) — openai/openai-openapi with
  openai-python: *http-spec* against 4–5 MB specs, above Rueca's 2 MiB
  per-file extraction budget.

**Gold** (`contracts_gold`), computed from the pinned checkouts by
labelers independent of Rueca: Python `ast` (HTTP client calls with a
literal or f-string path, gRPC stub calls, grpcio servicers), tree-sitter
for TypeScript client calls (one hop through a function that interpolates
its parameter into the URL), regular expressions for Go (gin / echo routes
and their handlers, gRPC clients and server methods), Rust (actix route
attributes, tonic `impl <Service> for <Type>`), `.proto` services and
OpenAPI documents (pyyaml / json). Keys are canonicalized like
`rueca_model::contract` (`contracts_gold.keys`); a consumer is paired with
the providers of best match quality (exact > template > method wildcard).
Only cross-repository pairs of a group's kinds are gold:

- *http-code*: (client file) → (server file, handler);
- *http-spec*: (client file) → (spec file, `METHOD /path` operation);
- *rpc-spec-client*: (stub-call file) → (`.proto` file, `Service.Method`);
- *rpc-spec-server*: (server method) → (`.proto` file, `Service.Method`), an
  IMPLEMENTS edge, matched on the implementing method too;
- *rpc-code*: (stub-call file) → (server file, implementing method).

Tests and generated stubs (`_pb2*.py`, `*.pb.go`, …) are out of scope for
both the gold and the scored predictions.

**Scoring** against `rueca links --org --json` (edges with `strategy ==
"contract"`): an edge is classified by its target (an `RpcMethod` / `.proto`
target is a spec rpc, a `Route` / spec file an operation, anything else a
handler or implementation, HTTP or RPC by the provider's contract kind from
`rueca contracts --side provided`), and is **correct** when it hits a gold
pair of that kind: same source repository and file (and implementing method
for *rpc-spec-server*), same target repository and file, and a target name
the pair accepts (handler / method name, `Service.Method`, the operation's
canonical key). Precision = correct / predicted, recall = found / gold,
pooled over the gated groups, per kind and overall, with percentile
bootstrap intervals over source files. Floors ≥ 90 / 75, targets ≥ 97 / 90,
like every edge metric. Dangling consumers come from `rueca contracts
--side dangling` (reported with the labelers' own count).

Raw outputs: .cache/runs/org/contracts-rueca.json (and contracts-gold.json
from `python -m rueca_bench.contracts gold`); results bench/results/s2.json.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import traceback
from collections import Counter
from collections.abc import Callable
from pathlib import Path

from .arms import make_arm
from .common import CACHE, RESULTS_DIR, log, read_json, run, scorecard_spec, write_json
from .contracts_gold import KINDS, GoldPair, build_gold
from .contracts_gold.keys import canonical_http
from .contracts_gold.model import in_scope, symbol_tokens
from .contracts_gold.specs import SPEC_EXTS
from .corpus import add_worktree, remove_worktree
from .org import run_path, write_org_file
from .stats import bootstrap_ratio, seeded

SUITE = "contracts"
FAMILY = "Cross-service edges"
FLOORS = {"precision": (0.90, 0.97), "recall": (0.75, 0.90)}  # (floor, target), RFC 0004 §6 edge metrics
AUDIT_SAMPLE = 15

GROUPS = [
    {
        "name": "ollama",
        "split": "gated",
        "kinds": ["http-code", "http-spec"],
        "repos": [
            {"repo": "ollama/ollama", "commit": "b8db8d5b5c8daedbbef7bb9ace2221a38bc384c2"},  # main 2026-11-16
            {"repo": "ollama/ollama-python", "commit": "35d9667e84ebbd9d792826868d050027f75c1cc1"},  # main 2026-10-16
            {"repo": "ollama/ollama-js", "commit": "c276d078eee37a0df003b9ee2120ec70327f32e9"},  # main 2026-10-16
        ],
    },
    {
        "name": "qdrant",
        "split": "gated",
        "kinds": ["http-code", "rpc-spec-client", "rpc-code"],
        "repos": [
            {"repo": "qdrant/qdrant", "commit": "23065a0bf6857c3e5da45f6c8939cc746cc0580e"},  # v1.20.2
            {"repo": "qdrant/qdrant-client", "commit": "1b5bb8589e5a8c124bcf80164736c7c92370c871"},  # master 2026-10-26
        ],
    },
    {
        "name": "dapr",
        "split": "gated",
        "kinds": ["rpc-spec-client", "rpc-spec-server", "rpc-code"],
        "repos": [
            {"repo": "dapr/dapr", "commit": "2707e0a96ccb8d89c672f6b47ddf27de5c7fceef"},  # master 2026-11-17
            {"repo": "dapr/python-sdk", "commit": "6bc75f12947f79a13d22baf08c64a44125033a53"},  # main 2026-11-16
            {"repo": "dapr/go-sdk", "commit": "012f725a57b7ca65df3c15261bee5d0512b14015"},  # main 2026-11-13
        ],
    },
    {
        "name": "weaviate",
        "split": "gated",
        "kinds": ["http-spec", "rpc-spec-client", "rpc-code"],
        "repos": [
            {"repo": "weaviate/weaviate", "commit": "a33c0ef138d41eef14247aba4fd70d63c007ee88"},  # main 2026-11-17
            {"repo": "weaviate/weaviate-python-client", "commit": "2ec127a3afcce49400df27d18dea3d04cea3e0d2"},  # main 2026-11-16
        ],
    },
    {
        "name": "openai",
        "split": "diagnostic",  # specs above the 2 MiB extraction budget
        "kinds": ["http-spec"],
        "repos": [
            {"repo": "openai/openai-openapi", "commit": "7a212a7e954ef487a2ac35cc4a969d1a50d5d635"},  # main 2026-11-17
            {"repo": "openai/openai-python", "commit": "311924f055110122991cc23b988a1836793551a1"},  # v3.52.0
        ],
    },
]

_ROUTE_QN = re.compile(r"paths\.(/.*)\.(get|put|post|delete|options|head|patch|trace)$")
_ROUTE_NAME = re.compile(r"^(GET|PUT|POST|DELETE|OPTIONS|HEAD|PATCH|TRACE) (/\S*)$")


# --- predicted edges ------------------------------------------------------------------------


def edge_tokens(e: dict) -> set[str]:
    """Names the target of a link edge may be accepted under: its bare name,
    `Parent.Name`, the canonical key of an OpenAPI operation, "" for a file."""
    qn = e.get("dst") or ""
    bare, two = symbol_tokens(qn)
    out = {bare, two}
    for text in (qn, e.get("dst_name") or ""):
        m = _ROUTE_QN.search(text) or _ROUTE_NAME.match(text)
        if m:
            path, method = (m.group(1), m.group(2)) if m.re is _ROUTE_QN else (m.group(2), m.group(1))
            out.add(canonical_http(method, path))
    if e.get("dst_label") == "File":
        out.add("")
    return out - {None}


def _is_spec_path(path: str | None) -> bool:
    return bool(path) and path.endswith(SPEC_EXTS)


def classify(e: dict, provider_kind: str | None = None, group_kinds: tuple[str, ...] = KINDS) -> str:
    """Pair kind of a contract link edge (see the module docs)."""
    implements = e.get("kind") == "IMPLEMENTS"
    label, path = e.get("dst_label"), e.get("dst_path") or ""
    if label == "RpcMethod" or path.endswith(".proto"):
        return "rpc-spec-server" if implements else "rpc-spec-client"
    if label == "Route" or _is_spec_path(path):
        return "http-spec-server" if implements else "http-spec"
    if implements:
        return "code-implements"
    if provider_kind in ("http", "rpc"):
        return f"{provider_kind}-code"
    code = [k for k in group_kinds if k.endswith("-code")]
    return code[0] if len(code) == 1 else "code-unknown"


def _hits(e: dict, src_repo: str, dst_repo: str, p: GoldPair, tokens: set[str]) -> bool:
    if (p.src_repo, p.src_path, p.dst_repo, p.dst_path) != (src_repo, e.get("src_path"), dst_repo, e.get("dst_path")):
        return False
    if p.src_accept and symbol_tokens(e.get("src"))[0] not in p.src_accept:
        return False
    return bool(tokens & set(p.dst_accept))


def _provider_kind(e: dict, provided: dict[tuple, str], names: dict[str, str]) -> str | None:
    repo = names.get(e.get("dst_repo") or "", e.get("dst_repo"))
    bare = symbol_tokens(e.get("dst"))[0]
    return provided.get((repo, e.get("dst_path"), bare)) or provided.get((repo, e.get("dst_path"), ""))


def _skip_reason(e: dict, kind: str, kinds: tuple[str, ...]) -> str | None:
    if not (in_scope(e.get("src_path")) and in_scope(e.get("dst_path"))):
        return "out_of_scope"
    return None if kind in kinds else "unscored"


def score_group(edges: list[dict], pairs: list[GoldPair], names: dict[str, str], kinds: tuple[str, ...], provided: dict[tuple, str] | None = None) -> dict:
    """Per kind {predicted, correct, gold, found}, per source-file items for
    the intervals, and the edges left out (out of scope, unscored kinds)."""
    by_kind: dict[str, list[GoldPair]] = {}
    for p in pairs:
        by_kind.setdefault(p.kind, []).append(p)
    items: dict[tuple, list[int]] = {}  # (kind, src repo, src path) -> [predicted, correct, gold, found]
    found: set[tuple] = set()
    skipped, unscored, wrong = Counter(), Counter(), []
    for e in (e for e in edges if e.get("strategy") == "contract"):
        kind = classify(e, _provider_kind(e, provided or {}, names), kinds)
        reason = _skip_reason(e, kind, kinds)
        if reason:
            skipped[reason] += 1
            unscored[kind] += reason == "unscored"
            continue
        src, dst = names.get(e.get("src_repo") or "", e.get("src_repo")), names.get(e.get("dst_repo") or "", e.get("dst_repo"))
        hit = [p for p in by_kind.get(kind, []) if _hits(e, src, dst, p, edge_tokens(e))]
        found.update(p.id for p in hit)
        it = items.setdefault((kind, src, e.get("src_path")), [0, 0, 0, 0])
        it[0] += 1
        it[1] += int(bool(hit))
        if not hit:
            wrong.append({"kind": kind, "src": [src, e.get("src_path"), e.get("src")], "dst": [dst, e.get("dst_path"), e.get("dst")], "confidence": e.get("confidence")})
    for p in pairs:
        it = items.setdefault((p.kind, p.src_repo, p.src_path), [0, 0, 0, 0])
        it[2] += 1
        it[3] += int(p.id in found)
    return _group_summary(items, pairs, found, skipped["out_of_scope"], +unscored, wrong)


def _group_summary(items: dict, pairs: list[GoldPair], found: set, out_of_scope: int, unscored: Counter, wrong: list) -> dict:
    kinds: dict[str, dict] = {}
    for (kind, _, _), (npred, ncorr, ngold, nfound) in items.items():
        k = kinds.setdefault(kind, {"predicted": 0, "correct": 0, "gold": 0, "found": 0})
        k["predicted"] += npred
        k["correct"] += ncorr
        k["gold"] += ngold
        k["found"] += nfound
    missed = [p.as_dict() for p in pairs if p.id not in found]
    return {
        "by_kind": dict(sorted(kinds.items())),
        "items": [[k[0], k[1], k[2], *v] for k, v in sorted(items.items(), key=lambda kv: tuple(str(x) for x in kv[0]))],
        "out_of_scope": out_of_scope,
        "unscored": dict(sorted(unscored.items())),
        "missed": missed[:40],
        "missed_count": len(missed),
        "wrong": wrong[:40],
        "wrong_count": len(wrong),
    }


def _ratio(num: int, den: int) -> float | None:
    return num / den if den else None


def _settings() -> tuple[int, int]:
    sc = scorecard_spec()
    return int(sc.get("bootstrap_resamples", 10000)), int(sc.get("bootstrap_seed", 20261025))


def pooled(groups: list[dict], kinds: tuple[str, ...] = KINDS) -> dict:
    """Per kind and `all`: counts, precision, recall and their bootstrap
    intervals over source files, pooled over the gated groups that ran."""
    resamples, seed = _settings()
    rows: dict[str, list[list[int]]] = {k: [] for k in (*kinds, "all")}
    for g in groups:
        if g.get("status") != "ok" or g.get("split") != "gated" or not g.get("score"):
            continue
        for kind, _, _, npred, ncorr, ngold, nfound in g["score"]["items"]:
            if kind in rows:
                rows[kind].append([npred, ncorr, ngold, nfound])
                rows["all"].append([npred, ncorr, ngold, nfound])
    return {k: _pooled_kind(k, v, resamples, seed) for k, v in rows.items() if v}


def _pooled_kind(kind: str, rows: list[list[int]], resamples: int, seed: int) -> dict:
    npred, ncorr, ngold, nfound = (sum(r[i] for r in rows) for i in range(4))
    ci_p = bootstrap_ratio([r[1] for r in rows], [r[0] for r in rows], resamples, seeded(seed, "s2", kind, "precision")) if npred else None
    ci_r = bootstrap_ratio([r[3] for r in rows], [r[2] for r in rows], resamples, seeded(seed, "s2", kind, "recall")) if ngold else None
    return {
        "predicted": npred,
        "correct": ncorr,
        "gold": ngold,
        "found": nfound,
        "files": len(rows),
        "precision": _ratio(ncorr, npred),
        "precision_ci": list(ci_p) if ci_p else None,
        "recall": _ratio(nfound, ngold),
        "recall_ci": list(ci_r) if ci_r else None,
    }


# --- gold ---------------------------------------------------------------------------------


def _roots(g: dict, prefix: str) -> dict[str, Path]:
    return {r["repo"]: add_worktree(r["repo"], r["commit"], f"{prefix}-{r['repo']}") for r in g["repos"]}


def _release(roots: dict[str, Path]) -> None:
    for repo, root in roots.items():
        remove_worktree(repo, root)


def gold_record(g: dict, roots: dict[str, Path]) -> dict:
    """The serialisable gold of a group (pairs as dicts)."""
    gold = build_gold(roots, tuple(g["kinds"]))
    return {
        "pairs": [p.as_dict() for p in gold["pairs"]],
        "counts": gold["counts"],
        "sites": gold["sites"],
        "dangling": gold["dangling"],
        "intra": gold["intra"],
        "other_kinds": gold["other_kinds"],
        "notes": gold["notes"],
        "files": {repo: sum(1 for p in root.rglob("*") if p.is_file() and ".git" not in p.parts) for repo, root in roots.items()},
    }


def pairs_of(gold: dict) -> list[GoldPair]:
    return [GoldPair.from_dict(d) for d in gold.get("pairs", [])]


def audit_sample(pairs: list[dict], n: int = AUDIT_SAMPLE) -> dict[str, list[dict]]:
    """A fixed sample of `n` gold pairs per kind (lowest sha256 of the pair),
    with their evidence, for a manual audit."""
    out: dict[str, list[dict]] = {}
    for p in pairs:
        out.setdefault(p["kind"], []).append(p)
    return {k: sorted(v, key=_pair_hash)[:n] for k, v in sorted(out.items())}


def _pair_hash(p: dict) -> str:
    return hashlib.sha256(json.dumps([p["src"], p["dst"]]).encode()).hexdigest()


def run_gold(only: list[str] | None = None) -> dict:
    """Label every group from its pinned checkouts (no Rueca); write
    .cache/runs/org/contracts-gold.json."""
    out = {"groups": []}
    for g in GROUPS:
        if only and g["name"] not in only:
            continue
        log(f"s2 gold: {g['name']}")
        roots = _roots(g, "s2g")
        try:
            out["groups"].append({**_meta(g), "gold": gold_record(g, roots)})
        finally:
            _release(roots)
    write_json(run_path("contracts", "gold"), out)
    return out


def _meta(g: dict) -> dict:
    return {"name": g["name"], "split": g["split"], "kinds": g["kinds"], "repos": g["repos"]}


# --- running ------------------------------------------------------------------------------


def run(arm_names: list[str], force: bool = False) -> None:
    """`--suite s2`: one organization per group, its contract edges scored
    against the group's gold."""
    if "rueca" not in arm_names:
        return
    arm = make_arm("rueca")
    if not arm.available()[0]:
        return
    p = run_path(SUITE, "rueca")
    if p.exists() and not force and read_json(p).get("status") == "ok":
        return
    groups = []
    for g in GROUPS:
        log(f"s2 rueca: {g['name']} ({', '.join(r['repo'] for r in g['repos'])})")
        try:
            groups.append(_group(arm, g))
        except Exception as exc:
            groups.append({**_meta(g), "status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"})
    write_json(p, {"status": "ok", "groups": groups, "arm_version": arm.version()})


def _json(res) -> dict | None:
    if res.rc != 0:
        return None
    try:
        return json.loads(res.stdout)
    except json.JSONDecodeError:
        return None


def _entries(doc: dict | None) -> list[dict] | None:
    """The entry list of a `rueca contracts --json` answer (RFC 0005
    envelope: the first list of objects among its top-level fields)."""
    if doc is None:
        return None
    for k in ("contracts", "entries", "results", "items"):
        if isinstance(doc.get(k), list):
            return doc[k]
    return next((v for v in doc.values() if isinstance(v, list)), [])


def contracts_list(arm, org_file: Path, store: Path, side: str) -> tuple[list[dict] | None, str | None]:
    base = [arm.bin, "contracts", "--org", str(org_file), "--store", str(store), "--json", "--side", side]
    res = run([*base, "--limit", "100000"], timeout=600)
    if res.rc != 0:
        res = run(base, timeout=600)
    return _entries(_json(res)), (None if res.rc == 0 else res.stderr[-300:])


def _provided(entries: list[dict] | None, names: dict[str, str]) -> dict[tuple, str]:
    out: dict[tuple, str] = {}
    for x in entries or []:
        repo = names.get(x.get("repo") or "", x.get("repo"))
        kind = str(x.get("kind") or "").lower()
        out[(repo, x.get("path"), symbol_tokens(x.get("symbol") or x.get("node"))[0])] = kind
    return out


def _group(arm, g: dict) -> dict:
    store = CACHE / "org" / "contracts-store"
    shutil.rmtree(store, ignore_errors=True)
    store.mkdir(parents=True, exist_ok=True)
    org_file = CACHE / "org" / "contracts-org.toml"
    roots: dict[str, Path] = {}
    try:
        roots = _roots(g, "s2")
        write_org_file(org_file, list(roots.values()), name=f"s2-{g['name']}")
        idx = run([arm.bin, "index", "--org", str(org_file), "--store", str(store), "--json"], timeout=arm.index_timeout)
        report = _json(idx)
        if report is None:
            return {**_meta(g), "status": "error", "error": idx.stderr[-500:]}
        ids = {Path(x["root"]).resolve(): x["segment"]["snapshot"]["repo"] for x in report["indexed"]}
        names = {ids[root.resolve()]: repo for repo, root in roots.items() if root.resolve() in ids}
        links = _json(run([arm.bin, "links", "--org", str(org_file), "--store", str(store), "--json"], timeout=600))
        edges = (links or {}).get("edges", [])
        dangling, dangling_err = contracts_list(arm, org_file, store, "dangling")
        provided, _ = contracts_list(arm, org_file, store, "provided")
        return {
            **_meta(g),
            "status": "ok",
            "index_seconds": idx.seconds,
            "org_ms": report.get("org_ms"),
            "link_ms": (report.get("update") or {}).get("link_ms"),
            "repo_ids": {repo: rid for rid, repo in names.items()},
            "edges_total": len(edges),
            "edges": [e for e in edges if e.get("strategy") == "contract"],
            "dangling": dangling,
            "dangling_error": dangling_err,
            "provided": sorted([list(k) + [v] for k, v in _provided(provided, names).items()], key=str),
            "gold": gold_record(g, roots),
        }
    finally:
        _release(roots)
        shutil.rmtree(store, ignore_errors=True)


# --- scoring / results ----------------------------------------------------------------------


def score_record(rec: dict) -> dict:
    """Score a raw run record (re-scorable when the matcher changes)."""
    groups = []
    for g in rec.get("groups", []):
        if g.get("status") != "ok":
            groups.append({k: g.get(k) for k in ("name", "split", "kinds", "repos", "status", "error")})
            groups[-1]["error"] = str(g.get("error"))[:300]
            continue
        names = {rid: repo for repo, rid in (g.get("repo_ids") or {}).items()}
        provided = {tuple(x[:3]): x[3] for x in g.get("provided") or []}
        sc = score_group(g["edges"], pairs_of(g["gold"]), names, tuple(g["kinds"]), provided)
        groups.append({**_group_view(g), "score": sc})
    gated_ok = [g for g in groups if g.get("status") == "ok" and g.get("split") == "gated"]
    return {
        "status": "ok",
        "version": rec.get("arm_version"),
        "groups": groups,
        "summary": pooled(groups),
        "complete": len(gated_ok) == sum(1 for g in GROUPS if g["split"] == "gated"),
    }


def _group_view(g: dict) -> dict:
    gold = g.get("gold") or {}
    d = g.get("dangling")
    return {
        **_meta(g),
        "status": "ok",
        "org_ms": g.get("org_ms"),
        "link_ms": g.get("link_ms"),
        "index_seconds": g.get("index_seconds"),
        "edges_total": g.get("edges_total"),
        "contract_edges": len(g.get("edges") or []),
        "dangling": len(d) if isinstance(d, list) else None,
        "dangling_error": g.get("dangling_error"),
        "gold_counts": gold.get("counts"),
        "gold_dangling": len(gold.get("dangling") or []),
        "gold_intra": gold.get("intra"),
        "gold_other_kinds": gold.get("other_kinds"),
        "openapi": (gold.get("notes") or {}).get("openapi"),
    }


_LAST: dict = {}


def score() -> dict:
    """bench/results/s2.json content: the latest Rueca run scored, else the
    published results (carried), else the gold counts alone (pending)."""
    p = run_path(SUITE, "rueca")
    published = RESULTS_DIR / "s2.json"
    if p.exists() and read_json(p).get("status") == "ok":
        out = score_record(read_json(p))
    elif published.exists():
        out = dict(read_json(published), carried=True)
    else:
        out = _pending()
    _LAST.clear()
    _LAST.update(out)
    return out


def last_score() -> dict:
    return dict(_LAST) if _LAST else score()


def _pending() -> dict:
    gp = run_path(SUITE, "gold")
    gold = read_json(gp) if gp.exists() else {}
    groups = [{**_meta(g), "status": "pending", "gold_counts": (g.get("gold") or {}).get("counts")} for g in gold.get("groups", [])]
    return {"status": "pending", "groups": groups, "summary": {}}


# --- scorecard family ------------------------------------------------------------------------


def scorecard_block(s2: dict, judge: Callable) -> dict:
    """The scorecard's `contracts` family: per kind precision and recall
    (gated, floors ≥ 90 / 75, targets ≥ 97 / 90), all kinds pooled (shown,
    not gated). `judge` is the report's floor check (RFC 0004 §6.1)."""
    summary = s2.get("summary") or {}
    metrics: dict[str, dict] = {}
    failing: list[str] = []
    for kind in (*KINDS, "all"):
        row = summary.get(kind) or {}
        for metric, (floor, target) in FLOORS.items():
            key = f"{kind}_{metric}"
            spec = {"floor": floor, "better": "higher"}
            value, ci = row.get(metric), row.get(f"{metric}_ci")
            n = row.get("predicted" if metric == "precision" else "gold")
            gated = kind != "all"
            status = judge(spec, value, ci, n, None)
            metrics[key] = {"label": f"{kind} {metric}", "value": value, "ci": ci, "n": n, "floor": floor, "target": target, "better": "higher", "gated": gated, "status": status}
            if gated and status not in ("✅", "⚠"):
                failing.append(key)
    return {"label": FAMILY, "status": "stable" if not failing else "preview", "failing": failing, "metrics": metrics}


def scorecard_row(block: dict) -> str:
    """The family's row of the scorecard summary table."""
    failing = "; ".join(f"{block['metrics'][k]['label']} {_with_ci(block['metrics'][k]['value'], block['metrics'][k]['ci'])}" for k in block["failing"])
    return f"| {FAMILY.lower()} (S2) | `{block['status']}` | {failing or '—'} |"


# --- markdown ---------------------------------------------------------------------------------


def _pct(x: float | None) -> str:
    return "—" if x is None else f"{100 * x:.1f}"


def _with_ci(x: float | None, ci: list | None) -> str:
    if x is None:
        return "—"
    return _pct(x) + (f" ({_pct(ci[0])}–{_pct(ci[1])})" if ci else "")


def render(L: list[str], s2: dict, judge: Callable) -> None:
    """The report section (after *Cross-repository edges (packages)*)."""
    L.append("### Cross-service edges (S2, contracts)")
    L.append("")
    if not s2 or not s2.get("groups"):
        L.append("Not run (`rueca-bench run --suite s2`; gold alone: `python -m rueca_bench.contracts gold`).")
        L.append("")
        return
    L.append(
        "Public services pinned with their clients, each group one organization (`rueca index --org`); contract edges from `rueca links --org` "
        "(`strategy: contract`). Gold from independent labelers (`contracts.py`, `contracts_gold/`): HTTP client calls, gin / echo / actix routes, "
        "OpenAPI operations, `.proto` rpcs, gRPC stubs and servers (Go, Python, Rust), keys canonicalized like `rueca_model::contract`; "
        "cross-repository pairs only; tests and generated stubs out of scope. Precision / recall pooled over the gated groups, bootstrap intervals "
        "over source files." + (" Carried from the published results." if s2.get("carried") else "")
    )
    L.append("")
    _render_groups(L, s2)
    _render_kinds(L, s2, judge)


def _render_groups(L: list[str], s2: dict) -> None:
    L.append("| Group | Split | Repositories | Gold pairs | Contract edges | Dangling (Rueca / labelers) | Index s | Link ms |")
    L.append("|---|---|---|---|---:|---:|---:|---:|")
    for g in s2["groups"]:
        repos = ", ".join(f"{r['repo']} `{r['commit'][:10]}`" for r in g.get("repos") or [])
        gold = ", ".join(f"{k} {v}" for k, v in (g.get("gold_counts") or {}).items()) or "—"
        if g.get("status") != "ok":
            L.append(f"| {g.get('name')} | {g.get('split')} | {repos} | {gold} | {g.get('status')} {str(g.get('error') or '')[:80]} | | | |")
            continue
        dangling = "—" if g.get("dangling") is None else str(g["dangling"])
        idx = "—" if g.get("index_seconds") is None else f"{g['index_seconds']:.1f}"
        L.append(f"| {g['name']} | {g['split']} | {repos} | {gold} | {g.get('contract_edges', 0)} | {dangling} / {g.get('gold_dangling')} | {idx} | {g.get('link_ms') or '—'} |")
    L.append("")


def _render_kinds(L: list[str], s2: dict, judge: Callable) -> None:
    summary = s2.get("summary") or {}
    if not summary:
        L.append("Rueca: pending (no contract edges scored yet).")
        L.append("")
        return
    L.append("| Kind | Gold | Predicted | Precision (95% CI) | Recall (95% CI) | Floor P / R | Target P / R | Status P / R |")
    L.append("|---|---:|---:|---|---|---|---|---|")
    for kind, row in summary.items():
        st = [judge({"floor": FLOORS[m][0], "better": "higher"}, row.get(m), row.get(f"{m}_ci"), 1, None) for m in ("precision", "recall")]
        label = f"**{kind}**" if kind == "all" else kind
        L.append(
            f"| {label} | {row['gold']} | {row['predicted']} | {_with_ci(row.get('precision'), row.get('precision_ci'))} ({row['correct']}/{row['predicted']}) | "
            f"{_with_ci(row.get('recall'), row.get('recall_ci'))} ({row['found']}/{row['gold']}) | ≥ 90 / 75 | ≥ 97 / 90 | {st[0]} / {st[1]} |"
        )
    L.append("")


# --- command line ------------------------------------------------------------------------------


def _print_audit(gold: dict) -> None:
    for g in gold["groups"]:
        print(f"## {g['name']} ({g['split']}): {g['gold']['counts']}")
        for kind, pairs in audit_sample(g["gold"]["pairs"]).items():
            print(f"### {kind}")
            for p in pairs:
                ev = p["evidence"][0]
                print(f"- {p['src'][0]}:{p['src'][1]}:{ev['line']} [{ev['symbol']}] `{ev['raw']}`")
                print(f"    -> {p['dst'][0]}:{p['dst'][1]}:{ev['provider_line']} {p['dst'][2]} `{ev['provider_raw']}` ({p['quality']}, {p['key']})")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="python -m rueca_bench.contracts", description="S2 gold: label the pinned groups (no Rueca).")
    ap.add_argument("cmd", choices=["gold", "audit", "score"])
    ap.add_argument("--only", nargs="*", help="group names")
    a = ap.parse_args(argv)
    if a.cmd == "gold":
        gold = run_gold(a.only)
        for g in gold["groups"]:
            print(g["name"], g["gold"]["counts"], "files", g["gold"]["files"], "dangling", len(g["gold"]["dangling"]), "intra", g["gold"]["intra"], "other", g["gold"]["other_kinds"])
    elif a.cmd == "audit":
        _print_audit(read_json(run_path(SUITE, "gold")))
    else:
        print(json.dumps(score().get("summary"), indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
