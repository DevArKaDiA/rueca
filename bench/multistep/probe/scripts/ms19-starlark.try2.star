REPO = "github.com/pallets/flask"
NAMES = ["HTTPException", "NotFound", "BadRequest"]
IDC = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_"
ANN = IDC + ".[]|, "

def has_word(s, w):
    start = 0
    for _ in range(100000):
        k = s.find(w, start)
        if k < 0:
            return False
        before = s[k - 1] if k > 0 else " "
        e = k + len(w)
        after = s[e] if e < len(s) else " "
        if (before not in IDC) and (after not in IDC):
            return True
        start = k + 1
    return False

def ann_like(content):
    if content == "":
        return False
    for x in range(len(content)):
        if content[x] not in ANN:
            return False
    return True

def strip_line(line, i):
    # returns (code, tq) where tq is an open triple-quote delimiter or None
    n = len(line)
    rest = line[i:]
    if ('"' not in rest) and ("'" not in rest) and ("#" not in rest):
        return (rest, None)
    buf = []
    j = i
    tq = None
    for _ in range(n + 2):
        if j >= n:
            break
        c = line[j]
        if c == "#":
            break
        if c == '"' or c == "'":
            d = c * 3
            if line[j:j + 3] == d:
                k = line.find(d, j + 3)
                if k < 0:
                    tq = d
                    break
                buf.append(" ")
                j = k + 3
                continue
            k = j + 1
            for _ in range(n + 2):
                if k >= n:
                    break
                if line[k] == "\\":
                    k += 2
                    continue
                if line[k] == c:
                    break
                k += 1
            content = line[j + 1:k]
            if ann_like(content):
                buf.append(" " + content + " ")
            else:
                buf.append(" ")
            j = k + 1
            continue
        buf.append(c)
        j += 1
    return ("".join(buf), tq)

def logical_lines(text):
    out = []
    cur = ""
    depth = 0
    tq = None
    for line in text.split("\n"):
        i = 0
        if tq != None:
            k = line.find(tq)
            if k < 0:
                continue
            i = k + 3
            tq = None
        r = strip_line(line, i)
        code = r[0]
        tq = r[1]
        cur = code if cur == "" else cur + " " + code
        depth += code.count("(") + code.count("[") + code.count("{")
        depth -= code.count(")") + code.count("]") + code.count("}")
        if depth < 0:
            depth = 0
        if tq != None:
            continue
        if depth > 0 or code.rstrip().endswith("\\"):
            continue
        out.append(cur)
        cur = ""
    if cur != "":
        out.append(cur)
    return out

def file_text(p):
    o = rueca.get_file_outline(repo = REPO, path = p)
    lc = o.get("line_count") or 1
    parts = []
    for st in range(1, lc + 1, 2000):
        en = min(st + 1999, lc)
        sn = rueca.get_snippet(repo = REPO, path = p, start_line = st, end_line = en, max_lines = 2000)
        t = sn["text"]
        if t.endswith("\n"):
            t = t[:-1]
        parts.append(t)
    return "\n".join(parts)

files = {}
for nm in NAMES:
    groups = rueca.search_code(repo = REPO, pattern = "\\b" + nm + "\\b", regex = True, path_glob = "src/flask/**", limit = 500)
    for g in groups:
        p = g["path"]
        if p.startswith("src/flask/") and (p.endswith(".py") or p.endswith(".pyi")):
            files[p] = True

res = {nm: [] for nm in NAMES}
for p in sorted(files.keys()):
    text = file_text(p)
    code_lines = []
    for l in logical_lines(text):
        s = l.strip()
        if s.startswith("import ") or s.startswith("from "):
            continue
        code_lines.append(l)
    for nm in NAMES:
        for l in code_lines:
            if has_word(l, nm):
                res[nm].append(p)
                print(nm + " " + p + ": " + l.strip()[:80])
                break

result = {nm: sorted(res[nm]) for nm in NAMES}
