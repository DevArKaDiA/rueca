import { fmt } from "./lib";

export function price(x: number): string {
  return fmt.money(x);
}
