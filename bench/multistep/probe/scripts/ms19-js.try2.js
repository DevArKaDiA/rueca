const REPO = "github.com/pallets/flask";
const NAMES = ["HTTPException", "NotFound", "BadRequest"];
const out = {};
for (const n of NAMES) out[n] = new Set();

let groups = [];
try {
  groups = await rueca.searchCode({ repo: REPO, pattern: "\\b(HTTPException|NotFound|BadRequest)\\b", regex: true, path_glob: "src/flask/**", limit: 500 });
} catch (e) {
  groups = await rueca.searchCode({ repo: REPO, pattern: "(HTTPException|NotFound|BadRequest)", regex: true, limit: 500 });
}

const fileMatches = new Map();
for (const g of groups || []) {
  if (!g || !g.path || !g.path.startsWith("src/flask/")) continue;
  if (!fileMatches.has(g.path)) fileMatches.set(g.path, []);
  fileMatches.get(g.path).push(...(g.matches || []));
}

async function fileLines(path) {
  const all = [];
  let start = 1;
  for (let i = 0; i < 10; i++) {
    let r;
    try { r = await rueca.getSnippet({ repo: REPO, path, start_line: start, max_lines: 2000 }); }
    catch (e) { if (i === 0) throw e; break; }
    let ls = (r.text || "").split("\n");
    if (r.lines) {
      const n = r.lines[1] - r.lines[0] + 1;
      if (ls.length > n) ls = ls.slice(0, n);
      all.push(...ls);
      if (n < 2000) break;
      start = r.lines[1] + 1;
    } else { all.push(...ls); break; }
  }
  if (all.length >= 2 && /^\s*1[\t│|:]/.test(all[0]) && /^\s*2[\t│|:]/.test(all[1])) {
    return all.map(l => l.replace(/^\s*\d+[\t│|:] ?/, ""));
  }
  return all;
}

// Blank comments and string contents (keeping newlines) only to find statement structure.
function blank(src) {
  let res = "";
  let i = 0;
  const L = src.length;
  while (i < L) {
    const c = src[i];
    if (c === "#") { while (i < L && src[i] !== "\n") { res += " "; i++; } continue; }
    const m = /^([rRbBuUfF]{0,2})('''|"""|'|")/.exec(src.slice(i, i + 5));
    if (m && (m[1] === "" || i === 0 || !/\w/.test(src[i - 1]))) {
      const pre = m[1], q = m[2];
      let j = i + pre.length + q.length;
      let closed = false;
      while (j < L) {
        if (src[j] === "\\") { j += 2; continue; }
        if (src.startsWith(q, j)) { closed = true; break; }
        if (q.length === 1 && src[j] === "\n") break;
        j++;
      }
      const end = Math.min(L, j + (closed ? q.length : 0));
      res += src.slice(i, end).replace(/[^\n]/g, " ");
      i = end;
      continue;
    }
    res += c;
    i++;
  }
  return res;
}

// Statements with their line ranges (0-based, inclusive).
function statements(code) {
  const stmts = [];
  let cur = "", depth = 0, line = 0, startLine = 0;
  const push = (endLine) => {
    for (const p of cur.split(";")) if (p.trim()) stmts.push({ text: p.trim(), start: startLine, end: endLine });
    cur = "";
  };
  for (let i = 0; i < code.length; i++) {
    const c = code[i];
    if (c === "\\" && code[i + 1] === "\n") { cur += " "; i++; line++; continue; }
    if ("([{".includes(c)) depth++;
    else if (")]}".includes(c)) depth = Math.max(0, depth - 1);
    if (c === "\n") {
      if (depth === 0) { push(line); line++; startLine = line; continue; }
      cur += " "; line++; continue;
    }
    cur += c;
  }
  push(line);
  return stmts;
}

function isImport(st) {
  return /^from\s+[\w.]+\s+import\b/.test(st) || /^import\s/.test(st);
}

function analyze(lines) {
  const code = blank(lines.join("\n"));
  const stmts = statements(code);
  const aliases = {};
  for (const n of NAMES) aliases[n] = new Set([n]);
  const importLines = new Set();
  for (const st of stmts) {
    if (!isImport(st.text)) continue;
    for (let k = st.start; k <= st.end; k++) importLines.add(k);
    const m = /^from\s+[\w.]+\s+import\s+([\s\S]*)$/.exec(st.text);
    if (!m) continue;
    for (const part of m[1].replace(/[()]/g, " ").split(",")) {
      const mm = /^\s*(\w+)(?:\s+as\s+(\w+))?\s*$/.exec(part);
      if (mm && mm[2] && NAMES.includes(mm[1])) aliases[mm[1]].add(mm[2]);
    }
  }
  const found = new Set();
  for (let k = 0; k < lines.length; k++) {
    if (importLines.has(k)) continue;
    const t = lines[k];
    for (const n of NAMES) {
      for (const a of aliases[n]) {
        if (new RegExp("\\b" + a + "\\b").test(t)) { found.add(n); break; }
      }
    }
  }
  return found;
}

function fallback(matches) {
  const found = new Set();
  for (const m of matches) {
    const t = m.text || "";
    if (/^\s*(from\s+\S+\s+)?import\b/.test(t)) continue;
    if (/^\s*[\w,\s]+,?\s*\)?\s*$/.test(t) && !/\w\s+\w/.test(t.trim())) continue;
    for (const n of NAMES) if (new RegExp("\\b" + n + "\\b").test(t)) found.add(n);
  }
  return found;
}

for (const [path, matches] of fileMatches) {
  let found;
  try {
    found = analyze(await fileLines(path));
  } catch (e) {
    found = fallback(matches);
  }
  for (const n of found) out[n].add(path);
}

const result = {};
for (const n of NAMES) result[n] = [...out[n]].sort();
return result;
