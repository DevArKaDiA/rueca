import { compute } from "./index";

export function run(xs: number[]): number[] {
  return xs.map((x) => compute(x));
}
