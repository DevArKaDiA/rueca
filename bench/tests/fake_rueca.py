"""A stand-in for `rueca embed` (the contract in rueca_bench/embed.py), used
by the tests through a generated executable. Vectors are deterministic: a
row is the text's sha256 bytes scaled to [-1, 1), width `FAKE_RUECA_DIMS`
(default 8); a query row is prefixed with its instruction first, so the
instruction changes the vector. `FAKE_RUECA_NO_INSTRUCTION=1` removes the
`--instruction` flag; `FAKE_RUECA_LOG` appends every argv as one JSON line."""

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


def main(argv: list[str]) -> int:
    if os.environ.get("FAKE_RUECA_LOG"):
        with open(os.environ["FAKE_RUECA_LOG"], "a") as fh:
            fh.write(json.dumps(argv) + "\n")
    if not argv or argv[0] != "embed":
        print("usage: rueca embed ...", file=sys.stderr)
        return 2
    args = argv[1:]
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
