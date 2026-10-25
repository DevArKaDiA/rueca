export function fn(): number {
  return 42;
}

export function run(fn: () => number): number {
  return fn() + 1;
}

export function direct(): number {
  return fn() + run(fn);
}
