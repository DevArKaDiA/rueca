const REPO = "github.com/psf/requests";
const REF = "6e83187b8feb";
const FILE = "src/requests/sessions.py";

function isFn(n) {
  const l = String(n.label || "").toLowerCase();
  return l.includes("function") || l.includes("method");
}
function valid(n) {
  return isFn(n) && typeof n.path === "string" && n.path.startsWith("src/requests/");
}

// Locate Session.request via the file outline.
const outline = await rueca.getFileOutline({ repo: REPO, ref: REF, path: FILE });
let rootId = null;
function walk(syms) {
  for (const s of syms || []) {
    if (rootId) return;
    if ((s.qn === "Session.request" || s.qn.endsWith(".Session.request") || s.qn.endsWith(":Session.request")) && isFn(s)) { rootId = s.id; return; }
    walk(s.children);
  }
}
walk(outline.symbols);
if (!rootId) {
  for (const s of outline.symbols || []) {
    if (s.name === "Session") for (const c of s.children || []) if (c.name === "request") rootId = c.id;
  }
}
if (!rootId) {
  const g = await rueca.getSymbol({ repo: REPO, ref: REF, name: "request", path: FILE });
  rootId = g.symbol.id;
}

async function callersOf(id) {
  const t = await rueca.trace({ id, direction: "callers", depth: 1, limit: 2000, include_tests: false });
  const out = new Map();
  for (const n of t.nodes) {
    if (n.d !== 1) continue;
    if (n.dir && n.dir !== "in" && n.dir !== "both") continue;
    if (!valid(n)) continue;
    out.set(n.id, n);
  }
  return out;
}

const direct = await callersOf(rootId);
direct.delete(rootId);

const result = new Set();
for (const [gid] of direct) {
  const c2 = await callersOf(gid);
  for (const [fid] of c2) {
    if (fid === rootId) continue;
    if (direct.has(fid)) continue;
    result.add(fid);
  }
}
return result.size;
