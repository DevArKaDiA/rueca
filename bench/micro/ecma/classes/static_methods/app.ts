import { Registry } from "./registry";

export function setup(): string {
  Registry.reset();
  return Registry.create().describe();
}
