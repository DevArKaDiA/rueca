const SERVER = "github.com/ollama/ollama";
const PY = "github.com/ollama/ollama-python";
const JS = "github.com/ollama/ollama-js";

function isTestPath(p) {
  if (!p) return false;
  const s = p.toLowerCase();
  if (/(^|\/)(tests?|__tests__|testing|testdata|spec|specs|integration|e2e|fixtures?|mocks?)\//.test(s)) return true;
  const base = s.split("/").pop();
  if (/_test\.go$/.test(base)) return true;
  if (/^test_.*\.py$/.test(base) || /_test\.py$/.test(base) || base === "conftest.py") return true;
  if (/\.(test|spec)\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/.test(base)) return true;
  return false;
}

function normPath(path) {
  let p = String(path || "").trim();
  p = p.replace(/^[a-z]+:\/\/[^\/]+/i, "");
  p = p.split("?")[0].split("#")[0];
  p = p.replace(/\{[^}]*\}/g, "{}");
  p = p.replace(/\/:[^\/]+/g, "/{}");
  p = p.replace(/\/\*[^\/]*$/g, "/{}");
  p = p.replace(/\$\{[^}]*\}/g, "{}");
  p = p.replace(/\/+/g, "/");
  if (!p.startsWith("/")) p = "/" + p;
  if (p.length > 1 && p.endsWith("/")) p = p.slice(0, -1);
  return p;
}

function parseKey(key) {
  const k = String(key || "").trim();
  const i = k.indexOf(" ");
  if (i < 0) return { method: "*", path: normPath(k) };
  return { method: k.slice(0, i).toUpperCase(), path: normPath(k.slice(i + 1)) };
}

async function contracts(repo, side) {
  let res = [];
  try {
    res = await rueca.listContracts({ repo, side, kind: "http", limit: 1000 });
  } catch (e) {
    res = [];
  }
  return res.filter(c => c.kind === "http" && (!c.repo || c.repo === repo));
}

const provided = await contracts(SERVER, "provided");
const serverRoutes = new Map(); // canonical -> {method, path}
for (const c of provided) {
  if (c.side !== "provides") continue;
  const fw = String(c.framework || "").toLowerCase();
  const path = c.path || "";
  if (fw.includes("openapi") || fw.includes("swagger")) continue;
  if (/\.(ya?ml|json)$/i.test(path)) continue;
  if (path && !/\.go$/i.test(path)) continue;
  if (isTestPath(path)) continue;
  const { method, path: p } = parseKey(c.key);
  serverRoutes.set(method + " " + p, { method, path: p });
}

function matchRoutes(consumed) {
  const out = new Set();
  for (const c of consumed) {
    if (c.side !== "consumes") continue;
    if (isTestPath(c.path)) continue;
    const { method, path } = parseKey(c.key);
    const anyMethod = method === "*" || method === "ANY" || method === "";
    for (const [k, r] of serverRoutes) {
      if (r.path !== path) continue;
      if (anyMethod || r.method === method || r.method === "*" || r.method === "ANY") out.add(k);
    }
  }
  return out;
}

const pyCalls = await contracts(PY, "consumed");
const jsCalls = await contracts(JS, "consumed");
const pySet = matchRoutes(pyCalls);
const jsSet = matchRoutes(jsCalls);

const result = [...pySet].filter(k => !jsSet.has(k)).sort();
return result;
