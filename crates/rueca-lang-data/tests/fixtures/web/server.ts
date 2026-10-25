import config from "config";

export function start(): string {
  const url = process.env.DATABASE_URL ?? "";
  const level = process.env["LOG_LEVEL"];
  const { REDIS_URL, QUEUE: queue } = process.env;
  const mode = import.meta.env.MODE;
  const port = config.get("server.port");
  return `${url}${level}${REDIS_URL}${queue}${mode}${port}`;
}
