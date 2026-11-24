const SRC = "src/flask/";
const fid = (n) => n.id.slice(n.id.indexOf(":") + 1); // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const a = new Set(await neighbours({ qualified_name: "Flask.ensure_sync" }, "callers"));
const b = await neighbours({ qualified_name: "App._find_error_handler" }, "callers");
return [...new Set(b.filter((x) => a.has(x)))].sort();
