const REPO = "github.com/honojs/hono";
const REF = "08a023cbfde5";
const FILE = "src/utils/jwt/jwt.ts";
const FN_LABELS = new Set(["Function", "Method", "Constructor"]);

function fid(id) {
  const at = id.indexOf("@");
  const c = id.indexOf(":", at);
  return id.slice(c + 1);
}
function pathOf(id) {
  const f = fid(id);
  const h = f.indexOf("#");
  return h >= 0 ? f.slice(0, h) : f;
}
function isTestPath(p) {
  return /(^|\/)(__tests__|__test__|tests?)\//.test(p) || /\.(test|spec)\.[cm]?[jt]sx?$/.test(p);
}
function okFunc(node) {
  if (!node || !FN_LABELS.has(node.label)) return false;
  const p = node.path || pathOf(node.id);
  return p.startsWith("src/") && !isTestPath(p);
}

// Locate verify in the file
const outline = await rueca.getFileOutline({ path: FILE, repo: REPO, ref: REF });
let root = null;
const stack = [...(outline.symbols || [])];
while (stack.length) {
  const s = stack.shift();
  if (s.name === "verify" && FN_LABELS.has(s.label)) {
    if (!root || fid(s.id).length < fid(root.id).length) root = s;
  }
  if (s.children) stack.push(...s.children);
}
if (!root) {
  const g = await rueca.getSymbol({ name: "verify", path: FILE, repo: REPO, ref: REF });
  root = g.symbol;
}

// Direct callees
const t = await rueca.trace({ id: root.id, direction: "callees", depth: 1, limit: 2000, repo: REPO, ref: REF });
const calleeIdx = new Set();
for (const e of t.edges) if (e[0] === 0 && e[2] === "CALLS" && e[1] !== 0) calleeIdx.add(e[1]);

// Map callees to target function(s): [canonicalId, [ids to trace callers of]]
const targets = new Map();
for (const i of calleeIdx) {
  const n = t.nodes[i];
  if (!n) continue;
  if (n.label === "Class") {
    const p = n.path || pathOf(n.id);
    if (!p.startsWith("src/") || isTestPath(p)) continue;
    let g;
    try { g = await rueca.getSymbol({ id: n.id }); } catch (e) { continue; }
    const ctor = (g.children || []).find(c => c.qn.endsWith("constructor") || fid(c.id).endsWith(".constructor"));
    if (!ctor) continue;
    const key = fid(ctor.id);
    const prev = targets.get(key) || new Set();
    prev.add(ctor.id); prev.add(n.id);
    targets.set(key, prev);
  } else if (okFunc(n)) {
    const key = fid(n.id);
    const prev = targets.get(key) || new Set();
    prev.add(n.id);
    if (key.endsWith(".constructor")) {
      // also count callers recorded against the class node
      try {
        const g = await rueca.getSymbol({ id: n.id });
        if (g.parent && g.parent.label === "Class") prev.add(g.parent.id);
      } catch (e) {}
    }
    targets.set(key, prev);
  }
}

// Count distinct direct callers
const results = [];
for (const [key, ids] of targets) {
  const callers = new Set();
  for (const id of ids) {
    const tc = await rueca.trace({ id, direction: "callers", depth: 1, limit: 2000, include_tests: false, repo: REPO, ref: REF });
    for (const e of tc.edges) {
      if (e[1] !== 0 || e[2] !== "CALLS" || e[0] === 0) continue;
      const n = tc.nodes[e[0]];
      if (okFunc(n) && !(n.qn && /(^|[.#:\/])(it|test|describe)$/.test(n.qn) && isTestPath(n.path || ""))) {
        callers.add(fid(n.id));
      }
    }
  }
  results.push([key, callers.size]);
}

results.sort((a, b) => (b[1] - a[1]) || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
return results.length ? results[0] : null;
