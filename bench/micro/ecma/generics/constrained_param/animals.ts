export class Animal {
  speak(): string {
    return "...";
  }
}

export class Dog extends Animal {
  speak(): string {
    return "woof";
  }

  fetch(): string {
    return "ball";
  }
}

export function chorus<T extends Animal>(xs: T[]): string[] {
  return xs.map((x) => x.speak());
}

export function walk<D extends Dog>(d: D): string {
  return d.fetch() + d.speak();
}
