import { describe, expect, it } from "vitest";
import { applySchema, d1Db, sqliteDb } from "../src/db.js";

describe("sqliteDb", () => {
  it("applies the schema twice without error", async () => {
    const db = sqliteDb(":memory:");
    await applySchema(db);
    await applySchema(db);
    const tables = await db.all<{ name: string }>(
      "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
      [],
    );
    expect(tables.map((t) => t.name)).toEqual(
      expect.arrayContaining(["app_codes", "login_tokens", "sessions", "users"]),
    );
  });

  it("round-trips run and all with params", async () => {
    const db = sqliteDb(":memory:");
    await applySchema(db);
    await db.run("INSERT INTO users (id, name, email, created_at) VALUES (?, ?, ?, ?)", [
      "u1",
      "Ana",
      "ana@example.com",
      123,
    ]);
    const rows = await db.all<{ name: string; created_at: number }>(
      "SELECT name, created_at FROM users WHERE email = ?",
      ["ana@example.com"],
    );
    expect(rows).toEqual([{ name: "Ana", created_at: 123 }]);
  });

  it("supports UPDATE ... RETURNING", async () => {
    const db = sqliteDb(":memory:");
    await applySchema(db);
    await db.run("INSERT INTO users (id, name, email, created_at) VALUES (?, ?, ?, ?)", [
      "u1",
      "Ana",
      "ana@example.com",
      1,
    ]);
    const rows = await db.all<{ id: string }>(
      "UPDATE users SET name = ? WHERE id = ? RETURNING id",
      ["Bia", "u1"],
    );
    expect(rows).toEqual([{ id: "u1" }]);
  });

  it("compares numbers against stringified params like D1 does", async () => {
    const db = sqliteDb(":memory:");
    await applySchema(db);
    await db.run("INSERT INTO users (id, name, email, created_at) VALUES (?, ?, ?, ?)", [
      "u1",
      "Ana",
      "ana@example.com",
      "100",
    ]);
    const rows = await db.all("SELECT id FROM users WHERE created_at > ?", ["99"]);
    expect(rows).toHaveLength(1);
  });
});

describe("d1Db", () => {
  it("sends stringified params and returns the first result's rows", async () => {
    const calls: unknown[] = [];
    const client = {
      d1: {
        database: {
          query: async (id: string, params: unknown) => {
            calls.push([id, params]);
            return { result: [{ results: [{ a: 1 }], success: true }] };
          },
        },
      },
    };
    const db = d1Db(client, "acct", "dbid");
    const rows = await db.all("SELECT ?", [5]);
    expect(rows).toEqual([{ a: 1 }]);
    expect(calls).toEqual([["dbid", { account_id: "acct", sql: "SELECT ?", params: ["5"] }]]);
  });
});
