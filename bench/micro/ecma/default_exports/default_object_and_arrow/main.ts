import app from "./app";

export function session(user: string): string {
  const hello = app.start(user);
  return hello + app.stop();
}
