function splitLine(line: string): string[] {
  return line.split(",");
}

export function parse(text: string): string[][] {
  return text.split("\n").map((line) => splitLine(line));
}
