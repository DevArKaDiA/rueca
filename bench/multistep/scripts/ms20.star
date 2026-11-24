repos = rueca.list_repositories()
flask = [r for r in repos if r["repo"] == "github.com/pallets/flask"][0]
result = sorted(flask.get("depends_on", []))
