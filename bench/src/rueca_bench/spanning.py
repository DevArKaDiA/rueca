"""Spanning gold: organization questions whose answer spans repositories.

An S1 instance of a consumer repository (Laravel, Axum, Dubbo, Apollo) is
asked of the organization; its answer is the consumer's own gold (the
instance's gold files) plus the **library side**: the symbols of the
consumer's libraries that are organization members which the fix is about.
The library side is derived from two real artifacts and nothing else:

1. the cross-repository labeler (`crossrepo.LABELERS`) run on the
   instance's gold files with every repository at its organization pin (the
   base commit of its first instance of the split, in manifest order; the
   pin the organization scenario indexes): the library symbols each gold
   file imports by name and uses, or reaches through a library module or a
   path (`uses`);
2. the instance's real fix patch: a used symbol is kept when its name (the
   last segment of its qualified name) is an identifier of one of the
   patch's changed lines (`+` / `-`, file headers left out; identifier
   match, never a substring of a longer identifier).

Pairs are the `crossrepo.GROUPS` edges whose consumer and library are both
repositories of the split (consumer → libraries): the gated split has
tokio-rs/axum → tokio-rs/tokio, laravel/framework → briannesbitt/carbon,
Seldaek/monolog, thephpleague/commonmark, apache/dubbo and
apolloconfig/apollo → google/gson; the calibration split has none (no
calibration repository depends on another one).

Per instance (`spanning_gold`), cached in .cache/gold/org/spanning/<id>.json:
`{"id", "consumer", "libraries": [{"repo", "symbol", "path", "consumer_files"}],
...}` — `path` is the library file defining the symbol, `consumer_files`
the gold files using it. `spanning_instances(split)` lists the instances of
the split with a non-empty library side, in manifest order.
"""

from __future__ import annotations

import re
import sys
from collections.abc import Callable

from .common import GOLD_DIR, load_manifest, log, read_json, safe_name, split_of, write_json
from .corpus import add_worktree, remove_worktree
from .crossrepo import GROUPS, LABELERS, make_libs, symbol_name, symbol_path
from .datasets import instance_row
from .s1 import instances as s1_instances
from .s1 import s1_gold

# Bump when the labelers, the pairs or the derivation change: cached records
# of another version are recomputed.
SPANNING_VERSION = 1

_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_HEADER = re.compile(r"^(?:\+\+\+|---) (?:a/|b/|/dev/null)")


def org_pins(split: str = "gated") -> dict[str, str]:
    """Repository → its organization pin: the base commit of its first S1
    instance (manifest order), for the repositories of `split`."""
    man = load_manifest()
    members = {r["repo"] for r in man["s1"]["repos"] if split_of(r) == split}
    pins: dict[str, str] = {}
    for inst in man["s1"].get("instances", []):
        if inst["repo"] in members:
            pins.setdefault(inst["repo"], inst["base_commit"])
    return pins


def spanning_pairs(split: str = "gated") -> dict[str, dict]:
    """Consumer → {"language", "libraries": [{"repo", ["packages"]}]}: the
    `crossrepo.GROUPS` edges whose two ends are repositories of `split`."""
    pins = org_pins(split)
    out: dict[str, dict] = {}
    for g in GROUPS:
        if g["consumer"] not in pins:
            continue
        for lib in g["libraries"]:
            if lib["repo"] not in pins:
                continue
            cur = out.setdefault(g["consumer"], {"language": g["language"], "libraries": []})
            if lib["repo"] not in {x["repo"] for x in cur["libraries"]}:
                cur["libraries"].append({k: v for k, v in lib.items() if k != "commit"})
    return out


def changed_identifiers(patch: str) -> set[str]:
    """Identifiers of a unified diff's changed lines (`+` / `-`)."""
    out: set[str] = set()
    for line in patch.splitlines():
        if line[:1] in ("+", "-") and not _HEADER.match(line):
            out.update(_IDENT.findall(line[1:]))
    return out


def spanning_libraries(uses: set[tuple[str, str, str]], gold_files: list[str], patch: str, language: str, path_of: Callable[[str, str], str | None]) -> list[dict]:
    """The library side of one instance: the `uses` (consumer file, library,
    symbol) of its gold files whose symbol name is an identifier of the
    patch's changed lines; one entry per (library, symbol), sorted."""
    idents = changed_identifiers(patch)
    files = set(gold_files)
    keep: dict[tuple[str, str], dict] = {}
    for f, lib, qn in sorted(uses):
        if f in files and symbol_name(qn, language) in idents:
            entry = keep.setdefault((lib, qn), {"repo": lib, "symbol": qn, "path": path_of(lib, qn), "consumer_files": []})
            entry["consumer_files"].append(f)
    return [keep[k] for k in sorted(keep)]


def _cache_path(iid: str):
    return GOLD_DIR / "org" / "spanning" / f"{safe_name(iid)}.json"


def _fresh(iid: str) -> dict | None:
    p = _cache_path(iid)
    if p.exists():
        rec = read_json(p)
        if rec.get("version") == SPANNING_VERSION:
            return rec
    return None


def _compute(consumer: str, split: str) -> None:
    """Spanning gold of every instance of `consumer`: one checkout of the
    consumer and its member libraries at their organization pins."""
    pair = spanning_pairs(split)[consumer]
    pins = org_pins(split)
    lang = pair["language"]
    insts = [i for i in s1_instances() if i["repo"] == consumer]
    golds = {i["id"]: s1_gold(i)["files"] for i in insts}
    roots = {}
    try:
        log(f"spanning: {consumer} with {', '.join(x['repo'] for x in pair['libraries'])} at their organization pins")
        roots[consumer] = add_worktree(consumer, pins[consumer], f"span-{consumer}")
        for lib in pair["libraries"]:
            roots[lib["repo"]] = add_worktree(lib["repo"], pins[lib["repo"]], f"span-{lib['repo']}")
        libs = make_libs(lang, roots, pair["libraries"])
        lib_of = dict(libs)
        wanted = {f for files in golds.values() for f in files}
        present = {f for f in wanted if (roots[consumer] / f).is_file()}
        _, uses, _, _ = LABELERS[lang](roots[consumer], libs, only=present)
        for inst in insts:
            files = golds[inst["id"]]
            rec = {
                "id": inst["id"],
                "consumer": consumer,
                "language": lang,
                "split": split,
                "version": SPANNING_VERSION,
                "consumer_commit": pins[consumer],
                "library_commits": {x["repo"]: pins[x["repo"]] for x in pair["libraries"]},
                "consumer_files": files,
                "missing_at_pin": sorted(set(files) - present),
                "libraries": spanning_libraries(uses, [f for f in files if f in present], instance_row(inst)["patch"] or "", lang, lambda lname, qn: symbol_path(lib_of[lname], qn, lang)),
            }
            write_json(_cache_path(inst["id"]), rec)
    finally:
        for repo, root in roots.items():
            remove_worktree(repo, root)


def spanning_gold(inst: dict, split: str = "gated") -> dict:
    """The spanning gold of one S1 instance (empty `libraries` when its
    repository has no member library or nothing qualifies)."""
    rec = _fresh(inst["id"])
    if rec is not None:
        return rec
    if inst["repo"] not in spanning_pairs(split):
        return {"id": inst["id"], "consumer": inst["repo"], "libraries": []}
    _compute(inst["repo"], split)
    return read_json(_cache_path(inst["id"]))


def spanning_instances(split: str = "gated") -> list[dict]:
    """The S1 instances of `split` whose answer spans repositories (a
    non-empty library side), in manifest order, as `spanning_gold` records."""
    pairs = spanning_pairs(split)
    out = []
    for inst in s1_instances():
        if inst["repo"] in pairs:
            rec = spanning_gold(inst, split)
            if rec["libraries"]:
                out.append(rec)
    return out


if __name__ == "__main__":
    for s in sys.argv[1:] or ["gated", "calibration"]:
        found = spanning_instances(s)
        print(f"{s}: {len(found)} spanning instances")
        for r in found:
            print(f"  {r['id']}: {', '.join(x['repo'] + ' ' + x['symbol'] for x in r['libraries'])}")
