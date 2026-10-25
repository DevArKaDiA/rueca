import { Resource } from "./base";

export class FileResource extends Resource {
  size(): number {
    return this.name.length;
  }
}

export class TempFile extends FileResource {}

export function openTemp(): number {
  const t = new TempFile("tmp");
  return t.size();
}
