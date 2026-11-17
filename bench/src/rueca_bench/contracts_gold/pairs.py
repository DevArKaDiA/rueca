"""Gold pairs: every consumer site joined with the providers its key
matches (best match quality only, like a router's static-before-parameter
precedence), across repositories."""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field

from .keys import QUALITY_RANK, match_key
from .model import CALL, HANDLER, IMPL, SPEC, Site

KINDS = ("http-code", "http-spec", "rpc-spec-client", "rpc-spec-server", "rpc-code")

# (consumer kind, consumer role, provider role) -> pair kind
_PAIR_KIND = {
    ("http", CALL, HANDLER): "http-code",
    ("http", CALL, SPEC): "http-spec",
    ("rpc", CALL, SPEC): "rpc-spec-client",
    ("rpc", CALL, IMPL): "rpc-code",
    ("rpc", IMPL, SPEC): "rpc-spec-server",
}


def pair_kind(src: Site, dst: Site) -> str | None:
    return _PAIR_KIND.get((src.kind, src.role, dst.role)) if src.kind == dst.kind else None


@dataclass
class GoldPair:
    """(source repo, file[, symbol]) -> (provider repo, file, symbol). The
    source is the call site's file (calls) or the implementing method
    (`rpc-spec-server`, an IMPLEMENTS edge)."""

    kind: str
    src_repo: str
    src_path: str
    src_symbol: str
    src_accept: tuple[str, ...]  # empty: any node of the file
    dst_repo: str
    dst_path: str
    dst_symbol: str
    dst_accept: tuple[str, ...]
    key: str  # the provider's key
    quality: str
    evidence: list[dict] = field(default_factory=list)

    @property
    def id(self) -> tuple:
        return (self.kind, self.src_repo, self.src_path, self.src_symbol if self.src_accept else "", self.dst_repo, self.dst_path, self.dst_symbol)

    def as_dict(self) -> dict:
        return {
            "kind": self.kind,
            "src": [self.src_repo, self.src_path, self.src_symbol],
            "src_accept": list(self.src_accept),
            "dst": [self.dst_repo, self.dst_path, self.dst_symbol],
            "dst_accept": list(self.dst_accept),
            "key": self.key,
            "quality": self.quality,
            "evidence": self.evidence[:5],
            "evidence_count": len(self.evidence),
        }

    @classmethod
    def from_dict(cls, d: dict) -> GoldPair:
        return cls(d["kind"], *d["src"][:2], d["src"][2], tuple(d["src_accept"]), *d["dst"][:2], d["dst"][2], tuple(d["dst_accept"]), d["key"], d["quality"], list(d.get("evidence") or []))


def _best(src: Site, providers: list[Site]) -> list[tuple[Site, str]]:
    """Providers whose key matches `src`'s, keeping per pair kind only the
    best match quality (a call may reach both a handler and a spec)."""
    hits = []
    for p in providers:
        kind = pair_kind(src, p)
        q = match_key(src.kind, p.key, src.key) if kind else None
        if q is not None:
            hits.append((p, q, kind))
    best: dict[str, int] = {}
    for _, q, kind in hits:
        best[kind] = max(best.get(kind, 0), QUALITY_RANK[q])
    return [(p, q) for p, q, kind in hits if QUALITY_RANK[q] == best[kind]]


def build_pairs(sites: list[Site], kinds: tuple[str, ...] = KINDS) -> dict:
    """{pairs: [GoldPair], dangling: [Site], intra: Counter(kind), other_kinds: Counter(kind)}."""
    sources = [s for s in sites if s.role in (CALL, IMPL)]
    providers = [s for s in sites if s.role in (HANDLER, SPEC, IMPL)]
    pairs: dict[tuple, GoldPair] = {}
    dangling: list[Site] = []
    intra: Counter = Counter()
    other: Counter = Counter()
    for src in sources:
        hits = _best(src, providers)
        if not hits and src.role == CALL:
            dangling.append(src)
        for dst, q in hits:
            kind = pair_kind(src, dst) or ""
            if dst.repo == src.repo:
                intra[kind] += 1
                continue
            if kind not in kinds:
                other[kind] += 1
                continue
            _add(pairs, kind, src, dst, q)
    return {"pairs": sorted(pairs.values(), key=lambda p: p.id), "dangling": dangling, "intra": intra, "other_kinds": other}


def _add(pairs: dict[tuple, GoldPair], kind: str, src: Site, dst: Site, quality: str) -> None:
    impl = src.role == IMPL
    gp = GoldPair(kind, src.repo, src.path, src.symbol, src.accept if impl else (), dst.repo, dst.path, dst.symbol, dst.accept, dst.key, quality)
    gp = pairs.setdefault(gp.id, gp)
    gp.evidence.append({"line": src.line, "symbol": src.symbol, "key": src.key, "raw": src.raw, "framework": src.framework, "provider_line": dst.line, "provider_raw": dst.raw})
