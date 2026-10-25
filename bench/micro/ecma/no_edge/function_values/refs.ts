function transform(x: number): number {
  return x * 3;
}

function tick(): void {
  console.log("tick");
}

function pick(flag: boolean): () => void {
  return flag ? tick : () => undefined;
}

export function wire(xs: number[], flag: boolean): number[] {
  setTimeout(tick, 5);
  const chosen = flag ? tick : pick(false);
  chosen();
  transform.call(null, 1);
  return xs.map(transform);
}
