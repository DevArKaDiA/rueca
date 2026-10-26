"""`rueca-bench check`: regression check against the committed scorecard
(RFC 0004 §7.8).

A floor metric regresses when its value moves the wrong way by more than
the half-width of its committed 95% interval. Checked: every gated metric
with a floor, for Rueca, per language (localization, structure, micro,
parse-error rate) and the configuration metrics. Not checked: query
latency (machine-dependent: CI runners are not the machine that produced
the published numbers) and tokens (no floor). A metric present in the
baseline and missing now is a failure; a new metric is reported, not
failed. Exit status 1 on any regression.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from .common import REPO_ROOT, RESULTS_DIR

SKIP = {"latency_p95_ms", "tokens_per_answer", "search_tokens_to_first_hit", "search_tokens_per_hit", "trace_tokens_per_true_edge"}
SCORECARD = "scorecard.json"


def _load(path: str | None, default_head: bool) -> dict:
    if path:
        return json.loads(Path(path).read_text())
    if default_head:
        rel = (RESULTS_DIR / SCORECARD).relative_to(REPO_ROOT)
        out = subprocess.run(["git", "show", f"HEAD:{rel}"], cwd=REPO_ROOT, capture_output=True, text=True)
        if out.returncode != 0:
            raise SystemExit(f"rueca-bench check: no committed {rel} at HEAD ({out.stderr.strip()})")
        return json.loads(out.stdout)
    return json.loads((RESULTS_DIR / SCORECARD).read_text())


def _rows(card: dict):
    for lang, entry in (card.get("languages") or {}).items():
        for key, m in (entry.get("metrics") or {}).items():
            yield lang, key, m
    for key, m in ((card.get("config") or {}).get("metrics") or {}).items():
        yield "config", key, m


def compare(baseline: dict, current: dict) -> tuple[list[str], list[str]]:
    """(failures, notes)."""
    cur = {(lang, key): m for lang, key, m in _rows(current)}
    failures: list[str] = []
    notes: list[str] = []
    for lang, key, b in _rows(baseline):
        if key in SKIP or not b.get("gated", True) or b.get("floor") is None:
            continue
        bv = b.get("value")
        if bv is None:
            continue
        c = cur.get((lang, key))
        if c is None or c.get("value") is None:
            failures.append(f"{lang} {key}: missing (baseline {bv:.4f})")
            continue
        ci = b.get("ci") or [bv, bv]
        half = (ci[1] - ci[0]) / 2 if ci and None not in ci else 0.0
        drop = (c["value"] - bv) if b.get("better") == "lower" else (bv - c["value"])
        line = f"{lang} {key}: {bv:.4f} -> {c['value']:.4f} (half-width {half:.4f})"
        if drop > half + 1e-12:
            failures.append(line)
        elif abs(drop) > 1e-12:
            notes.append(line)
    for (lang, key), c in sorted(cur.items()):
        if not any(lang == bl and key == bk for bl, bk, _ in _rows(baseline)):
            notes.append(f"{lang} {key}: new metric ({c.get('value')})")
    return failures, notes


def main(baseline_path: str | None = None, current_path: str | None = None) -> int:
    baseline = _load(baseline_path, default_head=True)
    current = _load(current_path, default_head=False)
    failures, notes = compare(baseline, current)
    print(f"rueca-bench check: baseline {baseline.get('generated')} ({baseline.get('rueca')}), current {current.get('generated')} ({current.get('rueca')})")
    for n in notes:
        print(f"  moved   {n}")
    for f in failures:
        print(f"  REGRESSION {f}")
    if failures:
        print(f"{len(failures)} floor metric(s) dropped by more than their interval half-width")
        return 1
    print("no floor metric dropped by more than its interval half-width")
    return 0
