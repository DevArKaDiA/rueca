export default class Parser {
  private pos = 0;

  constructor(private readonly src: string) {
    this.rewind();
  }

  rewind(): void {
    this.pos = 0;
  }

  parse(): string[] {
    return this.src.slice(this.pos).split(" ");
  }
}
