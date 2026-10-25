import { stamp } from "@lib/stamp";

export function log(msg: string): string {
  return stamp(msg);
}
