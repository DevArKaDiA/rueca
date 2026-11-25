const SRC = "src/flask/";
const fid = (n) => n.path + "#" + n.local; // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const t = await rueca.trace({ qualified_name: "Flask.wsgi_app", direction: "callees", limit: 200 });
const out = [];
for (const n of t.nodes.filter((n) => n.dir === "out" && inScope(n))) {
  if ((await neighbours({ id: n.id }, "callees")).length === 0) out.push(fid(n));
}
return [...new Set(out)].sort();
