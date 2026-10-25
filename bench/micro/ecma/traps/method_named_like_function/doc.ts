function save(text: string): string {
  return `disk:${text}`;
}

export class Doc {
  constructor(private text: string) {}

  save(): string {
    return save(this.text);
  }

  autosave(): string {
    return this.save();
  }
}
