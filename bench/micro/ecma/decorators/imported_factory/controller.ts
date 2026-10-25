import { route } from "./routes";

function render(items: string[]): string {
  return items.join(",");
}

export class Controller {
  @route("/items")
  list(): string {
    return render(["a", "b"]);
  }

  @route("/health")
  health(): string {
    return "ok";
  }
}
