PATH = "requests/sessions.py"
REFS = ["091991be0da1", "fa64e474afe1"]

def flat(syms):
    out = []
    for s in syms:
        out.append(s)
        out.extend(flat(s.get("children", [])))
    return out

outline = rueca.get_file_outline(path = PATH, ref = REFS)
syms = []
for v in [outline] + outline.get("versions", []):
    syms.extend(flat(v["symbols"]))
result = sorted(set([PATH + "#" + s["local"] for s in syms if s.get("changed") and s["label"] in ("Function", "Method")]))
