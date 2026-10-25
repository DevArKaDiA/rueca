import { makeClient } from "./client";

export function recent(): string {
  return makeClient().query().where("x > 1").limit(10).build();
}
