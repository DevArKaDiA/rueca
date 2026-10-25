"""Confidence intervals (RFC 0004 §7.6).

- Wilson score intervals for proportions (a 0/1 outcome per item:
  File Acc@5, Function Acc@10, definition found, files with parse errors);
- percentile bootstrap (fixed seed, `[scorecard] bootstrap_resamples`
  resamples, 10,000 by default) for macro averages (precision / recall over
  questions), pooled ratios (micro-benchmark F1 over callables) and paired
  differences (Rueca − BM25 per localization instance);
- a distribution-free interval for a quantile (latency p95) from binomial
  order statistics.

All intervals are 95%. Deterministic: same inputs, same seed, same numbers.
"""

from __future__ import annotations

import math
from typing import Callable, Sequence

import numpy as np

Z95 = 1.959963984540054
_CHUNK = 500


def wilson(successes: float, n: int, z: float = Z95) -> tuple[float, float] | None:
    """Wilson score interval of `successes / n` (None for n = 0)."""
    if n <= 0:
        return None
    p = successes / n
    den = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / den
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / den
    return max(0.0, centre - half), min(1.0, centre + half)


def _resample_indices(rng: np.random.Generator, n: int, resamples: int):
    done = 0
    while done < resamples:
        k = min(_CHUNK, resamples - done)
        yield rng.integers(0, n, size=(k, n))
        done += k


def bootstrap_mean(values: Sequence[float], resamples: int, seed: int) -> tuple[float, float] | None:
    """Percentile bootstrap interval of the mean of `values`."""
    x = np.asarray([v for v in values if v is not None], dtype=float)
    if x.size == 0:
        return None
    if x.size == 1 or np.all(x == x[0]):
        return float(x[0]), float(x[0])
    rng = np.random.default_rng(seed)
    means = np.concatenate([x[idx].mean(axis=1) for idx in _resample_indices(rng, x.size, resamples)])
    lo, hi = np.percentile(means, [2.5, 97.5])
    return float(lo), float(hi)


def bootstrap_ratio(num: Sequence[float], den: Sequence[float], resamples: int, seed: int) -> tuple[float, float] | None:
    """Percentile bootstrap interval of sum(num) / sum(den), resampling items
    (pooled precision or recall over callables)."""
    a = np.asarray(num, dtype=float)
    b = np.asarray(den, dtype=float)
    if a.size == 0 or b.sum() == 0:
        return None
    rng = np.random.default_rng(seed)
    out = []
    for idx in _resample_indices(rng, a.size, resamples):
        d = b[idx].sum(axis=1)
        out.append(np.where(d > 0, a[idx].sum(axis=1) / np.where(d > 0, d, 1), np.nan))
    r = np.concatenate(out)
    r = r[~np.isnan(r)]
    lo, hi = np.percentile(r, [2.5, 97.5])
    return float(lo), float(hi)


def bootstrap_pooled_f1(tp_pred: Sequence[float], n_pred: Sequence[float], tp_gold: Sequence[float], n_gold: Sequence[float], resamples: int, seed: int) -> tuple[float, float] | None:
    """Percentile bootstrap interval of the pooled F1 over items: per item the
    matched predictions, predictions, matched gold edges and gold edges."""
    cols = [np.asarray(c, dtype=float) for c in (tp_pred, n_pred, tp_gold, n_gold)]
    if cols[0].size == 0:
        return None
    rng = np.random.default_rng(seed)
    out = []
    for idx in _resample_indices(rng, cols[0].size, resamples):
        mp, npd, mg, ng = (c[idx].sum(axis=1) for c in cols)
        p = np.where(npd > 0, mp / np.where(npd > 0, npd, 1), 0.0)
        r = np.where(ng > 0, mg / np.where(ng > 0, ng, 1), 0.0)
        out.append(np.where(p + r > 0, 2 * p * r / np.where(p + r > 0, p + r, 1), 0.0))
    f = np.concatenate(out)
    lo, hi = np.percentile(f, [2.5, 97.5])
    return float(lo), float(hi)


def paired_bootstrap(a: Sequence[float], b: Sequence[float], resamples: int, seed: int) -> dict | None:
    """Paired bootstrap of mean(a − b) over items (RFC 0004 §6.1)."""
    x = np.asarray(a, dtype=float) - np.asarray(b, dtype=float)
    if x.size == 0:
        return None
    ci = bootstrap_mean(x.tolist(), resamples, seed)
    return {"diff": float(x.mean()), "ci": ci, "n": int(x.size)}


def quantile_ci(values: Sequence[float], q: float = 0.95, z: float = Z95) -> tuple[float, float, float] | None:
    """(estimate, low, high) of the q-quantile: order statistics whose ranks
    are the normal approximation of Binomial(n, q) at 95% (distribution-free;
    exact enough for the hundreds of queries per language)."""
    v = sorted(x for x in values if x is not None)
    n = len(v)
    if n == 0:
        return None
    est = v[max(0, min(n - 1, int(round(q * (n - 1)))))]
    half = z * math.sqrt(n * q * (1 - q))
    lo = v[max(0, min(n - 1, int(math.floor(n * q - half)) - 1))]
    hi = v[max(0, min(n - 1, int(math.ceil(n * q + half))))]
    return est, lo, hi


def seeded(base: int, *parts: str) -> int:
    """A per-metric seed derived from the base seed (stable across runs and
    platforms), so intervals do not depend on evaluation order."""
    import hashlib

    h = hashlib.sha256(("/".join(parts)).encode()).digest()
    return (base + int.from_bytes(h[:4], "big")) % (2**32)


def apply(fn: Callable, *args):
    try:
        return fn(*args)
    except (ValueError, ZeroDivisionError):
        return None
