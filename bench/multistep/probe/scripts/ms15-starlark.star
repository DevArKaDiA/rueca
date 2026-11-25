REPO = "github.com/honojs/hono"
REF = "08a023cbfde5"
FN = ["Function", "Method"]
FILE = "src/utils/jwt/jwt.ts"

def short(i):
    a = i.find("@")
    rest = i[a + 1:]
    c = rest.find(":")
    return rest[c + 1:]

def is_test_path(p):
    return (".test." in p) or (".spec." in p) or ("__tests__" in p) or ("/test/" in p) or ("/tests/" in p)

def path_ok(sid):
    p = sid.split("#")[0]
    return p.startswith("src/") and not is_test_path(p)

def ok_func(n):
    if n.get("label") not in FN:
        return False
    return path_ok(short(n["id"]))

def find_ctor(class_id):
    gs = rueca.get_symbol(id = class_id)
    for ch in gs.get("children", []):
        sid = short(ch["id"])
        if sid.endswith(".constructor") or ch.get("qn", "").endswith(".constructor"):
            return ch["id"]
    return None

def main():
    ol = rueca.get_file_outline(path = FILE, repo = REPO, ref = REF)
    root_id = None
    for s in ol["symbols"]:
        if s["name"] == "verify" and s.get("label") in FN:
            root_id = s["id"]
            break
    if root_id == None:
        for s in ol["symbols"]:
            if s["name"] == "verify":
                root_id = s["id"]
                break
    if root_id == None:
        g = rueca.get_symbol(name = "verify", path = FILE, repo = REPO, ref = REF)
        root_id = g["symbol"]["id"]

    t = rueca.trace(id = root_id, direction = "callees", depth = 1, limit = 2000)
    nodes = t["nodes"]
    targets = {}  # key short id -> list of graph ids to collect callers of
    for e in t["edges"]:
        if e[0] != 0:
            continue
        n = nodes[e[1]]
        lab = n.get("label")
        sid = short(n["id"])
        if not path_ok(sid):
            continue
        if lab == "Class":
            ctor = find_ctor(n["id"])
            if ctor == None:
                continue
            k = short(ctor)
            ids = targets.get(k, [])
            for x in [ctor, n["id"]]:
                if x not in ids:
                    ids.append(x)
            targets[k] = ids
        elif lab in FN:
            ids = targets.get(sid, [])
            if n["id"] not in ids:
                ids.append(n["id"])
            if sid.endswith(".constructor"):
                gs = rueca.get_symbol(id = n["id"])
                par = gs.get("parent")
                if par != None and par.get("id") and par["id"] not in ids:
                    ids.append(par["id"])
            targets[sid] = ids

    counts = []
    for k in targets:
        callers = {}
        for tid in targets[k]:
            tr = rueca.trace(id = tid, direction = "callers", depth = 1, limit = 2000)
            ns = tr["nodes"]
            for e in tr["edges"]:
                if e[1] != 0:
                    continue
                m = ns[e[0]]
                if ok_func(m):
                    callers[short(m["id"])] = True
        counts.append([k, len(callers)])

    if len(counts) == 0:
        return None
    best = sorted(counts, key = lambda x: (-x[1], x[0]))[0]
    return [best[0], best[1]]

result = main()
