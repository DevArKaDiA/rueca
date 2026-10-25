const store = new Map<string, string>();

export function cache(key: string, value: string): void {
  store.set(key, value);
}

export default function load(key: string): string {
  const value = store.get(key) ?? fetchRemote(key);
  cache(key, value);
  return value;
}

function fetchRemote(key: string): string {
  return `remote:${key}`;
}
