import { capitalize, round2 } from "./lib";

export function label(name: string, price: number): string {
  return `${capitalize(name)}: ${round2(price)}`;
}
