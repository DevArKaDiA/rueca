export function render(text: string): string {
  return `<p>${text}</p>`;
}

export function pad(text: string, width = 8): string {
  return text.padEnd(width);
}
