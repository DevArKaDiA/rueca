"""Spec readers: OpenAPI 3 / Swagger 2 operations (pyyaml / json) and
`.proto` services (regular expressions)."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path

import yaml

from .keys import canonical_http, rpc_key
from .model import SPEC, Site, clip, files, read

OPENAPI_METHODS = ("get", "put", "post", "delete", "options", "head", "patch", "trace")
SPEC_EXTS = (".yaml", ".yml", ".json")
EXTRACT_BUDGET = 2 * 1024 * 1024  # Rueca's default per-file extraction budget
_SPEC_HEAD = re.compile(r"""(?m)(?:^|[{,\s])["']?(?:openapi|swagger)["']?\s*:""")


def _load(text: str, rel: str):
    try:
        return json.loads(text) if rel.endswith(".json") else yaml.safe_load(text)
    except (ValueError, yaml.YAMLError):
        return None


def is_spec(doc) -> bool:
    if not isinstance(doc, dict):
        return False
    v = doc.get("openapi") or doc.get("swagger")
    return isinstance(v, str) and (v.startswith("3") or v.startswith("2."))


def _path_line(text: str, path: str) -> int:
    for rx in (rf'(?m)^\s+"?{re.escape(path)}"?\s*:', rf'"{re.escape(path)}"\s*:'):
        m = re.search(rx, text)
        if m:
            return text.count("\n", 0, m.start()) + 1
    return 0


def _operations(doc: dict):
    """(method, path) of every operation of an OpenAPI document."""
    for path, ops in (doc.get("paths") or {}).items():
        if not isinstance(ops, dict):
            continue
        for method in ops:
            if method in OPENAPI_METHODS:
                yield method, path


def _spec_doc(root: Path, rel: str) -> tuple[str, dict] | None:
    text = read(root, rel)
    if not _SPEC_HEAD.search(text):
        return None
    doc = _load(text, rel)
    return (text, doc) if is_spec(doc) else None


def openapi_operations(repo: str, root: Path) -> tuple[list[Site], list[dict]]:
    """One SPEC site per `paths.<path>.<method>` of every OpenAPI document;
    (sites, documents read with their size and operation count)."""
    sites: list[Site] = []
    docs: list[dict] = []
    for rel in files(root, SPEC_EXTS):
        found = _spec_doc(root, rel)
        if found is None:
            continue
        text, doc = found
        ops = list(_operations(doc))
        for method, path in ops:
            route = f"{method.upper()} {path}"
            key = canonical_http(method, path)
            sites.append(Site("http", SPEC, key, repo, rel, route, _path_line(text, path), route, "openapi", (key,)))
        size = len(text.encode())
        docs.append({"repo": repo, "path": rel, "bytes": size, "operations": len(ops), "over_budget": size > EXTRACT_BUDGET})
    return sites, docs


# --- proto -------------------------------------------------------------------------------

_COMMENT = re.compile(r"//[^\n]*|/\*.*?\*/", re.S)
_PACKAGE = re.compile(r"^\s*package\s+([\w.]+)\s*;", re.M)
_SERVICE = re.compile(r"\bservice\s+(\w+)\s*\{")
_GO_PACKAGE = re.compile(r'option\s+go_package\s*=\s*"([^"]+)"')
_RPC = re.compile(r"\brpc\s+(\w+)\s*\(\s*(stream\s+)?([\w.]+)\s*\)\s*returns\s*\(\s*(stream\s+)?([\w.]+)\s*\)")


@dataclass
class ProtoSet:
    """Every service of the `.proto` files of a group. Services are keyed
    `(package, Service)`: two packages may declare the same service name
    (`weaviate.v0.Weaviate`, `weaviate.v1.Weaviate`)."""

    sites: list[Site] = field(default_factory=list)
    # (package, service) -> rpc -> (request type, response type)
    rpcs: dict[tuple[str, str], dict[str, tuple[str, str]]] = field(default_factory=dict)
    # (package, service) -> (Go import path, Go package name), from `option go_package`
    go_package: dict[tuple[str, str], tuple[str, str]] = field(default_factory=dict)

    @property
    def services(self) -> dict[str, set[str]]:
        """Service name -> rpc names (every package)."""
        out: dict[str, set[str]] = {}
        for (_, svc), ms in self.rpcs.items():
            out.setdefault(svc, set()).update(ms)
        return out

    def package_of_go_path(self, path: str, service: str) -> str | None:
        for (pkg, svc), (gp, _) in self.go_package.items():
            if svc == service and gp == path:
                return pkg
        return None


def _blank_comments(text: str) -> str:
    """Comments replaced by spaces (newlines kept: offsets and lines hold)."""
    return _COMMENT.sub(lambda m: re.sub(r"[^\n]", " ", m.group(0)), text)


def _service_body(text: str, start: int) -> int:
    depth = 0
    for i in range(start, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return i
    return len(text)


def _go_package(text: str) -> tuple[str, str] | None:
    gm = _GO_PACKAGE.search(text)
    if not gm:
        return None
    path, _, name = gm.group(1).partition(";")
    return path, name or path.rsplit("/", 1)[-1]


def _service_rpcs(text: str, sm: re.Match[str]):
    yield from _RPC.finditer(text, sm.end(), _service_body(text, sm.end() - 1))


def proto_services(repo: str, root: Path, into: ProtoSet) -> None:
    for rel in files(root, (".proto",)):
        raw = read(root, rel)
        text = _blank_comments(raw)
        lines = raw.splitlines()
        pm = _PACKAGE.search(text)
        pkg = pm.group(1) if pm else ""
        go = _go_package(text)
        for sm in _SERVICE.finditer(text):
            svc = sm.group(1)
            if go:
                into.go_package[(pkg, svc)] = go
            for rm in _service_rpcs(text, sm):
                line = text.count("\n", 0, rm.start()) + 1
                qn = f"{pkg}.{svc}.{rm.group(1)}" if pkg else f"{svc}.{rm.group(1)}"
                into.sites.append(Site("rpc", SPEC, rpc_key(pkg, svc, rm.group(1)), repo, rel, qn, line, clip(lines[line - 1]), "proto", (f"{svc}.{rm.group(1)}",)))
                into.rpcs.setdefault((pkg, svc), {})[rm.group(1)] = (rm.group(3), rm.group(5))
