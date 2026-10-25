import { sealed } from "./registry";

@sealed
export class Service {
  run(): number {
    return this.step() * 2;
  }

  step(): number {
    return 1;
  }
}
