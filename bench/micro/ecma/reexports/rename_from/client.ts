import { fromBase64, toBase64 } from "./index";

export function roundTrip(s: string): string {
  const wire = toBase64(s);
  return fromBase64(wire);
}
