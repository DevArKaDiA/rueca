export function tokenize(src) {
  return src.split(/\s+/).filter(Boolean);
}

export function count(src) {
  return tokenize(src).length;
}
