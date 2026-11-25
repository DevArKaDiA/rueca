const SRC = "src/requests/";
const fid = (n) => n.path + "#" + n.local; // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const outline = await rueca.getFileOutline({ path: "src/requests/auth.py" });
const flat = (syms) => syms.flatMap((s) => [s, ...flat(s.children || [])]);
const out = [];
for (const s of flat(outline.symbols).filter((s) => ["Function", "Method"].includes(s.label))) {
  if ((await neighbours({ id: s.id }, "callers")).length === 0) out.push(outline.path + "#" + s.local);
}
return out.sort();
