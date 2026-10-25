export class Store {
  save(key: string): string {
    return `saved ${key}`;
  }
}

export class Cache extends Store {
  evict(key: string): string {
    return `evicted ${key}`;
  }
}

export function persist(key: string, cached: boolean): string {
  let target: Store;
  if (cached) {
    target = new Cache();
  } else {
    target = new Store();
  }
  return target.save(key);
}

export function purge(key: string): string {
  const c = new Cache();
  return c.evict(key) + c.save(key);
}
