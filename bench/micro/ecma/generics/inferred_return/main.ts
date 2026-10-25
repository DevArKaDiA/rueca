import { first, identity, User } from "./user";

export function welcome(users: User[]): string {
  const u = first(users);
  return identity(u).greet() + first<string>(["x"]);
}
