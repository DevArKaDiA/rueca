export class User {
  constructor(readonly name: string) {}

  greet(): string {
    return `hi ${this.name}`;
  }
}

export function identity<T>(x: T): T {
  return x;
}

export function first<T>(xs: T[]): T {
  return xs[0];
}
