REPO = "github.com/honojs/hono"
REF = "08a023cbfde5"
FILE = "src/utils/jwt/jwt.ts"

NON_FUNC = ["Interface", "Type", "TypeAlias", "Variable", "Field", "Property", "Enum", "EnumMember", "Module", "File", "Key", "Document", "Namespace", "Package", "Import", "Constant", "Folder", "Route"]

def is_test_path(p):
    if p == None:
        return True
    if not p.startswith("src/"):
        return True
    for frag in [".test.", ".spec.", "/__tests__/", "/test/", "/tests/"]:
        if frag in p:
            return True
    return False

def local_name(sid, qn):
    name = ""
    if "#" in sid:
        name = sid.split("#", 1)[1]
    if name == "":
        if ":" in qn:
            name = qn.split(":")[-1]
        else:
            name = qn
    return name

def path_of(node):
    p = node.get("path")
    if p == None:
        sid = node["id"]
        if "#" in sid:
            head = sid.split("#", 1)[0]
            if ":" in head:
                p = head.split(":")[-1]
    return p

def find_root(outline_syms, name):
    for s in outline_syms:
        if s["name"] == name and s["label"] not in ["Class", "Interface"]:
            return s["id"]
    return None

outline = rueca.get_file_outline(path = FILE, repo = REPO, ref = REF)
roots = {}
for nm in ["verify", "verifyWithJwks"]:
    rid = find_root(outline["symbols"], nm)
    if rid == None:
        sym = rueca.get_symbol(name = nm, path = FILE, repo = REPO, ref = REF)
        rid = sym["symbol"]["id"]
    roots[nm] = rid

ctor_cache = {}

def ctor_of(class_id):
    if class_id in ctor_cache:
        return ctor_cache[class_id]
    info = rueca.get_symbol(id = class_id)
    found = None
    for ch in info.get("children", []):
        qn = ch.get("qn", "")
        nm = local_name(ch["id"], qn)
        if nm.split(".")[-1] == "constructor":
            found = ch
            break
    ctor_cache[class_id] = found
    return found

def callees(rid):
    t = rueca.trace(id = rid, direction = "callees", depth = 1, limit = 2000, include_tests = False)
    nodes = t["nodes"]
    direct = set()
    for e in t["edges"]:
        if e[0] == 0 and e[1] != 0:
            direct.add(e[1])
    out = set()
    for i, n in enumerate(nodes):
        if i == 0:
            continue
        if i not in direct and not (n.get("d") == 1 and n.get("dir") in ["out", "both"]):
            continue
        if n.get("d", 1) != 1:
            continue
        label = n.get("label", "")
        target = n
        if label == "Class":
            c = ctor_of(n["id"])
            if c == None:
                continue
            target = c
        elif label in NON_FUNC:
            continue
        p = path_of(target)
        if is_test_path(p):
            continue
        out.add(p + "#" + local_name(target["id"], target.get("qn", "")))
    return out

a = callees(roots["verify"])
b = callees(roots["verifyWithJwks"])
result = sorted([x for x in a if x in b])
