"""Deterministic query derivation from issue text, shared by every arm.

Keyword query ("kw"): up to N tokens, in this priority order, each class
ordered by frequency (desc) then first occurrence, deduplicated
case-insensitively:
  1. identifiers inside inline backticks and fenced code blocks
     (dotted names are split into their parts; traceback/path noise and
     stopwords removed; length >= 3),
  2. CamelCase / camelCase / snake_case tokens from the prose,
  3. remaining non-stopword words (length >= 3) from the prose.
Raw query ("raw"): the issue text itself (whitespace collapsed, truncated to
RAW_MAX_CHARS characters).
"""

from __future__ import annotations

import re
from collections import Counter

KW_MAX_TOKENS = 12
RAW_MAX_CHARS = 4000

ENGLISH_STOP = set(
    """
a about above after again against all also am an and any are aren as at be because been before being below
between both but by can cannot could did didn do does doesn doing don down during each else even ever every
few for from further get gets got had has have having he her here hers herself him himself his how however i
if in into is isn it its itself just let like make makes me might more most much must my myself need needs no
nor not now of off on once one only or other our ours ourselves out over own same seems she should so some
such than that the their theirs them themselves then there these they this those through thus to too under
until up upon us use used uses using very via was wasn way we well were weren what when where which while who
whom why will with within without would wouldn yes yet you your yours yourself yourselves
""".split()
)

NOISE = set(
    """
issue issues bug bugs error errors problem problems expected behavior behaviour actual result results
example examples reproduce reproduction steps step version versions work works working worked fix fixed
fixes thanks thank please hello hi following follow currently current case cases want wanted think thought
looks look seem seemed see seen sure maybe instead still already able unable new old true false none null
undefined return returns returned code line lines file files run runs running call called calls value values
type types time times something anything nothing someone thing things lot lots really actually first second
two three output input test tests testing default python javascript typescript node npm pip doc docs
documentation https http www com github org html md png jpg gif img src usr lib site packages local bin home
users user traceback recent last most self cls args kwargs const var let function def class import from
export async await new this
""".split()
)

STOPWORDS = ENGLISH_STOP | NOISE

_FENCE = re.compile(r"```.*?```", re.S)
_INLINE = re.compile(r"`([^`\n]+)`")
_IDENT = re.compile(r"[A-Za-z_$][A-Za-z0-9_$]*")
_CAMEL = re.compile(r"\b(?:[a-z]+[A-Z][A-Za-z0-9]*|[A-Z][a-z0-9]+[A-Z][A-Za-z0-9]*|[A-Za-z]+[0-9]*_[A-Za-z0-9_]+)\b")
_WORD = re.compile(r"\b[A-Za-z][A-Za-z0-9]{2,}\b")
_URL = re.compile(r"https?://\S+")


def _ok(tok: str) -> bool:
    t = tok.strip("_$")
    return len(t) >= 3 and t.lower() not in STOPWORDS and not t.isdigit()


def _ranked(tokens: list[str]) -> list[str]:
    counts = Counter(t.lower() for t in tokens)
    first: dict[str, int] = {}
    canon: dict[str, str] = {}
    for i, t in enumerate(tokens):
        k = t.lower()
        if k not in first:
            first[k] = i
            canon[k] = t
    order = sorted(counts, key=lambda k: (-counts[k], first[k]))
    return [canon[k] for k in order]


def derive_keywords(text: str, n: int = KW_MAX_TOKENS) -> list[str]:
    text = _URL.sub(" ", text or "")
    code_parts = [m.group(0)[3:-3] for m in _FENCE.finditer(text)]
    prose = _FENCE.sub(" ", text)
    code_parts += [m.group(1) for m in _INLINE.finditer(prose)]
    prose = _INLINE.sub(" ", prose)

    code_tokens: list[str] = []
    for part in code_parts:
        # drop the first line of fences that only names a language
        for tok in _IDENT.findall(part):
            if _ok(tok):
                code_tokens.append(tok)
    camel_tokens = [t for t in _CAMEL.findall(prose) if _ok(t)]
    word_tokens = [t for t in _WORD.findall(prose) if _ok(t)]

    out: list[str] = []
    seen: set[str] = set()
    for group in (_ranked(code_tokens), _ranked(camel_tokens), _ranked(word_tokens)):
        for tok in group:
            k = tok.lower()
            if k in seen:
                continue
            seen.add(k)
            out.append(tok)
            if len(out) >= n:
                return out
    return out


def raw_query(text: str) -> str:
    return " ".join((text or "").split())[:RAW_MAX_CHARS]


# --- tokenizer shared by the BM25 baseline -----------------------------------

_SPLIT_CAMEL = re.compile(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|[0-9]+")


def tokenize(text: str) -> list[str]:
    """Identifier-aware tokens: whole identifier (lowercased) + camel/snake parts."""
    out: list[str] = []
    for ident in _IDENT.findall(text):
        low = ident.lower()
        parts = [p.lower() for chunk in ident.split("_") for p in _SPLIT_CAMEL.findall(chunk)]
        if len(low) >= 2 and low not in ENGLISH_STOP:
            out.append(low)
        if len(parts) > 1:
            out.extend(p for p in parts if len(p) >= 2 and p not in ENGLISH_STOP)
    return out
