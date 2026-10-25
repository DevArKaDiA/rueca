export class Account {
  private cents = 0;
  private cap = 0;

  get balance(): number {
    return this.toUnits(this.cents);
  }

  set limit(units: number) {
    this.cap = this.toCents(units);
  }

  toUnits(c: number): number {
    return c / 100;
  }

  toCents(u: number): number {
    return Math.round(u * 100);
  }

  deposit(units: number): number {
    this.cents += this.toCents(units);
    this.limit = units * 10;
    return this.balance + this.cap;
  }
}

export function open(initial: number): number {
  const a = new Account();
  a.deposit(initial);
  return a.balance;
}
