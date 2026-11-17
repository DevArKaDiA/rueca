"""Python labelers (`ast`): HTTP client call sites, gRPC stub calls and
gRPC servicer implementations."""

from __future__ import annotations

import ast
import re
import warnings
from collections.abc import Iterator
from pathlib import Path

from .keys import HOLE, HTTP_METHODS, canonical_http
from .model import CALL, IMPL, SKIP_DIRS, Site, clip, files, read

# callee name (leading `_` dropped) -> HTTP method
VERBS = {"get": "GET", "post": "POST", "put": "PUT", "patch": "PATCH", "delete": "DELETE", "head": "HEAD", "options": "OPTIONS", "get_api_list": "GET"}
_URL_KEYWORDS = ("url", "path")


def looks_like_path(s: str | None) -> bool:
    """A rendered string an HTTP client could send: `/…`, an absolute
    http(s) URL, or an interpolated base URL followed by `/…`."""
    if not s or " " in s or len(s) > 300:
        return False
    return s.startswith(("/", "http://", "https://", HOLE + "/"))


class _Scope(ast.NodeVisitor):
    """Walks a module keeping the enclosing `Class.method` name and, per
    function, the names assigned exactly once to a renderable string."""

    def __init__(self) -> None:
        self.stack: list[str] = []
        self.locals: list[dict[str, ast.AST]] = [{}]

    @property
    def symbol(self) -> str:
        return ".".join(self.stack)

    def _enter(self, node: ast.AST, name: str) -> None:
        self.stack.append(name)
        self.locals.append(_single_assignments(node))
        self.generic_visit(node)
        self.locals.pop()
        self.stack.pop()

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        self.stack.append(node.name)
        self.generic_visit(node)
        self.stack.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._enter(node, node.name)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._enter(node, node.name)


def _single_assignments(fn: ast.AST) -> dict[str, ast.AST]:
    seen: dict[str, list[ast.AST]] = {}
    for node in ast.walk(fn):
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            seen.setdefault(node.targets[0].id, []).append(node.value)
    return {k: v[0] for k, v in seen.items() if len(v) == 1}


def render(node: ast.AST | None, local: dict[str, ast.AST] | None = None, depth: int = 0) -> str | None:
    """The string an expression builds, `{}` for every interpolated part:
    literals, f-strings, `+` concatenation, a wrapper call whose first
    argument is a literal template (`path_template("/files/{id}", id=x)`),
    a local name assigned once to one of these."""
    if node is None or depth > 4:
        return None
    if isinstance(node, ast.Constant):
        return node.value if isinstance(node.value, str) else None
    if isinstance(node, ast.JoinedStr):
        return "".join(v.value if isinstance(v, ast.Constant) else HOLE for v in node.values)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _render_concat(node, local, depth)
    if isinstance(node, ast.Call) and node.args and isinstance(node.args[0], (ast.Constant, ast.JoinedStr)):
        return render(node.args[0], local, depth + 1)
    if isinstance(node, ast.Name) and local and node.id in local:
        return render(local[node.id], local, depth + 1)
    return None


def _render_concat(node: ast.BinOp, local: dict[str, ast.AST] | None, depth: int) -> str | None:
    left = render(node.left, local, depth + 1)
    right = render(node.right, local, depth + 1)
    if left is None and right is None:
        return None
    return (left if left is not None else HOLE) + (right if right is not None else HOLE)


def _callee_name(call: ast.Call) -> str:
    f = call.func
    if isinstance(f, ast.Attribute):
        return f.attr
    return f.id if isinstance(f, ast.Name) else ""


def _explicit_method(call: ast.Call) -> tuple[str | None, list[ast.AST]]:
    """(`'POST'` literal among the arguments or `method=`, the arguments after it)."""
    for i, a in enumerate(call.args):
        if isinstance(a, ast.Constant) and isinstance(a.value, str) and a.value.upper() in HTTP_METHODS:
            return a.value.upper(), list(call.args[i + 1 :])
    for k in call.keywords:
        if k.arg != "method":
            continue
        if isinstance(k.value, ast.Constant) and isinstance(k.value.value, str):
            return k.value.value.upper(), []
        verb = VERBS.get(_dotted(k.value).rsplit(".", 1)[-1].lstrip("_"))
        if verb:  # `method=self._connection.get`
            return verb, []
    return None, []


def http_call(call: ast.Call, local: dict[str, ast.AST]) -> tuple[str, str] | None:
    """(method, rendered path) of an HTTP client call, or None."""
    method, rest = _explicit_method(call)
    kw = [k.value for k in call.keywords if k.arg in _URL_KEYWORDS]
    if method is not None:
        for a in rest + kw:
            s = render(a, local)
            if looks_like_path(s):
                return method, s
        return None
    verb = VERBS.get(_callee_name(call).lstrip("_"))
    target = call.args[0] if call.args else (kw[0] if kw else None)
    if verb and target is not None:
        s = render(target, local)
        if looks_like_path(s):
            return verb, s
    return None


class _HttpCalls(_Scope):
    def __init__(self, repo: str, rel: str, lines: list[str], decorators: set[int]) -> None:
        super().__init__()
        self.repo, self.rel, self.lines = repo, rel, lines
        self.decorators = decorators
        self.sites: list[Site] = []

    def visit_Call(self, node: ast.Call) -> None:
        # `@app.get("/x")` declares a route (a provider), it calls nothing
        hit = None if id(node) in self.decorators else http_call(node, self.locals[-1])
        if hit:
            key = canonical_http(*hit)
            raw = clip(self.lines[node.lineno - 1] if node.lineno <= len(self.lines) else "")
            self.sites.append(Site("http", CALL, key, self.repo, self.rel, self.symbol, node.lineno, raw, "python-http", (self.symbol.rsplit(".", 1)[-1], "")))
        self.generic_visit(node)


def _parse(root: Path, rel: str) -> tuple[ast.Module | None, list[str]]:
    text = read(root, rel)
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", SyntaxWarning)
            return ast.parse(text), text.splitlines()
    except (SyntaxError, ValueError):
        return None, []


def python_http_calls(repo: str, root: Path) -> list[Site]:
    out: list[Site] = []
    for rel in files(root, (".py",)):
        tree, lines = _parse(root, rel)
        if tree is None:
            continue
        decorators = {id(d) for n in ast.walk(tree) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) for d in n.decorator_list}
        v = _HttpCalls(repo, rel, lines, decorators)
        v.visit(tree)
        out += v.sites
    return out


# --- gRPC -----------------------------------------------------------------------------------


def _dotted(node: ast.AST) -> str:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        inner = _dotted(node.value)
        return f"{inner}.{node.attr}" if inner else node.attr
    return ""


_FULL_METHOD = re.compile(r"""["']/([\w.]+)\.(\w+)/\w+["']""")


def stub_packages(root: Path) -> dict[str, dict[str, str]]:
    """Generated grpcio modules of a repository (`*_pb2_grpc.py`, dotted
    module name, `src/` layouts too) -> {service: proto package}, read from
    the full method names they register (`'/pkg.Service/Method'`)."""
    out: dict[str, dict[str, str]] = {}
    for p in root.rglob("*_pb2_grpc.py"):
        rel = p.relative_to(root).as_posix()
        if any(part in SKIP_DIRS for part in rel.split("/")[:-1]):
            continue
        found = {m.group(2): m.group(1) for m in _FULL_METHOD.finditer(read(root, rel))}
        mod = rel[:-3].replace("/", ".")
        for name in (mod, mod.removeprefix("src.")):
            out[name] = found
    return out


def _import_names(node: ast.AST) -> list[tuple[str, str]]:
    if isinstance(node, ast.Import):
        return [(a.asname, a.name) if a.asname else (a.name.split(".")[0],) * 2 for a in node.names]
    if isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
        return [(a.asname or a.name, f"{node.module}.{a.name}") for a in node.names]
    return []


def _imports(tree: ast.Module) -> dict[str, str]:
    """Local name -> dotted module or object it was imported as."""
    return {local: target for node in ast.walk(tree) for local, target in _import_names(node)}


def _qualified(svc: str, qualifier: str, imports: dict[str, str], packages: dict[str, dict[str, str]]) -> str:
    """`pkg.Service` when the stub / servicer module is a generated module
    whose package is known — by its dotted name, else (a package that picks
    its generated modules at run time) every generated module of that file
    name agreeing on the package, else (a module re-exporting a generated
    one under another name, `from dapr.proto import appcallback_service_v1`)
    every generated module of the repository that declares the service
    agreeing on the package — else `Service`."""
    module = imports.get(qualifier, "")
    if module in packages:
        pkg = packages[module].get(svc)
    else:
        base = "." + module.rsplit(".", 1)[-1]
        found = {v.get(svc) for k, v in packages.items() if k.endswith(base)} - {None}
        if not found and module:
            found = {v.get(svc) for v in packages.values()} - {None}
        pkg = found.pop() if len(found) == 1 else None
    return f"{pkg}.{svc}" if pkg else svc


def _stub_service(node: ast.AST, services: dict[str, set[str]]) -> tuple[str, str] | None:
    """(module qualifier, service) of a `<mod>.<Service>Stub` reference."""
    qualifier, _, callee = _dotted(node).rpartition(".")
    svc = callee[:-4] if callee.endswith("Stub") else ""
    return (qualifier, svc) if svc in services else None


def _target_name(t: ast.AST) -> str:
    if isinstance(t, ast.Attribute):
        return t.attr
    return t.id if isinstance(t, ast.Name) else ""


def _stub_bindings(tree: ast.Module, services: dict[str, set[str]], packages: dict[str, dict[str, str]]) -> dict[str, str]:
    """`x = <mod>.<Service>Stub(channel)` / `self.x = …`: name -> `[pkg.]Service`."""
    imports = _imports(tree)
    out: dict[str, str] = {}
    for node in ast.walk(tree):
        found = _stub_service(node.value.func, services) if isinstance(node, ast.Assign) and isinstance(node.value, ast.Call) else None
        if found is None:
            continue
        for name in filter(None, (_target_name(t) for t in node.targets)):
            out[name.lstrip("_")] = _qualified(found[1], found[0], imports, packages)
    return {**_annotated_stubs(tree, services, imports, packages), **out}


def _annotation(node: ast.AST) -> tuple[str, ast.AST | None]:
    """(name, annotation) of a function's return or an annotated assignment."""
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return node.name, node.returns
    if isinstance(node, ast.AnnAssign):
        return _target_name(node.target), node.annotation
    return "", None


def _annotated_stubs(tree: ast.Module, services: dict[str, set[str]], imports: dict[str, str], packages: dict[str, dict[str, str]]) -> dict[str, str]:
    """`def grpc_points(self) -> grpc.PointsStub` (a property returning a
    stub) and `x: <mod>.<Service>Stub` annotations: name -> `[pkg.]Service`."""
    out: dict[str, str] = {}
    for node in ast.walk(tree):
        name, ann = _annotation(node)
        if not name or ann is None:
            continue
        for found in filter(None, (_stub_service(sub, services) for sub in ast.walk(ann))):
            out.setdefault(name.lstrip("_"), _qualified(found[1], found[0], imports, packages))
    return out


def _unique_service(method: str, services: dict[str, set[str]]) -> str | None:
    owners = [s for s, ms in services.items() if method in ms]
    return owners[0] if len(owners) == 1 else None


class _StubCalls(_Scope):
    def __init__(self, repo: str, rel: str, lines: list[str], services: dict[str, set[str]], bindings: dict[str, str]) -> None:
        super().__init__()
        self.repo, self.rel, self.lines = repo, rel, lines
        self.services, self.bindings = services, bindings
        self.sites: list[Site] = []
        self.seen: set[tuple[int, str]] = set()

    def _service(self, holder: str, method: str) -> str | None:
        svc = self.bindings.get(holder.lstrip("_"))
        if svc is not None:
            return svc if method in self.services[svc.rsplit(".", 1)[-1]] else None
        if holder.lower().endswith("stub"):
            return _unique_service(method, self.services)
        return None

    def visit_Attribute(self, node: ast.Attribute) -> None:
        holder = node.value.attr if isinstance(node.value, ast.Attribute) else (node.value.id if isinstance(node.value, ast.Name) else "")
        svc = self._service(holder, node.attr) if holder else None
        if svc and (node.lineno, node.attr) not in self.seen:
            self.seen.add((node.lineno, node.attr))
            raw = clip(self.lines[node.lineno - 1])
            self.sites.append(Site("rpc", CALL, f"{svc}/{node.attr}", self.repo, self.rel, self.symbol, node.lineno, raw, "grpc-python", (self.symbol.rsplit(".", 1)[-1], "")))
        self.generic_visit(node)


def python_stub_calls(repo: str, root: Path, services: dict[str, set[str]]) -> list[Site]:
    """`<…>.<stub>.<Method>` where `<stub>` (leading `_` ignored) was bound to
    `<Service>Stub(…)` in the file, or is named `*stub` and `Method` belongs
    to one service. The key carries the proto package when the stub class
    comes from a generated module of the repository."""
    packages = stub_packages(root)
    out: list[Site] = []
    for rel in files(root, (".py",)):
        tree, lines = _parse(root, rel)
        if tree is None:
            continue
        v = _StubCalls(repo, rel, lines, services, _stub_bindings(tree, services, packages))
        v.visit(tree)
        out += v.sites
    return out


def _servicer_services(cls: ast.ClassDef, services: dict[str, set[str]], imports: dict[str, str], packages: dict[str, dict[str, str]]) -> list[str]:
    out = []
    for b in cls.bases:
        qualifier, _, name = _dotted(b).rpartition(".")
        svc = name[: -len("Servicer")] if name.endswith("Servicer") else ""
        if svc in services:
            out.append(_qualified(svc, qualifier, imports, packages))
    return out


def _servicer_methods(cls: ast.ClassDef, svcs: list[str], services: dict[str, set[str]]) -> Iterator[tuple[str, ast.AST]]:
    for node in cls.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for svc in svcs:
            if node.name in services[svc.rsplit(".", 1)[-1]]:
                yield svc, node


def python_servicers(repo: str, root: Path, services: dict[str, set[str]]) -> list[Site]:
    """Methods named after an rpc of `<Service>`, in classes deriving from
    `<…>.<Service>Servicer` (the grpcio base class)."""
    packages = stub_packages(root)
    out: list[Site] = []
    for rel in files(root, (".py",)):
        tree, lines = _parse(root, rel)
        if tree is None:
            continue
        imports = _imports(tree)
        for cls in (n for n in ast.walk(tree) if isinstance(n, ast.ClassDef)):
            svcs = _servicer_services(cls, services, imports, packages)
            for svc, fn in _servicer_methods(cls, svcs, services):
                raw = clip(lines[fn.lineno - 1])
                out.append(Site("rpc", IMPL, f"{svc}/{fn.name}", repo, rel, f"{cls.name}.{fn.name}", fn.lineno, raw, "grpc-python", (fn.name,)))
    return out
