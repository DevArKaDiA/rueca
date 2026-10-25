const names: string[] = [];

export function register(name: string): number {
  return names.push(name);
}

export function sealed(ctor: Function): void {
  Object.seal(ctor);
  register(ctor.name);
}
