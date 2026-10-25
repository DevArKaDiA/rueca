export function slugify(s: string): string {
  return s.trim().toLowerCase().replace(/\s+/g, "-");
}

export function joinPath(a: string, b: string): string {
  return `${slugify(a)}/${slugify(b)}`;
}
