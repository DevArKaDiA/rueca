import { logged } from "./trace";

export class Api {
  @logged("api")
  fetch(id: number): string {
    return this.parse(`${id}`);
  }

  parse(raw: string): string {
    return raw.trim();
  }
}
