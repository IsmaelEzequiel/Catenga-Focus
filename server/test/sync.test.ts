import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createApp } from "../src/app.js";
import type { BlobStore } from "../src/blobs.js";
import { createSession, createUser, findUserByEmail } from "../src/auth.js";
import { applySchema, sqliteDb, type Db } from "../src/db.js";

const nullBlobs: BlobStore = { put: async () => {}, get: async () => null, del: async () => {} };
let db: Db;
let server: Server;
let base: string;
let clock: number;
let tokenA: string;
let tokenB: string;

async function freePort(): Promise<number> {
  const s = createServer();
  await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
  const { port } = s.address() as AddressInfo;
  await new Promise((r) => s.close(r));
  return port;
}

async function userToken(email: string): Promise<string> {
  await createUser(db, "U", email, clock);
  const u = await findUserByEmail(db, email);
  return createSession(db, u!.id, "app", clock);
}

beforeEach(async () => {
  db = sqliteDb(":memory:");
  await applySchema(db);
  await applySchema(db); // idempotent
  clock = 1_000_000;
  tokenA = await userToken("a@example.com");
  tokenB = await userToken("b@example.com");
  const port = await freePort();
  base = `http://127.0.0.1:${port}`;
  const app = createApp({
    db,
    mailer: { sendLoginLink: async () => {} },
    baseUrl: base,
    blobs: nullBlobs,
    notesKek: Buffer.alloc(32),
    now: () => clock,
  });
  server = await new Promise<Server>((resolve) => {
    const s = app.listen(port, "127.0.0.1", () => resolve(s));
  });
});

afterEach(async () => {
  await new Promise((r) => server.close(r));
});

type Item = { kind: string; key: string; data: unknown; changedAt: number; deleted: boolean };
type Reply = {
  cursor: number;
  changes: Item[];
  hasMore: boolean;
  accepted: { kind: string; key: string }[];
  error?: string;
};

const item = (key: string, changedAt: number, data: unknown = { name: key, color: "sun" }, kind = "tag"): Item => ({
  kind,
  key,
  data,
  changedAt,
  deleted: false,
});
const tomb = (key: string, changedAt: number, kind = "tag"): Item => ({ kind, key, data: null, changedAt, deleted: true });

async function sync(token: string | null, body: unknown, raw?: string) {
  const res = await fetch(`${base}/api/sync`, {
    method: "POST",
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: raw ?? JSON.stringify(body),
  });
  return { status: res.status, body: (await res.json()) as Reply, headers: res.headers };
}
const push = (token: string, changes: Item[], cursor = 0) => sync(token, { cursor, changes });

describe("auth and CORS", () => {
  it("401 without or with a bad token", async () => {
    expect((await sync(null, { cursor: 0, changes: [] })).status).toBe(401);
    expect((await sync("nope", { cursor: 0, changes: [] })).status).toBe(401);
    const st = await fetch(`${base}/api/sync/status`);
    expect(st.status).toBe(401);
  });

  it("answers the CORS preflight for the app origin", async () => {
    const res = await fetch(`${base}/api/sync`, {
      method: "OPTIONS",
      headers: { origin: "tauri://localhost", "access-control-request-method": "POST" },
    });
    expect(res.status).toBe(204);
    expect(res.headers.get("access-control-allow-origin")).toBe("tauri://localhost");
    expect(res.headers.get("access-control-allow-headers")).toContain("Authorization");
  });
});

describe("push and pull", () => {
  it("stores items and returns them with increasing seq", async () => {
    const r = await push(tokenA, [item("a", 10), item("b", 11)]);
    expect(r.status).toBe(200);
    expect(r.body.accepted).toEqual([
      { kind: "tag", key: "a" },
      { kind: "tag", key: "b" },
    ]);
    expect(r.body.changes.map((c) => c.key)).toEqual(["a", "b"]);
    expect(r.body.changes[0]).toEqual({
      kind: "tag",
      key: "a",
      data: { name: "a", color: "sun" },
      changedAt: 10,
      deleted: false,
    });
    expect(r.body.cursor).toBeGreaterThan(0);
    expect(r.body.hasMore).toBe(false);
  });

  it("returns only rows newer than the cursor", async () => {
    const first = await push(tokenA, [item("a", 10)]);
    const second = await push(tokenA, [item("b", 11)], first.body.cursor);
    expect(second.body.changes.map((c) => c.key)).toEqual(["b"]);
    expect(second.body.cursor).toBeGreaterThan(first.body.cursor);
    const idle = await push(tokenA, [], second.body.cursor);
    expect(idle.body.changes).toEqual([]);
    expect(idle.body.cursor).toBe(second.body.cursor);
  });

  it("last write wins: newer replaces, older and ties are rejected", async () => {
    await push(tokenA, [item("a", 10, { name: "a", color: "sun" })]);
    const newer = await push(tokenA, [item("a", 20, { name: "a", color: "clay" })]);
    expect(newer.body.accepted).toHaveLength(1);
    const older = await push(tokenA, [item("a", 15, { name: "a", color: "sky" })]);
    expect(older.body.accepted).toEqual([]);
    const tie = await push(tokenA, [item("a", 20, { name: "a", color: "leaf" })]);
    expect(tie.body.accepted).toEqual([]);
    const all = await push(tokenA, [], 0);
    expect(all.body.changes).toHaveLength(1);
    expect(all.body.changes[0]!.data).toEqual({ name: "a", color: "clay" });
    expect(all.body.changes[0]!.changedAt).toBe(20);
  });

  it("a rejected change returns the newer server row on pull", async () => {
    const first = await push(tokenA, [item("a", 20, { name: "a", color: "clay" })]);
    const r = await push(tokenA, [item("a", 5)], first.body.cursor);
    expect(r.body.accepted).toEqual([]);
    expect(r.body.changes).toEqual([]); // nothing newer than the cursor
    const fresh = await push(tokenA, [item("a", 5)], 0);
    expect(fresh.body.changes[0]!.changedAt).toBe(20);
  });

  it("tombstones replace data and can be overwritten by newer writes", async () => {
    await push(tokenA, [item("g1", 10, { title: "x" }, "goal")]);
    const t = await push(tokenA, [tomb("g1", 20, "goal")]);
    expect(t.body.accepted).toHaveLength(1);
    const pulled = await push(tokenA, [], 0);
    expect(pulled.body.changes[0]).toEqual({ kind: "goal", key: "g1", data: null, changedAt: 20, deleted: true });
    const back = await push(tokenA, [item("g1", 30, { title: "y" }, "goal")]);
    expect(back.body.changes[0]!.deleted).toBe(false);
    expect(back.body.changes[0]!.data).toEqual({ title: "y" });
  });

  it("pages with hasMore", async () => {
    const many = Array.from({ length: 500 }, (_, i) => item(`k${i}`, 10));
    await push(tokenA, many);
    await push(tokenA, [item("k500", 10), item("k501", 10)]);
    const p1 = await push(tokenA, [], 0);
    expect(p1.body.changes).toHaveLength(500);
    expect(p1.body.hasMore).toBe(true);
    const p2 = await push(tokenA, [], p1.body.cursor);
    expect(p2.body.changes.map((c) => c.key)).toEqual(["k500", "k501"]);
    expect(p2.body.hasMore).toBe(false);
  });

  it("isolates users", async () => {
    await push(tokenA, [item("a", 10)]);
    const b = await push(tokenB, [item("a", 5, { name: "a", color: "sky" })]);
    expect(b.body.accepted).toHaveLength(1); // not blocked by A's newer row
    expect(b.body.changes).toHaveLength(1);
    expect(b.body.changes[0]!.data).toEqual({ name: "a", color: "sky" });
    const a = await push(tokenA, [], 0);
    expect(a.body.changes[0]!.data).toEqual({ name: "a", color: "sun" });
    const st = await fetch(`${base}/api/sync/status`, { headers: { authorization: `Bearer ${tokenB}` } });
    expect(await st.json()).toEqual({ items: 1, lastSeq: 1 });
  });
});

describe("status", () => {
  it("counts items and reports the last seq", async () => {
    const empty = await fetch(`${base}/api/sync/status`, { headers: { authorization: `Bearer ${tokenA}` } });
    expect(await empty.json()).toEqual({ items: 0, lastSeq: 0 });
    await push(tokenA, [item("a", 1), item("b", 1), tomb("c", 1)]);
    const st = await fetch(`${base}/api/sync/status`, { headers: { authorization: `Bearer ${tokenA}` } });
    expect(await st.json()).toEqual({ items: 3, lastSeq: 3 });
  });
});

describe("validation", () => {
  const bad = async (body: unknown) => {
    const r = await sync(tokenA, body);
    expect(r.status).toBe(400);
    expect(typeof r.body.error).toBe("string");
  };

  it("rejects malformed requests", async () => {
    await bad({ changes: [] });
    await bad({ cursor: -1, changes: [] });
    await bad({ cursor: 1.5, changes: [] });
    await bad({ cursor: 0 });
    await bad({ cursor: 0, changes: "x" });
    await bad({ cursor: 0, changes: [item("a", 1, {}, "nope")] });
    await bad({ cursor: 0, changes: [item("", 1)] });
    await bad({ cursor: 0, changes: [item("k".repeat(301), 1)] });
    await bad({ cursor: 0, changes: [{ ...item("a", 1), changedAt: "1" }] });
    await bad({ cursor: 0, changes: [{ ...item("a", 1), changedAt: -1 }] });
    await bad({ cursor: 0, changes: [{ ...item("a", 1), deleted: "no" }] });
    await bad({ cursor: 0, changes: [{ ...item("a", 1), data: null }] });
    await bad({ cursor: 0, changes: [{ ...tomb("a", 1), data: { x: 1 } }] });
    await bad({ cursor: 0, changes: [item("a", 1, [1])] });
    await bad({ cursor: 0, changes: [item("a", 1, { big: "x".repeat(16 * 1024) })] });
    await bad({ cursor: 0, changes: Array.from({ length: 501 }, (_, i) => item(`k${i}`, 1)) });
    const r = await sync(tokenA, null, "{not json");
    expect(r.status).toBe(400);
  });

  it("accepts the boundary values", async () => {
    const r = await push(tokenA, [item("k".repeat(300), 0, { v: "x".repeat(16 * 1024 - 8) })]);
    expect(r.status).toBe(200);
    expect(r.body.accepted).toHaveLength(1);
  });

  it("a bad request stores nothing", async () => {
    await sync(tokenA, { cursor: 0, changes: [item("ok", 1), item("", 1)] });
    const st = await fetch(`${base}/api/sync/status`, { headers: { authorization: `Bearer ${tokenA}` } });
    expect(await st.json()).toEqual({ items: 0, lastSeq: 0 });
  });

  it("refuses bodies over 1 MB with 413", async () => {
    const res = await fetch(`${base}/api/sync`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${tokenA}` },
      body: JSON.stringify({ cursor: 0, changes: [], pad: "x".repeat(1_100_000) }),
    });
    expect(res.status).toBe(413);
  });
});

describe("rate limit", () => {
  it("allows 120 requests per hour per token, then 429", async () => {
    for (let i = 0; i < 120; i++) expect((await push(tokenA, [])).status).toBe(200);
    expect((await push(tokenA, [])).status).toBe(429);
    expect((await push(tokenB, [])).status).toBe(200);
    clock += 3_600_001;
    expect((await push(tokenA, [])).status).toBe(200);
  });
});
