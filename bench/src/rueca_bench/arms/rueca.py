"""Rueca arm, driven through the CLI (docs/tools.md):

  rueca index <path> --store <dir> --json
  rueca query <tool> '<json>' --root <path> --store <dir> [--text]

Two response contracts are understood, detected from the binary:

- **alpha** (v0.1.0-alpha): flat `results` of brief symbols, `trace` edges
  as `{src, dst, ...}` objects with full ids; the JSON the CLI prints is what
  an agent reads.
- **RFC 0005** (`rueca query --text` exists): an envelope (`snapshots`,
  items referring to them by `s`), results grouped by file (`symbols` with
  `qn`, `lines`, `sig`), `trace` nodes by index and edges as arrays
  `[src, dst, kind, confidence, strategy, call_site_line]`, every tool
  taking `budget_tokens`. Structural scoring reads the JSON (with a budget
  large enough never to truncate a scoring call); the tokens an agent reads
  are counted on the same request rendered with `--text` (the MCP text
  content). Long field names (`"format": "verbose"`) are understood too.

The binary comes from $RUECA_BIN (or `rueca` on PATH when RUECA_BIN=rueca).

**Retrieval modes** (RFC 0007, `RuecaConfig`): `RUECA_BENCH_RUECA_MODE` =
`lexical` (default: every existing output stays valid; no `--embed`, no
`mode` argument), `hybrid` or `semantic`. A non-lexical mode is its own
localization-only arm (`rueca-hybrid`, `rueca-semantic`): it indexes with
`rueca index --embed=<preset>` (`RUECA_BENCH_VECTOR_PRESET`, default
`qwen3-0.6b-q8`), asks `search_symbols` with `{"mode": <mode>}` (an
explicit mode errors instead of silently ranking lexically) and passes the
fusion knobs `RUECA_FUSION_ALPHA` / `_N` / `_NORM` to every call (bench
defaults 0.5 / 100 / `minmax`, always explicit, so the run directory
`runs/<suite>/rueca-<mode>/<preset>-a<alpha>-n<N>-<norm>/` says what ran).
Stores stay per instance, but every call shares one Rust vector cache
(`RUECA_VECTOR_DIR` = `$RUECA_BENCH_RUECA_VECTOR_DIR`, default
`<bench cache>/rueca-vectors`), so a commit re-embeds only the symbols whose
canonical text changed; model weights come from `$RUECA_MODEL_DIR`, else
the default store's `models/` (`$RUECA_HOME`, else `~/.cache/rueca`).
"""

from __future__ import annotations

import copy
import json
import os
import shutil
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from ..common import CACHE, count_tokens, run, safe_name, sha256_file
from ..efficiency import json_prefix_tokens, text_prefix_tokens
from ..embed import DEFAULT_PRESET
from .base import Arm, CallResult, Handle, is_callable_label, line_distance, line_match
from .grep import scored

# Scoring calls must never be cut by the RFC 0005 token budget.
SCORING_BUDGET = 10_000_000
# Output-shaping arguments only: dropped for an alpha binary. Semantic ones
# (`repo`, `ref`) are never dropped, so an unsupported request fails.
_V2_ONLY_ARGS = ("budget_tokens", "format", "scores", "cursor")


def parse_id(sid: str | None) -> dict:
    """`repo@commit:path#qn` -> parts (any missing part is None)."""
    out: dict = {"repo": None, "commit": None, "path": None, "qn": None}
    if not sid or "@" not in sid:
        return out
    repo, _, rest = sid.partition("@")
    commit, _, rest = rest.partition(":")
    path, _, qn = rest.partition("#")
    out.update(repo=repo, commit=commit.rstrip("+"), path=path or None, qn=qn or None)
    return out


def local_name(qn: str | None) -> str | None:
    """Bare name of a (local) qualified name: the last segment, without an
    overload's parameter list (`geo::scale(double, double)` -> `scale`,
    `src/util/index:Class.method` -> `method`, `App\\X::save` -> `save`)."""
    if not qn:
        return None
    q = qn
    if q.endswith(")") and "(" in q:
        q = q[: q.index("(")]
    for sep in ("::", ":", "\\", "."):
        q = q.replace(sep, "\x00")
    return q.rsplit("\x00", 1)[-1] or None


def unwrap(data: Any) -> Any:
    """RFC 0005 envelopes put a tool's result under `results` (a list for
    searches, an object for `trace`, `get_symbol`, `get_file_outline`);
    `snapshots` are kept next to it. Alpha and verbose answers pass through."""
    if isinstance(data, dict) and "snapshots" in data and isinstance(data.get("results"), dict):
        return {**data["results"], "snapshots": data["snapshots"]}
    return data


def norm_symbol(d: dict, path: str | None = None, snap: dict | None = None) -> dict:
    """A brief symbol in either contract (alpha long names, RFC 0005 short
    names `qn` / `lines` / `sig` / `d` / `dir`) -> the arm's normal form."""
    lines = d.get("lines")
    start = d.get("start_line")
    end = d.get("end_line")
    if isinstance(lines, (list, tuple)) and lines:
        start = lines[0] if start is None else start
        end = lines[-1] if end is None else end
    sid = d.get("id")
    pid = parse_id(sid)
    qn = d.get("qualified_name") or d.get("qn") or pid["qn"]
    name = d.get("name")
    if name is None:
        name = local_name(pid["qn"] or qn)
    direction = d.get("direction") or d.get("dir")
    direction = {"in": "callers", "out": "callees"}.get(direction, direction)
    return {
        "path": d.get("path") or path or pid["path"],
        "name": name,
        "label": d.get("label"),
        "start_line": start,
        "end_line": end,
        "id": sid,
        "qualified_name": qn,
        "local": d.get("local") or pid["qn"],
        "repo": (snap or {}).get("repo") or pid["repo"],
        "depth": d.get("depth", d.get("d")),
        "direction": direction,
        "in": d.get("in"),
        "changed": d.get("changed"),
        "score": d.get("score"),
    }


def _brief(b: dict) -> dict:
    return {k: b.get(k) for k in ("path", "name", "label", "start_line", "end_line", "id", "qualified_name", "repo", "score")}


def flat_results(data: dict) -> list[dict]:
    """`results` of a search answer, flattened in rendered order (groups by
    snapshot and file in RFC 0005, flat in alpha)."""
    snaps = {s.get("s", i): s for i, s in enumerate(data.get("snapshots") or [])}
    out: list[dict] = []
    for item in data.get("results") or []:
        if isinstance(item, dict) and isinstance(item.get("symbols"), list):
            snap = snaps.get(item.get("s"))
            for sym in item["symbols"]:
                out.append(norm_symbol(sym, item.get("path"), snap))
        elif isinstance(item, dict):
            out.append(norm_symbol(item, None, snaps.get(item.get("s"))))
    return out


def trace_neighbours(data: dict, ref: Any) -> tuple[dict[str, list[dict]], dict]:
    """Depth-1 callers / callees of a `trace` answer (either contract), each
    with the best confidence of the edges linking it to the root. A node's
    direction comes from the node (`direction` / `dir`) when given, else from
    the edges that link it to the root. A self-edge (recursion) lists the
    root as its own caller and callee: `trace` never repeats the root among
    `nodes`, only its `edges` say it."""
    g = trace_graph(data)
    root_id = g["root"] or ref
    conf: dict[tuple[str, Any], float] = {}
    edge_dirs: dict[Any, set[str]] = {}
    for e in g["edges"]:
        c = e.get("confidence")
        c = float(c) if c is not None else 1.0
        if e["dst"] == root_id:
            conf[("callers", e["src"])] = max(conf.get(("callers", e["src"]), 0.0), c)
            edge_dirs.setdefault(e["src"], set()).add("callers")
        if e["src"] == root_id:
            conf[("callees", e["dst"])] = max(conf.get(("callees", e["dst"]), 0.0), c)
            edge_dirs.setdefault(e["dst"], set()).add("callees")
    out: dict[str, list[dict]] = {"callers": [], "callees": []}
    for n in g["node_list"]:
        if n["id"] == root_id or n.get("depth") not in (None, 1):
            continue
        if n.get("direction") in out:
            dirs = {n["direction"]}
        elif n.get("direction") == "both":
            dirs = {"callers", "callees"}
        else:
            dirs = edge_dirs.get(n["id"], set())
        for d in sorted(dirs):
            b = _brief(n)
            b["confidence"] = conf.get((d, n["id"]))
            out[d].append(b)
    root = g["nodes"].get(root_id)
    if root is not None and ("callers", root_id) in conf:
        for d in ("callers", "callees"):
            b = _brief(root)
            b["confidence"] = conf[(d, root_id)]
            out[d].append(b)
    return out, {"truncated": data.get("truncated")}


def trace_graph(data: dict) -> dict:
    """A `trace` answer as {root id, nodes by id (normal form), edges as
    dicts with ids and call_site}, from either contract."""
    data = unwrap(data)
    snaps = {s.get("s", i): s for i, s in enumerate(data.get("snapshots") or [])}
    raw_nodes = data.get("nodes") or []
    nodes = [norm_symbol(n, None, snaps.get(n.get("s"))) for n in raw_nodes]
    root = data.get("root")
    root_node = None
    if isinstance(root, dict):
        root_node = norm_symbol(root, None, snaps.get(root.get("s")))
        root_id = root_node.get("id")
    elif isinstance(root, int) and 0 <= root < len(nodes):
        root_id = nodes[root]["id"]
    else:
        root_id = next((n["id"] for n in nodes if n.get("depth") == 0), None)

    def key_of(x: Any) -> Any:
        if isinstance(x, int) and 0 <= x < len(nodes):
            return nodes[x]["id"]
        return x

    by_id = {n["id"]: n for n in nodes}
    edges = []
    for e in data.get("edges") or []:
        if isinstance(e, (list, tuple)):
            src, dst = key_of(e[0]), key_of(e[1])
            line = e[5] if len(e) > 5 else None
            path = e[6] if len(e) > 6 else (by_id.get(src) or {}).get("path")
            edges.append({"src": src, "dst": dst, "kind": e[2] if len(e) > 2 else None, "confidence": e[3] if len(e) > 3 else None, "strategy": e[4] if len(e) > 4 else None, "call_site": {"path": path, "line": line} if line is not None else None})
        else:
            edges.append({**e, "src": key_of(e.get("src")), "dst": key_of(e.get("dst"))})
    if root_node is not None:
        by_id.setdefault(root_id, root_node)
    return {"root": root_id, "nodes": by_id, "node_list": nodes, "edges": edges}


# --- retrieval modes (RFC 0007) ----------------------------------------------------

MODES = ("lexical", "hybrid", "semantic")
NORMS = ("minmax", "theoretical")
DEFAULT_ALPHA = 0.5
DEFAULT_FUSION_N = 100
DEFAULT_NORM = "minmax"
# Indexing with vectors embeds a repository's first commit from scratch.
EMBED_INDEX_TIMEOUT = 7200.0


def rueca_vector_dir() -> Path:
    """The Rust store vector cache every instance shares (`RUECA_VECTOR_DIR`)."""
    return Path(os.environ.get("RUECA_BENCH_RUECA_VECTOR_DIR") or CACHE / "rueca-vectors").resolve()


def rueca_model_dir() -> Path:
    """Model weights: `$RUECA_MODEL_DIR`, else the default store's `models/`
    (where `rueca models pull` put them), not the per-instance store's."""
    if os.environ.get("RUECA_MODEL_DIR"):
        return Path(os.environ["RUECA_MODEL_DIR"])
    home = os.environ.get("RUECA_HOME") or str(Path.home() / ".cache" / "rueca")
    return Path(home) / "models"


@dataclass(frozen=True)
class RuecaConfig:
    """What the arm asks for: retrieval mode, embedding preset and fusion knobs."""

    mode: str = "lexical"
    preset: str = DEFAULT_PRESET
    alpha: float = DEFAULT_ALPHA
    fusion_n: int = DEFAULT_FUSION_N
    norm: str = DEFAULT_NORM

    def __post_init__(self) -> None:
        if self.mode not in MODES:
            raise ValueError(f"retrieval mode {self.mode!r}: choose from {MODES}")
        if self.norm not in NORMS:
            raise ValueError(f"fusion normalization {self.norm!r}: choose from {NORMS}")
        if not 0.0 <= self.alpha <= 1.0 or self.fusion_n < 1:
            raise ValueError(f"fusion alpha {self.alpha} must be in [0, 1] and N {self.fusion_n} at least 1")

    @classmethod
    def from_env(cls, mode: str | None = None) -> "RuecaConfig":
        env = os.environ.get
        return cls(
            mode=mode or env("RUECA_BENCH_RUECA_MODE") or "lexical",
            preset=env("RUECA_BENCH_VECTOR_PRESET") or DEFAULT_PRESET,
            alpha=float(env("RUECA_FUSION_ALPHA") or DEFAULT_ALPHA),
            fusion_n=int(env("RUECA_FUSION_N") or DEFAULT_FUSION_N),
            norm=env("RUECA_FUSION_NORM") or DEFAULT_NORM,
        )

    @property
    def embeds(self) -> bool:
        return self.mode != "lexical"

    @property
    def arm_name(self) -> str:
        return f"rueca-{self.mode}" if self.embeds else "rueca"

    def tag(self) -> str:
        return f"{self.preset}-a{self.alpha:g}-n{self.fusion_n}-{self.norm}"

    def run_label(self) -> str:
        """Directory of the outputs under `runs/<suite>/`: `rueca` (lexical,
        as before) or `rueca-<mode>/<tag>`."""
        return f"{self.arm_name}/{self.tag()}" if self.embeds else self.arm_name

    def env(self) -> dict[str, str]:
        """Environment of every `rueca` call (empty for lexical)."""
        if not self.embeds:
            return {}
        return {
            "RUECA_FUSION_ALPHA": f"{self.alpha:g}",
            "RUECA_FUSION_N": str(self.fusion_n),
            "RUECA_FUSION_NORM": self.norm,
            "RUECA_VECTOR_DIR": str(rueca_vector_dir()),
            "RUECA_MODEL_DIR": str(rueca_model_dir()),
        }

    def describe(self) -> str:
        return f"mode {self.mode}, preset {self.preset}, alpha {self.alpha:g}, N {self.fusion_n}, {self.norm}" if self.embeds else "mode lexical"

    def as_dict(self) -> dict:
        return asdict(self)


def _best_per_file(hits: list[dict]) -> list[tuple[str, float]]:
    out: dict[str, float] = {}
    for h in hits:
        if h.get("path") and h.get("score") is not None and h["path"] not in out:
            out[h["path"]] = float(h["score"])
    return list(out.items())


def hit_scores(hits: list[dict], funcs: list[dict]) -> dict | None:
    """The `scores` record (fused scores as the binary reports them) when
    every hit carries one."""
    if not hits or any(h.get("score") is None for h in hits):
        return None
    return scored(_best_per_file(hits), [(f, float(f["score"])) for f in funcs])


def rueca_binary() -> str | None:
    """The Rueca binary: $RUECA_BIN (a path, or a name looked up on PATH)."""
    tb = os.environ.get("RUECA_BIN", "")
    return shutil.which(tb) if tb and os.path.sep not in tb else (tb or None)


class RuecaArm(Arm):
    name = "rueca"

    def __init__(self, call_timeout: float = 120.0, index_timeout: float = 1800.0, config: RuecaConfig | None = None):
        self.config = config or RuecaConfig()
        self.name = self.config.arm_name
        self.call_timeout = call_timeout
        self.index_timeout = max(index_timeout, EMBED_INDEX_TIMEOUT) if self.config.embeds else index_timeout
        self.bin = rueca_binary()
        self._contract: str | None = None
        self._bin_version: str | None = None

    def with_config(self, config: RuecaConfig) -> "RuecaArm":
        """The same binary under another setting (a sweep): shares the
        detected contract and version, queries an index built by any
        setting with the same preset."""
        other = copy.copy(self)
        other.config = config
        other.name = config.arm_name
        return other

    def preflight(self) -> None:
        """Non-lexical modes: the binary must index with `--embed` and the
        preset's weights must be on disk, before any instance runs (a
        failed index is recorded and never retried)."""
        if not self.config.embeds:
            return
        res = run([self.bin, "index", "--help"], timeout=30)
        if "--embed" not in res.stdout + res.stderr:
            raise RuntimeError("`rueca index` has no --embed flag (a binary from before RFC 0007 phase 1)")
        res = run([self.bin, "models", "path", self.config.preset], timeout=30, env=self.config.env())
        weights = Path(res.stdout.strip()) if res.rc == 0 and res.stdout.strip() else None
        if weights is None or not weights.exists():
            raise RuntimeError(f"no weights for {self.config.preset} under {rueca_model_dir()}: run `rueca models pull {self.config.preset}` (or set RUECA_MODEL_DIR)")

    def available(self) -> tuple[bool, str]:
        if not self.bin:
            return False, "RUECA_BIN not set"
        if not Path(self.bin).exists():
            return False, f"RUECA_BIN={self.bin} does not exist"
        return True, ""

    @property
    def contract(self) -> str:
        """`rfc0005` when `rueca query` takes `--text`, else `alpha`."""
        if self._contract is None:
            res = run([self.bin, "query", "--help"], timeout=30)
            self._contract = "rfc0005" if "--text" in (res.stdout + res.stderr) else "alpha"
        return self._contract

    def version(self) -> str:
        if not self.bin:
            return "pending (RUECA_BIN not set)"
        if self._bin_version is None:
            res = run([self.bin, "--version"], timeout=30)
            ver = (res.stdout.strip() or res.stderr.strip() or "unknown").splitlines()[0]
            # RUECA_BIN_LABEL names the source of a branch build (e.g.
            # `feat/lang-go@af4b3b6`), so the report can say which binary
            # produced each language's numbers.
            label = os.environ.get("RUECA_BIN_LABEL")
            self._bin_version = f"{ver} (binary sha256 {sha256_file(Path(self.bin))[:12]}, {self.contract} contract{', ' + label if label else ''}"
        return self._bin_version + (f"; {self.config.describe()})" if self.config.embeds else ")")

    def _store(self, key: str) -> Path:
        return CACHE / "rueca-store" / safe_name(key)

    def _query(self, h: Handle, tool: str, args: dict[str, Any]) -> tuple[Any, float, str | None]:
        data, secs, err, _ = self._query_raw(h, tool, args)
        return data, secs, err

    def _args(self, args: dict[str, Any], mode: str, text: bool) -> dict[str, Any]:
        """Arguments per call mode. alpha: the RFC 0005 arguments removed.
        RFC 0005, `scoring`: JSON in the verbose (alpha) shapes, which carry
        no budget, and the text of the same request with a budget large
        enough never to cut it; `agent`: the request as given (compact,
        default budget)."""
        if self.contract == "alpha":
            return {k: v for k, v in args.items() if k not in _V2_ONLY_ARGS}
        if mode == "scoring":
            return {**args, "budget_tokens": SCORING_BUDGET} if text else {**args, "format": "verbose"}
        return args

    def call(self, tool: str, args: dict[str, Any], root: Path | None, store: Path, text: bool = False, mode: str = "scoring", org: Path | None = None) -> tuple[Any, float, str | None, str]:
        """One `rueca query` process (the segment the index step wrote, warm
        OS page cache). (parsed JSON or None for text, seconds, error, raw
        stdout). An argument the binary rejects as unknown is dropped once
        and the call retried (contract drift between branches). `org`: an
        organization file (RFC 0006), served instead of `root`."""
        args = self._args(args, mode, text)
        for _ in range(2):
            cmd = [self.bin, "query", tool, json.dumps(args)]
            if text:
                cmd.append("--text")
            if root is not None:
                cmd += ["--root", str(root)]
            if org is not None:
                cmd += ["--org", str(org)]
            cmd += ["--store", str(store)]
            res = run(cmd, timeout=self.call_timeout, env=self.config.env() or None)
            if res.timed_out:
                return None, res.seconds, f"timeout after {res.seconds:.0f}s", ""
            out = res.stdout
            if text and res.rc == 0:
                return None, res.seconds, None, out
            try:
                data = json.loads(out)
            except json.JSONDecodeError:
                return None, res.seconds, f"non-json output rc={res.rc}: {out[:300]} {res.stderr[-300:]}", out
            if res.rc != 0 or (isinstance(data, dict) and "error" in data):
                msg = str(data.get("error") if isinstance(data, dict) else "")
                bad = [k for k in list(args) if k in _V2_ONLY_ARGS and f"`{k}`" in msg]
                if "unknown argument" in msg and bad:
                    args = {k: v for k, v in args.items() if k not in bad}
                    continue
                return data, res.seconds, f"rc={res.rc}: {msg}", out
            return data, res.seconds, None, out
        return None, 0.0, "argument retry exhausted", ""

    def _query_raw(self, h: Handle, tool: str, args: dict[str, Any]) -> tuple[Any, float, str | None, str]:
        return self.call(tool, args, h.root, h.state["store"])

    def agent_tokens(self, h: Handle | None, tool: str, args: dict[str, Any], raw_json: str, root: Path | None = None, store: Path | None = None, mode: str = "scoring") -> tuple[int | None, int | None]:
        """(tokens an agent reads, tokens of the JSON it would get) for this
        request. alpha: both are the JSON already printed. RFC 0005: the
        `--text` rendering (MCP text content) and the compact JSON
        (`structuredContent`) of the same request."""
        if self.contract == "alpha":
            t = count_tokens(raw_json)
            return t, t
        r = root if h is None else h.root
        s = store if h is None else h.state["store"]
        _, _, err, txt = self.call(tool, args, r, s, text=True, mode=mode)
        tj = count_tokens(raw_json)
        if mode == "scoring":
            # the scoring JSON is verbose; count the compact JSON of the same request
            cargs = {**args, "budget_tokens": SCORING_BUDGET}
            _, _, cerr, craw = self.call(tool, cargs, r, s, mode="agent")
            tj = None if cerr else count_tokens(craw)
        if err:
            return None, tj
        return count_tokens(txt), tj

    def index(self, root: Path, key: str, language: str) -> Handle:
        store = self._store(key)
        shutil.rmtree(store, ignore_errors=True)
        store.mkdir(parents=True, exist_ok=True)
        h = Handle(self.name, key, root, language, state={"store": store})
        cmd = [self.bin, "index", str(root), "--store", str(store), "--json"]
        if self.config.embeds:
            cmd.append(f"--embed={self.config.preset}")
        res = run(cmd, timeout=self.index_timeout, env=self.config.env() or None)
        h.index_seconds = res.seconds
        if res.timed_out or res.rc != 0:
            h.ok = False
            h.error = f"index failed rc={res.rc} timed_out={res.timed_out}: {res.stderr[-500:]} {res.stdout[-300:]}"
            return h
        try:
            rep = json.loads(res.stdout)
            h.info = {k: rep.get(k) for k in ("files_total", "files_extracted", "files_partial", "references_total", "references_resolved", "duration_ms")}
            if rep.get("vectors") is not None:
                h.info["vectors"] = rep["vectors"]
        except json.JSONDecodeError:
            h.info = {"raw": res.stdout[-300:]}
        return h

    def _mode_args(self, args: dict[str, Any]) -> dict[str, Any]:
        """`mode` for a non-lexical setting; lexical requests stay as they were."""
        return {**args, "mode": self.config.mode} if self.config.embeds else args

    def _search(self, h: Handle, query: str, limit: int, text: bool = True) -> CallResult:
        args = self._mode_args({"query": query, "limit": min(limit, 200)})
        data, secs, err, raw = self._query_raw(h, "search_symbols", args)
        if err:
            return CallResult(False, secs, error=err)
        hits = [_brief(b) for b in flat_results(data)]
        tokens, tj = self.agent_tokens(h, "search_symbols", args, raw) if text else (None, count_tokens(raw))
        return CallResult(True, secs, hits=hits, extra={"tokens": tokens, "tokens_json": tj})

    def search_eff(self, h: Handle, query: str, limit: int = 10, root: Path | None = None, store: Path | None = None, repo: str | None = None, org: Path | None = None) -> CallResult:
        """The search an agent would issue (limit `limit`, every other
        argument at its default): parsed items in rendered order, the tokens
        an agent reads, and the prefix cost after each item."""
        args: dict[str, Any] = self._mode_args({"query": query, "limit": limit})
        if repo is not None:
            args["repo"] = repo
        r = h.root if h is not None else root
        st = h.state["store"] if h is not None else store
        data, secs, err, raw = self.call("search_symbols", args, r, st, mode="agent", org=org)
        if err:
            return CallResult(False, secs, error=err)
        items = flat_results(data)
        prefix, approx = json_prefix_tokens(raw, data.get("results") or [])
        tokens, fmt = count_tokens(raw), "json"
        if self.contract != "alpha":
            _, _, terr, txt = self.call("search_symbols", args, r, st, text=True, mode="agent", org=org)
            if not terr:
                prefix, approx = text_prefix_tokens(txt, len(items))
                tokens, fmt = count_tokens(txt), "text"
        return CallResult(True, secs, hits=[_brief(x) for x in items], extra={"tokens": tokens, "tokens_json": count_tokens(raw), "prefix": prefix, "approx": approx, "format": fmt})

    def localize(self, h: Handle, query: str, limit: int = 100) -> CallResult:
        res = self._search(h, query, limit, text=False)
        if not res.ok:
            return res
        files: list[str] = []
        funcs: list[dict] = []
        for hit in res.hits:
            if hit["path"] and hit["path"] not in files:
                files.append(hit["path"])
            if is_callable_label(hit["label"]):
                funcs.append(hit)
        res.extra.update({"files": files, "functions": funcs})
        if self.config.embeds:
            sc = hit_scores(res.hits, funcs)
            if sc is not None:
                res.extra["scores"] = sc
        return res

    def resolve(self, h: Handle, func: dict) -> CallResult:
        """`get_symbol` by name and path; when neither the symbol nor its
        candidates match (many same-named methods in one file, overloads),
        the file's `get_file_outline` (every symbol of the file, cached per
        index) is scanned — as exhaustive as the cbm arm's node scan."""
        data, secs, err = self._query(h, "get_symbol", {"name": func["name"], "path": func["path"]})
        if not err and isinstance(data, dict):
            cands = [data.get("symbol") or {}] + list(data.get("candidates") or [])
            ok = [n for n in (norm_symbol(c, func["path"]) for c in cands if c) if n.get("path") == func["path"] and n.get("name") == func["name"] and line_match(n.get("start_line"), func)]
            if ok:
                c = min(ok, key=lambda n: line_distance(n.get("start_line"), func))
                return CallResult(True, secs, hits=[_brief(c)], extra={"ref": c.get("id")})
        outlines = h.state.setdefault("outlines", {})
        if func["path"] not in outlines:
            odata, osecs, oerr = self._query(h, "get_file_outline", {"path": func["path"]})
            secs += osecs
            flat: list[dict] = []

            def walk(syms: list[dict]) -> None:
                for sym in syms:
                    flat.append(norm_symbol(sym, func["path"]))
                    walk(sym.get("children") or [])

            if not oerr and isinstance(odata, dict):
                walk(odata.get("symbols") or [])
            outlines[func["path"]] = flat
        ok = [c for c in outlines[func["path"]] if c.get("name") == func["name"] and is_callable_label(c.get("label")) and line_match(c.get("start_line"), func)]
        if ok:
            c = min(ok, key=lambda n: line_distance(n.get("start_line"), func))
            return CallResult(True, secs, hits=[_brief(dict(c, path=func["path"]))], extra={"ref": c.get("id")})
        return CallResult(False, secs, error=err or "no candidate or outline symbol matched path+name+line")

    def neighbors(self, h: Handle, ref: Any) -> CallResult:
        """One `trace` (direction `both`, depth 1, default edge types and
        test exclusion): each depth-1 neighbour with the best confidence of
        the edges that link it to the root in that direction. `tokens` is
        what an agent reads for that request, `tokens_json` the JSON."""
        args = {"id": ref, "direction": "both", "depth": 1, "limit": 2000}
        data, secs, err, raw = self._query_raw(h, "trace", args)
        if err:
            return CallResult(False, secs, error=err)
        out, meta = trace_neighbours(data, ref)
        tokens, tj = self.agent_tokens(h, "trace", args, raw)
        return CallResult(True, secs, extra={**out, "truncated": meta.get("truncated"), "tokens": tokens, "tokens_json": tj})

    def lookup(self, h: Handle, name: str, limit: int = 10) -> CallResult:
        return self._search(h, name, limit)

    # -- data suite ----------------------------------------------------------------

    def data_nodes(self, h: Handle, files: list[str]) -> CallResult:
        """`get_file_outline` of every data file, flattened."""
        t0 = time.perf_counter()
        hits: list[dict] = []
        errors = 0

        def walk(path: str, syms: list[dict]) -> None:
            for s in syms:
                n = norm_symbol(s, path)
                hits.append({"path": n["path"], "name": n["name"], "label": n["label"], "start_line": n["start_line"]})
                walk(path, s.get("children") or [])

        for f in files:
            data, _, err = self._query(h, "get_file_outline", {"path": f})
            if err or not isinstance(data, dict):
                errors += 1
                continue
            walk(f, data.get("symbols") or [])
        return CallResult(True, time.perf_counter() - t0, hits=hits, extra={"outline_errors": errors})

    def env_edges(self, h: Handle, names: list[str]) -> CallResult:
        """`trace` callers over READS / WRITES from `$env:NAME` for every
        candidate name (Rueca has no enumeration tool)."""
        t0 = time.perf_counter()
        reads, writes = [], []
        for name in names:
            data, _, err = self._query(
                h,
                "trace",
                {"qualified_name": f"$env:{name}", "direction": "callers", "edge_types": ["READS", "WRITES"], "depth": 1, "limit": 2000, "include_tests": True},
            )
            if err or not isinstance(data, dict):
                continue
            g = trace_graph(data)
            root = g["root"]
            nodes = g["nodes"]
            for e in g["edges"]:
                if e["dst"] != root:
                    continue
                src = nodes.get(e["src"]) or {}
                site = e.get("call_site") or {}
                if e.get("kind") == "READS":
                    func = src.get("name") if src.get("label") in ("Function", "Method", "Test") else "<module>"
                    reads.append({"path": src.get("path") or site.get("path"), "function": func, "env": name, "line": site.get("line")})
                elif e.get("kind") == "WRITES":
                    writes.append({"path": src.get("path"), "env": name, "line": src.get("start_line")})
        return CallResult(True, time.perf_counter() - t0, extra={"reads": [r for r in reads if r["path"]], "writes": [w for w in writes if w["path"] and w["line"]]})

    # -- health (RFC 0004 §5, S5) ---------------------------------------------------

    def health(self, root: Path, key: str, files: list[str]) -> dict:
        """Parse-error rate and index throughput of one language: `files`
        (repository-relative, every file of the language) are hard-linked
        into a scratch tree (no copy, same relative paths, so the walk's
        excludes apply as in the repository) that is indexed alone, so
        `files_partial / files_extracted` of the IndexReport is the rate
        for that language."""
        scratch = CACHE / "health" / safe_name(key)
        shutil.rmtree(scratch, ignore_errors=True)
        tree = scratch / "tree"
        for rel in files:
            src = root / rel
            if src.is_symlink() or not src.is_file():
                continue
            dst = tree / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            try:
                os.link(src, dst)
            except OSError:
                shutil.copy2(src, dst)
        store = scratch / "store"
        store.mkdir(parents=True, exist_ok=True)
        try:
            res = run([self.bin, "index", str(tree), "--store", str(store), "--json"], timeout=self.index_timeout)
            if res.rc != 0:
                return {"ok": False, "error": f"index failed rc={res.rc}: {res.stderr[-300:]}", "seconds": res.seconds}
            rep = json.loads(res.stdout)
            return {
                "ok": True,
                "seconds": res.seconds,
                "files_listed": len(files),
                **{k: rep.get(k) for k in ("files_total", "files_extracted", "files_partial", "files_skipped", "duration_ms", "references_total", "references_resolved")},
            }
        finally:
            shutil.rmtree(scratch, ignore_errors=True)

    def cleanup(self, h: Handle) -> None:
        shutil.rmtree(h.state["store"], ignore_errors=True)
