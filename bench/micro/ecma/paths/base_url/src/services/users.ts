import { quote } from "@/util/text";

function lookup(id: number): string {
  return `user${id}`;
}

export function findUser(id: number): string {
  return quote(lookup(id));
}
