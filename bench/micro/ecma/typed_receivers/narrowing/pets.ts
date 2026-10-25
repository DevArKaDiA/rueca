export class Cat {
  meow(): string {
    return "meow";
  }
}

export class Bird {
  sing(): string {
    return "tweet";
  }
}

export function voice(p: Cat | Bird): string {
  if (p instanceof Cat) {
    return p.meow();
  }
  return p.sing();
}
