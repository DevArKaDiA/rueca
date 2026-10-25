export class Box<T> {
  constructor(private readonly value: T) {}

  map<U>(f: (v: T) => U): Box<U> {
    return new Box(f(this.value));
  }

  get(): T {
    return this.value;
  }
}

export function double(x: number): number {
  return x * 2;
}

export function pipeline(): number {
  const b = new Box(21);
  return b.map(double).get();
}
