import type { Mailer } from "./mailer";

export const notify = (m: Mailer, to: string): boolean => {
  return m.send(to);
};

export const notifyAll = (m: Mailer, list: string[]): boolean[] => {
  return list.map((to) => notify(m, to));
};
