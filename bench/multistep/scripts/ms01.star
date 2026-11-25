SRC = "src/flask/"

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

t = rueca.trace(qualified_name = "Flask.ensure_sync", direction = "callers", depth = 2, limit = 200)
ids = set([fid(n) for n in t["nodes"] if n["d"] > 0 and in_scope(n)])
result = sorted([x for x in ids if x != "src/flask/app.py#Flask.ensure_sync"])
