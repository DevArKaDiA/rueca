const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p || "");
async function consumed(repo) {
  const rows = await rueca.listContracts({ side: "consumed", repo, kind: "http", limit: 1000 });
  return new Set(rows.filter((c) => !isTest(c.path)).map((c) => c.key));
}
async function provided(framework) {
  const rows = await rueca.listContracts({ side: "provided", repo: "github.com/ollama/ollama", kind: "http", limit: 1000 });
  return rows.filter((c) => (framework === "openapi") === (c.framework === "openapi"));
}
const js = await consumed("github.com/ollama/ollama-js");
const ops = (await provided("openapi")).filter((c) => js.has(c.key));
return new Set(ops.map((c) => c.path + "#" + c.symbol)).size;
