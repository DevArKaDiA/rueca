"""A stand-in for `rueca embed` (the contract in rueca_bench/embed.py), used
by the tests through a generated executable. Vectors are deterministic: a
row is the text's sha256 bytes scaled to [-1, 1), width `FAKE_RUECA_DIMS`
(default 8); a query row is prefixed with its instruction first, so the
instruction changes the vector. `FAKE_RUECA_NO_INSTRUCTION=1` removes the
`--instruction` flag; `FAKE_RUECA_LOG` appends every argv as one JSON line.

It also stands in for the hybrid-search surface the Rueca arm drives
(`rueca index ... --embed=P`, `rueca index --org FILE ROOT [--embed=P]`,
`rueca query search_symbols [--org FILE]`, `rueca query --batch FILE`,
`rueca models path`, `--version`): one `Function` per `.py` file named
after the file; a store holds every repository indexed into it (an
organization member's id is `github.com/<owner>/<name>` from a worktree
named `org-<owner>__<name>`; `repo` narrows a query to one); `--embed`
records each file's text in a key file under `$RUECA_VECTOR_DIR` (else
`<store>/vectors`) and reports how many were cached; files named by a query
word rank first, then the lexical order is by path and the dense order its
reverse, fused with `$RUECA_FUSION_ALPHA` (`semantic`: 0, no mode: 1), so
the knobs show in the ranking. `RUECA_EXPAND` with `calls` puts the files
that call a top-3 seed (`name(` in their text) right after it; `rerank:
true` reverses the top `rerank_k` and reports `retrieval.reranker.ms`. An
explicit `mode` on an index without vectors is an error. `query --help`
lists `--batch` unless `FAKE_RUECA_NO_BATCH=1`, and `--cpu` with
`FAKE_RUECA_QUERY_CPU=1`."""

import hashlib
import json
import os
import struct
import sys

DEFAULT_INSTRUCTION = "Given a question about coding, retrieval code or passage that can solve user's question"


def _row(text: str, dims: int) -> list[float]:
    digest = b""
    i = 0
    while len(digest) < dims:
        digest += hashlib.sha256(f"{i}:{text}".encode()).digest()
        i += 1
    return [(b - 128) / 128 for b in digest[:dims]]


def _opt(args: list[str], name: str) -> str | None:
    return args[args.index(name) + 1] if name in args else None


def _py_files(root: str) -> list[str]:
    out = []
    for dirpath, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d != ".git"]
        out += [os.path.relpath(os.path.join(dirpath, f), root) for f in files if f.endswith(".py")]
    return sorted(out)


def _repo_id(root: str) -> str:
    """`github.com/<owner>/<name>` from a worktree named `org-<owner>__<name>`."""
    base = os.path.basename(os.path.normpath(root)).removeprefix("org-")
    return "github.com/" + base.replace("__", "/") if "__" in base else "local/fake"


def _load_index(store: str) -> dict:
    path = os.path.join(store, "fake-index.json")
    if not os.path.exists(path):
        return {"repos": {}}
    with open(path) as fh:
        return json.load(fh)


def _embed_report(root: str, files: list[str], embed: str) -> dict:
    vdir = os.environ.get("RUECA_VECTOR_DIR") or os.path.join(os.getcwd(), "vectors")
    os.makedirs(vdir, exist_ok=True)
    keys_path = os.path.join(vdir, f"{embed}.keys")
    have = set(open(keys_path).read().split()) if os.path.exists(keys_path) else set()
    keys = [hashlib.sha256(open(os.path.join(root, f), "rb").read()).hexdigest() for f in files]
    new = [k for k in keys if k not in have]
    with open(keys_path, "a") as fh:
        fh.write("".join(k + "\n" for k in new))
    return {"preset": embed, "units": len(keys), "cached": len(keys) - len(new), "embedded": len(new)}


def _member(root: str, embed: str | None, store: str) -> tuple[dict, dict | None]:
    files = _py_files(root)
    texts = {f: open(os.path.join(root, f), encoding="utf-8").read() for f in files}
    vectors = None
    if embed is not None:
        os.environ.setdefault("RUECA_VECTOR_DIR", os.path.join(store, "vectors"))
        vectors = _embed_report(root, files, embed)
    return {"files": files, "texts": texts, "embed": embed}, vectors


def _index(args: list[str]) -> int:
    if "--help" in args:
        print("--store --json --org --embed[=PRESET] --no-embed")
        return 0
    store = _opt(args, "--store")
    org = _opt(args, "--org")
    embed = next((a.split("=", 1)[1] for a in args if a.startswith("--embed=")), None)
    root = next(a for a in args if not a.startswith("--") and a not in (store, org))
    rid = _repo_id(root) if org else "local/fake"
    idx = _load_index(store)
    replaced = rid in idx["repos"]
    idx["repos"][rid], vectors = _member(root, embed, store)
    with open(os.path.join(store, "fake-index.json"), "w") as fh:
        json.dump(idx, fh)
    files = idx["repos"][rid]["files"]
    if org:
        seg = {"snapshot": {"repo": rid}, "node_count": len(files)}
        one = {"segment": seg, "duration_ms": 10, **({"vectors": vectors} if vectors else {})}
        print(json.dumps({"indexed": [one], "org_ms": 2, "update": {"repos": len(idx["repos"]), "generation": 1, "added": 0 if replaced else 1, "replaced": int(replaced), "bytes_copied": 0}}))
        return 0
    rep = {"files_total": len(files), "files_extracted": len(files), "files_partial": 0, "references_total": 0, "references_resolved": 0, "duration_ms": 1}
    if vectors:
        rep["vectors"] = vectors
    print(json.dumps(rep))
    return 0


def _ranked(files: list[str], mode: str | None) -> list[tuple[str, float]]:
    alpha = {"semantic": 0.0, None: 1.0, "lexical": 1.0}.get(mode, float(os.environ.get("RUECA_FUSION_ALPHA", "0.5")))
    n = len(files)
    scores = {f: alpha * (1 - i / n) + (1 - alpha) * (1 - (n - 1 - i) / n) for i, f in enumerate(files)}
    return sorted(scores.items(), key=lambda x: (-x[1], x[0]))


def _name(path: str) -> str:
    return os.path.splitext(os.path.basename(path))[0]


def _pool(idx: dict, req: dict) -> list[tuple[str, str]]:
    """(repository id, path) of every file the request selects; files that
    name a query word come first (a tiny lexical signal), then by path."""
    want = req.get("repo")
    pool = [(rid, f) for rid, r in sorted(idx["repos"].items()) if want in (None, rid) for f in r["files"]]
    words = set(str(req.get("query", "")).lower().split())
    return sorted(pool, key=lambda x: (_name(x[1]).lower() not in words, x[1], x[0]))


def _expand(ranked: list[tuple[tuple[str, str], float]], idx: dict) -> list[tuple[tuple[str, str], float]]:
    """`RUECA_EXPAND` with calls: a file whose text calls a top-3 seed's name
    (`name(`) joins right after that seed with decay x its score."""
    if "calls" not in os.environ.get("RUECA_EXPAND", "off"):
        return ranked
    decay = float(os.environ.get("RUECA_EXPAND_DECAY", "0.5"))
    out = list(ranked)
    for seed, score in ranked[:3]:
        callers = [(rid, f) for rid, r in idx["repos"].items() for f in r["files"] if (rid, f) != seed and _name(seed[1]) + "(" in r["texts"][f]]
        for c in callers:
            out = [x for x in out if x[0] != c]
            pos = next(i for i, x in enumerate(out) if x[0] == seed) + 1
            out.insert(pos, (c, score * decay))
    return out


def _rerank(ranked: list, req: dict) -> tuple[list, dict | None]:
    if not req.get("rerank"):
        return ranked, None
    k = int(req.get("rerank_k", 50))
    return list(reversed(ranked[:k])) + ranked[k:], {"preset": os.environ.get("RUECA_RERANK_PRESET", "qwen3-reranker-0.6b-q8"), "k": k, "ms": 1.5 * k}


def _answer(tool: str, req: dict, store: str, text: bool) -> tuple[str, int]:
    idx = _load_index(store)
    mode = req.get("mode")
    want = req.get("repo")
    lexical_only = all(r["embed"] is None for rid, r in idx["repos"].items() if want in (None, rid))
    if tool != "search_symbols" or (mode not in (None, "lexical") and lexical_only):
        return json.dumps({"error": "this snapshot has no vectors: run `rueca index --embed`"}), 2
    pool = _pool(idx, req)
    order = dict(_ranked([f"{rid}\x00{f}" for rid, f in pool], mode))
    ranked = sorted((((rid, f), order[f"{rid}\x00{f}"]) for rid, f in pool), key=lambda x: -x[1])
    ranked, reranker = _rerank(_expand(ranked, idx), req)
    ranked = ranked[: req.get("limit", 10)]
    repos = sorted({rid for (rid, _), _ in ranked})
    snaps = [{"s": i, "repo": rid} for i, rid in enumerate(repos)]
    syms = [{"rid": rid, "path": f, "name": _name(f), "label": "Function", "start_line": 1, "end_line": 2, "score": round(s, 4)} for (rid, f), s in ranked]
    retrieval = {"mode": mode or "lexical", **({"reranker": reranker} if reranker else {})}
    if text:
        return "\n".join(f"{x['path']}:1 {x['name']}" for x in syms), 0
    if req.get("format") == "verbose":
        flat = [{k: v for k, v in x.items() if k != "rid"} | {"id": f"{x['rid']}@0:{x['path']}#{x['name']}"} for x in syms]
        return json.dumps({"results": flat, "truncated": False, "retrieval": retrieval}), 0
    groups = [{"s": repos.index(x["rid"]), "path": x["path"], "symbols": [{"qn": x["name"], "label": "Function", "lines": [1, 2]}]} for x in syms]
    return json.dumps({"snapshots": snaps, "results": groups, "retrieval": retrieval}), 0


def _query_help() -> str:
    flags = "--root --store --org --text"
    if os.environ.get("FAKE_RUECA_NO_BATCH") != "1":
        flags += " --batch"
    if os.environ.get("FAKE_RUECA_QUERY_CPU") == "1":
        flags += " --cpu"
    return flags


def _batch(path: str, store: str) -> int:
    """`rueca query --batch FILE`: one JSON line per request, in order."""
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            r = json.loads(line)
            out, _ = _answer(r["tool"], r.get("args") or {}, store, False)
            print(out, flush=True)
    return 0


def _query(args: list[str]) -> int:
    if "--help" in args:
        print(_query_help())
        return 0
    store = _opt(args, "--store")
    if "--batch" in args:
        return _batch(_opt(args, "--batch"), store)
    out, rc = _answer(args[0], json.loads(args[1]), store, "--text" in args)
    print(out)
    return rc


def _models(args: list[str]) -> int:
    root = os.environ.get("RUECA_MODEL_DIR") or os.path.expanduser("~/.cache/rueca/models")
    print(os.path.join(root, args[1], "weights.gguf"))
    return 0


SURFACE = {"index": _index, "query": _query, "models": _models}


def main(argv: list[str]) -> int:
    if os.environ.get("FAKE_RUECA_LOG"):
        with open(os.environ["FAKE_RUECA_LOG"], "a") as fh:
            fh.write(json.dumps(argv) + "\n")
    if argv == ["--version"]:
        print("rueca 0.0.0-fake")
        return 0
    if argv and argv[0] in SURFACE:
        return SURFACE[argv[0]](argv[1:])
    if not argv or argv[0] != "embed":
        print("usage: rueca embed ...", file=sys.stderr)
        return 2
    return _embed(argv[1:])


def _embed(args: list[str]) -> int:
    no_instr = os.environ.get("FAKE_RUECA_NO_INSTRUCTION") == "1"
    dims = int(os.environ.get("FAKE_RUECA_DIMS", "8"))
    if "--help" in args:
        flags = "--preset --dims --max-tokens --batch --cpu --threads --input --output --format --info"
        print(flags if no_instr else flags + " --instruction")
        return 0
    if "--instruction" in args and no_instr:
        print("error: unexpected argument '--instruction'", file=sys.stderr)
        return 2
    preset = _opt(args, "--preset") or "qwen3-0.6b-q8"
    if "--info" in args:
        print(json.dumps({"preset": preset, "model_key": f"fake-{preset}", "sha256": "0" * 64, "dims": dims, "quant": "q8_0", "runtime": "fake", "runtime_version": "0", "backend": "cpu", "cpu_level": "neon"}))
        return 0
    if preset == "broken":
        print("model failed to load", file=sys.stderr)
        return 1
    instruction = _opt(args, "--instruction") or DEFAULT_INSTRUCTION
    out = _opt(args, "--output")
    lines = [json.loads(x) for x in open(_opt(args, "--input"), encoding="utf-8") if x.strip()]
    ids = [x["id"] for x in lines]
    if os.environ.get("FAKE_RUECA_SHUFFLE") == "1":
        ids = list(reversed(ids))
    with open(out, "wb") as fh:
        for x in lines:
            text = f"Instruct: {instruction}\nQuery:{x['text']}" if x["query"] else x["text"]
            fh.write(struct.pack(f"<{dims}f", *_row(text, dims)))
    with open(out + ".ids", "w") as fh:
        fh.write("".join(i + "\n" for i in ids))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
