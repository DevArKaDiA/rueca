import build from "./compile";

export function bundle(files: string[]): string {
  return files.map((f) => build(f)).join("\n");
}
