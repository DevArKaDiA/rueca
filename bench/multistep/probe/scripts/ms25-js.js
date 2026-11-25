const REPO = "github.com/psf/requests";
const PATH = "requests/sessions.py";
const REFS = [["091991be0da1", "worktree", "HEAD"], ["fa64e474afe1"]];

async function withRef(cands, fn) {
  let lastErr;
  for (const r of cands) {
    try { return { ref: r, value: await fn(r) }; }
    catch (e) { lastErr = e; }
  }
  throw lastErr;
}

async function fileLines(ref, lineCount) {
  const lines = [];
  let start = 1;
  let guard = 0;
  while (guard++ < 20) {
    const sn = await rueca.getSnippet({ repo: REPO, ref, path: PATH, start_line: start, max_lines: 2000 });
    const parts = (sn.text || "").split("\n");
    const s0 = sn.lines[0], s1 = sn.lines[1];
    for (let i = 0; i <= s1 - s0 && i < parts.length; i++) lines[s0 + i] = parts[i].replace(/\r$/, "");
    if (s1 >= lineCount || s1 < start) break;
    start = s1 + 1;
  }
  return lines;
}

const isFn = (lab) => /func|method|constructor/i.test(lab || "");

function collect(symbols, prefix, out) {
  for (const sym of symbols || []) {
    const qual = prefix ? prefix + "." + sym.name : sym.name;
    if (isFn(sym.label) && sym.lines) {
      if (!out.has(qual)) out.set(qual, []);
      out.get(qual).push(sym.lines);
    }
    collect(sym.children, qual, out);
  }
}

async function snapshot(cands) {
  const { ref, value: outline } = await withRef(cands, (r) => rueca.getFileOutline({ repo: REPO, ref: r, path: PATH }));
  const fns = new Map();
  collect(outline.symbols, "", fns);
  const lines = await fileLines(ref, outline.line_count);
  const texts = new Map();
  for (const [q, ranges] of fns) {
    const bodies = ranges.map(([a, b]) => {
      const seg = [];
      for (let i = a; i <= b; i++) seg.push(lines[i] === undefined ? "" : lines[i]);
      return seg.join("\n");
    });
    texts.set(q, bodies.join("\n\u0000\n"));
  }
  return texts;
}

const A = await snapshot(REFS[0]);
const B = await snapshot(REFS[1]);
const diff = new Set();
for (const [q, t] of A) if (!B.has(q) || B.get(q) !== t) diff.add(q);
for (const q of B.keys()) if (!A.has(q)) diff.add(q);
return [...diff].map((q) => PATH + "#" + q).sort();
