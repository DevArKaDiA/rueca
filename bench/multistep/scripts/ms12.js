const SRC = "src/requests/";
const fid = (n) => n.id.slice(n.id.indexOf(":") + 1); // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const groups = await rueca.searchSymbols({ query: "proxy", path_glob: "src/requests/utils.py", labels: ["Function", "Method"], limit: 50 });
const out = {};
for (const g of groups) for (const s of g.symbols) {
  if (s.name.toLowerCase().includes("proxy")) out[fid(s)] = new Set(await neighbours({ id: s.id }, "callers")).size;
}
return out;
