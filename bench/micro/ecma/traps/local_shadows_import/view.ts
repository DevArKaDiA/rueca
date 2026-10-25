import { format } from "./fmt";

export function loud(s: string): string {
  const format = (x: string): string => {
    return x.toUpperCase();
  };
  return format(s);
}

export function quiet(s: string): string {
  return format(s);
}
