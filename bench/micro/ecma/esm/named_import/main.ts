import { joinPath, slugify } from "./util";

export function route(section: string, title: string): string {
  const slug = slugify(title);
  return joinPath(section, slug);
}
