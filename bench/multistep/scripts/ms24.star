rows = rueca.list_contracts(side = "provided", query = "/api/tags")
result = sorted(set([c["id"].partition(":")[2] for c in rows if c["key"] == "GET /api/tags" and c["framework"] != "openapi"]))
