REPO = "github.com/psf/requests"
REF = "6e83187b8feb"
TARGET_PATH = "src/requests/sessions.py"
LABELS = ["Function", "Method"]

def find_root():
    outline = rueca.get_file_outline(path = TARGET_PATH, repo = REPO, ref = REF)
    for sym in outline["symbols"]:
        if sym["name"] == "Session" and sym["label"] == "Class":
            for ch in sym.get("children") or []:
                if ch["name"] == "request" and ch["label"] in LABELS:
                    return ch["id"]
    g = rueca.get_symbol(name = "request", path = TARGET_PATH, repo = REPO, ref = REF)
    return g["symbol"]["id"]

def ok(n):
    p = n.get("path") or ""
    if not p.startswith("src/requests/"):
        return False
    if n.get("label") not in LABELS:
        return False
    low = p.lower()
    if "/tests/" in low or "/test_" in low or low.endswith("_test.py") or "conftest" in low:
        return False
    return True

def direct_callers(sid):
    t = rueca.trace(id = sid, direction = "callers", depth = 1, labels = LABELS, limit = 2000, include_tests = False)
    out = {}
    for i, n in enumerate(t["nodes"]):
        if i == 0:
            continue
        if n.get("d") != 1:
            continue
        if n.get("dir") not in ("in", "both"):
            continue
        if not ok(n):
            continue
        out[n["id"]] = n
    return out

root = find_root()
direct = direct_callers(root)

two_hop = {}
for gid in direct:
    if gid == root:
        continue
    callers = direct_callers(gid)
    for fid in callers:
        two_hop[fid] = True

count = 0
for fid in two_hop:
    if fid == root:
        continue
    if fid in direct:
        continue
    count += 1

result = count
