SRC = "src/"

def fid(n):
    # "<repo>@<commit>:<path>#<Qualname>" -> "<path>#<Qualname>"
    return n["id"].partition(":")[2]

def is_test(p):
    return ".test." in p or "/test/" in "/" + p or "/tests/" in "/" + p

def in_scope(n):
    p = n.get("path", "")
    return n["label"] in ("Function", "Method") and p.startswith(SRC) and not is_test(p)

def neighbours(root, direction):
    t = rueca.trace(direction = direction, limit = 200, **root)
    want = "in" if direction == "callers" else "out"
    return [fid(n) for n in t["nodes"] if n["d"] == 1 and n.get("dir") in (want, "both") and in_scope(n)]

t = rueca.trace(name = "verify", path = "src/utils/jwt/jwt.ts", direction = "callees", limit = 200)
counts = [[fid(n), len(set(neighbours({"id": n["id"]}, "callers")))] for n in t["nodes"] if n.get("dir") == "out" and in_scope(n)]
result = sorted(counts, key = lambda c: (-c[1], c[0]))[0]
