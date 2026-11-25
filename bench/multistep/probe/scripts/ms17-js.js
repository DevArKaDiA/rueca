const CLICK = "github.com/pallets/click";
const FLASK = "github.com/pallets/flask";
const ALL = [FLASK, CLICK, "github.com/pallets/jinja", "github.com/pallets/werkzeug"];
const files = new Set();

function pathOf(n) {
  if (n.path) return n.path;
  const id = n.id || "";
  const c = id.indexOf(":");
  if (c < 0) return null;
  let p = id.slice(c + 1);
  const h = p.indexOf("#");
  if (h >= 0) p = p.slice(0, h);
  return p;
}

// 1) Call graph: callers of click's echo, across repositories.
let echoId = null;
try {
  const g = await rueca.getSymbol({ repo: CLICK, name: "echo", path: "src/click/utils.py" });
  echoId = g.symbol.id;
} catch (e) {
  try {
    const g = await rueca.getSymbol({ repo: CLICK, qualified_name: "click.utils.echo" });
    echoId = g.symbol.id;
  } catch (e2) {
    console.log("echo lookup failed: " + e2.message);
  }
}

if (echoId) {
  let cursor;
  for (let i = 0; i < 10; i++) {
    let t;
    try {
      const args = { id: echoId, repo: ALL, direction: "callers", depth: 1, limit: 2000 };
      if (cursor) args.cursor = cursor;
      t = await rueca.trace(args);
    } catch (e) {
      try {
        t = await rueca.trace({ id: echoId, repo: "*", direction: "callers", depth: 1, limit: 2000 });
      } catch (e2) {
        console.log("trace failed: " + e2.message);
        break;
      }
    }
    for (const n of t.nodes) {
      if (n.d === 0) continue;
      if (n.dir && n.dir === "out") continue;
      if (!(n.id || "").startsWith(FLASK + "@")) continue;
      const p = pathOf(n);
      if (p && p.startsWith("src/flask/")) files.add(p);
    }
    break;
  }
}

// 2) Text check in flask: `click.echo` uses, or `echo(` in files importing echo from click.
try {
  const r = await rueca.searchCode({ repo: FLASK, pattern: "\\bclick\\.echo\\b", regex: true, path_glob: "src/flask/**", limit: 500 });
  for (const f of r) if (f.path.startsWith("src/flask/") && f.matches.some(m => !/^\s*#/.test(m.text))) files.add(f.path);
} catch (e) { console.log("grep1 failed: " + e.message); }

try {
  const imp = await rueca.searchCode({ repo: FLASK, pattern: "from\\s+click(\\.utils)?\\s+import\\s+[^\\n]*\\becho\\b", regex: true, path_glob: "src/flask/**", limit: 500 });
  const importers = new Set(imp.map(f => f.path));
  if (importers.size) {
    const uses = await rueca.searchCode({ repo: FLASK, pattern: "(^|[^\\w.])echo\\(", regex: true, path_glob: "src/flask/**", limit: 500 });
    for (const f of uses) if (importers.has(f.path)) files.add(f.path);
  }
} catch (e) { console.log("grep2 failed: " + e.message); }

return [...files].sort();
