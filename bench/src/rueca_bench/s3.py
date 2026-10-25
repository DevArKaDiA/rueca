"""S3 — structural questions (callers / callees at depth 1, symbol lookup).

Gold sources (manifest `[[s3.repos]]` / `[[s3.micro]]`):
- `gold = "scip"`: SCIP (scip-python / scip-typescript) at pinned commits,
  call edges derived per RFC 0002 §4.2 (see scip.py);
- `gold = "phpstan"`: the bench's PHPStan collector (PHP has no viable SCIP
  indexer; see phpstan.py), same edge derivation;
  both with ~50 functions per repo stratified by fan-in / fan-out;
- micro-benchmarks: hand-written call graphs of small programs, every
  function of every snippet is a question — PyCG (Python, Apache-2.0,
  `format = "pycg"`) and the bench's own snippets (`format = "rueca"`, e.g.
  bench/micro/php).

Raw arm outputs are cached in .cache/runs/s3/<arm>/<suite>.json.
"""

from __future__ import annotations

import json
import shutil
import traceback
from pathlib import Path

from .arms import make_arm
from .arms.base import Arm, Handle
from .common import BENCH_DIR, CACHE, GOLD_DIR, RUNS_DIR, SCIP_DIR, WORK_DIR, is_code_file, lang_family, load_manifest, log, read_json, safe_name, write_json
from .common import run as sh
from .corpus import add_worktree, remove_worktree, worktree
from .phpstan import docker_available, edges_from_calls, phpstan_call_graph, run_phpstan, version_string
from .scip import derive_call_graph, fid, in_scope, read_index, sample_functions
from .tools import fetch_tool, tool_available, tool_spec
from .treesitter import extract_functions

PYCG_KEY = "pycg"
MICRO_SUITES = ("pycg", "micro")


def repo_name(spec: dict) -> str:
    return spec["repo"].split("/")[1]


def gold_path(name: str) -> Path:
    return GOLD_DIR / "s3" / f"{name}.json"


def run_path(arm: str, name: str) -> Path:
    return RUNS_DIR / "s3" / arm / f"{safe_name(name)}.json"


# --- gold: SCIP -----------------------------------------------------------------


def _scip_command(spec: dict, root: Path, out: Path) -> tuple[list[str], Path, dict]:
    """(argv, cwd, env) of the pinned SCIP indexer. npm indexers run through
    npx outside the worktree (so the repository's .npmrc does not govern the
    pinned indexer); binary indexers (scip-go, scip-clang) run in the
    worktree with `{out}` / `{root}` / `{compdb}` substituted in `args`."""
    tool = load_manifest()["tools"][spec["indexer"]]
    if "npm" in tool:
        return ["npx", "--yes", f"{tool['npm']}@{tool['version']}", *spec["args"], "--cwd", str(root), "--output", str(out)], SCIP_DIR, {}
    binary = fetch_tool(spec["indexer"])
    compdb = SCIP_DIR / f"{repo_name(spec)}-compile_commands.json"
    args = [a.format(out=out, root=root, compdb=compdb) for a in spec.get("args", [])]
    env = {}
    if spec["indexer"] == "scip_go":
        # module downloads stay inside the bench cache; never fetch a toolchain
        env = {"GOMODCACHE": str(CACHE / "go" / "mod"), "GOCACHE": str(CACHE / "go" / "build"), "GOTOOLCHAIN": "local", "GOFLAGS": "-mod=mod"}
    return [str(binary), *args], root, env


_JAVAC_JVM = [
    f"-J--add-exports=jdk.compiler/com.sun.tools.javac.{m}=ALL-UNNAMED" for m in ("api", "code", "model", "tree", "util")
]


def _scip_java_plugin() -> Path:
    """The javac plugin embedded in the pinned scip-java launcher (a shell
    stub + jar): extracted once, never downloaded separately."""
    import zipfile

    dest = SCIP_DIR / f"scip-java-{tool_spec('scip_java')['version']}-plugin.jar"
    if dest.exists():
        return dest
    launcher = fetch_tool("scip_java")
    SCIP_DIR.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(launcher) as outer:
        inner_name = next(n for n in outer.namelist() if n.endswith(f"/scip-java-{tool_spec('scip_java')['version']}.jar"))
        with outer.open(inner_name) as fh, zipfile.ZipFile(fh) as inner:
            dest.write_bytes(inner.read("scip-plugin.jar"))
    return dest


def run_scip_java(root: Path, out: Path, sources: list[str], classpath: list[str], name: str):
    """scip-java without a build tool: javac (pinned JDK image, no network,
    read-only sources) with scip-java's own javac plugin over the listed
    source directories (`module-info.java` excluded, annotation processing
    off), then `scip-java aggregate` of the per-file shards."""
    from .tools import fetch_artifact

    spec = tool_spec("scip_java")
    launcher = fetch_tool("scip_java")
    plugin = _scip_java_plugin()
    jars = [fetch_artifact(k) for k in classpath]
    work = SCIP_DIR / f"{name}-java"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    mounts = ["-v", f"{root}:/src:ro", "-v", f"{work}:/out", "-v", f"{launcher}:/tools/scip-java:ro", "-v", f"{plugin}:/tools/scip-plugin.jar:ro"]
    cp = ["/tools/scip-plugin.jar"]
    for j in jars:
        mounts += ["-v", f"{j}:/tools/{j.name}:ro"]
        cp.append(f"/tools/{j.name}")
    find = " ".join(f"'{s}'" for s in sources)
    script = (
        f"cd /src && find {find} -name '*.java' ! -name module-info.java | sort > /out/srcs && "
        f"javac {' '.join(_JAVAC_JVM)} -encoding UTF-8 -proc:none -nowarn -cp {':'.join(cp)} "
        f"'-Xplugin:scip -sourceroot:/src -targetroot:/out/t' -d /out/classes @/out/srcs && "
        "sh /tools/scip-java aggregate --output /out/index.scip --targetroot /out/t"
    )
    res = sh(["docker", "run", "--rm", "--network=none", *mounts, spec["image"], "sh", "-c", script], timeout=3600)
    if (work / "index.scip").exists():
        shutil.move(str(work / "index.scip"), out)
    shutil.rmtree(work, ignore_errors=True)
    return res


def write_compdb(spec: dict, root: Path) -> Path:
    """compile_commands.json for scip-clang, synthesized from the manifest
    (`compdb = { std, includes, files, defines, all_headers }`): one entry
    per listed translation unit, plus — with `all_headers` — one bench-owned
    TU outside the worktree that includes every header under those
    directories, so headers no source file includes are indexed too. Uses
    the host SDK (`xcrun --show-sdk-path`) on macOS. No build is run."""
    cd = spec["compdb"]
    lang = spec["language"]
    cc = "clang++" if lang == "cpp" else "clang"
    flags = [f"-std={cd.get('std', 'c++17' if lang == 'cpp' else 'c11')}"]
    flags += [f"-I{root / inc}" for inc in cd.get("includes", [])]
    flags += [f"-D{d}" for d in cd.get("defines", [])]
    sdk = sh(["xcrun", "--show-sdk-path"], timeout=60)
    if sdk.rc == 0 and sdk.stdout.strip():
        flags += ["-isysroot", sdk.stdout.strip()]
    entries = []
    for f in cd.get("files", []):
        entries.append({"directory": str(root), "file": str(root / f), "arguments": [cc, *flags, "-c", str(root / f)]})
    if cd.get("all_headers"):
        tu_dir = SCIP_DIR / f"{repo_name(spec)}-tu"
        tu_dir.mkdir(parents=True, exist_ok=True)
        heads = sorted(p for inc in cd["all_headers"] for p in (root / inc).rglob("*") if p.suffix in (".h", ".hpp", ".hh"))
        tu = tu_dir / ("all-headers.cc" if lang == "cpp" else "all-headers.c")
        tu.write_text("".join(f'#include "{p}"\n' for p in heads))
        entries.append({"directory": str(root), "file": str(tu), "arguments": [cc, *flags, "-c", str(tu)]})
    out = SCIP_DIR / f"{repo_name(spec)}-compile_commands.json"
    out.write_text(json.dumps(entries, indent=1))
    return out


def build_repo_gold(spec: dict, force: bool = False) -> dict:
    method = spec.get("gold", "scip")
    if method == "scip":
        return build_scip_gold(spec, force)
    if method == "phpstan":
        return build_phpstan_gold(spec, force)
    raise ValueError(f"unknown S3 gold method {method!r} for {spec['repo']}")


def build_phpstan_gold(spec: dict, force: bool = False) -> dict:
    name = repo_name(spec)
    gp = gold_path(name)
    if gp.exists() and not force:
        return read_json(gp)
    man = load_manifest()["s3"]
    with worktree(spec["repo"], spec["commit"], f"s3-{name}") as root:
        log(f"phpstan {name}: {', '.join(spec['include'])}")
        graph = phpstan_call_graph(root, spec["include"], name)
    sample = sample_functions(graph, man["sample_per_repo"], man["seed"], man["min_lines"])
    gold = {
        "suite": "phpstan",
        "name": name,
        "repo": spec["repo"],
        "language": spec["language"],
        "commit": spec["commit"],
        "include": spec["include"],
        "indexer": version_string(),
        "indexer_seconds": None,
        "stats": graph["stats"],
        "callables": graph["callables"],
        "callers": graph["callers"],
        "callees": graph["callees"],
        "sample": sample,
    }
    write_json(gp, gold)
    log(f"s3 gold {name}: {graph['stats'].get('edges')} edges, {len(graph['callables'])} callables, sample {len(sample)}")
    return gold


def build_scip_gold(spec: dict, force: bool = False) -> dict:
    name = repo_name(spec)
    gp = gold_path(name)
    if gp.exists() and not force:
        return read_json(gp)
    man = load_manifest()["s3"]
    scip_out = SCIP_DIR / f"{name}.scip"
    with worktree(spec["repo"], spec["commit"], f"s3-{name}") as root:
        indexer_seconds = None
        if not scip_out.exists() or force:
            SCIP_DIR.mkdir(parents=True, exist_ok=True)
            if spec["indexer"] == "scip_java":
                log(f"scip {name}: javac + scip-java plugin over {', '.join(spec['javac']['sources'])}")
                res = run_scip_java(root, scip_out, spec["javac"]["sources"], spec["javac"].get("classpath", []), name)
            else:
                if "compdb" in spec:
                    write_compdb(spec, root)
                cmd, cwd, env = _scip_command(spec, root, scip_out)
                log(f"scip {name}: {' '.join(cmd[1:])}")
                res = sh(cmd, cwd=cwd, env=env, timeout=3600)
            indexer_seconds = res.seconds
            if res.rc != 0 or not scip_out.exists():
                raise RuntimeError(f"SCIP indexer failed for {name} rc={res.rc}: {res.stderr[-1500:]} {res.stdout[-500:]}")
        docs = read_index(scip_out)
        graph = derive_call_graph(root, docs, spec["include"], spec["language"])
    sample = sample_functions(graph, man["sample_per_repo"], man["seed"], man["min_lines"])
    gold = {
        "suite": "scip",
        "name": name,
        "repo": spec["repo"],
        "language": spec["language"],
        "commit": spec["commit"],
        "include": spec["include"],
        "indexer": f"{tool_spec(spec['indexer']).get('npm') or tool_spec(spec['indexer'])['name']}@{tool_spec(spec['indexer'])['version']}",
        "indexer_seconds": indexer_seconds,
        "stats": graph["stats"],
        "callables": graph["callables"],
        "callers": graph["callers"],
        "callees": graph["callees"],
        "sample": sample,
    }
    write_json(gp, gold)
    log(f"s3 gold {name}: {graph['stats'].get('edges')} edges, {len(graph['callables'])} callables, sample {len(sample)}")
    return gold


# --- gold: PyCG micro-benchmark ---------------------------------------------------


def micro_specs() -> list[dict]:
    return list(load_manifest()["s3"].get("micro", []))


def micro_spec(name: str) -> dict:
    return next(m for m in micro_specs() if m["name"] == name)


def snippets_root(name: str) -> Path:
    return WORK_DIR / f"{name}-snippets"


def pycg_root() -> Path:
    return snippets_root(PYCG_KEY)


def _module_name(rel: str) -> str:
    mod = rel[: -len(".py")].replace("/", ".")
    return mod[: -len(".__init__")] if mod.endswith(".__init__") else mod


def _git_snapshot(path: Path) -> None:
    """Each snippet becomes a tiny git repo (fixed identity and date) so every
    arm sees an ordinary working tree rooted at the snippet."""
    env = {
        "GIT_AUTHOR_NAME": "rueca-bench",
        "GIT_AUTHOR_EMAIL": "bench@invalid",
        "GIT_COMMITTER_NAME": "rueca-bench",
        "GIT_COMMITTER_EMAIL": "bench@invalid",
        "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
        "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
    }
    sh(["git", "init", "-q"], cwd=path, check=True)
    sh(["git", "add", "-A"], cwd=path, check=True)
    sh(["git", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "snapshot"], cwd=path, env=env, check=True)


def pycg_graph(snip: Path) -> dict:
    cg = json.loads((snip / "callgraph.json").read_text())
    files = sorted(str(p.relative_to(snip)) for p in snip.rglob("*.py") if ".git" not in p.parts)
    modules = {_module_name(rel): rel for rel in files}
    funcs: dict[str, list[dict]] = {}
    callables: dict[str, dict] = {}
    for rel in files:
        lst = []
        for f in extract_functions(rel, (snip / rel).read_bytes()):
            d = f.as_dict()
            d["path"] = rel
            lst.append(d)
            callables[fid(rel, d)] = d
        funcs[rel] = lst
    stats = {"names": 0, "external_or_builtin": 0, "module_nodes": 0, "unmapped": 0}

    def resolve(name: str) -> str | None:
        stats["names"] += 1
        best = None
        for m in modules:
            if (name == m or name.startswith(m + ".")) and (best is None or len(m) > len(best)):
                best = m
        if best is None:
            stats["external_or_builtin"] += 1
            return None
        if name == best:
            stats["module_nodes"] += 1
            return None
        qual = name[len(best) + 1 :]
        rel = modules[best]
        hits = [d for d in funcs[rel] if d["qualname"] == qual]
        if not hits:
            stats["unmapped"] += 1
            return None
        return fid(rel, hits[0])

    callers: dict[str, set[str]] = {}
    callees: dict[str, set[str]] = {}
    questions: set[str] = set()
    for src, dsts in sorted(cg.items()):
        s = resolve(src)
        if s is None:
            continue
        questions.add(s)
        for dst in dsts:
            d = resolve(dst)
            if d is None:
                continue
            callees.setdefault(s, set()).add(d)
            callers.setdefault(d, set()).add(s)
    return {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in callers.items()},
        "callees": {k: sorted(v) for k, v in callees.items()},
        "sample": sorted(questions),
        "stats": stats,
    }


def build_pycg_gold(force: bool = False) -> dict:
    gp = gold_path(PYCG_KEY)
    if gp.exists() and not force:
        return read_json(gp)
    spec = micro_spec(PYCG_KEY)
    dest = pycg_root()
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    src_root = add_worktree(spec["repo"], spec["commit"], "pycg-src")
    try:
        base = src_root / spec["path"]
        snippets = sorted(p.parent for p in base.glob("*/*/callgraph.json"))
        for s in snippets:
            name = f"{s.parent.name}__{s.name}"
            shutil.copytree(s, dest / name)
    finally:
        remove_worktree(spec["repo"], src_root)
    gold: dict = {"suite": "pycg", "name": PYCG_KEY, "repo": spec["repo"], "commit": spec["commit"], "language": "python", "snippets": {}}
    for snip in sorted(dest.iterdir()):
        g = pycg_graph(snip)
        # the answer key must not be visible to the arms
        for extra in ("callgraph.json", "README.md"):
            (snip / extra).unlink(missing_ok=True)
        _git_snapshot(snip)
        gold["snippets"][snip.name] = g
    n_q = sum(len(g["sample"]) for g in gold["snippets"].values())
    n_e = sum(len(v) for g in gold["snippets"].values() for v in g["callees"].values())
    log(f"s3 gold pycg: {len(gold['snippets'])} snippets, {n_q} functions, {n_e} edges")
    write_json(gp, gold)
    return gold


# --- gold: rueca-format micro-benchmarks (bench/micro/<lang>) --------------------------
#
# bench/micro/<lang>/<category>/<snippet>/ holds a few source files and
# callgraph.json: {"path:Qualified": ["path:Qualified", ...]} where Qualified
# is the tree-sitter labeler's local qualified name (PHP `Class::method`,
# `function`). Every key is a question (functions without calls have []).


def rueca_micro_graph(snip: Path, answer: dict, language: str | None = None) -> dict:
    files = sorted(str(p.relative_to(snip)) for p in snip.rglob("*") if p.is_file() and ".git" not in p.parts and p.name != "callgraph.json")
    callables: dict[str, dict] = {}
    by_label: dict[str, str] = {}
    for rel in files:
        for f in extract_functions(rel, (snip / rel).read_bytes(), language):
            d = f.as_dict()
            d["path"] = rel
            k = fid(rel, d)
            callables[k] = d
            by_label.setdefault(f"{rel}:{d['qualname']}", k)
            # overloads share a qualified name: `path:Qualified@<name line>` picks one
            by_label[f"{rel}:{d['qualname']}@{d['def_line']}"] = k
    unmapped = sorted({lab for src, dsts in answer.items() for lab in [src, *dsts] if lab not in by_label})
    if unmapped:
        raise ValueError(f"{snip.name}: answer-key labels without a tree-sitter callable: {unmapped}")
    callers: dict[str, set[str]] = {}
    callees: dict[str, set[str]] = {}
    for src, dsts in sorted(answer.items()):
        s = by_label[src]
        for dst in dsts:
            d = by_label[dst]
            callees.setdefault(s, set()).add(d)
            callers.setdefault(d, set()).add(s)
    return {
        "callables": callables,
        "callers": {k: sorted(v) for k, v in callers.items()},
        "callees": {k: sorted(v) for k, v in callees.items()},
        "sample": sorted(by_label[k] for k in answer),
        "stats": {"labels": len(answer), "edges": sum(len(v) for v in answer.values())},
    }


def _phpstan_agreement(dest: Path, snippets: dict[str, dict], name: str) -> dict:
    """Cross-check the hand-written answer keys against the PHPStan extractor
    (when Docker is available): edge-level agreement per snippet."""
    ok, why = docker_available()
    if not ok:
        return {"available": False, "reason": why}
    raw = run_phpstan(dest, sorted(snippets), WORK_DIR.parent / "phpstan" / f"{name}-check")
    per: dict[str, dict] = {}
    tp = fp = fn = 0
    for sname, g in sorted(snippets.items()):
        by_file: dict[str, list[dict]] = {}
        for d in g["callables"].values():
            by_file.setdefault(d["path"], []).append(d)
        _, callees, _ = edges_from_calls(raw[sname], f"/src/{sname}/", by_file, lambda rel: True)
        got = {(a, b) for a, bs in callees.items() for b in bs}
        want = {(a, b) for a, bs in g["callees"].items() for b in bs}
        tp += len(got & want)
        fp += len(got - want)
        fn += len(want - got)
        per[sname] = {
            "phpstan_only": sorted(f"{a} -> {b}" for a, b in got - want),
            "key_only": sorted(f"{a} -> {b}" for a, b in want - got),
        }
    return {
        "available": True,
        "tool": version_string(),
        "edges_key": tp + fn,
        "edges_phpstan": tp + fp,
        "agree": tp,
        "precision": tp / (tp + fp) if tp + fp else None,
        "recall": tp / (tp + fn) if tp + fn else None,
        "snippets_identical": sum(1 for v in per.values() if not v["phpstan_only"] and not v["key_only"]),
        "per_snippet": {k: v for k, v in per.items() if v["phpstan_only"] or v["key_only"]},
    }


def _scip_agreement(spec: dict, dest: Path, snippets: dict[str, dict]) -> dict:
    """Cross-check hand-written answer keys against a SCIP indexer
    (manifest `crosscheck = "scip_go" | "scip_clang"`): each snippet is
    copied to a scratch directory, indexed, and its call edges derived exactly
    like the repository gold (scip.derive_call_graph); edge-level agreement
    with the key is reported. Nothing is written into the snippet trees the
    arms index."""
    tool = spec["crosscheck"]
    ok, why = tool_available(tool)
    if not ok:
        return {"available": False, "reason": why}
    binary = fetch_tool(tool)
    scratch = CACHE / "scip" / "micro" / spec["name"]
    shutil.rmtree(scratch, ignore_errors=True)
    scratch.mkdir(parents=True)
    lang = spec["language"]
    per: dict[str, dict] = {}
    tp = fp = fn = 0
    failed: list[str] = []
    for sname, g in sorted(snippets.items()):
        root = scratch / sname
        shutil.copytree(dest / sname, root)
        out = scratch / f"{sname}.scip"
        if tool == "scip_go":
            env = {"GOMODCACHE": str(CACHE / "go" / "mod"), "GOCACHE": str(CACHE / "go" / "build"), "GOTOOLCHAIN": "local", "GOFLAGS": "-mod=mod"}
            res = sh([str(binary), "--output", str(out)], cwd=root, env=env, timeout=600)
        elif tool == "scip_java":
            res = run_scip_java(root, out, ["."], [], f"micro-{sname}")
        else:
            files = sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.suffix in (".c", ".cc", ".cpp", ".cxx"))
            cspec = {"repo": f"micro/{sname}", "language": lang, "compdb": {"includes": ["."], "files": files}}
            compdb = write_compdb(cspec, root)
            res = sh([str(binary), f"--compdb-path={compdb}", f"--index-output-path={out}"], cwd=root, timeout=600)
        if res.rc != 0 or not out.exists():
            failed.append(sname)
            continue
        graph = derive_call_graph(root, read_index(out), [""], lang)
        got = {(a, b) for a, bs in graph["callees"].items() for b in bs}
        want = {(a, b) for a, bs in g["callees"].items() for b in bs}
        tp += len(got & want)
        fp += len(got - want)
        fn += len(want - got)
        per[sname] = {"scip_only": sorted(f"{a} -> {b}" for a, b in got - want), "key_only": sorted(f"{a} -> {b}" for a, b in want - got)}
    shutil.rmtree(scratch, ignore_errors=True)
    ts = tool_spec(tool)
    return {
        "available": True,
        "tool": f"{ts['name']}@{ts['version']}",
        "edges_key": tp + fn,
        "edges_scip": tp + fp,
        "agree": tp,
        "precision": tp / (tp + fp) if tp + fp else None,
        "recall": tp / (tp + fn) if tp + fn else None,
        "snippets_identical": sum(1 for v in per.values() if not v["scip_only"] and not v["key_only"]),
        "snippets_failed": failed,
        "per_snippet": {k: v for k, v in per.items() if v["scip_only"] or v["key_only"]},
    }


def build_rueca_micro_gold(spec: dict, force: bool = False) -> dict:
    name = spec["name"]
    gp = gold_path(name)
    if gp.exists() and not force:
        return read_json(gp)
    src = BENCH_DIR / spec["local"]
    dest = snippets_root(name)
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    keys: dict[str, dict] = {}
    for cg in sorted(src.glob("*/*/callgraph.json")):
        snip = cg.parent
        sname = f"{snip.parent.name}__{snip.name}"
        shutil.copytree(snip, dest / sname)
        keys[sname] = json.loads(cg.read_text())
    gold: dict = {"suite": "micro", "name": name, "language": spec["language"], "source": spec["local"], "snippets": {}}
    for sname in sorted(keys):
        snip = dest / sname
        gold["snippets"][sname] = rueca_micro_graph(snip, keys[sname], spec["language"])
        (snip / "callgraph.json").unlink()
    if lang_family(spec["language"]) == "php":
        gold["phpstan_agreement"] = _phpstan_agreement(dest, gold["snippets"], name)
    if spec.get("crosscheck"):
        gold["crosscheck"] = _scip_agreement(spec, dest, gold["snippets"])
    for sname in sorted(keys):
        _git_snapshot(dest / sname)
    n_q = sum(len(g["sample"]) for g in gold["snippets"].values())
    n_e = sum(len(v) for g in gold["snippets"].values() for v in g["callees"].values())
    log(f"s3 gold {name}: {len(gold['snippets'])} snippets, {n_q} functions, {n_e} edges")
    write_json(gp, gold)
    return gold


def build_micro_gold(spec: dict, force: bool = False) -> dict:
    if spec.get("format") == "pycg":
        return build_pycg_gold(force)
    return build_rueca_micro_gold(spec, force)


def ensure_snippet_tree(spec: dict) -> None:
    """Re-create the snippet repos if the work dir was cleaned after setup."""
    root = snippets_root(spec["name"])
    if root.exists() and any(root.iterdir()):
        return
    build_micro_gold(spec, force=True)


def ensure_pycg_tree() -> None:
    ensure_snippet_tree(micro_spec(PYCG_KEY))


# --- run ------------------------------------------------------------------------------


def _brief(h: dict) -> dict:
    return {k: h.get(k) for k in ("path", "name", "label", "start_line", "end_line")}


def _ask(arm: Arm, h: Handle, graph: dict, questions: list[str], do_lookup: bool) -> list[dict]:
    out = []
    for q in questions:
        func = graph["callables"][q]
        rec: dict = {"fid": q}
        r = arm.resolve(h, func)
        rec["resolve"] = {"ok": r.ok, "seconds": r.seconds, "error": r.error}
        if r.ok:
            ref = r.extra.get("ref")
            for direction in ("callers", "callees"):
                n = arm.neighbors(h, ref, direction)
                rec[direction] = {
                    "ok": n.ok,
                    "seconds": n.seconds,
                    "error": n.error,
                    "hits": [_brief(x) for x in n.hits],
                }
        if do_lookup:
            lk = arm.lookup(h, func["name"], 10)
            rec["lookup"] = {"ok": lk.ok, "seconds": lk.seconds, "error": lk.error, "hits": [_brief(x) for x in lk.hits]}
        out.append(rec)
    return out


def _run_scip_repo(arm: Arm, spec: dict) -> dict:
    name = repo_name(spec)
    gold = read_json(gold_path(name))
    rec: dict = {"arm": arm.name, "suite": "scip", "name": name, "repo": spec["repo"], "language": spec["language"]}
    with worktree(spec["repo"], spec["commit"], f"s3-{name}") as root:
        h = arm.index(root, f"s3-{name}", spec["language"])
        rec["index_seconds"] = h.index_seconds
        rec["index_info"] = h.info
        if not h.ok:
            rec.update(status="index_failed", error=h.error)
            arm.cleanup(h)
            return rec
        try:
            rec["answers"] = _ask(arm, h, gold, gold["sample"], do_lookup=True)
        finally:
            arm.cleanup(h)
    rec["status"] = "ok"
    return rec


def _run_micro(arm: Arm, spec: dict) -> dict:
    name = spec["name"]
    gold = read_json(gold_path(name))
    ensure_snippet_tree(spec)
    rec: dict = {"arm": arm.name, "suite": gold["suite"], "name": name, "language": spec["language"], "snippets": {}}
    for sname, g in sorted(gold["snippets"].items()):
        if not g["sample"]:
            continue
        root = snippets_root(name) / sname
        h = arm.index(root, f"{name}-{sname}", spec["language"])
        srec: dict = {"index_seconds": h.index_seconds}
        if not h.ok:
            srec.update(status="index_failed", error=h.error)
        else:
            try:
                srec["answers"] = _ask(arm, h, g, g["sample"], do_lookup=False)
                srec["status"] = "ok"
            except Exception as exc:
                srec.update(status="error", error=str(exc))
        arm.cleanup(h)
        rec["snippets"][sname] = srec
    rec["status"] = "ok"
    return rec


def suites(languages: set[str] | None = None) -> list[tuple[str, dict, bool]]:
    """(name, spec, is_micro) for every S3 gold set, repos first then
    micro-benchmarks, optionally restricted to language families."""
    man = load_manifest()["s3"]
    out: list[tuple[str, dict, bool]] = [(repo_name(s), s, False) for s in man["repos"]]
    out += [(m["name"], m, True) for m in man.get("micro", [])]
    if languages:
        out = [x for x in out if lang_family(x[1]["language"]) in languages]
    return out


def run(arm_names: list[str], only: list[str] | None = None, force: bool = False, languages: set[str] | None = None) -> None:
    for name, spec, micro in suites(languages):
        if only and name not in only:
            continue
        if not gold_path(name).exists():
            log(f"s3 {name}: gold missing (run `rueca-bench setup`); skipped")
            continue
        for arm_name in arm_names:
            arm = make_arm(arm_name)
            ok, _ = arm.available()
            if not ok:
                continue
            p = run_path(arm_name, name)
            if p.exists() and not force and read_json(p).get("status") in ("ok", "index_failed"):
                continue
            log(f"s3 {arm_name:5s} {name}")
            try:
                rec = _run_micro(arm, spec) if micro else _run_scip_repo(arm, spec)
            except Exception as exc:
                rec = {"arm": arm_name, "name": name, "status": "error", "error": f"{exc}\n{traceback.format_exc()[-1500:]}"}
            rec["arm_version"] = arm.version()
            write_json(p, rec)


def prediction_in_scope(path: str | None, gold: dict) -> bool:
    if not path:
        return False
    if gold.get("suite") in MICRO_SUITES:
        return is_code_file(path, gold["language"]) and not path.startswith("<")
    return in_scope(path, gold["include"])
