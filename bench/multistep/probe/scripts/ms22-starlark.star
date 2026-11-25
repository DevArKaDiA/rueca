SERVER = "github.com/ollama/ollama"
PY = "github.com/ollama/ollama-python"
JS = "github.com/ollama/ollama-js"

def is_test_path(p):
    if p == None:
        return False
    lp = p.lower()
    parts = lp.split("/")
    for seg in parts[:-1]:
        if seg in ["test", "tests", "__tests__", "testing", "testdata", "spec", "specs", "e2e"]:
            return True
    name = parts[-1]
    if name.startswith("test_") or name.startswith("conftest"):
        return True
    if name.endswith("_test.go") or name.endswith("_test.py"):
        return True
    for marker in [".test.", ".spec.", "-test.", "_test."]:
        if marker in name:
            return True
    return False

def norm_key(k):
    k = k.strip()
    idx = k.find(" ")
    if idx < 0:
        return None
    m = k[:idx].strip().upper()
    p = k[idx + 1:].strip()
    q = p.find("?")
    if q >= 0:
        p = p[:q]
    if len(p) > 1 and p.endswith("/"):
        p = p[:-1]
    if not p.startswith("/"):
        p = "/" + p
    return (m, p)

def is_spec(c):
    fw = (c.get("framework") or "").lower()
    if "openapi" in fw or "swagger" in fw:
        return True
    p = (c.get("path") or "").lower()
    if p.endswith(".yaml") or p.endswith(".yml") or p.endswith(".json"):
        return True
    return False

WILD = ["*", "ANY", "ALL"]

provided = rueca.list_contracts(repo = SERVER, side = "provided", kind = "http", limit = 1000)
routes = {}
for c in provided:
    if c.get("side") != "provides":
        continue
    if is_spec(c):
        continue
    if is_test_path(c.get("path")):
        continue
    nk = norm_key(c["key"])
    if nk == None:
        continue
    routes[nk] = True

def consumed_keys(repo):
    out = {}
    calls = rueca.list_contracts(repo = repo, side = "consumed", kind = "http", limit = 1000)
    for c in calls:
        if c.get("side") != "consumes":
            continue
        if is_test_path(c.get("path")):
            continue
        nk = norm_key(c["key"])
        if nk == None:
            continue
        out[nk] = True
    return out

def matched_routes(calls):
    hit = {}
    for (rm, rp) in routes.keys():
        for (cm, cp) in calls.keys():
            if cp != rp:
                continue
            if cm == rm or cm in WILD or rm in WILD:
                m = rm
                if rm in WILD:
                    m = cm
                hit[m + " " + rp] = True
    return hit

py_hit = matched_routes(consumed_keys(PY))
js_hit = matched_routes(consumed_keys(JS))

result = sorted([k for k in py_hit.keys() if k not in js_hit])
