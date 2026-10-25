export class Button {
  private clicks = 0;

  onClick = (): number => {
    this.clicks += 1;
    return this.render();
  };

  render(): number {
    return this.clicks;
  }
}

export function press(b: Button): number {
  b.onClick();
  return b.render();
}
