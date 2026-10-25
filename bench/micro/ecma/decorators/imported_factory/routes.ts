const table = new Map<string, string>();

function validate(path: string): string {
  if (!path.startsWith("/")) {
    throw new Error(path);
  }
  return path;
}

function remember(path: string, key: string): void {
  table.set(path, key);
}

export function route(path: string) {
  const clean = validate(path);
  return (_target: object, key: string): void => {
    remember(clean, key);
  };
}
