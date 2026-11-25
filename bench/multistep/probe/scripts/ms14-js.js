const FILE = "src/utils/jwt/jwt.ts";

function localOf(id) {
  const at = id.indexOf("@");
  const rest = at >= 0 ? id.slice(at + 1) : id;
  const colon = rest.indexOf(":");
  return colon >= 0 ? rest.slice(colon + 1) : rest;
}
function isTestPath(p) {
  return /(^|\/)(__tests__|tests?)\//.test(p) || /\.(test|spec)\.[cm]?[jt]sx?$/.test(p);
}

async function findRoot(name) {
  let r;
  try {
    r = await rueca.getSymbol({ name, path: FILE });
  } catch (e) {
    r = null;
  }
  const all = [];
  if (r) {
    all.push(r.symbol);
    for (const c of r.candidates || []) all.push(c);
  }
  let hit = all.find(s => s.path === FILE && localOf(s.id) === FILE + "#" + name);
  if (!hit) hit = all.find(s => s.path === FILE);
  if (!hit) {
    const o = await rueca.getFileOutline({ path: FILE });
    const walk = (arr) => {
      for (const s of arr || []) {
        if (s.name === name && (s.label === "Function" || s.label === "Method")) return s;
        const x = walk(s.children);
        if (x) return x;
      }
      return null;
    };
    hit = walk(o.symbols);
  }
  if (!hit) throw new Error("root not found: " + name);
  return hit.id;
}

const ctorCache = {};
async function ctorOf(classNode) {
  if (classNode.id in ctorCache) return ctorCache[classNode.id];
  let res = null;
  try {
    const g = await rueca.getSymbol({ id: classNode.id });
    const c = (g.children || []).find(ch => {
      const loc = localOf(ch.id);
      return loc.endsWith(".constructor") || ch.qn.endsWith(".constructor") || ch.qn.endsWith("constructor");
    });
    if (c) res = localOf(c.id);
  } catch (e) {
    res = null;
  }
  ctorCache[classNode.id] = res;
  return res;
}

async function directCallees(rootId) {
  const t = await rueca.trace({ id: rootId, direction: "callees", depth: 1, limit: 2000 });
  const out = new Set();
  const dsts = new Set();
  for (const e of t.edges) {
    if (e[0] === 0 && e[2] === "CALLS") dsts.add(e[1]);
  }
  for (const i of dsts) {
    const n = t.nodes[i];
    if (!n || i === 0) continue;
    const loc = localOf(n.id);
    const path = n.path || loc.split("#")[0];
    if (!path.startsWith("src/") || isTestPath(path)) continue;
    if (n.label === "Function" || n.label === "Method") {
      out.add(loc);
    } else if (n.label === "Class") {
      const c = await ctorOf(n);
      if (c) out.add(c);
    }
  }
  return out;
}

const a = await directCallees(await findRoot("verify"));
const b = await directCallees(await findRoot("verifyWithJwks"));
return [...a].filter(x => b.has(x)).sort();
