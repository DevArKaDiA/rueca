import { now } from "@lib/clock";

export function stamp(msg: string): string {
  return `${now()} ${msg}`;
}
