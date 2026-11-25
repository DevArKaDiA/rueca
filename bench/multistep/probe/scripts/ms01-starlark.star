REPO = "github.com/pallets/flask"
REF = "22d924701a6a"
OK_LABELS = ["Function", "Method"]

def local_id(node_id):
    # pinned id: repo@commit:path#name -> path#name
    at = node_id.find("@")
    rest = node_id[at + 1:] if at >= 0 else node_id
    colon = rest.find(":")
    if colon >= 0:
        rest = rest[colon + 1:]
    h = rest.find("#")
    if h < 0:
        return rest
    path = rest[:h]
    name = rest[h + 1:]
    clean = ""
    for ch in name.elems():
        if ch.isalnum() or ch == "_" or ch == ".":
            clean += ch
        else:
            break
    return path + "#" + clean

def keep(n):
    if n.get("label") not in OK_LABELS:
        return False
    p = n.get("path") or ""
    if not p.startswith("src/flask/"):
        return False
    return True

def direct_callers(sym_id):
    t = rueca.trace(id = sym_id, direction = "callers", depth = 1, labels = OK_LABELS, include_tests = False, limit = 2000)
    out = {}
    for n in t["nodes"]:
        if n.get("d") != 1:
            continue
        if n.get("dir") not in ("in", "both"):
            continue
        if not keep(n):
            continue
        out[n["id"]] = n
    return out

root = rueca.get_symbol(name = "ensure_sync", path = "src/flask/app.py", repo = REPO, ref = REF)
root_id = root["symbol"]["id"]
root_local = local_id(root_id)

found = {}
level1 = direct_callers(root_id)
for nid in level1:
    found[local_id(nid)] = True
for nid in level1:
    if nid == root_id:
        continue
    level2 = direct_callers(nid)
    for mid in level2:
        found[local_id(mid)] = True

result = sorted([k for k in found if k != root_local and k != "src/flask/app.py#Flask.ensure_sync"])
