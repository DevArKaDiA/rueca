"""Minimal Okapi BM25 (k1=1.2, b=0.75), deterministic tie-breaking by doc key.

`BM25.restrict` keeps, for a fixed set of query terms, what a ranking needs
from one index (document count, total length, the postings of those terms
and the lengths of the documents they hold), so several indexes can be
ranked as one (`union_search`: the organization's BM25 ranking, statistics
over the union) after the indexes themselves are gone. A union of one part
ranks exactly like `BM25.search` (same formula, same float operations, same
tie order)."""

from __future__ import annotations

import math
from collections import Counter
from dataclasses import dataclass

import numpy as np


class BM25:
    def __init__(self, docs: list[tuple[str, list[str]]], k1: float = 1.2, b: float = 0.75):
        self.keys = [k for k, _ in docs]
        self.tfs = [Counter(toks) for _, toks in docs]
        self.lens = [len(toks) for _, toks in docs]
        self.avgdl = (sum(self.lens) / len(self.lens)) if self.lens else 0.0
        self.k1, self.b = k1, b
        df: Counter = Counter()
        for tf in self.tfs:
            df.update(tf.keys())
        n = len(docs)
        self.idf = {t: math.log(1 + (n - d + 0.5) / (d + 0.5)) for t, d in df.items()}
        self.postings: dict[str, list[int]] = {}
        for i, tf in enumerate(self.tfs):
            for t in tf:
                self.postings.setdefault(t, []).append(i)

    def search(self, query_tokens: list[str], limit: int = 100) -> list[tuple[str, float]]:
        qtf = Counter(t for t in query_tokens if t in self.idf)
        scores: dict[int, float] = {}
        for t, qn in qtf.items():
            idf = self.idf[t]
            for i in self.postings[t]:
                f = self.tfs[i][t]
                denom = f + self.k1 * (1 - self.b + self.b * self.lens[i] / (self.avgdl or 1))
                scores[i] = scores.get(i, 0.0) + qn * idf * f * (self.k1 + 1) / denom
        ranked = sorted(scores.items(), key=lambda kv: (-kv[1], self.keys[kv[0]]))
        return [(self.keys[i], s) for i, s in ranked[:limit]]

    def restrict(self, terms: set[str] | list[str]) -> TermStats:
        """The part of this index the given query terms can reach."""
        local: dict[int, int] = {}
        postings: dict[str, tuple[np.ndarray, np.ndarray]] = {}
        for t in sorted(set(terms)):
            docs = self.postings.get(t)
            if not docs:
                continue
            idx = np.fromiter((local.setdefault(i, len(local)) for i in docs), dtype=np.int64, count=len(docs))
            tf = np.fromiter((self.tfs[i][t] for i in docs), dtype=np.float64, count=len(docs))
            postings[t] = (idx, tf)
        order = sorted(local, key=local.__getitem__)
        return TermStats(
            n=len(self.keys),
            len_sum=sum(self.lens),
            keys=[self.keys[i] for i in order],
            lens=np.array([self.lens[i] for i in order], dtype=np.float64),
            postings=postings,
            k1=self.k1,
            b=self.b,
        )


@dataclass
class TermStats:
    """One index restricted to a set of query terms: `n` documents of total
    length `len_sum`; `keys` / `lens` of the documents holding at least one
    of the terms; per term, (local document indexes, term frequencies)."""

    n: int
    len_sum: int
    keys: list[str]
    lens: np.ndarray
    postings: dict[str, tuple[np.ndarray, np.ndarray]]
    k1: float = 1.2
    b: float = 0.75


def union_search(parts: list[tuple[str, TermStats]], query_tokens: list[str], limit: int = 100) -> list[tuple[str, str, float]]:
    """One BM25 ranking over the union of several restricted indexes (`parts`:
    (part name, stats)), with document count, average length and document
    frequencies of the union. Query terms outside the restricted set count as
    absent. Returns (part name, document key, score), ties broken by (part
    name, key). Every part must use the same k1 / b."""
    if not parts:
        return []
    k1, b = parts[0][1].k1, parts[0][1].b
    n = sum(p.n for _, p in parts)
    avgdl = (sum(p.len_sum for _, p in parts) / n) if n else 0.0
    df: Counter = Counter()
    for _, p in parts:
        for t, (idx, _) in p.postings.items():
            df[t] += len(idx)
    idf = {t: math.log(1 + (n - d + 0.5) / (d + 0.5)) for t, d in df.items()}
    qtf = Counter(t for t in query_tokens if t in idf)
    cands: list[tuple[float, str, str]] = []
    for name, p in parts:
        scores = np.zeros(len(p.keys), dtype=np.float64)
        for t, qn in qtf.items():
            post = p.postings.get(t)
            if post is None:
                continue
            idx, tf = post
            denom = tf + k1 * (1 - b + b * p.lens[idx] / (avgdl or 1))
            scores[idx] += qn * idf[t] * tf * (k1 + 1) / denom
        nz = np.flatnonzero(scores > 0)
        if len(nz) > limit:
            # every document scoring at least the limit-th best one (ties kept)
            thr = np.partition(scores[nz], len(nz) - limit)[len(nz) - limit]
            nz = nz[scores[nz] >= thr]
        cands.extend((float(scores[i]), name, p.keys[i]) for i in nz)
    cands.sort(key=lambda c: (-c[0], c[1], c[2]))
    return [(name, key, s) for s, name, key in cands[:limit]]
