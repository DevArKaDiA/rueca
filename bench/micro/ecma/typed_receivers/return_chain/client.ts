export class Query {
  private parts: string[] = [];

  where(cond: string): Query {
    this.parts.push(cond);
    return this;
  }

  limit(n: number): this {
    this.parts.push(`limit ${n}`);
    return this;
  }

  build(): string {
    return this.parts.join(" ");
  }
}

export class Client {
  query(): Query {
    return new Query();
  }
}

export function makeClient(): Client {
  return new Client();
}
