export class Mailer {
  send(to: string): boolean {
    return to.includes("@");
  }
}
