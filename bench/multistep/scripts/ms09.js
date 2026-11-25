const SRC = "src/requests/";
const fid = (n) => n.path + "#" + n.local; // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const t = await rueca.trace({ qualified_name: "Session.request", direction: "callers", depth: 2, limit: 200 });
const nodes = t.nodes.filter(inScope);
const first = new Set(nodes.filter((n) => n.d === 1).map(fid));
const second = new Set(nodes.filter((n) => n.d === 2).map(fid).filter((x) => !first.has(x)));
second.delete("src/requests/sessions.py#Session.request");
return second.size;
