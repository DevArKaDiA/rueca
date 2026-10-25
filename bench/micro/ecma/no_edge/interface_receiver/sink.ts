export interface Sink {
  write(s: string): void;
}

export class FileSink implements Sink {
  write(s: string): void {
    console.log(s);
  }
}

export abstract class Job {
  abstract execute(): void;

  start(): void {
    this.execute();
  }
}

export class PrintJob extends Job {
  execute(): void {
    new FileSink().write("job");
  }
}

export function emit(sink: Sink): void {
  sink.write("x");
}
