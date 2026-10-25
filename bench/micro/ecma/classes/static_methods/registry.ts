export class Registry {
  private static instances = 0;

  static create(): Registry {
    Registry.bump();
    return new Registry();
  }

  static bump(): number {
    Registry.instances += 1;
    return Registry.instances;
  }

  static reset(): number {
    this.instances = 0;
    return this.bump();
  }

  describe(): string {
    return `#${Registry.bump()}`;
  }
}
