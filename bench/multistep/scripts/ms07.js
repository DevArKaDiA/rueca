const SRC = "src/requests/";
const fid = (n) => n.id.slice(n.id.indexOf(":") + 1); // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const path = "src/requests/cookies.py";
const outline = await rueca.getFileOutline({ path });
const flat = (syms) => syms.flatMap((s) => [s, ...flat(s.children || [])]);
const counts = [];
for (const s of flat(outline.symbols).filter((s) => ["Function", "Method"].includes(s.label))) {
  counts.push([fid(s), new Set(await neighbours({ id: s.id }, "callers")).size]);
}
counts.sort((x, y) => y[1] - x[1] || (x[0] < y[0] ? -1 : 1));
return counts.slice(0, 3);
