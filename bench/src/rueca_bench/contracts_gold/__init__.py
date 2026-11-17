"""S2 gold (cross-service contract edges): independent labelers per
contract kind, run on the pinned checkouts of a group's repositories.

`build_gold(roots, kinds)` runs every labeler on every repository (each
only finds what its language and framework hold), joins consumers with
providers (`pairs.build_pairs`) and keeps the cross-repository pairs of the
group's kinds.
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path

from .golang import GoRepo, gin_routes, go_grpc_calls, go_grpc_impls, nethttp_routes
from .model import Site
from .pairs import KINDS, GoldPair, build_pairs
from .rust import actix_routes, tonic_impls
from .pyclient import python_http_calls, python_servicers, python_stub_calls
from .specs import ProtoSet, openapi_operations, proto_services
from .tsclient import ts_http_calls

__all__ = ["KINDS", "GoldPair", "Site", "build_gold", "label_sites"]


def label_sites(roots: dict[str, Path]) -> tuple[list[Site], dict]:
    """Every site of every repository; (sites, notes: spec documents,
    unresolved route handlers)."""
    protos = ProtoSet()
    for repo, root in roots.items():
        proto_services(repo, root, protos)
    services = protos.services
    sites: list[Site] = list(protos.sites)
    notes: dict = {"openapi": [], "unresolved_handlers": []}
    for repo, root in roots.items():
        specs, docs = openapi_operations(repo, root)
        sites += specs
        notes["openapi"] += docs
        sites += python_http_calls(repo, root)
        sites += ts_http_calls(repo, root)
        sites += actix_routes(repo, root)
        if services:
            sites += tonic_impls(repo, root, protos)
            sites += python_stub_calls(repo, root, services)
            sites += python_servicers(repo, root, services)
        go = GoRepo(repo, root)
        if not go.text:
            continue
        for labeler in (gin_routes, nethttp_routes):
            routes, unresolved = labeler(go)
            sites += routes
            notes["unresolved_handlers"] += unresolved
        if services:
            sites += go_grpc_calls(go, protos)
            sites += go_grpc_impls(go, protos)
    return sites, notes


def build_gold(roots: dict[str, Path], kinds: tuple[str, ...] = KINDS) -> dict:
    """{pairs, counts, sites, dangling, intra, other_kinds, notes} of a group."""
    sites, notes = label_sites(roots)
    res = build_pairs(sites, kinds)
    pairs: list[GoldPair] = res["pairs"]
    site_counts = Counter(f"{s.repo} {s.kind}/{s.role}" for s in sites)
    return {
        "pairs": pairs,
        "counts": dict(sorted(Counter(p.kind for p in pairs).items())),
        "sites": dict(sorted(site_counts.items())),
        "dangling": [{"repo": s.repo, "path": s.path, "line": s.line, "key": s.key, "symbol": s.symbol, "raw": s.raw} for s in res["dangling"]],
        "intra": dict(res["intra"]),
        "other_kinds": dict(res["other_kinds"]),
        "notes": notes,
    }
