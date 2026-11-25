CTX = {"repo": "github.com/psf/requests", "ref": "6e83187b8feb"}
FILE = "src/requests/cookies.py"
FUNC_LABELS = ["Function", "Method"]

def path_of(node):
    p = node.get("path")
    if p:
        return p
    i = node["id"]
    c = i.find(":")
    h = i.rfind("#")
    if c >= 0 and h > c:
        return i[c + 1:h]
    return ""

outline = rueca.get_file_outline(path = FILE, **CTX)

# Flatten outline (no recursion in Starlark): explicit stack
funcs = []
stack = [(s, []) for s in reversed(outline["symbols"])]
for _ in range(100000):
    if not stack:
        break
    sym, chain = stack.pop()
    qual = chain + [sym["name"]]
    if sym["label"] in FUNC_LABELS:
        funcs.append({"id": sym["id"], "qual": ".".join(qual)})
    kids = sym.get("children") or []
    for k in reversed(kids):
        stack.append((k, qual))

rows = []
for f in funcs:
    t = rueca.trace(id = f["id"], direction = "callers", depth = 1, limit = 2000, include_tests = False)
    nodes = t["nodes"]
    callers = set()
    for e in t["edges"]:
        if e[1] != 0 or e[2] != "CALLS":
            continue
        n = nodes[e[0]]
        if n["label"] not in FUNC_LABELS:
            continue
        if not path_of(n).startswith("src/requests/"):
            continue
        callers.add(n["id"])
    rows.append([FILE + "#" + f["qual"], len(callers)])

rows = sorted(rows, key = lambda r: (-r[1], r[0]))
result = rows[:3]
