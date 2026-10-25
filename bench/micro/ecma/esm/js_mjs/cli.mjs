import { count } from "./lib.mjs";

export function main(argv) {
  const n = count(argv.join(" "));
  console.log(n);
  return n;
}
