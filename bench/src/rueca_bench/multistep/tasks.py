"""The multi-step suite's tasks (RFC 0009 §8): an agent-facing statement, the
corpus it runs on, the answer shape, and the derivation of its gold answer
from `inputs.json` (`derive.py`). Reference tool-call plans are in
`plans.py`, cbm plans in `cbm_plans.py`, reference scripts in
`bench/multistep/scripts/<id>.js` and `<id>.star`.

Statements are self-contained: repository ids, files, refs and the exact
answer shape, never the steps. `expect` says whether code mode is expected
to win (`win`), or not: a near single-step question (`single`) or one whose
answer is inherently large (`large`).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from . import derive as d

ID_RULE = (
    "Identify a function as `<path>#<Qualname>`: its repository-relative file path, `#`, and its "
    "class-qualified name inside that file (e.g. `src/flask/app.py#Flask.wsgi_app`; a nested function "
    "is `outer.inner`)."
)


def scope(repo: str, commit: str, src: str) -> str:
    return (
        f"Repository `{repo}` (indexed at commit {commit[:12]}). Consider only non-test functions and methods "
        f"defined under `{src}` (classes, fields and module-level code are not functions; calls from or to "
        f"anything else are ignored). {ID_RULE}"
    )


FLASK = scope("github.com/pallets/flask", "22d924701a6ae2e4cd01e9a15bbaf3946094af65", "src/flask/")
REQUESTS = scope("github.com/psf/requests", "6e83187b8feb273ed4c6cdab5efd8d54901dfab3", "src/requests/")
HONO = scope("github.com/honojs/hono", "08a023cbfde55b434fb0fb30fae35d42e4bf20aa", "src/") + " `new C(...)` is a call to `C.constructor`."
XR = (
    "An organization of four repositories: `github.com/pallets/flask`, `github.com/pallets/click`, "
    "`github.com/pallets/jinja` and `github.com/pallets/werkzeug`, each indexed at its default ref."
)
OLLAMA = (
    "An organization of three repositories: `github.com/ollama/ollama` (the Go server, with an OpenAPI "
    "document at `docs/openapi.yaml`), `github.com/ollama/ollama-python` and `github.com/ollama/ollama-js` "
    "(its clients), each indexed at its default ref. HTTP routes are written in canonical form: method, "
    "space, path, with every path parameter as `{}` (e.g. `POST /api/blobs/{}`). Calls from test code are ignored."
)
MULTIREF = (
    "Repository `github.com/psf/requests` is indexed at its working tree (commit 091991be0da1) and at the "
    "ref `fa64e474afe1` (a later commit). " + ID_RULE
)


@dataclass(frozen=True)
class Task:
    id: str
    kind: str
    corpus: str
    answer: str  # set | list | int | dict_int | dict_set
    expect: str  # win | single | large
    statement: str
    gold: Callable[[dict], Any]


def _g(corpus: str) -> Callable[[dict], d.Graph]:
    return lambda inp: d.Graph.from_inputs(inp["s3"][corpus])


F, R, H = _g("flask"), _g("requests"), _g("hono")


def _methods_of(g: d.Graph, path: str, cls: str) -> list[str]:
    return [c for c in g.in_file(path) if c.split("#", 1)[1].startswith(cls + ".") and c.count(".") == path.count(".") + 1]


def _xr(inp: dict) -> list:
    return inp["crossrepo"]["uses"]


def _pairs(inp: dict) -> list:
    return inp["contracts"]["pairs"]


PY, JS = "ollama/ollama-python", "ollama/ollama-js"

TASKS: list[Task] = [
    Task("ms01", "callers-2hop", "flask", "set", "single",
         f"{FLASK} Which functions call `Flask.ensure_sync` (in `src/flask/app.py`) either directly or through exactly one intermediate function? Return a sorted list of ids, without `Flask.ensure_sync` itself.",
         lambda i: d.two_hop_callers(F(i), "src/flask/app.py#Flask.ensure_sync")),
    Task("ms02", "callers-intersection", "flask", "set", "win",
         f"{FLASK} Which functions directly call both `Flask.ensure_sync` (in `src/flask/app.py`) and `App._find_error_handler` (in `src/flask/sansio/app.py`)? Return a sorted list of ids.",
         lambda i: d.common_callers(F(i), "src/flask/app.py#Flask.ensure_sync", "src/flask/sansio/app.py#App._find_error_handler")),
    Task("ms03", "callees-intersection", "flask", "set", "win",
         f"{FLASK} Which functions are called directly by both `Flask.handle_user_exception` and `Flask.handle_exception` (both in `src/flask/app.py`)? Return a sorted list of ids.",
         lambda i: d.common_callees(F(i), "src/flask/app.py#Flask.handle_user_exception", "src/flask/app.py#Flask.handle_exception")),
    Task("ms04", "single-step", "flask", "set", "single",
         f"{FLASK} Which functions directly call `Flask.handle_exception` (in `src/flask/app.py`)? Return a sorted list of ids.",
         lambda i: d.callers_of(F(i), "src/flask/app.py#Flask.handle_exception")),
    Task("ms05", "outline-fanout", "flask", "dict_int", "win",
         f"{FLASK} For every method of the class `RequestContext` in `src/flask/ctx.py` (methods directly in the class body), how many distinct functions call it directly? Return an object mapping each method's id to its number of callers, including methods with 0.",
         lambda i: d.caller_counts(F(i), _methods_of(F(i), "src/flask/ctx.py", "RequestContext"))),
    Task("ms06", "callee-leaves", "flask", "set", "win",
         f"{FLASK} Among the functions that `Flask.wsgi_app` (in `src/flask/app.py`) calls directly, which ones call no function at all? Return a sorted list of ids.",
         lambda i: d.leaf_callees(F(i), "src/flask/app.py#Flask.wsgi_app")),
    Task("ms07", "top-fanin-file", "requests", "list", "win",
         f"{REQUESTS} Which three functions or methods defined in `src/requests/cookies.py` have the most distinct direct callers? Return a list of `[id, number of callers]` pairs, most callers first, ties broken by id in ascending order.",
         lambda i: d.top_by_callers(R(i), R(i).in_file("src/requests/cookies.py"), 3)),
    Task("ms08", "uncalled-in-file", "requests", "set", "win",
         f"{REQUESTS} Which functions and methods defined in `src/requests/auth.py` (nested functions included) have no direct caller? Return a sorted list of ids.",
         lambda i: d.uncalled(R(i), R(i).in_file("src/requests/auth.py"))),
    Task("ms09", "count-2hop", "requests", "int", "win",
         f"{REQUESTS} How many distinct functions call `Session.request` (in `src/requests/sessions.py`) through exactly one intermediate function, but do not call it directly (and are not `Session.request` itself)? Return an integer.",
         lambda i: d.exactly_two_hop_count(R(i), "src/requests/sessions.py#Session.request")),
    Task("ms10", "callers-filter", "requests", "set", "single",
         f"{REQUESTS} Which functions defined outside `src/requests/utils.py` directly call `to_native_string` (in `src/requests/_internal_utils.py`)? Return a sorted list of ids.",
         lambda i: d.callers_where(R(i), "src/requests/_internal_utils.py#to_native_string", lambda c: not c.startswith("src/requests/utils.py#"))),
    Task("ms11", "callees-intersection", "requests", "set", "win",
         f"{REQUESTS} Which functions are called directly by both `HTTPDigestAuth.handle_401` (in `src/requests/auth.py`) and `SessionRedirectMixin.resolve_redirects` (in `src/requests/sessions.py`)? Return a sorted list of ids.",
         lambda i: d.common_callees(R(i), "src/requests/auth.py#HTTPDigestAuth.handle_401", "src/requests/sessions.py#SessionRedirectMixin.resolve_redirects")),
    Task("ms12", "search-filter-details", "requests", "dict_int", "win",
         f"{REQUESTS} For every function or method defined in `src/requests/utils.py` whose own name contains `proxy` (case-insensitive), how many distinct functions call it directly? Return an object mapping each id to its number of callers, including those with 0.",
         lambda i: d.caller_counts(R(i), [c for c in R(i).in_file("src/requests/utils.py") if "proxy" in d.name_of(c).lower()])),
    Task("ms13", "callers-intersection", "hono", "set", "win",
         f"{HONO} Which functions directly call both `renderChildren` and `isUntrustedObject` (both in `src/jsx/base.ts`)? Return a sorted list of ids.",
         lambda i: d.common_callers(H(i), "src/jsx/base.ts#renderChildren", "src/jsx/base.ts#isUntrustedObject")),
    Task("ms14", "callees-intersection", "hono", "set", "win",
         f"{HONO} Which functions are called directly by both `verify` and `verifyWithJwks` (both in `src/utils/jwt/jwt.ts`)? Return a sorted list of ids.",
         lambda i: d.common_callees(H(i), "src/utils/jwt/jwt.ts#verify", "src/utils/jwt/jwt.ts#verifyWithJwks")),
    Task("ms15", "max-fanin-callee", "hono", "list", "win",
         f"{HONO} Among the functions that `verify` (in `src/utils/jwt/jwt.ts`) calls directly, which one has the most distinct direct callers? Return `[id, number of callers]`; on a tie, the smallest id.",
         lambda i: d.top_by_callers(H(i), H(i).out("src/utils/jwt/jwt.ts#verify"), 1)[0]),
    Task("ms16", "callers-in-dir", "hono", "set", "win",
         f"{HONO} Which functions defined under `src/jsx/` directly call `raw` (in `src/utils/html.ts`)? Return a sorted list of ids.",
         lambda i: d.callers_where(H(i), "src/utils/html.ts#raw", lambda c: c.startswith("src/jsx/"))),
    Task("ms17", "cross-repo-callers", "xr-flask", "set", "win",
         f"{XR} Which files under `src/flask/` of `github.com/pallets/flask` call Click's function `echo` (defined in `src/click/utils.py` of `github.com/pallets/click`)? Return a sorted list of repository-relative file paths.",
         lambda i: d.files_using(_xr(i), "click.utils.echo", "src/flask/")),
    Task("ms18", "cross-repo-fanout", "xr-flask", "dict_set", "win",
         f"{XR} For each of the Werkzeug functions `werkzeug.http.http_date` (`src/werkzeug/http.py`), `werkzeug.utils.import_string` (`src/werkzeug/utils.py`) and `werkzeug.serving.is_running_from_reloader` (`src/werkzeug/serving.py`) of `github.com/pallets/werkzeug`: which files under `src/flask/` of `github.com/pallets/flask` call it? Return an object mapping each of the three names exactly as written here to a sorted list of repository-relative file paths.",
         lambda i: d.files_using_each(_xr(i), ["werkzeug.http.http_date", "werkzeug.utils.import_string", "werkzeug.serving.is_running_from_reloader"], "src/flask/")),
    Task("ms19", "cross-repo-classes", "xr-flask", "dict_set", "win",
         f"{XR} For each of the Werkzeug classes `werkzeug.exceptions.HTTPException`, `werkzeug.exceptions.NotFound` and `werkzeug.exceptions.BadRequest` (all in `src/werkzeug/exceptions.py` of `github.com/pallets/werkzeug`): which files under `src/flask/` of `github.com/pallets/flask` use it (instantiate, raise, catch, subclass or otherwise reference it; an import alone does not count)? Return an object mapping each of the three names exactly as written here to a sorted list of repository-relative file paths.",
         lambda i: d.files_using_each(_xr(i), ["werkzeug.exceptions.HTTPException", "werkzeug.exceptions.NotFound", "werkzeug.exceptions.BadRequest"], "src/flask/")),
    Task("ms20", "org-deps", "xr-flask", "set", "single",
         f"{XR} Which repositories of the organization does `github.com/pallets/flask` depend on? Return a sorted list of repository ids (`github.com/<owner>/<name>`).",
         lambda i: sorted(f"github.com/{r}" for r in i["org_deps"]["pallets/flask"])),
    Task("ms21", "routes-both-clients", "s2-ollama", "set", "win",
         f"{OLLAMA} Which HTTP routes implemented by the Go server code of `github.com/ollama/ollama` (route handlers, not the OpenAPI document) are called by both `github.com/ollama/ollama-python` and `github.com/ollama/ollama-js`? Return a sorted list of canonical routes.",
         lambda i: sorted(d.contract_keys(_pairs(i), "http-code", PY) & d.contract_keys(_pairs(i), "http-code", JS))),
    Task("ms22", "routes-difference", "s2-ollama", "set", "win",
         f"{OLLAMA} Which HTTP routes implemented by the Go server code of `github.com/ollama/ollama` (route handlers, not the OpenAPI document) are called by `github.com/ollama/ollama-python` but not by `github.com/ollama/ollama-js`? Return a sorted list of canonical routes.",
         lambda i: sorted(d.contract_keys(_pairs(i), "http-code", PY) - d.contract_keys(_pairs(i), "http-code", JS))),
    Task("ms23", "spec-ops-count", "s2-ollama", "int", "win",
         f"{OLLAMA} How many distinct operations of `docs/openapi.yaml` in `github.com/ollama/ollama` are called by `github.com/ollama/ollama-js`? Return an integer.",
         lambda i: d.spec_ops_count(_pairs(i), JS)),
    Task("ms24", "route-providers", "s2-ollama", "set", "single",
         f"{OLLAMA} Which Go functions or methods of `github.com/ollama/ollama` serve the route `GET /api/tags` (route handlers, not the OpenAPI document)? Return a sorted list of `<path>#<Qualname>` ids (e.g. `server/routes.go#Server.ChatHandler`).",
         lambda i: d.contract_dsts(_pairs(i), "http-code", "GET /api/tags")),
    Task("ms25", "ref-diff", "mr-requests", "set", "win",
         f"{MULTIREF} Which functions and methods of `requests/sessions.py` differ between the two (their code changed, or they exist in only one of them)? Return a sorted list of ids.",
         lambda i: d.differing(i["multiref"]["symbols"], "requests/sessions.py")),
]

BY_ID = {t.id: t for t in TASKS}


def statements() -> list[dict]:
    """The task list as the probe reads it."""
    return [{"id": t.id, "kind": t.kind, "corpus": t.corpus, "expect": t.expect, "answer": t.answer, "statement": t.statement} for t in TASKS]
