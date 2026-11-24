PATH = "requests/sessions.py"
REFS = ["091991be0da1", "fa64e474afe1"]

def funcs(syms):
    out = []
    for s in syms:
        if s["label"] in ("Function", "Method"):
            out.append(s)
        out.extend(funcs(s.get("children", [])))
    return out

def bodies(syms, lines):
    return {s["id"].partition("#")[2]: "\n".join(lines[s["lines"][0] - 1:s["lines"][1]]) for s in funcs(syms)}

outline = rueca.get_file_outline(path = PATH, ref = REFS)
versions = [v["symbols"] for v in outline["versions"]] if outline.get("versions") else [outline["symbols"], outline["symbols"]]
texts = [rueca.get_snippet(path = PATH, start_line = 1, max_lines = 2000, ref = r)["text"].split("\n") for r in REFS]
a = bodies(versions[0], texts[0])
b = bodies(versions[-1], texts[1])
names = set(list(a.keys()) + list(b.keys()))
result = sorted([PATH + "#" + q for q in names if a.get(q) != b.get(q)])
