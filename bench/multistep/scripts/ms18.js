const FLASK = "github.com/pallets/flask";
// files under src/flask/ of Flask among the callers of `qn`
async function flaskFiles(qn, edgeTypes) {
  const args = { qualified_name: qn, direction: "callers", limit: 1000 };
  if (edgeTypes) args.edge_types = edgeTypes;
  const t = await rueca.trace(args);
  const files = t.nodes.filter((n) => n.dir === "in" && n.repo === FLASK && (n.path || "").startsWith("src/flask/")).map((n) => n.path);
  return [...new Set(files)].sort();
}
const out = {};
for (const qn of ["werkzeug.http.http_date", "werkzeug.utils.import_string", "werkzeug.serving.is_running_from_reloader"]) {
  out[qn] = await flaskFiles(qn);
}
return out;
