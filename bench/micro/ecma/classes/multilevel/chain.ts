export class A {
  ping(): string {
    return "a";
  }
}

export class B extends A {
  pong(): string {
    return this.ping() + "b";
  }
}

export class C extends B {
  ping(): string {
    return super.ping() + super.pong();
  }
}

export function run(): string {
  const c = new C();
  const b: B = c;
  return c.pong() + b.ping();
}
