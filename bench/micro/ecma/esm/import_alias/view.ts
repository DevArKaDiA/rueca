import { pad as padRight, render as renderText } from "./format";

function render(value: number): string {
  return renderText(padRight(String(value)));
}

export function show(value: number): string {
  const html = render(value);
  return html;
}
