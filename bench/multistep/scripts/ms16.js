const SRC = "src/";
const fid = (n) => n.id.slice(n.id.indexOf(":") + 1); // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const found = await neighbours({ name: "raw", path: "src/utils/html.ts" }, "callers");
return [...new Set(found.filter((x) => x.startsWith("src/jsx/")))].sort();
