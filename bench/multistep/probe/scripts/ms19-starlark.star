REPO = "github.com/pallets/flask"
NAMES = ["HTTPException", "NotFound", "BadRequest"]

def is_ident(c):
    return c.isalnum() or c == "_"

def has_word(s, w):
    start = 0
    n = len(s)
    for _ in range(n + 1):
        i = s.find(w, start)
        if i < 0:
            return False
        j = i + len(w)
        ok_left = i == 0 or not is_ident(s[i - 1])
        ok_right = j >= n or not is_ident(s[j])
        if ok_left and ok_right:
            return True
        start = i + 1
    return False

def file_lines(path, need):
    out = []
    start = 1
    for _ in range(50):
        sn = rueca.get_snippet(repo = REPO, path = path, start_line = start, max_lines = 2000)
        a = sn["lines"][0]
        b = sn["lines"][1]
        ls = sn["text"].split("\n")
        ls = ls[:b - a + 1]
        out.extend(ls)
        if b >= need or len(ls) == 0:
            break
        start = b + 1
    return out

def code_lines(lines):
    res = []
    in_doc = False
    in_imp = False
    in_bs = False
    for ln in lines:
        segs = ln.split('"""')
        code = ""
        for k, sg in enumerate(segs):
            inside = in_doc if k % 2 == 0 else (not in_doc)
            if not inside:
                code += sg + " "
        if (len(segs) - 1) % 2 == 1:
            in_doc = not in_doc
        h = code.find("#")
        if h >= 0:
            code = code[:h]
        st = code.strip()
        if in_imp:
            if ")" in st:
                in_imp = False
            res.append("")
            continue
        if in_bs:
            if not st.endswith("\\"):
                in_bs = False
            res.append("")
            continue
        if st.startswith("from ") or st.startswith("import "):
            if "(" in st and ")" not in st:
                in_imp = True
            elif st.endswith("\\"):
                in_bs = True
            res.append("")
            continue
        res.append(code)
    return res

hits = {}
for name in NAMES:
    groups = rueca.search_code(repo = REPO, pattern = "\\b" + name + "\\b", regex = True, path_glob = "src/flask/**", limit = 500)
    for g in groups:
        p = g.get("path")
        if p == None or not p.startswith("src/flask/"):
            continue
        for m in g["matches"]:
            hits.setdefault(p, {}).setdefault(name, []).append(m["line"])

answer = {}
for name in NAMES:
    answer[name] = []

for p in sorted(hits.keys()):
    per = hits[p]
    need = 0
    for name in per:
        for l in per[name]:
            if l > need:
                need = l
    cl = code_lines(file_lines(p, need))
    for name in NAMES:
        if name not in per:
            continue
        used = False
        for l in per[name]:
            if l >= 1 and l <= len(cl) and has_word(cl[l - 1], name):
                used = True
                break
        if used and p not in answer[name]:
            answer[name].append(p)

for name in NAMES:
    answer[name] = sorted(answer[name])

result = answer
