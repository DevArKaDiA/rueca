import { Logger } from "./base";

export class AuditLogger extends Logger {
  audit(action: string): string {
    return this.log(`audit ${action}`);
  }
}

export function record(action: string): string {
  const l = new AuditLogger();
  l.audit(action);
  return l.log(action);
}
