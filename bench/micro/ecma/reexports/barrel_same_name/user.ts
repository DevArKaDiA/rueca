import { normalize, version } from "./helpers";

export function clean(s: string): string {
  return `${version()}:${normalize(s)}`;
}
