class Point {
  x = 0;
  y = 0;

  norm(): number {
    return Math.hypot(this.x, this.y);
  }
}

class Base {}

class Named extends Base {
  constructor(readonly name: string) {
    super();
    this.init();
  }

  init(): void {
    console.log(this.name);
  }
}

export function demo(): number {
  const p = new Point();
  const n = new Named("n");
  return p.norm() + n.name.length;
}
