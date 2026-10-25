async function fetchData(url: string): Promise<string> {
  return `data from ${url}`;
}

function parse(raw: string): string[] {
  return raw.split(" ");
}

function valid(s: string): boolean {
  return s.length > 0;
}

function report(err: unknown): string[] {
  console.error(err);
  return [];
}

export function load(url: string): Promise<string[]> {
  return fetchData(url)
    .then((raw) => parse(raw))
    .then(function (words) {
      return words.filter((w) => valid(w));
    })
    .catch((err) => report(err));
}

export function later(url: string): void {
  setTimeout(() => {
    void load(url);
  }, 10);
  (() => {
    report("boot");
  })();
}
