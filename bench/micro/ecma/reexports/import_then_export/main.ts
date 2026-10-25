import beta, { alpha } from "./barrel";

export function both(): string {
  return alpha() + beta();
}
