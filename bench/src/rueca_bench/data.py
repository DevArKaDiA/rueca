"""Data-format suite (RFC 0003 §3.3): key recall, span accuracy, env bridge.

Corpus: manifest `[[data.repos]]` at pinned commits (a docker/awesome-compose
subset, a Terraform module, a Kubernetes microservices repository).

1. **Key recall vs a reference parser.** Every data file in scope (JSON /
   JSONC, YAML, TOML, Terraform HCL) is parsed with a reference parser —
   Python's `json` grammar (a small position-keeping scanner that accepts
   the same documents plus JSONC comments / trailing commas), PyYAML's
   composer, `tomllib`, `python-hcl2` — and flattened into **leaf keys**
   (a key whose value is not a mapping; arrays of mappings are entered,
   ≤ 64 elements, depth ≤ 32, RFC 0003 §4.5). HCL contributes its blocks
   (`resource`, `data`, `module`, `variable`, `output`, `locals` entries),
   since python-hcl2 reports lines for blocks only. Files cut by the
   RFC §4.5 rules (lockfiles, fixtures / testdata, > 256 KiB, templated YAML
   with `{{ }}`) are not in the reference.
   An arm *covers* a reference key when it has a node in the same file named
   like the key (its last segment, or a qualified name ending in it) whose
   start line lies inside the key's span (key line .. end of its value);
   arms are free to qualify the key differently (Rueca key paths, cbm's
   flattened names). **Span line
   accuracy** = covered keys whose node starts exactly on the key's line.
2. **Env bridge precision / recall** against hand-labelled
   `(reader, env var)` and `(provider, env var)` pairs
   (`bench/labels/bridge/<name>.json`, Rueca's own labels): readers are the
   innermost function that reads the variable with a string-literal name
   through a standard env API (module-level code is the reader `<module>`),
   providers the manifest entries that set it. cbm's `CONFIGURES` edges are
   mapped by direction (`env_access` → reads, `env_set` → writes), Rueca's
   `READS` / `WRITES` edges come from `trace` on `$env:NAME` for every
   candidate name. A grep baseline (regexes over code and manifests) is
   scored the same way.

Raw arm outputs are cached in .cache/runs/data/<arm>/<name>.json.
"""

from __future__ import annotations

import bisect
import json
import re
import tomllib
import traceback
from pathlib import Path

import hcl2
import yaml

from .arms import make_arm
from .common import BENCH_DIR, GOLD_DIR, RUNS_DIR, load_manifest, log, read_json, safe_name, write_json
from .corpus import ensure_commit, list_files, worktree
from .treesitter import extract_functions, grammar_for, innermost

DATA_EXTS = {".json": "json", ".jsonc": "json", ".yaml": "yaml", ".yml": "yaml", ".toml": "toml", ".tf": "hcl"}
LOCKFILES = {
    "package-lock.json", "npm-shrinkwrap.json", "yarn.lock", "pnpm-lock.yaml", "composer.lock", "poetry.lock",
    "Cargo.lock", "go.sum", ".terraform.lock.hcl", "packages.lock.json", "gradle.lockfile", "flake.lock",
    "Podfile.lock", "Package.resolved", "pubspec.lock", "mix.lock", "Pipfile.lock", "uv.lock",
}
SKIP_DIRS = {"fixtures", "__fixtures__", "testdata", "__snapshots__", "node_modules", "vendor"}
MAX_BYTES = 256 * 1024
MAX_DEPTH = 32
MAX_ELEMENTS = 64
LABELS_DIR = BENCH_DIR / "labels" / "bridge"

# env APIs with a string-literal name (RFC 0003 §7.2), per code extension;
# group 1 = variable name. Used by the grep baseline only.
ENV_READ_RES: dict[tuple[str, ...], list[re.Pattern]] = {
    (".go",): [re.compile(r'\bos\.(?:Getenv|LookupEnv)\(\s*"([A-Za-z_][A-Za-z0-9_]*)"')],
    (".py",): [
        re.compile(r'\bos\.environ\.get\(\s*[\'"]([A-Za-z_][A-Za-z0-9_]*)[\'"]'),
        re.compile(r'\bos\.environ\[\s*[\'"]([A-Za-z_][A-Za-z0-9_]*)[\'"]\s*\]'),
        re.compile(r'\bos\.getenv\(\s*[\'"]([A-Za-z_][A-Za-z0-9_]*)[\'"]'),
    ],
    (".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx"): [
        re.compile(r'\bprocess\.env\.([A-Za-z_][A-Za-z0-9_]*)'),
        re.compile(r'\bprocess\.env\[\s*[\'"]([A-Za-z_][A-Za-z0-9_]*)[\'"]\s*\]'),
    ],
    (".java",): [re.compile(r'\bSystem\.getenv\(\s*"([A-Za-z_][A-Za-z0-9_]*)"')],
    (".php",): [re.compile(r'\b(?:getenv|env)\(\s*[\'"]([A-Za-z_][A-Za-z0-9_]*)[\'"]'), re.compile(r'\$_(?:ENV|SERVER)\[\s*[\'"]([A-Za-z_][A-Za-z0-9_]*)[\'"]')],
    (".c", ".h", ".cc", ".cpp"): [re.compile(r'\b(?:std::)?(?:secure_)?getenv\(\s*"([A-Za-z_][A-Za-z0-9_]*)"')],
}
CODE_EXTS = tuple(e for k in ENV_READ_RES for e in k)


def repos() -> list[dict]:
    return list(load_manifest().get("data", {}).get("repos", []))


def gold_path(name: str) -> Path:
    return GOLD_DIR / "data" / f"{name}.json"


def run_path(arm: str, name: str) -> Path:
    return RUNS_DIR / "data" / arm / f"{safe_name(name)}.json"


def in_prefixes(path: str, prefixes: list[str]) -> bool:
    return any(path == p or path.startswith(p) for p in prefixes)


# --- file selection -------------------------------------------------------------------


def data_format(path: str) -> str | None:
    name = path.rsplit("/", 1)[-1]
    for ext, fmt in DATA_EXTS.items():
        if name.endswith(ext):
            return fmt
    return None


def skip_reason(path: str, data: bytes) -> str | None:
    """RFC 0003 §4.5 rules the reference applies before parsing."""
    parts = path.split("/")
    if parts[-1] in LOCKFILES:
        return "lockfile"
    if parts[-1].endswith(".min.json"):
        return "data"
    if any(p in SKIP_DIRS for p in parts[:-1]) or "test/data" in path:
        return "data"
    if len(data) > MAX_BYTES:
        return "data-size"
    head = data[:512].decode("utf-8", errors="replace")
    if "DO NOT EDIT" in head and "Code generated" in head or "@generated" in head:
        return "generated"
    if data_format(path) == "yaml" and re.search(rb"(?<!\$)\{\{", data):  # Helm / Go templates; GHA `${{ }}` is YAML
        return "template"
    return None


# --- reference parsers -----------------------------------------------------------------


class _Lines:
    def __init__(self, text: str):
        self.starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]

    def line(self, offset: int) -> int:
        return bisect.bisect_right(self.starts, offset)


class _Json:
    """Position-keeping JSON(C) reader: same grammar as `json` plus comments
    and trailing commas (JSONC, RFC 0003 §8)."""

    def __init__(self, text: str):
        self.s = text
        self.i = 0
        self.lines = _Lines(text)
        self.keys: list[dict] = []

    def ws(self) -> None:
        s, n = self.s, len(self.s)
        while self.i < n:
            c = s[self.i]
            if c in " \t\r\n﻿":
                self.i += 1
            elif s.startswith("//", self.i):
                j = s.find("\n", self.i)
                self.i = n if j < 0 else j + 1
            elif s.startswith("/*", self.i):
                j = s.find("*/", self.i + 2)
                self.i = n if j < 0 else j + 2
            else:
                return

    def string(self) -> str:
        start = self.i
        self.i += 1
        while self.s[self.i] != '"':
            self.i += 2 if self.s[self.i] == "\\" else 1
        self.i += 1
        return json.loads(self.s[start : self.i])

    def value(self, path: list, depth: int) -> tuple[str, int]:
        """Parses a value; returns (kind, end line)."""
        self.ws()
        c = self.s[self.i]
        if c == "{":
            self.i += 1
            members = 0
            while True:
                self.ws()
                if self.s[self.i] == "}":
                    self.i += 1
                    return ("map" if members else "scalar"), self.lines.line(self.i - 1)
                if self.s[self.i] == ",":
                    self.i += 1
                    continue
                kline = self.lines.line(self.i)
                members += 1
                key = self.string()
                self.ws()
                assert self.s[self.i] == ":", "expected ':'"
                self.i += 1
                rec = {"path": [*path, key], "name": key, "line": kline}
                kind, end = self.value([*path, key], depth + 1)
                rec["end_line"] = end
                if kind not in ("map", "seq-map") or depth >= MAX_DEPTH:
                    self.keys.append(rec)
        if c == "[":
            self.i += 1
            idx = 0
            has_map = False
            while True:
                self.ws()
                if self.s[self.i] == "]":
                    self.i += 1
                    return ("seq-map" if has_map else "seq"), self.lines.line(self.i - 1)
                if self.s[self.i] == ",":
                    self.i += 1
                    continue
                if self.s[self.i] == "{" and idx < MAX_ELEMENTS:
                    has_map = True
                    self.value([*path, idx], depth + 1)
                else:
                    save = len(self.keys)
                    self.value([*path, idx], depth + 1)
                    del self.keys[save:]
                idx += 1
        if c == '"':
            self.string()
            return "scalar", self.lines.line(self.i - 1)
        m = re.compile(r"-?[0-9][0-9.eE+-]*|true|false|null").match(self.s, self.i)
        assert m, f"unexpected {c!r}"
        self.i = m.end()
        return "scalar", self.lines.line(self.i - 1)


def json_keys(text: str) -> list[dict]:
    p = _Json(text)
    p.value([], 0)
    return p.keys


def yaml_keys(text: str) -> list[dict]:
    docs = [d for d in yaml.compose_all(text, Loader=yaml.SafeLoader) if d is not None]
    multi = len(docs) > 1
    keys: list[dict] = []
    seen: set[int] = set()

    def end_line(node) -> int:
        m = node.end_mark
        return m.line + (1 if m.column > 0 else 0)

    def walk(node, path: list, depth: int) -> None:
        if id(node) in seen:
            return
        seen.add(id(node))
        if isinstance(node, yaml.MappingNode):
            for k, v in node.value:
                if not isinstance(k, yaml.ScalarNode) or k.value == "<<":
                    continue
                rec = {"path": [*path, k.value], "name": k.value, "line": k.start_mark.line + 1, "end_line": max(k.start_mark.line + 1, end_line(v))}
                if id(v) in seen:  # alias: a leaf pointing at the anchor
                    keys.append(rec)
                    continue
                if isinstance(v, yaml.MappingNode) and v.value and depth < MAX_DEPTH:
                    walk(v, [*path, k.value], depth + 1)
                elif isinstance(v, yaml.SequenceNode) and any(isinstance(e, yaml.MappingNode) for e in v.value) and depth < MAX_DEPTH:
                    walk(v, [*path, k.value], depth + 1)
                else:
                    keys.append(rec)
        elif isinstance(node, yaml.SequenceNode):
            for i, e in enumerate(node.value[:MAX_ELEMENTS]):
                if isinstance(e, yaml.MappingNode):
                    walk(e, [*path, i], depth + 1)

    for i, d in enumerate(docs):
        walk(d, [f"{{{i}}}"] if multi else [], 0)
    return keys


_TOML_HEADER = re.compile(r"^\s*(\[\[?)\s*([^\]]+?)\s*\]\]?\s*(#.*)?$")
_TOML_KEY = re.compile(r"""^\s*((?:[A-Za-z0-9_-]+|"(?:[^"\\]|\\.)*"|'[^']*')(?:\s*\.\s*(?:[A-Za-z0-9_-]+|"(?:[^"\\]|\\.)*"|'[^']*'))*)\s*=""")


def _toml_split(dotted: str) -> list[str]:
    out = []
    for m in re.finditer(r"""[A-Za-z0-9_-]+|"((?:[^"\\]|\\.)*)"|'([^']*)'""", dotted):
        out.append(m.group(1) if m.group(1) is not None else m.group(2) if m.group(2) is not None else m.group(0))
    return out


def toml_keys(text: str) -> list[dict]:
    data = tomllib.loads(text)
    lines: dict[tuple, int] = {}
    table: list = []
    counters: dict[tuple, int] = {}
    in_ml = None
    depth = 0
    for no, raw in enumerate(text.splitlines(), 1):
        if in_ml:
            if in_ml in raw:
                in_ml = None
            continue
        if depth > 0:
            depth += raw.count("[") - raw.count("]")
            continue
        h = _TOML_HEADER.match(raw)
        if h and not _TOML_KEY.match(raw):
            parts = _toml_split(h.group(2))
            if h.group(1) == "[[":
                n = counters.get(tuple(parts), -1) + 1
                counters[tuple(parts)] = n
                table = [*parts, n]
            else:
                table = parts
            lines.setdefault(tuple(table), no)
            continue
        k = _TOML_KEY.match(raw)
        if k:
            path = tuple([*table, *_toml_split(k.group(1))])
            lines.setdefault(path, no)
            rest = raw[k.end():]
            for q in ('"""', "'''"):
                if rest.count(q) == 1:
                    in_ml = q
            if rest.strip().startswith("[") and rest.count("[") > rest.count("]"):
                depth = rest.count("[") - rest.count("]")
    keys: list[dict] = []

    def line_of(path: tuple) -> int | None:
        for n in range(len(path), 0, -1):
            if path[:n] in lines:
                return lines[path[:n]]
        return None

    def walk(node, path: list, depth_: int) -> None:
        for key, v in node.items():
            p = [*path, key]
            if isinstance(v, dict) and v and depth_ < MAX_DEPTH:
                walk(v, p, depth_ + 1)
            elif isinstance(v, list) and v and all(isinstance(e, dict) for e in v) and depth_ < MAX_DEPTH:
                for i, e in enumerate(v[:MAX_ELEMENTS]):
                    walk(e, [*p, i], depth_ + 1)
            else:
                ln = line_of(tuple(p))
                if ln is not None:
                    keys.append({"path": p, "name": key, "line": ln, "end_line": ln})

    walk(data, [], 0)
    return keys


def hcl_keys(path: Path) -> list[dict]:
    """Terraform / HCL from python-hcl2's syntax tree (it keeps lines):
    every attribute (`bucket = ...`, inside any block, `locals` included) and
    every labelled block (`resource "t" "n"` -> name `n`, `variable "x"` ->
    `x`, `module "m"` -> `m`) is a key; unlabelled blocks (`locals`,
    `lifecycle`, `ingress`) are containers only."""
    tree = hcl2.parses_to_tree(path.read_text())
    keys: list[dict] = []

    def text(node) -> str:
        if not hasattr(node, "data"):
            return str(node)
        return "".join(text(c) for c in node.children if hasattr(c, "data") or getattr(c, "type", "") in ("NAME", "STRING_CHARS"))

    def walk(node, path: list, depth: int) -> None:
        for c in getattr(node, "children", []):
            kind = getattr(c, "data", None)
            if kind == "block":
                parts = [ch for ch in c.children if getattr(ch, "data", None) in ("identifier", "string")]
                body = next((ch for ch in c.children if getattr(ch, "data", None) == "body"), None)
                names = [text(p) for p in parts]
                if len(names) > 1:
                    keys.append({"path": [*path, *names], "name": names[-1], "line": c.meta.line, "end_line": c.meta.end_line})
                if body is not None and depth < MAX_DEPTH:
                    walk(body, [*path, *names], depth + 1)
            elif kind == "attribute":
                ident = next((ch for ch in c.children if getattr(ch, "data", None) == "identifier"), None)
                if ident is not None:
                    name = text(ident)
                    keys.append({"path": [*path, name], "name": name, "line": c.meta.line, "end_line": c.meta.end_line})
            elif kind in ("body", "start"):
                walk(c, path, depth)

    walk(tree, [], 0)
    return keys


def reference_keys(root: Path, rel: str) -> dict:
    data = (root / rel).read_bytes()
    reason = skip_reason(rel, data)
    if reason:
        return {"path": rel, "format": data_format(rel), "skipped": reason, "keys": []}
    fmt = data_format(rel)
    text = data.decode("utf-8", errors="replace")
    try:
        keys = {"json": json_keys, "yaml": yaml_keys, "toml": toml_keys}[fmt](text) if fmt != "hcl" else hcl_keys(root / rel)
    except Exception as exc:  # unparseable for the reference: not in the reference set
        return {"path": rel, "format": fmt, "skipped": f"reference-parse-error: {str(exc)[:120]}", "keys": []}
    return {"path": rel, "format": fmt, "skipped": None, "keys": keys, "kind": file_kind(rel, text)}


def file_kind(rel: str, text: str) -> str | None:
    """Coarse kind of a data file (RFC 0003 §5), for the "kinds covered" column."""
    name = rel.rsplit("/", 1)[-1]
    if re.fullmatch(r"(docker-)?compose[\w.-]*\.ya?ml", name):
        return "compose"
    if rel.startswith(".github/workflows/") or name in ("action.yml", "action.yaml"):
        return "github-actions"
    if name.endswith((".yaml", ".yml")) and re.search(r"(?m)^apiVersion:", text) and re.search(r"(?m)^kind:", text):
        return "kubernetes"
    if name == "package.json":
        return "package.json"
    if name.endswith(".tf"):
        return "terraform"
    if name == "pyproject.toml":
        return "pyproject"
    if re.search(r'(?m)^\s*"?(openapi|swagger)"?\s*:', text[:2000]):
        return "openapi"
    return None


# --- gold -------------------------------------------------------------------------------


def build_data_gold(spec: dict, force: bool = False) -> dict:
    name = spec["name"]
    gp = gold_path(name)
    if gp.exists() and not force:
        return read_json(gp)
    ensure_commit(spec["repo"], spec["commit"])
    with worktree(spec["repo"], spec["commit"], f"data-{name}") as root:
        files = [f for f in list_files(root) if in_prefixes(f, spec["include"]) and data_format(f)]
        refs = [reference_keys(root, f) for f in files]
    labels = None
    lp = BENCH_DIR / spec["bridge"] if spec.get("bridge") else None
    if lp and lp.exists():
        labels = json.loads(lp.read_text())
    gold = {"name": name, "repo": spec["repo"], "commit": spec["commit"], "include": spec["include"], "files": refs, "bridge": labels}
    write_json(gp, gold)
    n_keys = sum(len(r["keys"]) for r in refs)
    log(f"data gold {name}: {len(refs)} data files, {sum(1 for r in refs if r['skipped'])} skipped, {n_keys} reference keys")
    return gold


# --- grep baseline for the bridge ----------------------------------------------------------


def grep_env_edges(root: Path, readers: list[str], providers: list[str]) -> dict:
    """Textual baseline: env API regexes over code (reader = innermost
    tree-sitter function, else `<module>`), and manifest lines that look like
    an env entry (`- name: X` followed by `value`, `- X=...`, or `X: ...`
    under an `environment:` block)."""
    reads, writes = [], []
    for rel in list_files(root):
        if in_prefixes(rel, readers) and rel.endswith(CODE_EXTS):
            pats = next(p for exts, p in ENV_READ_RES.items() if rel.endswith(exts))
            data = (root / rel).read_bytes()
            text = data.decode("utf-8", errors="replace")
            funcs = extract_functions(rel, data) if grammar_for(rel) else []
            for no, line in enumerate(text.splitlines(), 1):
                for pat in pats:
                    for m in pat.finditer(line):
                        f = innermost(funcs, no)
                        reads.append({"path": rel, "function": f.name if f else "<module>", "env": m.group(1), "line": no})
        if in_prefixes(rel, providers) and data_format(rel) == "yaml":
            lines = (root / rel).read_text(errors="replace").splitlines()
            env_block = None
            for no, line in enumerate(lines, 1):
                ind = len(line) - len(line.lstrip())
                if re.match(r"^\s*(environment|env):\s*$", line):
                    env_block = ind
                    continue
                if env_block is not None and line.strip() and ind <= env_block and not line.lstrip().startswith("-"):
                    env_block = None
                m = re.match(r"^\s*-\s*name:\s*[\"']?([A-Za-z_][A-Za-z0-9_]*)[\"']?\s*$", line)
                if m and env_block is not None:
                    writes.append({"path": rel, "env": m.group(1), "line": no})
                    continue
                if env_block is None:
                    continue
                m = re.match(r"^\s*-\s*[\"']?([A-Za-z_][A-Za-z0-9_]*)=", line) or re.match(r"^\s*[\"']?([A-Za-z_][A-Za-z0-9_]*)[\"']?\s*:\s*\S", line)
                if m and ind > env_block:
                    writes.append({"path": rel, "env": m.group(1), "line": no})
    return {"reads": reads, "writes": writes}


def candidate_env_names(root: Path, spec: dict, gold: dict) -> list[str]:
    """Names to ask a tool without an enumeration API about (Rueca's
    `trace` on `$env:NAME`): every gold name plus every UPPER_CASE token and
    env-looking assignment in scope (code, data files, Dockerfiles, `.env`),
    so spurious edges can be seen."""
    names = {r["env"] for r in (gold.get("bridge") or {}).get("reads", [])} | {w["env"] for w in (gold.get("bridge") or {}).get("writes", [])}
    br = spec.get("bridge_scope", {})
    scope = br.get("readers", []) + br.get("providers", [])
    tok = re.compile(
        r"""\b([A-Z][A-Z0-9_]{1,})\b|process\.env\.([A-Za-z_][A-Za-z0-9_]*)|^\s*ENV\s+([A-Za-z_][A-Za-z0-9_]*)|^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=""",
        re.M,
    )
    for rel in list_files(root):
        if not in_prefixes(rel, scope) or not (rel.endswith(CODE_EXTS) or data_format(rel) or rel.rsplit("/", 1)[-1].startswith((".env", "Dockerfile"))):
            continue
        try:
            text = (root / rel).read_text(errors="replace")
        except OSError:
            continue
        for m in tok.finditer(text):
            names.add(next(g for g in m.groups() if g))
    return sorted(names)


# --- run --------------------------------------------------------------------------------------


def run(arm_names: list[str], only: list[str] | None = None, force: bool = False) -> None:
    for spec in repos():
        name = spec["name"]
        if only and name not in only:
            continue
        if not gold_path(name).exists():
            log(f"data {name}: gold missing (run `rueca-bench setup --lang data`); skipped")
            continue
        gold = read_json(gold_path(name))
        files = [f["path"] for f in gold["files"]]
        br = spec.get("bridge_scope", {})
        for arm_name in [*arm_names]:
            p = run_path(arm_name, name)
            if p.exists() and not force and read_json(p).get("status") in ("ok", "index_failed"):
                continue
            log(f"data {arm_name:5s} {name}")
            rec: dict = {"arm": arm_name, "name": name}
            try:
                with worktree(spec["repo"], spec["commit"], f"data-{name}") as root:
                    if arm_name == "grep":
                        rec["bridge"] = grep_env_edges(root, br.get("readers", []), br.get("providers", []))
                        rec["status"] = "ok"
                    else:
                        arm = make_arm(arm_name)
                        if not arm.available()[0]:
                            continue
                        h = arm.index(root, f"data-{name}", "data")
                        rec["index_seconds"] = h.index_seconds
                        if not h.ok:
                            rec.update(status="index_failed", error=h.error)
                        else:
                            try:
                                nodes = arm.data_nodes(h, files)
                                rec["nodes"] = nodes.hits if nodes.ok else []
                                rec["nodes_error"] = nodes.error
                                edges = arm.env_edges(h, candidate_env_names(root, spec, gold))
                                rec["bridge"] = edges.extra if edges.ok else {"reads": [], "writes": []}
                                rec["bridge_error"] = edges.error
                                rec["status"] = "ok"
                            finally:
                                arm.cleanup(h)
                        rec["arm_version"] = arm.version()
            except Exception as exc:
                rec.update(status="error", error=f"{exc}\n{traceback.format_exc()[-1500:]}")
            if arm_name == "grep":
                rec["arm_version"] = "rueca-bench env regexes + tree-sitter"
            write_json(p, rec)


# --- scoring -------------------------------------------------------------------------------------


def _same_key_name(node_name: str, key: str) -> bool:
    """A node names a key when its name is the key, or ends with it after a
    `.` / `/` / `:` separator (tools that use a qualified path as the name,
    e.g. cbm's `resource.aws_s3_bucket.logs` for block `logs`)."""
    return node_name == key or any(node_name.endswith(sep + key) for sep in (".", "/", ":"))


def score_keys(gold: dict, nodes: list[dict]) -> dict:
    """Per format: reference keys, covered (same file, same name, node start
    inside the key's span), exact (node starts on the key line)."""
    by_file: dict[str, list[dict]] = {}
    for n in nodes:
        if n.get("path") and n.get("start_line") is not None:
            by_file.setdefault(n["path"], []).append(n)
    out: dict[str, dict] = {}
    for f in gold["files"]:
        if f["skipped"]:
            continue
        cands = by_file.get(f["path"], [])
        used: set[int] = set()
        o = out.setdefault(f["format"], {"keys": 0, "covered": 0, "exact": 0, "files": 0})
        o["files"] += 1
        for k in f["keys"]:
            o["keys"] += 1
            best = None
            for i, n in enumerate(cands):
                if i in used or not _same_key_name(str(n.get("name")), str(k["name"])):
                    continue
                ln = n["start_line"]
                if k["line"] <= ln <= k["end_line"] and (best is None or abs(ln - k["line"]) < abs(cands[best]["start_line"] - k["line"])):
                    best = i
            if best is not None:
                used.add(best)
                o["covered"] += 1
                o["exact"] += int(cands[best]["start_line"] == k["line"])
    for o in out.values():
        o["recall"] = o["covered"] / o["keys"] if o["keys"] else None
        o["span_accuracy"] = o["exact"] / o["covered"] if o["covered"] else None
    return out


def kinds_covered(gold: dict, nodes: list[dict]) -> dict[str, dict]:
    """Per kind present in the corpus: files of that kind, and files where the
    arm emitted a kind node (`Resource`, `Image`, `EnvVar`, `Route`)."""
    kind_labels = {"Resource", "Image", "EnvVar", "Route"}
    with_kind = {n["path"] for n in nodes if n.get("label") in kind_labels and n.get("path")}
    out: dict[str, dict] = {}
    for f in gold["files"]:
        if f.get("skipped") or not f.get("kind"):
            continue
        o = out.setdefault(f["kind"], {"format": f["format"], "files": 0, "with_kind_nodes": 0})
        o["files"] += 1
        o["with_kind_nodes"] += int(f["path"] in with_kind)
    return out


def _read_key(r: dict) -> tuple:
    return (r["path"], r.get("function") or "<module>", r["env"])


def score_bridge(gold: dict, spec: dict, pred: dict) -> dict:
    labels = gold.get("bridge") or {}
    br = spec.get("bridge_scope", {})
    readers, providers = br.get("readers", []), br.get("providers", [])
    g_reads = {_read_key(r) for r in labels.get("reads", [])}
    p_reads = {_read_key(r) for r in pred.get("reads", []) if in_prefixes(r["path"], readers) and r["path"].endswith(CODE_EXTS)}
    tp_r = len(g_reads & p_reads)
    g_writes = [(w["path"], w["env"], w["line"]) for w in labels.get("writes", [])]
    p_writes = sorted({(w["path"], w["env"], w["line"]) for w in pred.get("writes", []) if in_prefixes(w["path"], providers)})
    matched_g: set[int] = set()
    tp_w = 0
    for pw in p_writes:
        for gi, gw in enumerate(g_writes):
            if gi not in matched_g and gw[0] == pw[0] and gw[1] == pw[1] and abs(gw[2] - pw[2]) <= 1:
                matched_g.add(gi)
                tp_w += 1
                break

    def pr(tp, n_pred, n_gold):
        p = tp / n_pred if n_pred else None
        r = tp / n_gold if n_gold else None
        return {"tp": tp, "pred": n_pred, "gold": n_gold, "precision": p, "recall": r, "f1": (2 * p * r / (p + r)) if p and r else 0.0}

    return {
        "reads": pr(tp_r, len(p_reads), len(g_reads)),
        "writes": pr(tp_w, len(p_writes), len(g_writes)),
        "all": pr(tp_r + tp_w, len(p_reads) + len(p_writes), len(g_reads) + len(g_writes)),
        "reads_missed": sorted("|".join(k) for k in g_reads - p_reads)[:50],
        "reads_spurious": sorted("|".join(k) for k in p_reads - g_reads)[:50],
    }


def _pool(out: dict, pooled_keys: dict, pooled_bridge: dict, arm: str, res: dict) -> None:
    for fmt, o in (res.get("keys") or {}).items():
        a = pooled_keys.setdefault(fmt, {}).setdefault(arm, {"keys": 0, "covered": 0, "exact": 0, "files": 0})
        for k in a:
            a[k] += o[k]
    for kind, o in (res.get("kinds") or {}).items():
        a = out["kinds"].setdefault(kind, {}).setdefault(arm, {"format": o["format"], "files": 0, "with_kind_nodes": 0})
        a["files"] += o["files"]
        a["with_kind_nodes"] += o["with_kind_nodes"]
    if res.get("bridge"):
        pb = pooled_bridge.setdefault(arm, {"reads": [0, 0, 0], "writes": [0, 0, 0]})
        for d in ("reads", "writes"):
            pb[d][0] += res["bridge"][d]["tp"]
            pb[d][1] += res["bridge"][d]["pred"]
            pb[d][2] += res["bridge"][d]["gold"]


def score(published: dict | None = None) -> dict:
    """{repos: {name: {arm: {...}}}, formats: {fmt: {arm: {...}}}, bridge: {arm: pooled}, gold: {...}}.
    An (arm, repository) without a local run keeps its entry from the
    published `bench/results/data.json` (`published`), so a Rueca-only run
    (the CI regression check) still has the baseline arms."""
    published = published or {}
    pub_repos = published.get("repos", {})
    out: dict = {"repos": {}, "formats": {}, "bridge": {}, "kinds": {}, "gold": {}, "failures": []}
    pooled_keys: dict = {}
    pooled_bridge: dict = {}
    for spec in repos():
        name = spec["name"]
        gp = gold_path(name)
        if not gp.exists():
            out["gold"][name] = published.get("gold", {}).get(name) or {"missing": True}
            carried = pub_repos.get(name, {})
            for arm, res in carried.items():
                _pool(out, pooled_keys, pooled_bridge, arm, dict(res, carried=True))
                out["repos"].setdefault(name, {})[arm] = dict(res, carried=True)
            continue
        gold = read_json(gp)
        refs = [f for f in gold["files"] if not f["skipped"]]
        out["gold"][name] = {
            "repo": gold["repo"],
            "commit": gold["commit"],
            "data_files": len(gold["files"]),
            "skipped": {r: sum(1 for f in gold["files"] if (f["skipped"] or "").split(":")[0] == r) for r in sorted({(f["skipped"] or "").split(":")[0] for f in gold["files"] if f["skipped"]})},
            "reference_keys": {fmt: sum(len(f["keys"]) for f in refs if f["format"] == fmt) for fmt in sorted({f["format"] for f in refs})},
            "kinds": sorted({f["kind"] for f in refs if f.get("kind")}),
            "bridge_reads": len((gold.get("bridge") or {}).get("reads", [])),
            "bridge_writes": len((gold.get("bridge") or {}).get("writes", [])),
        }
        for arm in ("grep", "cbm", "rueca"):
            rp = run_path(arm, name)
            if not rp.exists():
                res = pub_repos.get(name, {}).get(arm)
                if res is not None:
                    _pool(out, pooled_keys, pooled_bridge, arm, dict(res, carried=True))
                    out["repos"].setdefault(name, {})[arm] = dict(res, carried=True)
                continue
            rec = read_json(rp)
            if rec.get("status") != "ok":
                out["failures"].append({"arm": arm, "repo": name, "error": str(rec.get("error"))[:300]})
            res: dict = {"arm_version": rec.get("arm_version"), "index_seconds": rec.get("index_seconds")}
            if arm != "grep":
                res["keys"] = score_keys(gold, rec.get("nodes", []))
                res["kinds"] = kinds_covered(gold, rec.get("nodes", []))
            if gold.get("bridge") and rec.get("bridge") is not None:
                res["bridge"] = score_bridge(gold, spec, rec["bridge"])
            _pool(out, pooled_keys, pooled_bridge, arm, res)
            out["repos"].setdefault(name, {})[arm] = res
    for fmt, arms in pooled_keys.items():
        for arm, a in arms.items():
            a["recall"] = a["covered"] / a["keys"] if a["keys"] else None
            a["span_accuracy"] = a["exact"] / a["covered"] if a["covered"] else None
        out["formats"][fmt] = arms
    for arm, pb in pooled_bridge.items():
        res = {}
        tot = [0, 0, 0]
        for d in ("reads", "writes"):
            tp, npred, ngold = pb[d]
            tot = [tot[0] + tp, tot[1] + npred, tot[2] + ngold]
            res[d] = {"tp": tp, "pred": npred, "gold": ngold, "precision": tp / npred if npred else None, "recall": tp / ngold if ngold else None}
        tp, npred, ngold = tot
        p, r = (tp / npred if npred else None), (tp / ngold if ngold else None)
        res["all"] = {"tp": tp, "pred": npred, "gold": ngold, "precision": p, "recall": r, "f1": (2 * p * r / (p + r)) if p and r else 0.0}
        out["bridge"][arm] = res
    return out
