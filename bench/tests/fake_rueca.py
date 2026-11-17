"""A stand-in for `rueca embed` (the contract in rueca_bench/embed.py), used
by the tests through a generated executable. Vectors are deterministic: a
row is the text's sha256 bytes scaled to [-1, 1), width `FAKE_RUECA_DIMS`
(default 8); a query row is prefixed with its instruction first, so the
instruction changes the vector. `FAKE_RUECA_NO_INSTRUCTION=1` removes the
`--instruction` flag; `FAKE_RUECA_LOG` appends every argv as one JSON line.

It also stands in for the hybrid-search surface the Rueca arm drives
(`rueca index ... --embed=P`, `rueca query search_symbols`, `rueca models
path`, `--version`): one `Function` per `.py` file named after the file;
`--embed` records each file's text in a key file under `$RUECA_VECTOR_DIR`
(else `<store>/vectors`) and reports how many were cached; the lexical
order is by path, the dense order its reverse, fused with
`$RUECA_FUSION_ALPHA` (`semantic`: 0, no mode: 1), so the knobs show in the
ranking. An explicit `mode` on an index without vectors is an error."""

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


def _index(args: list[str]) -> int:
    if "--help" in args:
        print("--store --json --embed[=PRESET] --no-embed")
        return 0
    root, store = args[0], _opt(args, "--store")
    embed = next((a.split("=", 1)[1] for a in args if a.startswith("--embed=")), None)
    files = _py_files(root)
    rep = {"files_total": len(files), "files_extracted": len(files), "files_partial": 0, "references_total": 0, "references_resolved": 0, "duration_ms": 1}
    if embed is not None:
        vdir = os.environ.get("RUECA_VECTOR_DIR") or os.path.join(store, "vectors")
        os.makedirs(vdir, exist_ok=True)
        keys_path = os.path.join(vdir, f"{embed}.keys")
        have = set(open(keys_path).read().split()) if os.path.exists(keys_path) else set()
        keys = [hashlib.sha256(open(os.path.join(root, f), "rb").read()).hexdigest() for f in files]
        new = [k for k in keys if k not in have]
        with open(keys_path, "a") as fh:
            fh.write("".join(k + "\n" for k in new))
        rep["vectors"] = {"preset": embed, "units": len(keys), "cached": len(keys) - len(new), "embedded": len(new)}
    with open(os.path.join(store, "fake-index.json"), "w") as fh:
        json.dump({"files": files, "embed": embed}, fh)
    print(json.dumps(rep))
    return 0


def _ranked(files: list[str], mode: str | None) -> list[tuple[str, float]]:
    alpha = {"semantic": 0.0, None: 1.0, "lexical": 1.0}.get(mode, float(os.environ.get("RUECA_FUSION_ALPHA", "0.5")))
    n = len(files)
    scores = {f: alpha * (1 - i / n) + (1 - alpha) * (1 - (n - 1 - i) / n) for i, f in enumerate(files)}
    return sorted(scores.items(), key=lambda x: (-x[1], x[0]))


def _query(args: list[str]) -> int:
    if "--help" in args:
        print("--root --store --org --text")
        return 0
    tool, req = args[0], json.loads(args[1])
    with open(os.path.join(_opt(args, "--store"), "fake-index.json")) as fh:
        idx = json.load(fh)
    mode = req.get("mode")
    if tool != "search_symbols" or (mode not in (None, "lexical") and idx["embed"] is None):
        print(json.dumps({"error": "this snapshot has no vectors: run `rueca index --embed`"}))
        return 1
    ranked = _ranked(idx["files"], mode)[: req.get("limit", 10)]
    syms = [{"path": f, "name": os.path.splitext(os.path.basename(f))[0], "label": "Function", "start_line": 1, "end_line": 2, "score": round(s, 4)} for f, s in ranked]
    if "--text" in args:
        print("\n".join(f"{x['path']}:1 {x['name']}" for x in syms))
    elif req.get("format") == "verbose":
        print(json.dumps({"results": syms, "truncated": False}))
    else:
        groups = [{"s": 0, "path": x["path"], "symbols": [{"qn": x["name"], "label": "Function", "lines": [1, 2]}]} for x in syms]
        print(json.dumps({"snapshots": [{"s": 0, "repo": "local/fake"}], "results": groups, "retrieval": {"mode": mode or "lexical"}}))
    return 0


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
