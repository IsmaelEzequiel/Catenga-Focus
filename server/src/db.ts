import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";

// D1's HTTP API only accepts string params, so both backends stringify them.
// SQLite's column affinity turns numeric-looking text back into integers.
export type Param = string | number;

export type Db = {
  all<T>(sql: string, params: Param[]): Promise<T[]>;
  run(sql: string, params: Param[]): Promise<void>;
  exec(script: string): Promise<void>;
};

const strings = (params: Param[]): string[] => params.map(String);

export function sqliteDb(path: string): Db {
  const db = new DatabaseSync(path);
  return {
    all: async <T>(sql: string, params: Param[]) =>
      db.prepare(sql).all(...strings(params)) as T[],
    run: async (sql, params) => {
      db.prepare(sql).run(...strings(params));
    },
    exec: async (script) => {
      db.exec(script);
    },
  };
}

// The slice of the `cloudflare` client we use (structural, so tests can fake it).
export type D1Client = {
  d1: {
    database: {
      query(
        databaseId: string,
        params: { account_id: string; sql: string; params?: string[] },
      ): PromiseLike<{ result: Array<{ results?: unknown[] }> }>;
    };
  };
};

export function d1Db(client: D1Client, accountId: string, databaseId: string): Db {
  const query = async (sql: string, params: Param[]) => {
    const page = await client.d1.database.query(databaseId, {
      account_id: accountId,
      sql,
      params: strings(params),
    });
    return page.result[0]?.results ?? [];
  };
  return {
    all: async <T>(sql: string, params: Param[]) => (await query(sql, params)) as T[],
    run: async (sql, params) => {
      await query(sql, params);
    },
    exec: async (script) => {
      await query(script, []);
    },
  };
}

export async function applySchema(db: Db): Promise<void> {
  await db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));
}
