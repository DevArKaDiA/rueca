function clamp(x: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, x));
}

export function add(a: number, b: number): number {
  return a + b;
}

export function scale(x: number, k: number): number {
  return clamp(x * k, 0, 1000);
}
