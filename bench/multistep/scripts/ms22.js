const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p || "");
async function consumed(repo) {
  const rows = await rueca.listContracts({ side: "consumed", repo, kind: "http", limit: 1000 });
  return new Set(rows.filter((c) => !isTest(c.path)).map((c) => c.key));
}
async function provided(framework) {
  const rows = await rueca.listContracts({ side: "provided", repo: "github.com/ollama/ollama", kind: "http", limit: 1000 });
  return rows.filter((c) => (framework === "openapi") === (c.framework === "openapi"));
}
const py = await consumed("github.com/ollama/ollama-python");
const js = await consumed("github.com/ollama/ollama-js");
const served = new Set((await provided("code")).map((c) => c.key));
return [...py].filter((k) => !js.has(k) && served.has(k)).sort();
