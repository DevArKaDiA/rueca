import welcome from "./greet";

export default {
  start(user: string): string {
    return welcome(user);
  },
  stop(): string {
    return "bye";
  },
};
