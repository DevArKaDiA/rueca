export const calls: string[] = [];

export function trace(tag: string, key: string): void {
  calls.push(`${tag}.${key}`);
}

export function logged(tag: string) {
  return function (_target: object, key: string, desc: PropertyDescriptor): PropertyDescriptor {
    const original = desc.value as (...args: unknown[]) => unknown;
    desc.value = function (this: unknown, ...args: unknown[]): unknown {
      trace(tag, key);
      return original.apply(this, args);
    };
    return desc;
  };
}
