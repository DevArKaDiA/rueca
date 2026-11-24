const path = "requests/sessions.py";
const refs = ["091991be0da1", "fa64e474afe1"];
const flat = (syms) => syms.flatMap((s) => [s, ...flat(s.children || [])]);
const funcs = (syms) => flat(syms).filter((s) => ["Function", "Method"].includes(s.label));
const outline = await rueca.getFileOutline({ path, ref: refs });
const versions = outline.versions ? outline.versions.map((v) => v.symbols) : [outline.symbols, outline.symbols];
const texts = [];
for (const ref of refs) {
  const snip = await rueca.getSnippet({ path, start_line: 1, max_lines: 2000, ref });
  texts.push(snip.text.split("\n"));
}
const bodies = (syms, lines) => Object.fromEntries(funcs(syms).map((s) => [s.id.slice(s.id.indexOf("#") + 1), lines.slice(s.lines[0] - 1, s.lines[1]).join("\n")]));
const a = bodies(versions[0], texts[0]);
const b = bodies(versions[versions.length - 1], texts[1]);
const names = new Set([...Object.keys(a), ...Object.keys(b)]);
return [...names].filter((q) => a[q] !== b[q]).map((q) => path + "#" + q).sort();
