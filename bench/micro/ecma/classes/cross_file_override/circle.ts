import { Shape } from "./shape";

export class Circle extends Shape {
  constructor(private readonly r: number) {
    super();
  }

  area(): number {
    return Math.PI * this.r * this.r;
  }
}

export function report(c: Circle): string {
  return c.describe() + c.area();
}
