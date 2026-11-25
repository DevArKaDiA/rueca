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

t = rueca.trace(qualified_name = "Session.request", direction = "callers", depth = 2, limit = 200)
nodes = [n for n in t["nodes"] if in_scope(n)]
first = set([fid(n) for n in nodes if n["d"] == 1])
second = set([fid(n) for n in nodes if n["d"] == 2 and fid(n) not in first and fid(n) != "src/requests/sessions.py#Session.request"])
result = len(second)
