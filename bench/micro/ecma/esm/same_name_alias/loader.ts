import { parse } from "./json";
import { parse as parseCsv } from "./csv";

export function loadConfig(text: string): unknown {
  return parse(text);
}

export function loadTable(text: string): string[][] {
  return parseCsv(text);
}
