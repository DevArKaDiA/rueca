SRC = "src/requests/"

def fid(n, path = None):
    # "<path>#<Qualname>"
    return (path or n["path"]) + "#" + n["local"]

def is_test(p):
    return ".test." in p or "/test/" in "/" + p or "/tests/" in "/" + p

def in_scope(n):
    p = n.get("path", "")
    return n["label"] in ("Function", "Method") and p.startswith(SRC) and not is_test(p)

def neighbours(root, direction):
    t = rueca.trace(direction = direction, limit = 200, **root)
    want = "in" if direction == "callers" else "out"
    return [fid(n) for n in t["nodes"] if n["d"] == 1 and n["dir"] in (want, "both") and in_scope(n)]

groups = rueca.search_symbols(query = "proxy", path_glob = "src/requests/utils.py", labels = ["Function", "Method"], limit = 50)
result = {}
for g in groups:
    for s in g["symbols"]:
        if "proxy" in s["name"].lower():
            result[fid(s, g["path"])] = len(set(neighbours({"id": s["id"]}, "callers")))
