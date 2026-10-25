import * as m from "./math";

export function total(xs: number[]): number {
  let acc = 0;
  for (const x of xs) {
    acc = m.add(acc, x);
  }
  return m.scale(acc, 2);
}
