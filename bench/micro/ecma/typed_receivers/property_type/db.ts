export class Db {
  query(sql: string): string[] {
    return [sql];
  }
}

export class Repository {
  private readonly db = new Db();
  private backup?: Db;

  all(): string[] {
    return this.db.query("select *");
  }

  mirror(): string[] {
    return this.backup?.query("select 1") ?? [];
  }
}
