"""Go labelers (regular expressions over the source, tree-sitter only for
function boundaries): gin / echo route registrations and their handlers,
gRPC client calls and gRPC server implementation methods."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from ..treesitter import Func, extract_functions, innermost
from .keys import canonical_http, rpc_key
from .model import CALL, HANDLER, IMPL, Site, clip, files, go_generated, read
from .specs import ProtoSet


@dataclass(frozen=True)
class GoFunc:
    path: str
    name: str
    qualname: str  # `Recv.Name` or `Name`
    start: int
    end: int
    signature: str  # `func (…) Name(…) (…)`, whitespace collapsed


class GoRepo:
    """The in-scope Go files of a repository: text and functions."""

    def __init__(self, repo: str, root: Path) -> None:
        self.repo, self.root = repo, root
        self.text: dict[str, str] = {}
        self.funcs: dict[str, list[Func]] = {}
        self.by_name: dict[str, list[GoFunc]] = {}
        self._imported: set[str] | None = None
        for rel in files(root, (".go",)):
            text = read(root, rel)
            if go_generated(text):
                continue
            self.text[rel] = text
            fs = extract_functions(rel, text.encode(), "go")
            self.funcs[rel] = fs
            lines = text.splitlines()
            for f in fs:
                self.by_name.setdefault(f.name, []).append(GoFunc(rel, f.name, f.qualname, f.start, f.end, _signature(lines, f.start)))

    def imported_paths(self) -> set[str]:
        if self._imported is None:
            self._imported = {p for text in self.text.values() for p in file_imports(text).values()}
        return self._imported

    def enclosing(self, rel: str, line: int) -> str:
        f = innermost(self.funcs.get(rel, []), line)
        return f.qualname if f else ""

    def define(self, name: str, near: str) -> GoFunc | None:
        """The definition of function / method `name`: in the directory of
        `near` first, else the only one in the repository."""
        cands = self.by_name.get(name, [])
        here = [c for c in cands if c.path.rsplit("/", 1)[0] == near.rsplit("/", 1)[0]]
        if len(here) == 1:
            return here[0]
        return cands[0] if len(cands) == 1 else None


def _signature(lines: list[str], start: int) -> str:
    out = []
    for line in lines[start - 1 : start + 7]:
        out.append(line)
        if line.rstrip().endswith("{"):
            break
    return " ".join(" ".join(out).split())


def _line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


# --- HTTP routes (gin, echo) ---------------------------------------------------------------

_ROUTE = re.compile(r"\b(\w+)\.(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|Any|Handle)\(\s*(?:\"([A-Z]+)\"\s*,\s*)?\"([^\"]*)\"\s*,")
_GROUP = re.compile(r"\b(\w+)\s*:?=\s*(\w+)\.Group\(\s*\"([^\"]*)\"")
_HANDLER_REF = re.compile(r"\b(?:\w+\.)?([A-Z]\w*)\b(?!\s*[({])")
_ROUTERS = ("github.com/gin-gonic/gin", "github.com/labstack/echo")


def call_args(text: str, start: int) -> str:
    """The text from `start` (inside a call's parentheses) to its closing
    parenthesis, skipping string and rune literals."""
    depth, i, quote = 1, start, ""
    while i < len(text) and depth:
        c = text[i]
        if quote:
            if c == "\\" and quote != "`":
                i += 1
            elif c == quote:
                quote = ""
        elif c in "\"'`":
            quote = c
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        i += 1
    return text[start : i - 1]


def _prefixes(text: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for m in _GROUP.finditer(text):
        out[m.group(1)] = out.get(m.group(2), "") + m.group(3)
    return out


def _handler(go: GoRepo, rel: str, line: int, args: str) -> tuple[str, str, int, tuple[str, ...]] | None:
    """(file, symbol, line, accepted names) of a route's handler: the last
    function reference among the arguments (gin: middlewares first, handler
    last), or the enclosing function for an inline `func(…)` literal."""
    if "func(" in args:
        sym = go.enclosing(rel, line)
        return rel, sym, line, (sym.rsplit(".", 1)[-1], "")
    for name in reversed(_HANDLER_REF.findall(args)):
        d = go.define(name, rel)
        if d is not None:
            return d.path, d.qualname, d.start, (d.name,)
    return None


def gin_routes(go: GoRepo) -> tuple[list[Site], list[dict]]:
    """Route registrations with a literal path; (sites, unresolved handlers)."""
    sites: list[Site] = []
    unresolved: list[dict] = []
    for rel, text in go.text.items():
        if not any(r in text for r in _ROUTERS):
            continue
        prefix = _prefixes(text)
        lines = text.splitlines()
        for m in _ROUTE.finditer(text):
            verb = m.group(3) if m.group(2) == "Handle" else m.group(2)
            method = "*" if verb == "Any" else verb
            line = _line_of(text, m.start())
            key = canonical_http(method, prefix.get(m.group(1), "") + m.group(4))
            h = _handler(go, rel, line, call_args(text, m.end()))
            if h is None:
                unresolved.append({"repo": go.repo, "path": rel, "line": line, "key": key})
                continue
            raw = clip(f"{rel}:{line}: {lines[line - 1]}")
            sites.append(Site("http", HANDLER, key, go.repo, h[0], h[1], h[2], raw, "gin", h[3]))
    return sites, unresolved


_MUX = re.compile(r"\b(\w+)\.(Handle|HandleFunc)\(\s*\"([^\"]*)\"\s*,")
_MUX_PATTERN = re.compile(r"^(?:([A-Z]+)\s+)?[^/\s]*(/\S*)$")  # `[METHOD ][host]/path` (Go 1.22 ServeMux)
_ANY_REF = re.compile(r"\b(?:\w+\.)?([A-Za-z_]\w*)\b(?!\s*[({.])")


def _mux_handler(go: GoRepo, rel: str, line: int, args: str) -> tuple[str, str, int, tuple[str, ...]] | None:
    """Like `_handler`, for `net/http` (handlers are often unexported)."""
    if "func(" in args:
        return _handler(go, rel, line, args)
    for name in reversed(_ANY_REF.findall(args)):
        d = go.define(name, rel)
        if d is not None:
            return d.path, d.qualname, d.start, (d.name,)
    return None


def nethttp_routes(go: GoRepo) -> tuple[list[Site], list[dict]]:
    """`net/http` ServeMux registrations with a literal pattern
    (`mux.Handle("GET /api/tags", h)`, `http.HandleFunc("/x", f)`);
    (sites, unresolved handlers)."""
    sites: list[Site] = []
    unresolved: list[dict] = []
    for rel, text in go.text.items():
        if '"net/http"' not in text:
            continue
        lines = text.splitlines()
        for m in _MUX.finditer(text):
            pm = _MUX_PATTERN.match(m.group(3))
            if pm is None:
                continue
            line = _line_of(text, m.start())
            key = canonical_http(pm.group(1) or "*", pm.group(2))
            h = _mux_handler(go, rel, line, call_args(text, m.end()))
            if h is None:
                unresolved.append({"repo": go.repo, "path": rel, "line": line, "key": key})
                continue
            raw = clip(f"{rel}:{line}: {lines[line - 1]}")
            sites.append(Site("http", HANDLER, key, go.repo, h[0], h[1], h[2], raw, "net/http", h[3]))
    return sites, unresolved


# --- gRPC ----------------------------------------------------------------------------------

_CLIENT_TYPED = re.compile(r"\b(\w+)[ \t]+\*?(?:(\w+)\.)?(\w+)Client\b")
_CLIENT_NEW = re.compile(r"\b(\w+)[ \t]*:?=[ \t]*(?:(\w+)\.)?New(\w+)Client\(")
_CHAINED = re.compile(r"\b(?:(\w+)\.)?New(\w+)Client\([^()]*\)\.([A-Z]\w*)\(")
_CALL = re.compile(r"\b(\w+)\.([A-Z]\w*)\(")
_IMPORT = re.compile(r'(?m)^\s*(?:import\s+)?(\w+)?\s*"([^"]+)"\s*$')


def file_imports(text: str) -> dict[str, str]:
    """Package name -> import path of a Go file (explicit alias, else the
    path's last element; a `/vN` suffix gives the element before it)."""
    out = {}
    for m in _IMPORT.finditer(text):
        path = m.group(2)
        parts = path.split("/")
        default = parts[-2] if len(parts) > 1 and re.fullmatch(r"v\d+", parts[-1]) else parts[-1]
        out[m.group(1) or default] = path
    return out


def _service_key(protos: ProtoSet, imports: dict[str, str], qualifier: str | None, svc: str) -> str:
    """`pkg.Svc` when the qualifier's import path is a `go_package` of a
    proto declaring `Svc`, else `Svc`."""
    pkg = protos.package_of_go_path(imports.get(qualifier or "", ""), svc)
    return f"{pkg}.{svc}" if pkg else svc


def _bindings(texts: list[str], protos: ProtoSet) -> dict[str, str]:
    """Names (fields, variables, parameters) typed or built as
    `<pkg>.<Service>Client` -> `[proto package.]Service`."""
    services = protos.services
    out: dict[str, str] = {}
    for text in texts:
        imports = file_imports(text)
        for rx in (_CLIENT_TYPED, _CLIENT_NEW):
            for m in rx.finditer(text):
                if m.group(3) in services:
                    out[m.group(1)] = _service_key(protos, imports, m.group(2), m.group(3))
    return out


def _calls_in(text: str, binds: dict[str, str], protos: ProtoSet):
    services = protos.services
    imports = file_imports(text)
    for m in _CHAINED.finditer(text):
        if m.group(2) in services and m.group(3) in services[m.group(2)]:
            yield m.start(), _service_key(protos, imports, m.group(1), m.group(2)), m.group(3)
    for m in _CALL.finditer(text):
        svc = binds.get(m.group(1))
        if svc and m.group(2) in services[svc.rsplit(".", 1)[-1]]:
            yield m.start(), svc, m.group(2)


def go_grpc_calls(go: GoRepo, protos: ProtoSet) -> list[Site]:
    """`x.M(…)` where `x` is bound to a `<Service>Client` in the same package
    directory and `M` is an rpc of that service; `New<Service>Client(c).M(…)`.
    The key carries the proto package when the client's Go package is a
    `go_package` of the group's protos."""
    by_dir: dict[str, list[str]] = {}
    for rel in go.text:
        by_dir.setdefault(rel.rsplit("/", 1)[0] if "/" in rel else "", []).append(rel)
    sites: list[Site] = []
    for rels in by_dir.values():
        binds = _bindings([go.text[r] for r in rels], protos)
        for rel in rels:
            text = go.text[rel]
            lines = text.splitlines()
            for pos, svc, method in _calls_in(text, binds, protos):
                line = _line_of(text, pos)
                sym = go.enclosing(rel, line)
                sites.append(Site("rpc", CALL, f"{svc}/{method}", go.repo, rel, sym, line, clip(lines[line - 1]), "grpc-go", (sym.rsplit(".", 1)[-1], "")))
    return sites


def _type_name(t: str) -> str:
    t = t.strip().removeprefix("stream ").strip()
    return {"google.protobuf.Empty": "Empty"}.get(t, t.rsplit(".", 1)[-1])


def import_alias(text: str, path: str, name: str) -> str | None:
    """The name a Go file uses for import path `path` (its explicit alias,
    else the package name), or None when it does not import it."""
    m = re.search(rf'(?m)^\s*(?:import\s+)?(\w+)?\s*"{re.escape(path)}"', text)
    if not m:
        return None
    return m.group(1) or name


def implements(sig: str, service: str, method: str, req: str, resp: str, alias: str = r"\w+") -> bool:
    """A Go method signature of the gRPC server side of `service.method`:
    unary `(ctx context.Context, in *<alias>.Req) (*<alias>.Resp, error)`,
    or a streaming one taking `<alias>.<Service>_<Method>Server` (or the
    generic `grpc.…StreamingServer[…]`); `alias` is the name the file gives
    the service's Go package (`go_package`), so a same-named rpc of another
    service does not pass. Types qualified with another proto package (`common.v1.X`,
    `google.protobuf.Empty`) take any qualifier.
    Client-side signatures (`…grpc.CallOption`) are not."""
    if "grpc.CallOption" in sig or not re.search(rf"\)\s*{re.escape(method)}\s*\(", sig):
        return False
    if re.search(rf"\b{alias}\.{re.escape(service)}_{re.escape(method)}Server\b", sig) or "StreamingServer[" in sig:
        return True
    if "context.Context" not in sig:
        return False
    return all(re.search(rf"\*{_qualifier(t, alias)}\.{re.escape(_type_name(t))}\b", sig) for t in (req, resp))


def _qualifier(proto_type: str, alias: str) -> str:
    """Types of the service's own proto package are checked against its Go
    package; qualified ones (`common.v1.InvokeRequest`, Empty) are not."""
    return r"\w+" if "." in proto_type.strip().removeprefix("stream ").strip() else alias


def served(go: GoRepo) -> set[str]:
    """Services the repository serves: `Register<S>Server(` or an embedded
    `Unimplemented<S>Server`."""
    out: set[str] = set()
    for text in go.text.values():
        out.update(m.group(1) for m in re.finditer(r"\b(?:Register|Unimplemented)(\w+)Server\b", text))
    return out


_STRUCT = re.compile(r"(?m)^type\s+(\w+)(?:\[[^\]]*\])?\s+struct\s*\{")
_EMBEDDED = re.compile(r"(?m)^\s*\*?(?:\w+\.)?(\w+)\s*(?://.*)?$")


def server_types(go: GoRepo) -> dict[str, set[str]]:
    """Service -> the struct types embedding `Unimplemented<S>Server` and the
    types those structs embed (where the rpc methods may live). A service
    absent here has no such declaration: any receiver may implement it."""
    out: dict[str, set[str]] = {}
    for text in go.text.values():
        for m in _STRUCT.finditer(text):
            body = text[m.end() : text.find("\n}", m.end())]
            embedded = {e.group(1) for e in _EMBEDDED.finditer(body)}
            for svc in re.findall(r"\bUnimplemented(\w+)Server\b", body):
                out.setdefault(svc, set()).update({m.group(1)} | embedded)
    return out


def _impl_alias(go: GoRepo, rel: str, key: tuple[str, str], protos: ProtoSet) -> str | None:
    """Qualifiers the impl's message types may carry: the file's name for the
    service's `go_package`; when the file does not import that path (code
    generated elsewhere than `go_package` says), the names of its imports
    whose last element is the proto package's last (`…/protocol/v1` for
    `weaviate.v1`) — only when no file of the repository imports the
    `go_package` path. None: the file uses no package of that service."""
    if key not in protos.go_package:
        return r"\w+"
    path, name = protos.go_package[key]
    a = import_alias(go.text[rel], path, name)
    if a:
        return re.escape(a)
    if path in go.imported_paths():
        return None
    last = key[0].rsplit(".", 1)[-1]
    names = [alias for alias, p in file_imports(go.text[rel]).items() if p.rsplit("/", 1)[-1] == last]
    return "(?:" + "|".join(map(re.escape, names)) + ")" if names else None


def _own_types(req: str, resp: str) -> bool:
    """Whether a request or response type is of the service's own package."""
    return any("." not in t.strip().removeprefix("stream ").strip() for t in (req, resp))


def _is_impl(go: GoRepo, f: GoFunc, key: tuple[str, str], method: str, types: tuple[str, str], protos: ProtoSet) -> bool:
    alias = _impl_alias(go, f.path, key, protos) if _own_types(*types) else r"\w+"
    return alias is not None and implements(f.signature, key[1], method, types[0], types[1], alias)


def _receiver_ok(f: GoFunc, svc: str, server: dict[str, set[str]]) -> bool:
    recv = f.qualname.rsplit(".", 1)[0] if "." in f.qualname else ""
    return bool(recv) and (svc not in server or recv in server[svc])


def go_grpc_impls(go: GoRepo, protos: ProtoSet) -> list[Site]:
    """Methods (with a receiver) whose name is an rpc of a service the
    repository serves (`served`), on a server type of that service when the
    repository declares one (`server_types`), and whose signature is that
    rpc's server side (`implements`, types from the service's Go package).
    Key `pkg.Service/Method`."""
    sites: list[Site] = []
    serving = served(go)
    server = server_types(go)
    for (pkg, svc), methods in protos.rpcs.items():
        if svc not in serving:
            continue
        for method, types in methods.items():
            for f in go.by_name.get(method, []):
                if _receiver_ok(f, svc, server) and _is_impl(go, f, (pkg, svc), method, types, protos):
                    sites.append(Site("rpc", IMPL, rpc_key(pkg, svc, method), go.repo, f.path, f.qualname, f.start, clip(f.signature), "grpc-go", (method,)))
    return sites
