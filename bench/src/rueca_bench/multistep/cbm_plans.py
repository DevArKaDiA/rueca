"""Reference cbm plans (codebase-memory-mcp 0.12.0, the bench's `cbm` arm):
the same tasks answered with cbm's tools, reading only cbm's default text
renderings. cbm's strongest shape is one `query_graph` (its Cypher subset)
that does the join, so every plan is a single query when the subset can
express it: one `MATCH` with a path pattern (`(b)<-[:CALLS]-(a)-[:CALLS]->(c)`
joins two neighbourhoods), `OPTIONAL
MATCH` and `count(DISTINCT …)`; it has no second `MATCH`, no `WHERE` after
`OPTIONAL MATCH`, no `NOT` pattern and no case-insensitive match, so set
differences and filters happen on the rows. The statement names the file
and the function, so the plan filters by `file_path` and the end of
`qualified_name` (`<project>.<path as dots>.<Qualname>`) without a lookup
call. Scope (the statement's directory, non-test files, functions and
methods) is applied to the rows, as an agent would.

Tasks cbm cannot express are listed in `CBM_SKIP` with the reason.
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from typing import Any

from ..arms.cbm import CbmArm
from ..common import count_tokens, run
from .plans import FUNCS, is_test_file

CBM_SKIP = {
    **dict.fromkeys(("ms17", "ms18", "ms19", "ms20"), "cbm indexes one project per repository and has no cross-project edges, queries or dependency view"),
    **dict.fromkeys(("ms21", "ms22", "ms23", "ms24"), "cbm has no service-contract model across projects (routes matched to the clients of other repositories)"),
    "ms25": "cbm indexes one working tree per project: no refs, no comparison of two commits",
}


class CbmAgent:
    """cbm's CLI (the bench's warm daemon) returning its default text."""

    def __init__(self, arm: CbmArm, project: str, record: Any):
        self.arm = arm
        self.project = project
        self.rec = record

    def call(self, tool: str, args: dict[str, Any]) -> str:
        full = {"project": self.project, **args}
        self.arm._ensure_daemon()
        res = run([str(self.arm.bin), "cli", "--quiet", tool, json.dumps(full)], env=self.arm.env, timeout=self.arm.call_timeout)
        text = res.stdout if res.rc == 0 else (res.stdout or res.stderr)
        self.rec.calls += 1
        self.rec.read += count_tokens(text) or 0
        self.rec.written += count_tokens(json.dumps(args, separators=(",", ":"))) or 0
        self.rec.seconds += res.seconds
        self.rec.truncated |= "truncated: true" in text or "has_more: true" in text
        self.rec.timeout |= res.timed_out
        self.rec.trace.append({"tool": tool, "args": args, "tokens": count_tokens(text) or 0})
        if res.rc != 0:
            raise RuntimeError(f"cbm {tool}: {text.strip()[-300:]}")
        return text


def rows(text: str) -> list[list[str]]:
    """Rows of a `query_graph` text answer, `@N+suffix` references expanded
    (`rows_refs`), quoted values unquoted."""
    refs: dict[str, str] = {}
    out: list[list[str]] = []
    section = None
    for line in text.splitlines():
        if not line.startswith("  "):
            section = line.split(":", 1)[0]
            continue
        cells = line.strip().split(" ")
        if section == "rows_refs":
            refs[cells[0]] = cells[1] if len(cells) > 1 else ""
        elif section == "rows":
            out.append([_cell(c, refs) for c in cells])
    return out


def _cell(c: str, refs: dict[str, str]) -> str:
    m = re.fullmatch(r"@(\d+)\+(.*)", c)
    if m:
        return refs.get(m.group(1), "") + m.group(2)
    return c[1:-1] if len(c) >= 2 and c[0] == c[-1] == '"' else c


def local(qn: str, path: str) -> str:
    """`<project>.src.flask.app.Flask.wsgi_app` + `src/flask/app.py` -> `Flask.wsgi_app`."""
    mod = re.sub(r"\.[^./]+$", "", path).replace("/", ".")
    i = qn.find("." + mod + ".")
    return qn[i + len(mod) + 2 :] if i >= 0 else qn.rsplit(".", 1)[-1]


def fid(qn: str, path: str) -> str:
    return f"{path}#{local(qn, path)}"


def in_scope(path: str, label: str, prefix: str) -> bool:
    return path.startswith(prefix) and not is_test_file(path) and label in FUNCS


def target(var: str, path: str, qn: str) -> str:
    return f'{var}.file_path = "{path}" AND {var}.qualified_name ENDS WITH ".{qn}"'


def query(agent: CbmAgent, cypher: str) -> list[list[str]]:
    return rows(agent.call("query_graph", {"query": cypher, "max_rows": 5000}))


def neighbour_ids(agent: CbmAgent, cypher: str, prefix: str) -> list[str]:
    """Rows `qualified_name, file_path, label` -> in-scope ids."""
    return sorted({fid(q, p) for q, p, lb in query(agent, cypher) if in_scope(p, lb, prefix)})


RET = "RETURN DISTINCT {v}.qualified_name, {v}.file_path, {v}.label"
FL, RQ, HN = "src/flask/", "src/requests/", "src/"


def _callers(path: str, qn: str) -> str:
    return f"MATCH (a)-[:CALLS]->(b) WHERE {target('b', path, qn)} " + RET.format(v="a")


def _common_callers(p1: str, q1: str, p2: str, q2: str) -> str:
    return f"MATCH (b)<-[:CALLS]-(a)-[:CALLS]->(c) WHERE {target('b', p1, q1)} AND {target('c', p2, q2)} " + RET.format(v="a")


def _common_callees(p1: str, q1: str, p2: str, q2: str) -> str:
    return f"MATCH (x)-[:CALLS]->(b)<-[:CALLS]-(y) WHERE {target('x', p1, q1)} AND {target('y', p2, q2)} " + RET.format(v="b")


def _two_hop(path: str, qn: str) -> str:
    """Callers at exactly two hops. cbm 0.12.0 answers `[:CALLS*1..2]` and
    `[:CALLS*2]` with the direct callers only, so the hop is spelled out."""
    return f"MATCH (a)-[:CALLS]->(m)-[:CALLS]->(b) WHERE {target('b', path, qn)} " + RET.format(v="a")


def ms01(agent: CbmAgent) -> list[str]:
    first = neighbour_ids(agent, _callers("src/flask/app.py", "Flask.ensure_sync"), FL)
    second = neighbour_ids(agent, _two_hop("src/flask/app.py", "Flask.ensure_sync"), FL)
    return sorted(set(first + second) - {"src/flask/app.py#Flask.ensure_sync"})


def ms02(agent: CbmAgent) -> list[str]:
    return neighbour_ids(agent, _common_callers("src/flask/app.py", "Flask.ensure_sync", "src/flask/sansio/app.py", "App._find_error_handler"), FL)


def ms03(agent: CbmAgent) -> list[str]:
    return neighbour_ids(agent, _common_callees("src/flask/app.py", "Flask.handle_user_exception", "src/flask/app.py", "Flask.handle_exception"), FL)


def ms04(agent: CbmAgent) -> list[str]:
    return neighbour_ids(agent, _callers("src/flask/app.py", "Flask.handle_exception"), FL)


def _caller_rows(agent: CbmAgent, where: str) -> dict[str, set[str]]:
    """Members matched by `where` (on `b`) -> their in-scope callers' ids
    (one row per member and caller, `OPTIONAL MATCH`)."""
    cy = f"MATCH (b) WHERE {where} OPTIONAL MATCH (a)-[:CALLS]->(b) RETURN b.qualified_name, b.file_path, b.label, a.qualified_name, a.file_path, a.label"
    out: dict[str, set[str]] = {}
    for r in query(agent, cy):
        bq, bp, bl = r[0], r[1], r[2]
        if bl not in FUNCS:
            continue
        callers = out.setdefault(fid(bq, bp), set())
        if len(r) >= 6 and in_scope(r[4], r[5], _prefix_of(bp)):
            callers.add(fid(r[3], r[4]))
    return out


def _prefix_of(path: str) -> str:
    return FL if path.startswith(FL) else RQ if path.startswith(RQ) else HN


def ms05(agent: CbmAgent) -> dict[str, int]:
    found = _caller_rows(agent, 'b.file_path = "src/flask/ctx.py" AND b.label = "Method" AND b.qualified_name CONTAINS ".ctx.RequestContext."')
    return {k: len(v) for k, v in sorted(found.items()) if k.count(".") == 2}


def ms06(agent: CbmAgent) -> list[str]:
    cy = f"MATCH (f)-[:CALLS]->(b) WHERE {target('f', 'src/flask/app.py', 'Flask.wsgi_app')} OPTIONAL MATCH (b)-[:CALLS]->(c) RETURN b.qualified_name, b.file_path, b.label, c.file_path, c.label"
    calls: dict[str, bool] = {}
    for r in query(agent, cy):
        if in_scope(r[1], r[2], FL):
            key = fid(r[0], r[1])
            calls[key] = calls.get(key, False) or (len(r) >= 5 and in_scope(r[3], r[4], FL))
    return sorted(k for k, v in calls.items() if not v)


def _counts_in_file(agent: CbmAgent, path: str) -> dict[str, int]:
    return {k: len(v) for k, v in _caller_rows(agent, f'b.file_path = "{path}"').items()}


def ms07(agent: CbmAgent) -> list[list]:
    ranked = sorted(_counts_in_file(agent, "src/requests/cookies.py").items(), key=lambda x: (-x[1], x[0]))
    return [[k, c] for k, c in ranked[:3]]


def ms08(agent: CbmAgent) -> list[str]:
    return sorted(k for k, c in _counts_in_file(agent, "src/requests/auth.py").items() if c == 0)


def ms09(agent: CbmAgent) -> int:
    first = set(neighbour_ids(agent, _callers("src/requests/sessions.py", "Session.request"), RQ))
    second = set(neighbour_ids(agent, _two_hop("src/requests/sessions.py", "Session.request"), RQ))
    return len(second - first - {"src/requests/sessions.py#Session.request"})


def ms10(agent: CbmAgent) -> list[str]:
    found = neighbour_ids(agent, _callers("src/requests/_internal_utils.py", "to_native_string"), RQ)
    return [f for f in found if not f.startswith("src/requests/utils.py#")]


def ms11(agent: CbmAgent) -> list[str]:
    return neighbour_ids(agent, _common_callees("src/requests/auth.py", "HTTPDigestAuth.handle_401", "src/requests/sessions.py", "SessionRedirectMixin.resolve_redirects"), RQ)


def ms12(agent: CbmAgent) -> dict[str, int]:
    found = _caller_rows(agent, 'b.file_path = "src/requests/utils.py" AND (b.name CONTAINS "proxy" OR b.name CONTAINS "Proxy")')
    return {k: len(v) for k, v in sorted(found.items())}


def ms13(agent: CbmAgent) -> list[str]:
    return neighbour_ids(agent, _common_callers("src/jsx/base.ts", "renderChildren", "src/jsx/base.ts", "isUntrustedObject"), HN)


def ms14(agent: CbmAgent) -> list[str]:
    return neighbour_ids(agent, _common_callees("src/utils/jwt/jwt.ts", "verify", "src/utils/jwt/jwt.ts", "verifyWithJwks"), HN)


def ms15(agent: CbmAgent) -> list:
    cy = f"MATCH (v)-[:CALLS]->(b) WHERE {target('v', 'src/utils/jwt/jwt.ts', 'verify')} OPTIONAL MATCH (a)-[:CALLS]->(b) RETURN b.qualified_name, b.file_path, b.label, a.qualified_name, a.file_path, a.label"
    counts: dict[str, set[str]] = {}
    for r in query(agent, cy):
        if not in_scope(r[1], r[2], HN):
            continue
        callers = counts.setdefault(fid(r[0], r[1]), set())
        if len(r) >= 6 and in_scope(r[4], r[5], HN):
            callers.add(fid(r[3], r[4]))
    best = min(((k, len(v)) for k, v in counts.items()), key=lambda x: (-x[1], x[0]))
    return [best[0], best[1]]


def ms16(agent: CbmAgent) -> list[str]:
    return [f for f in neighbour_ids(agent, _callers("src/utils/html.ts", "raw"), HN) if f.startswith("src/jsx/")]


CBM_PLANS: dict[str, Callable[[CbmAgent], Any]] = {name: obj for name, obj in globals().items() if re.fullmatch(r"ms\d\d", name)}
