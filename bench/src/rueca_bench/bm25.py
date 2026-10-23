"""Minimal Okapi BM25 (k1=1.2, b=0.75), deterministic tie-breaking by doc key."""

from __future__ import annotations

import math
from collections import Counter


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
