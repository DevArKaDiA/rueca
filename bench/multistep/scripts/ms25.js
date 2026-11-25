const path = "requests/sessions.py";
const refs = ["091991be0da1", "fa64e474afe1"];
const flat = (syms) => syms.flatMap((s) => [s, ...flat(s.children || [])]);
const outline = await rueca.getFileOutline({ path, ref: refs });
const syms = [outline, ...(outline.versions || [])].flatMap((v) => flat(v.symbols));
const changed = syms.filter((s) => s.changed && ["Function", "Method"].includes(s.label));
return [...new Set(changed.map((s) => path + "#" + s.local))].sort();
