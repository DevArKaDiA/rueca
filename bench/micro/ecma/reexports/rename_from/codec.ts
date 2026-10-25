export function encode(s: string): string {
  return btoa(s);
}

export function decode(s: string): string {
  return atob(s);
}
