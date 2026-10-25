export class Cart {
  private items: number[] = [];
  private total = 0;

  add(price: number): number {
    this.items.push(price);
    this.recalc();
    return this.total;
  }

  private recalc(): void {
    this.total = this.round(this.items.reduce((a, b) => a + b, 0));
  }

  private round(x: number): number {
    return Math.round(x * 100) / 100;
  }
}
