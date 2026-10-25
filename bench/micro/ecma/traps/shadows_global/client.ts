import { fetch } from "./http";

export function get(url: string): string {
  return fetch(url);
}

export async function remote(url: string): Promise<number> {
  const res = await globalThis.fetch(url);
  return res.status;
}
