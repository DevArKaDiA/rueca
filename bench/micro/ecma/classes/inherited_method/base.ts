export class Logger {
  log(msg: string): string {
    return this.stamp() + msg;
  }

  stamp(): string {
    return "[t] ";
  }
}
