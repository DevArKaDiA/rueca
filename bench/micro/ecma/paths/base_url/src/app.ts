import { findUser } from "services/users";
import { quote } from "@/util/text";

export function handle(id: number): string {
  return quote(findUser(id));
}
