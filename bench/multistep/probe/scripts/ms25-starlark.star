REPO = "github.com/psf/requests"
P = "requests/sessions.py"
REFS = ["091991be0da1", "fa64e474afe1"]

def funcs(ref):
    o = rueca.get_file_outline(path = P, repo = REPO, ref = ref)
    syms = o.get("symbols")
    n = o.get("line_count")
    if syms == None and o.get("versions"):
        v = o["versions"][0]
        syms = v["symbols"]
        n = v["line_count"]
    snip = rueca.get_snippet(path = P, start_line = 1, end_line = n, max_lines = 2000, repo = REPO, ref = ref)
    start = snip["lines"][0]
    lines = snip["text"].split("\n")
    found = {}
    stack = [("", s) for s in (syms or [])]
    for _ in range(1000000):
        if not stack:
            break
        item = stack.pop()
        pfx = item[0]
        s = item[1]
        q = pfx + s["name"]
        if s.get("label") in ("Function", "Method") and s.get("lines"):
            a = s["lines"][0]
            b = s["lines"][1]
            body = "\n".join(lines[a - start:b - start + 1])
            if q not in found:
                found[q] = []
            found[q].append((a, body))
        for c in (s.get("children") or []):
            stack.append((q + ".", c))
    out = {}
    for q, vs in found.items():
        out[q] = [x[1] for x in sorted(vs, key = lambda t: t[0])]
    return out

A = funcs(REFS[0])
B = funcs(REFS[1])
keys = sorted(set(list(A.keys()) + list(B.keys())))
result = sorted([P + "#" + k for k in keys if A.get(k) != B.get(k)])
