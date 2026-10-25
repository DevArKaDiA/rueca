export class Conn {
  private open = false;

  constructor(readonly url: string) {
    this.connect();
  }

  connect(): void {
    this.open = this.url.length > 0;
  }
}

export function dial(url: string): Conn {
  return new Conn(url);
}
