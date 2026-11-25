FLASK = "github.com/pallets/flask"
CLICK = "github.com/pallets/click"

def repo_of(id):
    return id.split("@")[0]

def ident_char(c):
    return c.isalnum() or c == "_"

def has_call(text, needle):
    # needle followed by optional spaces and "(" and not part of a longer identifier
    i = 0
    n = len(needle)
    for _ in range(len(text) + 1):
        j = text.find(needle, i)
        if j < 0:
            return False
        before_ok = j == 0 or not (ident_char(text[j - 1]) or text[j - 1] == ".")
        if needle.startswith("click."):
            before_ok = j == 0 or not ident_char(text[j - 1])
        k = j + n
        after = text[k:].lstrip(" ")
        if before_ok and (k >= len(text) or not ident_char(text[k])):
            if after.startswith("(") or after.startswith(",") or after.startswith(")") or after == "" or after.startswith(" "):
                return True
        i = j + 1
    return False

files = set()

# 1) Graph: callers of click's echo
sym = rueca.get_symbol(name = "echo", path = "src/click/utils.py", repo = CLICK)
root = sym["symbol"]
if root.get("path") != "src/click/utils.py" or root.get("name") != "echo":
    for c in sym.get("candidates", []):
        if c.get("path") == "src/click/utils.py" and c["qn"].split(".")[-1] == "echo":
            root = c
            break
t = rueca.trace(id = root["id"], direction = "callers", depth = 1, limit = 2000, include_tests = True)
for n in t["nodes"]:
    if n.get("d", 0) < 1:
        continue
    if n.get("dir") not in ("in", "both"):
        continue
    p = n.get("path") or ""
    if repo_of(n["id"]) == FLASK and p.startswith("src/flask/"):
        files.add(p)

# 2) Text: click.echo(...) or echo imported from click, in flask's src/flask/
groups = rueca.search_code(pattern = "\\becho\\b", regex = True, path_glob = "src/flask/**", repo = FLASK, limit = 500)
imports = rueca.search_code(pattern = "from\\s+click(\\.utils)?\\s+import", regex = True, path_glob = "src/flask/**", repo = FLASK, limit = 500, context_lines = 5)

imports_echo = set()
for g in imports:
    for m in g["matches"]:
        block = [m["text"]]
        if "(" in m["text"] and ")" not in m["text"]:
            block = block + (m.get("after") or [])
        joined = " ".join(block)
        stop = joined.find(")")
        if "(" in m["text"] and stop >= 0:
            joined = joined[:stop]
        names = joined.split("import", 1)[-1].replace("(", " ").replace(")", " ").replace(",", " ").split()
        if "echo" in names:
            imports_echo.add(g["path"])

grep_files = set()
for g in groups:
    p = g["path"]
    if not p.startswith("src/flask/"):
        continue
    for m in g["matches"]:
        txt = m["text"].strip()
        if txt.startswith("#") or txt.startswith(">>>"):
            continue
        if has_call(txt, "click.echo"):
            grep_files.add(p)
        elif p in imports_echo and has_call(txt, "echo") and not txt.startswith("from ") and not txt.startswith("import "):
            grep_files.add(p)

for p in grep_files:
    files.add(p)

result = sorted(files)
