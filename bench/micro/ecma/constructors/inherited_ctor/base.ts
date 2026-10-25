export function track(name: string): number {
  return name.length;
}

export class Resource {
  constructor(readonly name: string) {
    track(name);
  }
}
