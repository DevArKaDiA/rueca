const REPO = "github.com/pallets/flask";
const REF = "22d924701a6a";
const TARGET_PATH = "src/flask/app.py";

function idPath(id) {
  const hash = id.lastIndexOf("#");
  const head = hash >= 0 ? id.slice(0, hash) : id;
  const at = head.indexOf("@");
  const rest = at >= 0 ? head.slice(at + 1) : head;
  const colon = rest.indexOf(":");
  return colon >= 0 ? rest.slice(colon + 1) : rest;
}

function localName(node, path) {
  const id = node.id || "";
  const hash = id.lastIndexOf("#");
  let name = hash >= 0 ? id.slice(hash + 1) : (node.qn || "");
  const noExt = path.replace(/\.py$/, "");
  const prefixes = [
    noExt.replace(/\//g, ".") + ".",
    noExt.replace(/^src\//, "").replace(/\//g, ".") + ".",
    noExt + ":",
    noExt.replace(/^src\//, "") + ":",
    path + ":",
  ];
  for (const p of prefixes) {
    if (name.startsWith(p)) { name = name.slice(p.length); break; }
  }
  return name;
}

function nodePath(n) {
  return n.path || idPath(n.id || "");
}

function keep(n) {
  const p = nodePath(n);
  if (!p || !p.startsWith("src/flask/")) return false;
  if (n.label !== "Function" && n.label !== "Method") return false;
  if (/(^|\/)tests?(\/|$)/.test(p) || /(^|\/)test_[^/]*\.py$/.test(p)) return false;
  return true;
}

function fid(n) {
  const p = nodePath(n);
  return p + "#" + localName(n, p);
}

let root;
try {
  root = await rueca.getSymbol({ repo: REPO, ref: REF, qualified_name: "Flask.ensure_sync" });
} catch (e) {
  root = await rueca.getSymbol({ repo: REPO, ref: REF, name: "ensure_sync", path: TARGET_PATH });
}
const rootId = root.symbol.id;
const rootFid = TARGET_PATH + "#Flask.ensure_sync";

async function directCallers(id) {
  const t = await rueca.trace({
    id, direction: "callers", depth: 1, limit: 2000,
    labels: ["Function", "Method"], include_tests: false,
  });
  const out = [];
  const seen = new Set();
  for (let i = 1; i < t.nodes.length; i++) {
    const n = t.nodes[i];
    if (n.d !== 1) continue;
    if (n.dir && n.dir !== "in" && n.dir !== "both") continue;
    if (n.id === id) continue;
    if (!keep(n)) continue;
    if (seen.has(n.id)) continue;
    seen.add(n.id);
    out.push(n);
  }
  return out;
}

const result = new Set();
const level1 = (await directCallers(rootId)).filter(n => n.id !== rootId && fid(n) !== rootFid);
for (const n of level1) result.add(fid(n));
for (const mid of level1) {
  const l2 = await directCallers(mid.id);
  for (const n of l2) result.add(fid(n));
}
result.delete(rootFid);
return [...result].sort();
