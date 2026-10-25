export class EmailJob {
  run(): string {
    return "email";
  }
}

export class SmsJob {
  run(): string {
    return "sms";
  }
}

export function dispatch(e: EmailJob, s: SmsJob): string {
  return e.run() + s.run();
}

export function onlySms(jobs: SmsJob[]): string[] {
  return jobs.map((j) => j.run());
}
