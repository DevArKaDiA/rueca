FLASK = "github.com/pallets/flask"

def flask_files(qn, edge_types = None):
    # files under src/flask/ of Flask among the callers of `qn`
    args = {"qualified_name": qn, "direction": "callers", "limit": 1000}
    if edge_types:
        args["edge_types"] = edge_types
    t = rueca.trace(**args)
    files = [n["path"] for n in t["nodes"] if n["dir"] == "in" and n["repo"] == FLASK and n.get("path", "").startswith("src/flask/")]
    return sorted(set(files))

USES = ["CALLS", "INHERITS", "REFERENCES", "THROWS", "DECORATES"]
result = {}
for qn in ["werkzeug.exceptions.HTTPException", "werkzeug.exceptions.NotFound", "werkzeug.exceptions.BadRequest"]:
    result[qn] = flask_files(qn, USES)
