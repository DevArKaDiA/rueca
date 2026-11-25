const REPO = "github.com/psf/requests";
const REF = "6e83187b8feb";
const FILE = "src/requests/cookies.py";

const outline = await rueca.getFileOutline({ repo: REPO, ref: REF, path: FILE });
const syms = outline.symbols || [];

const funcs = [];
function walk(list, prefix) {
  for (const s of list || []) {
    const qual = prefix ? prefix + "." + s.name : s.name;
    if (s.label === "Function" || s.label === "Method") {
      funcs.push({ id: s.id, qual });
    }
    if (s.children && s.children.length) walk(s.children, qual);
  }
}
walk(syms, "");

const isFn = (n) => n && (n.label === "Function" || n.label === "Method");
const inSrc = (n) => n && typeof n.path === "string" && n.path.startsWith("src/requests/");

const results = [];
for (const f of funcs) {
  let t;
  try {
    t = await rueca.trace({
      id: f.id,
      direction: "callers",
      depth: 1,
      limit: 2000,
      labels: ["Function", "Method"],
      include_tests: false,
    });
  } catch (e) {
    continue;
  }
  const nodes = t.nodes || [];
  const edges = t.edges || [];
  const callers = new Set();
  let usedEdges = false;
  for (const e of edges) {
    if (e[2] !== "CALLS") continue;
    if (e[1] !== 0) continue;
    usedEdges = true;
    const n = nodes[e[0]];
    if (isFn(n) && inSrc(n)) callers.add(n.id);
  }
  if (!usedEdges) {
    for (let i = 1; i < nodes.length; i++) {
      const n = nodes[i];
      if (n.d !== 1) continue;
      if (n.dir && n.dir !== "in" && n.dir !== "both") continue;
      if (isFn(n) && inSrc(n)) callers.add(n.id);
    }
  }
  results.push([FILE + "#" + f.qual, callers.size]);
}

results.sort((a, b) => (b[1] - a[1]) || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
return results.slice(0, 3);
