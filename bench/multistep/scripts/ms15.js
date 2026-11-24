const SRC = "src/";
const fid = (n) => n.id.slice(n.id.indexOf(":") + 1); // "<path>#<Qualname>"
const isTest = (p) => /\.test\.|(^|\/)tests?\//.test(p);
const inScope = (n) => ["Function", "Method"].includes(n.label) && (n.path || "").startsWith(SRC) && !isTest(n.path);
async function neighbours(root, direction) {
  const t = await rueca.trace({ ...root, direction, limit: 200 });
  const dir = direction === "callers" ? "in" : "out";
  return t.nodes.filter((n) => n.d === 1 && (n.dir === dir || n.dir === "both") && inScope(n)).map(fid);
}
const t = await rueca.trace({ name: "verify", path: "src/utils/jwt/jwt.ts", direction: "callees", limit: 200 });
let best = null;
for (const n of t.nodes.filter((n) => n.dir === "out" && inScope(n))) {
  const count = new Set(await neighbours({ id: n.id }, "callers")).size;
  const cand = [fid(n), count];
  if (!best || cand[1] > best[1] || (cand[1] === best[1] && cand[0] < best[0])) best = cand;
}
return best;
