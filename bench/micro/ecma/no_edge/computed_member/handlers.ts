export class Router {
  home(): string {
    return "home";
  }

  about(): string {
    return "about";
  }
}

export function dispatch(r: Router, name: "home" | "about"): string {
  return r[name]();
}

function onOpen(): string {
  return "open";
}

function onClose(): string {
  return "close";
}

const table: Record<string, () => string> = { open: onOpen, close: onClose };

export function fire(event: string): string {
  const handler = table[event];
  return handler ? handler() : "";
}
