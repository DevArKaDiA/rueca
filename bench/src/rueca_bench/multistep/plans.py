"""Reference tool-call plans: the calls a competent agent makes for each task,
using only what the compact text answers say (`rueca query ... --text`, the
MCP text content). A plan is a function of an `agent` whose
`call(tool, args)` returns that text; later calls may depend on what earlier
answers said. The parsers below read the text renderings (`docs/tools.md`).

Plans use the arguments a good agent would: a `limit` high enough not to be
cut (an agent that sees `(truncated …)` asks again with a larger one, which
costs more), `labels` / `path` narrowing where the tools take them, and the
one call that answers the question when there is one (a depth-2 `trace`
instead of a trace per caller).
"""

from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Protocol

FUNCS = ("Function", "Method")
# Exhaustive questions read whole answers: the tools' default text budget
# would cut them (`(truncated …)`, then a second page), so plans ask for the
# whole answer up front (the same tokens as paging, fewer round trips).
WHOLE = {"budget_tokens": 50000}


class Agent(Protocol):
    def call(self, tool: str, args: dict[str, Any]) -> str: ...


@dataclass(frozen=True)
class Node:
    idx: int
    snap: int
    dir: str | None
    depth: int
    label: str
    qn: str
    path: str
    target: int | None

    @property
    def fid(self) -> str:
        return f"{self.path}#{self.qn}"


_NODE = re.compile(r"^(\d+) (?:\[(\d+)\] )?(?:(in|out|both) )?(?:d(\d+) )?(\w+) (.+?)  (\S+) L\d+(?:-\d+)?(.*)$")
_ARROW = re.compile(r"[→←](\d+) ")


def trace_nodes(text: str) -> list[Node]:
    """Every node line of a `trace` text answer (the root first)."""
    out = []
    for line in text.splitlines():
        m = _NODE.match(line)
        if not m:
            continue
        idx, snap, dir_, depth, label, qn, path, rest = m.groups()
        arrow = _ARROW.search(rest)
        out.append(Node(int(idx), int(snap or 0), dir_, int(depth or 0), label, qn, path, int(arrow.group(1)) if arrow else None))
    return out


def snapshots(text: str) -> dict[int, str]:
    """`[s] repo@commit (ref)` header lines -> {s: repo}."""
    out = {}
    for line in text.splitlines():
        m = re.match(r"^\[(\d+)\] (\S+?)@[0-9a-f]+\+? \(", line)
        if m:
            out[int(m.group(1))] = m.group(2)
    return out


def neighbours(text: str, direction: str, prefix: str, depth: int = 1) -> list[Node]:
    """Function / method neighbours at `depth` in `direction` (`in` / `out`)
    whose file is under `prefix`."""
    return [
        n for n in trace_nodes(text) if n.dir in (direction, "both") and n.depth == depth and n.label in FUNCS and n.path.startswith(prefix) and not is_test_file(n.path)
    ]


def fids(nodes: list[Node]) -> set[str]:
    return {n.fid for n in nodes}


@dataclass(frozen=True)
class OutlineSym:
    indent: int
    label: str
    qn: str
    start: int
    end: int

    def fid(self, path: str) -> str:
        return f"{path}#{self.qn}"


_OUT = re.compile(r"^( +)(\w+) (\S+) L(\d+)(?:-(\d+))?")


def outline_versions(text: str) -> list[list[OutlineSym]]:
    """Symbol lines of a `get_file_outline` answer, one list per version
    (one list for a single ref)."""
    versions: list[list[OutlineSym]] = [[]]
    for line in text.splitlines():
        if line.startswith("version "):
            versions.append([])
            continue
        m = _OUT.match(line)
        if m:
            ind, label, qn, s, e = m.groups()
            versions[-1].append(OutlineSym(len(ind), label, qn, int(s), int(e or s)))
    return versions


def outline_funcs(text: str) -> list[OutlineSym]:
    return [s for s in outline_versions(text)[0] if s.label in FUNCS]


@dataclass(frozen=True)
class Entry:
    key: str
    side: str
    framework: str
    repo: str
    path: str
    symbol: str


_CONTRACT = re.compile(r"^(.+?)  (provides|consumes)  (\S+)  (\S+?):(\S+?)(?:#(.+?))? L\d+  \(\d+\)$")


def contracts(text: str) -> list[Entry]:
    out = []
    for line in text.splitlines():
        m = _CONTRACT.match(line)
        if m:
            key, side, fw, repo, path, sym = m.groups()
            out.append(Entry(key, side, fw, repo, path, sym or ""))
    return out


def is_test_file(path: str) -> bool:
    return bool(re.search(r"(^|/)(tests?|__tests__)/|\.test\.|_test\.|(^|/)test_", path))


# --- helpers shared by plans ----------------------------------------------------------


def _trace(agent: Agent, root: dict, direction: str, **extra: Any) -> str:
    return agent.call("trace", {**root, "direction": direction, **extra, **WHOLE})


def callers(agent: Agent, root: dict, prefix: str, **extra: Any) -> set[str]:
    return fids(neighbours(_trace(agent, root, "callers", limit=200, **extra), "in", prefix))


def callees(agent: Agent, root: dict, prefix: str) -> set[str]:
    return fids(neighbours(_trace(agent, root, "callees", limit=200), "out", prefix))


def by_id(fid: str) -> dict:
    """A trace root for an id read from an earlier answer: `name` + `path`
    (what the text shows), or the qualified name for a method."""
    path, qn = fid.split("#", 1)
    return {"qualified_name": qn, "path": path} if "." in qn else {"name": qn, "path": path}


def q(qn: str) -> dict:
    return {"qualified_name": qn}


def fn(name: str, path: str) -> dict:
    return {"name": name, "path": path}


# --- the plans ----------------------------------------------------------------------------

FL, RQ, HN = "src/flask/", "src/requests/", "src/"


def ms01(agent: Agent) -> list[str]:
    text = _trace(agent, q("Flask.ensure_sync"), "callers", depth=2, limit=200)
    nodes = [n for n in trace_nodes(text) if n.depth in (1, 2) and n.label in FUNCS and n.path.startswith(FL)]
    return sorted(fids(nodes) - {"src/flask/app.py#Flask.ensure_sync"})


def ms02(agent: Agent) -> list[str]:
    return sorted(callers(agent, q("Flask.ensure_sync"), FL) & callers(agent, q("App._find_error_handler"), FL))


def ms03(agent: Agent) -> list[str]:
    return sorted(callees(agent, q("Flask.handle_user_exception"), FL) & callees(agent, q("Flask.handle_exception"), FL))


def ms04(agent: Agent) -> list[str]:
    return sorted(callers(agent, q("Flask.handle_exception"), FL))


def _per_member(agent: Agent, path: str, keep: Callable[[OutlineSym], bool], prefix: str) -> dict[str, int]:
    text = agent.call("get_file_outline", {"path": path, **WHOLE})
    out = {}
    for s in outline_funcs(text):
        if keep(s):
            out[s.fid(path)] = len(callers(agent, by_id(s.fid(path)), prefix))
    return out


def ms05(agent: Agent) -> dict[str, int]:
    return _per_member(agent, "src/flask/ctx.py", lambda s: s.label == "Method" and s.qn.count(".") == 1 and s.qn.startswith("RequestContext."), FL)


def ms06(agent: Agent) -> list[str]:
    first = callees(agent, q("Flask.wsgi_app"), FL)
    return sorted(f for f in first if not callees(agent, by_id(f), FL))


def ms07(agent: Agent) -> list[list]:
    counts = _per_member(agent, "src/requests/cookies.py", lambda s: True, RQ)
    ranked = sorted(counts.items(), key=lambda x: (-x[1], x[0]))
    return [[f, c] for f, c in ranked[:3]]


def ms08(agent: Agent) -> list[str]:
    counts = _per_member(agent, "src/requests/auth.py", lambda s: True, RQ)
    return sorted(f for f, c in counts.items() if c == 0)


def ms09(agent: Agent) -> int:
    text = _trace(agent, q("Session.request"), "callers", depth=2, limit=200)
    nodes = [n for n in trace_nodes(text) if n.label in FUNCS and n.path.startswith(RQ)]
    first = {n.fid for n in nodes if n.depth == 1}
    second = {n.fid for n in nodes if n.depth == 2}
    return len(second - first - {"src/requests/sessions.py#Session.request"})


def ms10(agent: Agent) -> list[str]:
    found = callers(agent, fn("to_native_string", "src/requests/_internal_utils.py"), RQ)
    return sorted(f for f in found if not f.startswith("src/requests/utils.py#"))


def ms11(agent: Agent) -> list[str]:
    return sorted(callees(agent, q("HTTPDigestAuth.handle_401"), RQ) & callees(agent, q("SessionRedirectMixin.resolve_redirects"), RQ))


_SEARCH_LINE = re.compile(r"^  (\w+) (\S+) L\d+")


def ms12(agent: Agent) -> dict[str, int]:
    path = "src/requests/utils.py"
    text = agent.call("search_symbols", {"query": "proxy", "path_glob": path, "labels": list(FUNCS), "limit": 50})
    out = {}
    for line in text.splitlines():
        m = _SEARCH_LINE.match(line)
        if m and "proxy" in m.group(2).rsplit(".", 1)[-1].lower():
            fid = f"{path}#{m.group(2)}"
            out[fid] = len(callers(agent, by_id(fid), RQ))
    return out


def ms13(agent: Agent) -> list[str]:
    return sorted(callers(agent, fn("renderChildren", "src/jsx/base.ts"), HN) & callers(agent, fn("isUntrustedObject", "src/jsx/base.ts"), HN))


def ms14(agent: Agent) -> list[str]:
    return sorted(callees(agent, fn("verify", "src/utils/jwt/jwt.ts"), HN) & callees(agent, fn("verifyWithJwks", "src/utils/jwt/jwt.ts"), HN))


def ms15(agent: Agent) -> list:
    counts = {f: len(callers(agent, by_id(f), HN)) for f in callees(agent, fn("verify", "src/utils/jwt/jwt.ts"), HN)}
    best = min(counts.items(), key=lambda x: (-x[1], x[0]))
    return [best[0], best[1]]


def ms16(agent: Agent) -> list[str]:
    return sorted(f for f in callers(agent, fn("raw", "src/utils/html.ts"), HN) if f.startswith("src/jsx/"))


# --- organization ----------------------------------------------------------------------------

FLASK_REPO = "github.com/pallets/flask"
USE_EDGES = ["CALLS", "INHERITS", "REFERENCES", "THROWS", "DECORATES"]


def flask_files(text: str) -> list[str]:
    snaps = snapshots(text)
    return sorted({n.path for n in trace_nodes(text) if n.dir == "in" and snaps.get(n.snap) == FLASK_REPO and n.path.startswith(FL)})


def ms17(agent: Agent) -> list[str]:
    return flask_files(agent.call("trace", {"qualified_name": "click.utils.echo", "direction": "callers", "limit": 500, **WHOLE}))


def ms18(agent: Agent) -> dict[str, list[str]]:
    names = ["werkzeug.http.http_date", "werkzeug.utils.import_string", "werkzeug.serving.is_running_from_reloader"]
    return {n: flask_files(agent.call("trace", {"qualified_name": n, "direction": "callers", "limit": 500, **WHOLE})) for n in names}


def ms19(agent: Agent) -> dict[str, list[str]]:
    names = ["werkzeug.exceptions.HTTPException", "werkzeug.exceptions.NotFound", "werkzeug.exceptions.BadRequest"]
    return {n: flask_files(agent.call("trace", {"qualified_name": n, "direction": "callers", "edge_types": USE_EDGES, "limit": 1000, **WHOLE})) for n in names}


def ms20(agent: Agent) -> list[str]:
    text = agent.call("list_repositories", {})
    for line in text.splitlines():
        if re.match(rf"^\[\d+\] {re.escape(FLASK_REPO)}  ", line) and "depends on " in line:
            return sorted(x.strip() for x in line.split("depends on ", 1)[1].split(","))
    return []


OLLAMA, PY, JS = "github.com/ollama/ollama", "github.com/ollama/ollama-python", "github.com/ollama/ollama-js"


def consumed_keys(agent: Agent, repo: str) -> set[str]:
    text = agent.call("list_contracts", {"side": "consumed", "repo": repo, "kind": "http", "limit": 200, **WHOLE})
    return {e.key for e in contracts(text) if not is_test_file(e.path)}


def provided(agent: Agent) -> list[Entry]:
    return contracts(agent.call("list_contracts", {"side": "provided", "repo": OLLAMA, "kind": "http", "limit": 500, **WHOLE}))


def code_routes(agent: Agent) -> set[str]:
    return {e.key for e in provided(agent) if e.framework != "openapi"}


def ms21(agent: Agent) -> list[str]:
    return sorted(consumed_keys(agent, PY) & consumed_keys(agent, JS) & code_routes(agent))


def ms22(agent: Agent) -> list[str]:
    return sorted((consumed_keys(agent, PY) - consumed_keys(agent, JS)) & code_routes(agent))


def ms23(agent: Agent) -> int:
    used = consumed_keys(agent, JS)
    ops = {(e.path, e.symbol) for e in provided(agent) if e.framework == "openapi" and e.key in used}
    return len(ops)


def ms24(agent: Agent) -> list[str]:
    text = agent.call("list_contracts", {"side": "provided", "query": "/api/tags"})
    return sorted({f"{e.path}#{e.symbol}" for e in contracts(text) if e.key == "GET /api/tags" and e.framework != "openapi"})


# --- multi-ref -------------------------------------------------------------------------------

BASE, FIX = "091991be0da1", "fa64e474afe1"


def snippet_lines(text: str) -> list[str]:
    """The code lines of a `get_snippet` text answer (after its header)."""
    lines = text.split("\n")
    start = next(i for i, ln in enumerate(lines) if re.match(r"^\S+ L\d+-\d+$", ln)) + 1
    return lines[start:]


def ms25(agent: Agent) -> list[str]:
    path = "requests/sessions.py"
    versions = outline_versions(agent.call("get_file_outline", {"path": path, "ref": [BASE, FIX], **WHOLE}))
    base, fix = versions[0], versions[-1]
    texts = [snippet_lines(agent.call("get_snippet", {"path": path, "start_line": 1, "max_lines": 2000, "ref": r, **WHOLE})) for r in (BASE, FIX)]
    return diff_functions(path, base, fix, texts[0], texts[1])


def diff_functions(path: str, base: list[OutlineSym], fix: list[OutlineSym], a: list[str], b: list[str]) -> list[str]:
    old = {s.qn: a[s.start - 1 : s.end] for s in base if s.label in FUNCS}
    new = {s.qn: b[s.start - 1 : s.end] for s in fix if s.label in FUNCS}
    return sorted(f"{path}#{qn}" for qn in set(old) | set(new) if old.get(qn) != new.get(qn))


PLANS: dict[str, Callable[[Agent], Any]] = {name: obj for name, obj in globals().items() if re.fullmatch(r"ms\d\d", name)}
