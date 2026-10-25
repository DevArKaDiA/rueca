export interface Closable {
  close(): void;
}

export class FileHandle implements Closable {
  close(): void {
    this.flush();
  }

  flush(): void {
    console.log("flush");
  }
}

export function closeAll<T extends Closable>(xs: T[]): void {
  for (const x of xs) {
    x.close();
  }
}

export function closeFile<F extends FileHandle>(f: F): void {
  f.close();
}
