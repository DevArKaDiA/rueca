export function fixed(x: number): string {
  return x.toFixed(2);
}

export function money(x: number): string {
  return `$${fixed(x)}`;
}
