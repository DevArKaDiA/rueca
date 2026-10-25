export class Shape {
  area(): number {
    return 0;
  }

  describe(): string {
    return `area=${this.area()}`;
  }
}
