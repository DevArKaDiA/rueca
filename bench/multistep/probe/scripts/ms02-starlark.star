REPO = "github.com/pallets/flask"
REF = "22d924701a6a"

def resolve(name, path, qn_suffix):
    r = rueca.get_symbol(repo = REPO, ref = REF, name = name, path = path)
    cands = [r["symbol"]] + r.get("candidates", [])
    for c in cands:
        if c.get("path") == path and (c["qn"].endswith(qn_suffix) or c["id"].endswith("#" + qn_suffix)):
            return c["id"]
    for c in cands:
        if c.get("path") == path:
            return c["id"]
    return r["symbol"]["id"]

def local_name(n):
    nid = n["id"]
    if "#" in nid:
        return nid[nid.index("#") + 1:]
    qn = n["qn"]
    if ":" in qn:
        return qn[qn.rindex(":") + 1:]
    return qn

def callers(root_id):
    t = rueca.trace(id = root_id, direction = "callers", depth = 1, limit = 2000, include_tests = False)
    nodes = t["nodes"]
    idxs = set()
    for e in t["edges"]:
        if e[1] == 0 and e[0] != 0 and e[2] == "CALLS":
            idxs.add(e[0])
    if len(idxs) == 0:
        for i, n in enumerate(nodes):
            if i != 0 and n.get("d") == 1:
                idxs.add(i)
    out = set()
    for i in idxs:
        n = nodes[i]
        if n.get("label") not in ("Function", "Method"):
            continue
        p = n.get("path") or ""
        if not p.startswith("src/flask/"):
            continue
        base = p.split("/")[-1]
        if base.startswith("test_") or "/tests/" in p:
            continue
        out.add(p + "#" + local_name(n))
    return out

a = resolve("ensure_sync", "src/flask/app.py", "Flask.ensure_sync")
b = resolve("_find_error_handler", "src/flask/sansio/app.py", "App._find_error_handler")

ca = callers(a)
cb = callers(b)
result = sorted([x for x in ca if x in cb])
