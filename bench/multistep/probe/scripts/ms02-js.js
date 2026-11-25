async function findSym(name, path, qn) {
  let r = null;
  try { r = await rueca.getSymbol({ name: name, path: path }); } catch (e) { r = null; }
  if (!r || !r.symbol || r.symbol.path !== path) {
    try { r = await rueca.getSymbol({ qualified_name: qn }); } catch (e) { r = null; }
  }
  if (r && r.symbol && r.symbol.path !== path && r.candidates) {
    const c = r.candidates.find(b => b.path === path && (b.qn === qn || b.qn.endsWith("." + qn) || b.qn.endsWith(":" + qn)));
    if (c) return c.id;
  }
  if (!r || !r.symbol) throw new Error("not found " + qn);
  return r.symbol.id;
}

function localName(node) {
  const path = node.path || "";
  let name = null;
  const idx = node.id.lastIndexOf("#");
  if (idx >= 0) name = node.id.slice(idx + 1);
  if (!name) name = node.qn;
  const noExt = path.replace(/\.py$/, "");
  const prefixes = [
    noExt.replace(/\//g, ".") + ".",
    noExt.replace(/^src\//, "").replace(/\//g, ".") + ".",
    noExt.replace(/\/__init__$/, "").replace(/\//g, ".") + ".",
    noExt.replace(/^src\//, "").replace(/\/__init__$/, "").replace(/\//g, ".") + ".",
    noExt + ":",
    path + ":",
  ];
  for (const p of prefixes) {
    if (name.startsWith(p)) { name = name.slice(p.length); break; }
  }
  return name;
}

async function callers(id) {
  const out = new Map();
  let cursor = undefined;
  for (let i = 0; i < 10; i++) {
    const args = { id: id, direction: "callers", depth: 1, limit: 2000, labels: ["Function", "Method"], include_tests: false };
    if (cursor) args.cursor = cursor;
    const t = await rueca.trace(args);
    const callerIdx = new Set();
    for (const e of t.edges) {
      if (e[1] === 0 && e[2] === "CALLS") callerIdx.add(e[0]);
    }
    t.nodes.forEach((n, i2) => {
      if (i2 === 0) return;
      const isCaller = callerIdx.has(i2) || (n.d === 1 && (n.dir === "in" || n.dir === "both"));
      if (!isCaller) return;
      if (!n.path || !n.path.startsWith("src/flask/")) return;
      if (n.label !== "Function" && n.label !== "Method") return;
      out.set(n.path + "#" + localName(n), true);
    });
    cursor = t.next_cursor;
    if (!cursor) break;
  }
  return out;
}

const a = await findSym("ensure_sync", "src/flask/app.py", "Flask.ensure_sync");
const b = await findSym("_find_error_handler", "src/flask/sansio/app.py", "App._find_error_handler");
const ca = await callers(a);
const cb = await callers(b);
const res = [];
for (const k of ca.keys()) if (cb.has(k)) res.push(k);
res.sort();
return res;
