import P from "./parser";

export function words(text: string): string[] {
  const p = new P(text);
  return p.parse();
}
