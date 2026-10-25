function validate(id: string): void {
  if (!id) {
    throw new Error("empty id");
  }
}

export class Model {
  constructor(protected readonly id: string) {
    validate(id);
  }
}

export class User extends Model {
  constructor(id: string, readonly email: string) {
    super(id);
  }
}

export function makeUser(): User {
  return new User("u1", "a@b.c");
}
