def is_test(p):
    p = p or ""
    return ".test." in p or "/test/" in "/" + p or "/tests/" in "/" + p

def consumed(repo):
    rows = rueca.list_contracts(side = "consumed", repo = repo, kind = "http", limit = 1000)
    return set([c["key"] for c in rows if not is_test(c["path"])])

def provided(openapi):
    rows = rueca.list_contracts(side = "provided", repo = "github.com/ollama/ollama", kind = "http", limit = 1000)
    return [c for c in rows if (c["framework"] == "openapi") == openapi]

js = consumed("github.com/ollama/ollama-js")
result = len(set([c["path"] + "#" + c["symbol"] for c in provided(True) if c["key"] in js]))
