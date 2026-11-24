FLASK = "github.com/pallets/flask@"

def flask_files(qn, edge_types = None):
    # files under src/flask/ of Flask among the callers of `qn` (ids: "<repo>@<commit>:<path>#<name>")
    args = {"qualified_name": qn, "direction": "callers", "limit": 1000}
    if edge_types:
        args["edge_types"] = edge_types
    t = rueca.trace(**args)
    files = [n["path"] for n in t["nodes"] if n.get("dir") == "in" and n["id"].startswith(FLASK) and n.get("path", "").startswith("src/flask/")]
    return sorted(set(files))

result = {}
for qn in ["werkzeug.http.http_date", "werkzeug.utils.import_string", "werkzeug.serving.is_running_from_reloader"]:
    result[qn] = flask_files(qn)
