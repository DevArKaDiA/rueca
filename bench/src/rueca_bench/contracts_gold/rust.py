"""Rust labelers (regular expressions): actix-web route attributes
(`#[get("/path")] async fn handler`) and tonic server implementations
(`impl <Service> for <Type> { async fn <snake_rpc>(…) }`)."""

from __future__ import annotations

import re
from pathlib import Path

from .keys import canonical_http, rpc_key
from .model import HANDLER, IMPL, Site, clip, files, read
from .specs import ProtoSet

_ROUTE_ATTR = re.compile(r'#\[(get|post|put|patch|delete|head|options)\(\s*"([^"]*)"')
_FN = re.compile(r"\bfn\s+(\w+)")
_IMPL = re.compile(r"(?m)^\s*impl(?:<[^>]*>)?\s+(?:[\w:]+::)?(\w+)\s+for\s+(\w+)(?:<[^{;]*>)?\s*(?:where\b[^{;]*)?\{")
_ASYNC_FN = re.compile(r"\basync\s+fn\s+(\w+)\s*[(<]")


def _line(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def actix_routes(repo: str, root: Path) -> list[Site]:
    """`#[<method>("<path>")]` on the next `fn`: that function handles it."""
    sites: list[Site] = []
    for rel in files(root, (".rs",)):
        text = read(root, rel)
        if "#[" not in text:
            continue
        lines = text.splitlines()
        for m in _ROUTE_ATTR.finditer(text):
            fn = _FN.search(text, m.end())
            if fn is None:
                continue
            line = _line(text, m.start())
            key = canonical_http(m.group(1), m.group(2))
            sites.append(Site("http", HANDLER, key, repo, rel, fn.group(1), line, clip(lines[line - 1]), "actix", (fn.group(1),)))
    return sites


def snake(name: str) -> str:
    """`QueryBatch` -> `query_batch` (tonic's method names)."""
    return re.sub(r"(?<=[a-z0-9])([A-Z])", r"_\1", name).lower()


def _block_end(text: str, open_brace: int) -> int:
    depth = 0
    for i in range(open_brace, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return i
    return len(text)


def _packages(protos: ProtoSet, svc: str) -> list[str]:
    return sorted({pkg for (pkg, s) in protos.rpcs if s == svc})


def _impl_methods(text: str, m: re.Match[str], services: dict[str, set[str]]):
    """(rpc, fn name, offset) of the methods of one `impl <Service> for <Type>`."""
    by_snake = {snake(r): r for r in services[m.group(1)]}
    end = _block_end(text, m.end() - 1)
    for f in _ASYNC_FN.finditer(text, m.end(), end):
        rpc = by_snake.get(f.group(1))
        if rpc is not None:
            yield rpc, f.group(1), f.start()


def tonic_impls(repo: str, root: Path, protos: ProtoSet) -> list[Site]:
    """Methods of `impl <Service> for <Type>` (a tonic server trait) whose
    snake_case name is an rpc of that service. Key `pkg.Service/Method`
    when one package declares the service, else `Service/Method`."""
    services = protos.services
    sites: list[Site] = []
    for rel in files(root, (".rs",)):
        text = read(root, rel)
        lines = text.splitlines()
        for m in (m for m in _IMPL.finditer(text) if m.group(1) in services):
            svc, typ = m.group(1), m.group(2)
            pkgs = _packages(protos, svc)
            pkg = pkgs[0] if len(pkgs) == 1 else ""
            for rpc, fn, pos in _impl_methods(text, m, services):
                line = _line(text, pos)
                sites.append(Site("rpc", IMPL, rpc_key(pkg, svc, rpc), repo, rel, f"{typ}.{fn}", line, clip(lines[line - 1]), "tonic", (fn,)))
    return sites
