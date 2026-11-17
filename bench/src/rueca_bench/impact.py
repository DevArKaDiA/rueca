"""Impact instances: a library's issue asked of the organization, scored on
the consumers that use what the fix changes.

An S1 instance of a **library member** (a repository of the split that a
`crossrepo.GROUPS` consumer of the same split depends on: today tokio-rs/tokio
for tokio-rs/axum; briannesbitt/carbon, Seldaek/monolog and
thephpleague/commonmark for laravel/framework; google/gson for apache/dubbo
and apolloconfig/apollo) is asked of the organization with the same keyword
query as an issue instance. Its answer has two sides, both grounded in real
artifacts only:

1. **own side** — the instance's gold at the organization pin (the issue
   gold: the fix patch's files, its functions re-resolved at the pin;
   `org.gold_at_pin`), scored like an issue (File Acc@5, Function Acc@10);
2. **consumer side** — every (consumer repository, consumer file) whose
   *uses* (`crossrepo.LABELERS`, the independent cross-repository labeler,
   run over the whole consumer with the consumer and its member libraries
   at their organization pins) hit one of the **library gold symbols**:

   - for a gold file with function gold at the pin: every segment of each
     gold function's qualified name (`Streams.AppendableWriter.write` →
     `Streams`, `AppendableWriter`, `write`: the callable the patch touches
     and every class / impl type / trait / outer function containing it);
   - for a gold file without function gold at the pin: every symbol the
     labeler resolves to that file (its top-level symbols).

   A use (consumer file, library, qualified name `q`) hits when something
   it reaches under the edge scorer's acceptance rule — `q`, the class
   owning `q` (PHP, Java: "the symbol or a member of it"), or a library
   ancestor of `q` (base classes, PHP traits and interfaces, Rust traits
   it implements: `Carbon\\Carbon` reaches the trait `Carbon\\Traits\\Date`
   whose methods it carries) — is defined in a gold file
   (`crossrepo.symbol_path`) under a gold symbol's name
   (`crossrepo.symbol_name`), or under any name in a gold file without
   function gold. Consumer files are restricted to non-test source files
   (the organization view's candidate filter, passed in as `keep`).

Instances with an empty consumer side are dropped. Per library member the
instances are every S1 instance (manifest order) whose gold is at the pin
and whose fix is not already there (the issue rule without the per-member
cap). An instance that is also an issue instance keeps one query: both kinds
are scored from the same answer.

Caches: the labeler's uses of each consumer at the pins,
.cache/gold/org/impact/uses/<consumer>.json (`{version, consumer, language,
commits, uses: [{file, repo, symbol, candidates: [[name, path]]}]}`), and
one record per instance, .cache/gold/org/impact/<id>.json:
`{id, repo, language, kind: "impact", pin, base_commit, query, gold,
consumers: [{repo, path, symbols}], symbols: [{path, names}], version}`
(`names` null = every symbol of the file).
"""

from __future__ import annotations

import re
from collections.abc import Callable
from pathlib import Path

from .common import GOLD_DIR, log, read_json, safe_name, write_json
from .corpus import add_worktree, remove_worktree
from .crossrepo import LABELERS, make_libs, symbol_name, symbol_path
from .spanning import spanning_pairs

# Bump when the labelers, the pairs or the derivation change: cached uses of
# another version are recomputed.
IMPACT_VERSION = 1

_SEGMENTS = re.compile(r"::|\.|\\")


def library_consumers(split: str = "gated") -> dict[str, list[str]]:
    """Library member → its consumer members (`spanning.spanning_pairs`
    inverted), consumers in `crossrepo.GROUPS` order."""
    out: dict[str, list[str]] = {}
    for consumer, pair in spanning_pairs(split).items():
        for lib in pair["libraries"]:
            out.setdefault(lib["repo"], []).append(consumer)
    return out


def owner_candidates(qn: str, language: str) -> list[str]:
    """`qn` and, for PHP and Java, the classes owning it (`A\\B::m` → `A\\B`;
    `a.b.C.D.m` → `a.b.C.D`, `a.b.C`, …: package prefixes resolve to no
    class file and never match)."""
    out = [qn]
    if language == "php" and "::" in qn:
        out.append(qn.split("::", 1)[0])
    elif language == "java":
        parts = qn.split(".")
        out += [".".join(parts[:k]) for k in range(len(parts) - 1, 1, -1)]
    return out


def _uses_path(consumer: str) -> Path:
    return GOLD_DIR / "org" / "impact" / "uses" / f"{safe_name(consumer)}.json"


def _record_path(iid: str) -> Path:
    return GOLD_DIR / "org" / "impact" / f"{safe_name(iid)}.json"


def use_candidates(qn: str, ancestors: set[str], language: str) -> list[str]:
    """What a use of `qn` reaches, the edge scorer's accepted targets: `qn`,
    its owners (`owner_candidates`) and its library ancestors (base classes,
    PHP traits and interfaces, Rust traits it implements, as the labeler
    reads them)."""
    out = owner_candidates(qn, language)
    return out + sorted(a for a in ancestors if a not in out)


def label_uses(consumer_root: Path, libs: list[tuple], language: str) -> list[dict]:
    """The labeler's uses of a checked-out consumer, each with the library
    file of every symbol it reaches (`use_candidates`)."""
    lib_of = dict(libs)
    _, uses, ancestors, _ = LABELERS[language](consumer_root, libs)
    return [
        {
            "file": f,
            "repo": lname,
            "symbol": qn,
            "candidates": [[c, symbol_path(lib_of[lname], c, language)] for c in use_candidates(qn, set(ancestors.get((lname, qn), ())), language)],
        }
        for f, lname, qn in sorted(uses)
    ]


def consumer_uses(consumer: str, pair: dict, pins: dict[str, str]) -> list[dict]:
    """`label_uses` of `consumer` with its member libraries (`pair`, a
    `spanning_pairs` entry), every repository at its organization pin;
    cached per consumer (recomputed when a pin or `IMPACT_VERSION` moves)."""
    commits = {consumer: pins[consumer], **{x["repo"]: pins[x["repo"]] for x in pair["libraries"]}}
    p = _uses_path(consumer)
    if p.exists():
        rec = read_json(p)
        if rec.get("version") == IMPACT_VERSION and rec.get("commits") == commits:
            return rec["uses"]
    lang = pair["language"]
    roots: dict[str, Path] = {}
    try:
        log(f"impact: labelling {consumer} with {', '.join(x['repo'] for x in pair['libraries'])} at their organization pins")
        roots[consumer] = add_worktree(consumer, pins[consumer], f"impact-{consumer}")
        for x in pair["libraries"]:
            roots[x["repo"]] = add_worktree(x["repo"], pins[x["repo"]], f"impact-{x['repo']}")
        uses = label_uses(roots[consumer], make_libs(lang, roots, pair["libraries"]), lang)
    finally:
        for repo, root in roots.items():
            remove_worktree(repo, root)
    write_json(p, {"version": IMPACT_VERSION, "consumer": consumer, "language": lang, "commits": commits, "uses": uses})
    return uses


def gold_symbols(gold: dict) -> dict[str, list[str] | None]:
    """Gold file → names of its library gold symbols (every segment of each
    gold function's qualified name), or None (no function gold in that file:
    every symbol defined there)."""
    names: dict[str, set[str]] = {f: set() for f in gold.get("files") or []}
    for fn in gold.get("functions") or []:
        if fn.get("path") in names:
            names[fn["path"]].update(s for s in _SEGMENTS.split(fn.get("qualname") or fn.get("name") or "") if s)
    return {f: (sorted(n) if n else None) for f, n in names.items()}


def use_hits(candidates: list[list], symbols: dict[str, list[str] | None], language: str) -> bool:
    """A use hits the library gold symbols (module docstring)."""
    for name, path in candidates:
        if path is None or path not in symbols:
            continue
        allowed = symbols[path]
        if allowed is None or symbol_name(name, language) in allowed:
            return True
    return False


def impact_consumers(library: str, language: str, symbols: dict[str, list[str] | None], uses_by_consumer: dict[str, list[dict]], keep: Callable[[str], bool]) -> list[dict]:
    """[{repo, path, symbols}] of the consumer files whose uses of `library`
    hit `symbols`, consumers in the given order, files sorted."""
    out: list[dict] = []
    for consumer, uses in uses_by_consumer.items():
        by_file: dict[str, set[str]] = {}
        for u in uses:
            if u["repo"] == library and keep(u["file"]) and use_hits(u["candidates"], symbols, language):
                by_file.setdefault(u["file"], set()).add(u["symbol"])
        out += [{"repo": consumer, "path": f, "symbols": sorted(by_file[f])} for f in sorted(by_file)]
    return out


def impact_record(inst: dict, uses_by_consumer: dict[str, list[dict]], keep: Callable[[str], bool]) -> dict | None:
    """The impact record of one selected library instance (`inst`: an S1
    instance with `pin` and `gold` at the pin), or None when no consumer
    file is hit; written to .cache/gold/org/impact/<id>.json."""
    symbols = gold_symbols(inst["gold"])
    consumers = impact_consumers(inst["repo"], inst["language"], symbols, uses_by_consumer, keep)
    if not consumers:
        return None
    rec = {
        "id": inst["id"],
        "repo": inst["repo"],
        "language": inst["language"],
        "kind": "impact",
        "pin": inst["pin"],
        "base_commit": inst.get("base_commit"),
        "query": inst.get("query"),
        "gold": inst["gold"],
        "consumers": consumers,
        "symbols": [{"path": p, "names": n} for p, n in symbols.items()],
        "version": IMPACT_VERSION,
    }
    write_json(_record_path(inst["id"]), rec)
    return rec
