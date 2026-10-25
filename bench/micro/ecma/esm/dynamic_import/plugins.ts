function announce(name: string): string {
  return `plugin ${name}`;
}

export function register(name: string): string {
  return announce(name);
}
