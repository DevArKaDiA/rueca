"""Canonical contract keys and the key matcher (the bench's own copy of the
rules of `rueca_model::contract`, RFC 0006 phase 4).

HTTP keys are `"<METHOD> <path>"`: the method upper-case, `*` when unknown;
the path with scheme and host stripped from absolute URLs, a leading
interpolated base URL (`${host}/api`, `{base}/api`) stripped, query string
and fragment cut, a leading `/`, no trailing `/` (the root stays `/`), and
every parameter segment written `{}` — `:id`, `{id}`, `<id>`, `<int:id>`,
`[id]`, `${x}`, f-string `{x}`, `%s` / `%d`, string-concatenation gaps,
`*` / `*rest` wildcards; a segment holding any of these is a parameter
segment. Literal segments stay literal and keys are case-sensitive.

RPC keys are `"<package>.<Service>/<Method>"` from a `.proto` file
(`"<Service>/<Method>"` when the file has no package); code (stubs, server
implementations) knows `"Service/Method"`, which matches any package.

Matching (`match_http`): `exact` (same key) > `template` (a provider `{}`
segment matches any one consumer segment; a consumer `{}` matches only a
provider `{}`) > `method-wildcard` (either method is `*`); the segment
counts must be equal. No prefix or suffix tolerance.
"""

from __future__ import annotations

import re

HTTP_METHODS = ("GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE")
HOLE = "{}"
EXACT, TEMPLATE, METHOD_WILDCARD = "exact", "template", "method-wildcard"
QUALITY_RANK = {EXACT: 3, TEMPLATE: 2, METHOD_WILDCARD: 1}

_SCHEME_HOST = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*://[^/?#]*")
_LEADING_HOLE = re.compile(r"^(?:\$\{[^}]*\}|\{[^}/]*\})(?=/)")
_PARAM_IN_SEGMENT = re.compile(r"\{[^}]*\}|<[^>]*>|%[sdifvx]")
_NEXT_SEGMENT = re.compile(r"^\[[^\]]+\]$")


def canonical_method(method: str | None) -> str:
    m = (method or "").strip().upper()
    return m if m in HTTP_METHODS else "*"


def _is_param_segment(seg: str) -> bool:
    if seg.startswith((":", "*")):
        return True
    return bool(_PARAM_IN_SEGMENT.search(seg) or _NEXT_SEGMENT.match(seg))


def canonical_path(path: str) -> str:
    """The path part of an HTTP key (see the module docs)."""
    p = path.strip()
    p = _SCHEME_HOST.sub("", p, count=1)
    p = _LEADING_HOLE.sub("", p, count=1)
    p = re.split(r"[?#]", p, maxsplit=1)[0]
    segs = [s for s in p.split("/") if s]
    out = [HOLE if _is_param_segment(s) else s for s in segs]
    return "/" + "/".join(out)


def canonical_http(method: str | None, path: str) -> str:
    """`canonical_http("get", "https://h:8/users/:id/?x=1")` -> `GET /users/{}`."""
    return f"{canonical_method(method)} {canonical_path(path)}"


def split_http(key: str) -> tuple[str, list[str]]:
    method, _, path = key.partition(" ")
    return method, [s for s in path.split("/") if s]


def _path_quality(provider: list[str], consumer: list[str]) -> str | None:
    if len(provider) != len(consumer):
        return None
    if provider == consumer:
        return EXACT
    for p, c in zip(provider, consumer):
        if p != c and p != HOLE:
            return None
    return TEMPLATE


def match_http(provider_key: str, consumer_key: str) -> str | None:
    """Match quality of a consumer key against a provider key, or None."""
    pm, pp = split_http(provider_key)
    cm, cp = split_http(consumer_key)
    quality = _path_quality(pp, cp)
    if quality is None:
        return None
    if pm == cm:
        return quality
    if "*" in (pm, cm):
        return METHOD_WILDCARD
    return None


def rpc_key(package: str, service: str, method: str) -> str:
    return f"{package}.{service}/{method}" if package else f"{service}/{method}"


def match_rpc(provider_key: str, consumer_key: str) -> str | None:
    """`pkg.Svc/M` vs `pkg.Svc/M` (exact); a package-less `Svc/M` on either
    side matches that service and method in any package (template)."""
    if provider_key == consumer_key:
        return EXACT
    p_svc, _, p_m = provider_key.rpartition("/")
    c_svc, _, c_m = consumer_key.rpartition("/")
    if p_m != c_m or ("." in p_svc and "." in c_svc):
        return None
    return TEMPLATE if p_svc.rsplit(".", 1)[-1] == c_svc.rsplit(".", 1)[-1] else None


def match_key(kind: str, provider_key: str, consumer_key: str) -> str | None:
    return match_http(provider_key, consumer_key) if kind == "http" else match_rpc(provider_key, consumer_key)
