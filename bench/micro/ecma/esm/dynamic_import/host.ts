export async function boot(name: string): Promise<string> {
  const plugins = await import("./plugins");
  return plugins.register(name);
}
