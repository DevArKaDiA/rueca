const SRC = "src/flask/";
const fid = (n) => n.path + "#" + n.local; // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
return [...new Set(await neighbours({ qualified_name: "Flask.handle_exception" }, "callers"))].sort();
