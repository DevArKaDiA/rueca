const seen: number[] = [];

function record(x: number): void {
  seen.push(x);
}

function prepare(): void {
  seen.length = 0;
}

export function each(xs: number[]): number {
  prepare();
  xs.forEach(function visit(x: number): void {
    record(x);
  });
  return seen.length;
}
