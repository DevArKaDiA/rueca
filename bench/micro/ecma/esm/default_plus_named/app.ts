import fetchValue, { cache } from "./loader";

export function warm(keys: string[]): void {
  for (const k of keys) {
    cache(k, fetchValue(k));
  }
}
