import { createHash } from "node:crypto";
import { mkdtempSync, readdirSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createApp } from "../src/app.js";
import { createSession, createUser, findUserByEmail } from "../src/auth.js";
import { localBlobStore, type BlobStore } from "../src/blobs.js";
import { applySchema, sqliteDb, type Db } from "../src/db.js";
import { unwrapKey, writeFile } from "../src/files.js";

let db: Db;
let server: Server;
let base: string;
let clock: number;
let tokenA: string;
let tokenB: string;
let blobDir: string;
let blobs: BlobStore;
let inner: BlobStore;
let onPut: ((key: string) => Promise<void>) | null = null;
const kek = Buffer.alloc(32, 7);

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
  clock = 1_000_000;
  tokenA = await userToken("a@example.com");
  tokenB = await userToken("b@example.com");
  blobDir = mkdtempSync(join(tmpdir(), "catenga-files-"));
  inner = localBlobStore(blobDir);
  onPut = null;
  blobs = {
    ...inner,
    put: async (key, bytes) => {
      await inner.put(key, bytes);
      await onPut?.(key);
    },
  };
  const port = await freePort();
  base = `http://127.0.0.1:${port}`;
  const app = createApp({
    db,
    mailer: { sendLoginLink: async () => {} },
    baseUrl: base,
    now: () => clock,
    blobs,
    notesKek: kek,
  });
  server = await new Promise<Server>((resolve) => {
    const s = app.listen(port, "127.0.0.1", () => resolve(s));
  });
});

afterEach(async () => {
  await new Promise((r) => server.close(r));
  rmSync(blobDir, { recursive: true, force: true });
});

const sha = (b: Uint8Array | string) => createHash("sha256").update(b).digest("hex");
const auth = (t: string | null): Record<string, string> => (t ? { authorization: `Bearer ${t}` } : {});
const P = "2026/10/2026-10-22.md";

async function put(token: string | null, path: string, body: Uint8Array, rev: string, baseRev = "") {
  const res = await fetch(`${base}/api/files/content?path=${encodeURIComponent(path)}`, {
    method: "PUT",
    headers: { ...auth(token), "x-rev": rev, "x-base-rev": baseRev, "content-type": "application/octet-stream" },
    body: body as unknown as BodyInit,
  });
  return { status: res.status, body: (await res.json()) as { seq?: number; rev?: string; error?: string } };
}
const get = (token: string | null, path: string) =>
  fetch(`${base}/api/files/content?path=${encodeURIComponent(path)}`, { headers: auth(token) });
const del = async (token: string | null, path: string, baseRev = "") => {
  const res = await fetch(`${base}/api/files/content?path=${encodeURIComponent(path)}`, {
    method: "DELETE",
    headers: { ...auth(token), "x-base-rev": baseRev },
  });
  return { status: res.status, body: (await res.json()) as { seq?: number; rev?: string; error?: string } };
};
type Change = { path: string; rev: string; size: number; deleted: boolean; seq: number };
async function changes(token: string | null, cursor: number) {
  const res = await fetch(`${base}/api/files/changes?cursor=${cursor}`, { headers: auth(token) });
  return {
    status: res.status,
    body: (await res.json()) as { cursor: number; hasMore: boolean; changes: Change[]; error?: string },
  };
}
const blob = (...b: number[]) => new Uint8Array(b);
/** Every stored object as its path relative to the blob dir. */
const objects = (): string[] =>
  readdirSync(blobDir, { recursive: true, withFileTypes: true })
    .filter((e) => e.isFile())
    .map((e) => join(e.parentPath, e.name).slice(blobDir.length + 1))
    .sort();

describe("GET /api/notes/key", () => {
  const getKey = async (token: string | null) => {
    const res = await fetch(`${base}/api/notes/key`, { headers: auth(token) });
    return { status: res.status, body: (await res.json()) as { key?: string; error?: string } };
  };

  it("creates a 32-byte key once and returns the same one afterwards", async () => {
    const a = await getKey(tokenA);
    expect(a.status).toBe(200);
    expect(Buffer.from(a.body.key!, "base64")).toHaveLength(32);
    expect((await getKey(tokenA)).body.key).toBe(a.body.key);
  });

  it("gives each user a different key", async () => {
    expect((await getKey(tokenA)).body.key).not.toBe((await getKey(tokenB)).body.key);
  });

  it("stores the key only wrapped: unreadable without the KEK, bound to the user", async () => {
    const { body } = await getKey(tokenA);
    const rows = await db.all<{ user_id: string; wrapped: string }>("SELECT user_id, wrapped FROM user_keys", []);
    expect(rows).toHaveLength(1);
    const raw = Buffer.from(body.key!, "base64");
    expect(rows[0]!.wrapped).not.toContain(body.key!);
    expect(Buffer.from(rows[0]!.wrapped, "base64").includes(raw)).toBe(false);
    expect(unwrapKey(kek, rows[0]!.user_id, rows[0]!.wrapped).equals(raw)).toBe(true);
    expect(() => unwrapKey(Buffer.alloc(32, 8), rows[0]!.user_id, rows[0]!.wrapped)).toThrow();
    expect(() => unwrapKey(kek, "someone-else", rows[0]!.wrapped)).toThrow();
  });

  it("requires sign-in", async () => {
    expect((await getKey(null)).status).toBe(401);
    expect((await getKey("nope")).status).toBe(401);
  });
});

describe("files content", () => {
  it("round-trips a blob and sends X-Rev", async () => {
    const data = blob(1, 2, 3, 4);
    const rev = sha("v1");
    const up = await put(tokenA, P, data, rev);
    expect(up.status).toBe(200);
    expect(up.body.seq).toBe(1);
    const res = await get(tokenA, P);
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("application/octet-stream");
    expect(res.headers.get("x-rev")).toBe(rev);
    expect([...new Uint8Array(await res.arrayBuffer())]).toEqual([1, 2, 3, 4]);
  });

  it("stores the object under u/<userId>/<sha256(path)>/<rev> with no readable path", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    const u = await findUserByEmail(db, "a@example.com");
    expect(objects()).toEqual([`u/${u!.id}/${sha(P)}/${sha("v1")}`]);
  });

  it("removes the previous version's object after a successful update", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    await put(tokenA, P, blob(2), sha("v2"), sha("v1"));
    expect(objects()).toHaveLength(1);
    expect(objects()[0]).toMatch(new RegExp(`/${sha("v2")}$`));
  });

  it("removes the loser's object on 409", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    await put(tokenA, P, blob(2), sha("v2"), sha("v1"));
    expect((await put(tokenA, P, blob(3), sha("v3"), sha("v1"))).status).toBe(409);
    expect(objects()).toHaveLength(1);
    expect(objects()[0]).toMatch(new RegExp(`/${sha("v2")}$`));
  });

  it("keeps the winner's row and content when a racing upload loses after writing its blob", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    const u = await findUserByEmail(db, "a@example.com");
    // The winner commits between the loser's blob write and the loser's row update.
    onPut = async (key) => {
      if (!key.endsWith(sha("loser"))) return;
      onPut = null;
      const w = await writeFile(db, blobs, u!.id, P, blob(2, 2), sha("winner"), sha("v1"), clock);
      expect(w.status).toBe(200);
    };
    const lost = await put(tokenA, P, blob(9), sha("loser"), sha("v1"));
    expect(lost.status).toBe(409);
    expect(lost.body).toEqual({ rev: sha("winner") });
    const res = await get(tokenA, P);
    expect(res.headers.get("x-rev")).toBe(sha("winner"));
    expect([...new Uint8Array(await res.arrayBuffer())]).toEqual([2, 2]);
    expect(objects()).toHaveLength(1);
  });

  it("accepts every syncable path shape", async () => {
    for (const p of ["config.json", "2026/01/2026-01-31.md", "2026/01/2026-01-31.sessions.json"]) {
      expect((await put(tokenA, p, blob(1), sha(p))).status).toBe(200);
    }
  });

  it("rejects paths outside the synced set with 400", async () => {
    const bad = [
      "../2026/10/2026-10-22.md",
      "/2026/10/2026-10-22.md",
      "2026/10/2026-10-22.md/",
      "2026/10/2026-10-22.md.tmp",
      "2026/10/notes.md",
      "2026/1/2026-01-22.md",
      "goals.json.imported",
      "config.json.bak",
      "a/../config.json",
      "2026\\10\\2026-10-22.md",
      "",
    ];
    for (const p of bad) {
      expect((await put(tokenA, p, blob(1), sha("x"))).status, p).toBe(400);
      expect((await get(tokenA, p)).status, p).toBe(400);
      expect((await del(tokenA, p)).status, p).toBe(400);
    }
    const missing = await fetch(`${base}/api/files/content`, { headers: auth(tokenA) });
    expect(missing.status).toBe(400);
  });

  it("round-trips conflict copies (spaces and parentheses URL-encoded)", async () => {
    const paths = [
      "2026/10/2026-10-22 (conflict 2026-10-22 1010).md",
      "2026/10/2026-10-22 (conflict 2026-10-23 0905) 2.md",
      "config (conflict 2026-10-22 1010).json",
      "config (conflict 2026-10-22 1010) 12.json",
    ];
    for (const [i, p] of paths.entries()) {
      expect((await put(tokenA, p, blob(i + 1), sha(p))).status, p).toBe(200);
      const res = await get(tokenA, p);
      expect(res.status, p).toBe(200);
      expect([...new Uint8Array(await res.arrayBuffer())]).toEqual([i + 1]);
      expect(res.headers.get("x-rev")).toBe(sha(p));
    }
    const u = await findUserByEmail(db, "a@example.com");
    expect(objects()).toContain(`u/${u!.id}/${sha(paths[0]!)}/${sha(paths[0]!)}`);
    expect((await changes(tokenA, 0)).body.changes.map((c) => c.path)).toEqual(paths);
    expect((await del(tokenA, paths[0]!, sha(paths[0]!))).status).toBe(200);
  });

  it("rejects near-miss conflict paths", async () => {
    const bad = [
      "2026/10/2026-10-22(conflict 2026-10-22 1010).md",
      "2026/10/2026-10-22 (conflict 2026-10-22 10:10).md",
      "2026/10/2026-10-22 (conflict 2026-10-22 101).md",
      "2026/10/2026-10-22 (conflict 2026-10-22 1010) x.md",
      "2026/10/2026-10-22 (conflict 2026-10-22 1010).sessions.json",
      "2026/10/2026-10-22 (conflict 2026-10-22 1010).md.tmp",
      "2026/10/2026-10-32 (conflict 2026-10-22 1010).md",
      "2026/11/2026-10-22 (conflict 2026-10-22 1010).md",
      "../2026/10/2026-10-22 (conflict 2026-10-22 1010).md",
      "2026/10/../2026-10-22 (conflict 2026-10-22 1010).md",
      "config(conflict 2026-10-22 1010).json",
      "config (conflict 2026-10-22 1010).md",
      "config (conflict 2026-10-22 1010) x.json",
      "../config (conflict 2026-10-22 1010).json",
      "sub/config (conflict 2026-10-22 1010).json",
    ];
    for (const p of bad) expect((await put(tokenA, p, blob(1), sha("x"))).status, p).toBe(400);
  });

  it("returns 404 for a missing or deleted file", async () => {
    expect((await get(tokenA, P)).status).toBe(404);
    await put(tokenA, P, blob(1), sha("v1"));
    await del(tokenA, P, sha("v1"));
    expect((await get(tokenA, P)).status).toBe(404);
  });

  it("replaces content when the base rev matches", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    const up = await put(tokenA, P, blob(2, 2), sha("v2"), sha("v1"));
    expect(up.status).toBe(200);
    expect(up.body.seq).toBe(2);
    expect([...new Uint8Array(await (await get(tokenA, P)).arrayBuffer())]).toEqual([2, 2]);
    expect((await get(tokenA, P)).headers.get("x-rev")).toBe(sha("v2"));
  });

  it("answers 409 with the current rev on a stale base and stores nothing", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    await put(tokenA, P, blob(2), sha("v2"), sha("v1"));
    const stale = await put(tokenA, P, blob(3), sha("v3"), sha("v1"));
    expect(stale.status).toBe(409);
    expect(stale.body).toEqual({ rev: sha("v2") });
    expect([...new Uint8Array(await (await get(tokenA, P)).arrayBuffer())]).toEqual([2]);
    expect((await changes(tokenA, 0)).body.changes).toHaveLength(1);
    // a new upload (empty base) against an existing file is also stale
    expect((await put(tokenA, P, blob(4), sha("v4"), "")).status).toBe(409);
  });

  it("treats re-uploading the same rev as a no-op 200", async () => {
    const first = await put(tokenA, P, blob(1), sha("v1"));
    const again = await put(tokenA, P, blob(1), sha("v1"), "");
    expect(again.status).toBe(200);
    expect(again.body.seq).toBe(first.body.seq);
    expect((await changes(tokenA, 0)).body.changes).toHaveLength(1);
  });

  it("lets anyone re-create a file after a delete", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    await del(tokenA, P, sha("v1"));
    const up = await put(tokenA, P, blob(5), sha("v5"), sha("anything"));
    expect(up.status).toBe(200);
    expect((await get(tokenA, P)).status).toBe(200);
  });

  it("validates the rev headers and the body", async () => {
    expect((await put(tokenA, P, blob(1), "not-hex")).status).toBe(400);
    expect((await put(tokenA, P, blob(1), sha("x").toUpperCase())).status).toBe(400);
    expect((await put(tokenA, P, blob(1), sha("x"), "zz")).status).toBe(400);
    expect((await put(tokenA, P, new Uint8Array(0), sha("x"))).status).toBe(400);
  });

  it("enforces the 6 MB body limit with 413", async () => {
    const ok = await put(tokenA, P, new Uint8Array(6 * 1024 * 1024), sha("big"));
    expect(ok.status).toBe(200);
    const big = await put(tokenA, "2026/10/2026-10-23.md", new Uint8Array(6 * 1024 * 1024 + 1), sha("bigger"));
    expect(big.status).toBe(413);
  });

  it("requires sign-in on every route", async () => {
    expect((await put(null, P, blob(1), sha("x"))).status).toBe(401);
    expect((await get(null, P)).status).toBe(401);
    expect((await del(null, P)).status).toBe(401);
    expect((await changes(null, 0)).status).toBe(401);
  });

  it("isolates users from each other", async () => {
    await put(tokenA, P, blob(1), sha("a"));
    expect((await get(tokenB, P)).status).toBe(404);
    expect((await changes(tokenB, 0)).body.changes).toEqual([]);
    expect((await put(tokenB, P, blob(9), sha("b"))).status).toBe(200); // no conflict with A's file
    expect((await get(tokenA, P)).headers.get("x-rev")).toBe(sha("a"));
    expect((await del(tokenB, P, sha("b"))).status).toBe(200);
    expect((await get(tokenA, P)).status).toBe(200);
  });
});

describe("DELETE /api/files/content", () => {
  it("tombstones the row, removes the blob and shows up in changes", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    const d = await del(tokenA, P, sha("v1"));
    expect(d.status).toBe(200);
    expect(d.body.seq).toBe(2);
    const c = (await changes(tokenA, 1)).body.changes;
    expect(c).toEqual([{ path: P, rev: sha("v1"), size: 1, deleted: true, seq: 2 }]);
    expect(objects()).toEqual([]);
  });

  it("answers 409 on a stale base and keeps the file", async () => {
    await put(tokenA, P, blob(1), sha("v1"));
    const d = await del(tokenA, P, sha("old"));
    expect(d.status).toBe(409);
    expect(d.body).toEqual({ rev: sha("v1") });
    expect((await get(tokenA, P)).status).toBe(200);
  });

  it("is idempotent for missing or already deleted files", async () => {
    expect((await del(tokenA, P, sha("v1"))).status).toBe(200);
    await put(tokenA, P, blob(1), sha("v1"));
    await del(tokenA, P, sha("v1"));
    expect((await del(tokenA, P, sha("v1"))).status).toBe(200);
    expect((await changes(tokenA, 0)).body.changes).toHaveLength(1);
  });
});

describe("GET /api/files/changes", () => {
  it("lists changes after the cursor in seq order with the latest state per path", async () => {
    await put(tokenA, "config.json", blob(1), sha("c1"));
    await put(tokenA, P, blob(2, 2), sha("p1"));
    await put(tokenA, "config.json", blob(3), sha("c2"), sha("c1"));
    const all = await changes(tokenA, 0);
    expect(all.body).toEqual({
      cursor: 3,
      hasMore: false,
      changes: [
        { path: P, rev: sha("p1"), size: 2, deleted: false, seq: 2 },
        { path: "config.json", rev: sha("c2"), size: 1, deleted: false, seq: 3 },
      ],
    });
    expect(await changes(tokenA, 3)).toMatchObject({ body: { cursor: 3, hasMore: false, changes: [] } });
  });

  it("pages at 500 with hasMore", async () => {
    const u = await findUserByEmail(db, "a@example.com");
    const rows: string[] = [];
    for (let i = 1; i <= 501; i++) {
      rows.push(`('${u!.id}','2026/10/f${i}','${sha(String(i))}',1,0,1,${i})`);
    }
    await db.exec(
      `INSERT INTO note_files(user_id,path,rev,size,deleted,changed_at,seq) VALUES ${rows.join(",")}`,
    );
    const first = await changes(tokenA, 0);
    expect(first.body.changes).toHaveLength(500);
    expect(first.body.hasMore).toBe(true);
    expect(first.body.cursor).toBe(500);
    const second = await changes(tokenA, first.body.cursor);
    expect(second.body.changes.map((c) => c.seq)).toEqual([501]);
    expect(second.body.hasMore).toBe(false);
  });

  it("validates the cursor", async () => {
    for (const c of ["-1", "abc", "1.5", ""]) {
      const res = await fetch(`${base}/api/files/changes?cursor=${c}`, { headers: auth(tokenA) });
      expect(res.status, c).toBe(400);
    }
    const none = await fetch(`${base}/api/files/changes`, { headers: auth(tokenA) });
    expect(none.status).toBe(200); // cursor defaults to 0
  });

  it("shares the per-user seq counter with /api/sync", async () => {
    await fetch(`${base}/api/sync`, {
      method: "POST",
      headers: { ...auth(tokenA), "content-type": "application/json" },
      body: JSON.stringify({
        cursor: 0,
        changes: [{ kind: "tag", key: "x", data: { name: "x", color: "sun" }, changedAt: 5, deleted: false }],
      }),
    });
    expect((await put(tokenA, P, blob(1), sha("v1"))).body.seq).toBe(2);
  });
});

describe("notes sync not configured", () => {
  it("answers 503 on the key and file routes but keeps /api/sync and /api/me working", async () => {
    const port = await freePort();
    const off = await new Promise<Server>((resolve) => {
      const a = createApp({
        db,
        mailer: { sendLoginLink: async () => {} },
        baseUrl: `http://127.0.0.1:${port}`,
        now: () => clock,
      });
      const s = a.listen(port, "127.0.0.1", () => resolve(s));
    });
    try {
      const o = `http://127.0.0.1:${port}`;
      const h = auth(tokenA);
      for (const [m, path] of [
        ["GET", "/api/notes/key"],
        ["GET", "/api/files/changes"],
        ["GET", `/api/files/content?path=${P}`],
        ["PUT", `/api/files/content?path=${P}`],
        ["DELETE", `/api/files/content?path=${P}`],
      ] as const) {
        const res = await fetch(o + path, { method: m, headers: h });
        expect(res.status, `${m} ${path}`).toBe(503);
        expect(await res.json()).toEqual({ error: "Notes sync isn't configured on this server" });
      }
      expect((await fetch(`${o}/api/me`, { headers: h })).status).toBe(200);
      expect((await fetch(`${o}/api/sync/status`, { headers: h })).status).toBe(200);
    } finally {
      await new Promise((r) => off.close(r));
    }
  });
});

describe("CORS and rate limit", () => {
  it("allows PUT/DELETE and the rev headers for the app origin", async () => {
    const res = await fetch(`${base}/api/files/content?path=${P}`, {
      method: "OPTIONS",
      headers: { origin: "tauri://localhost", "access-control-request-method": "PUT" },
    });
    expect(res.status).toBe(204);
    expect(res.headers.get("access-control-allow-methods")).toMatch(/PUT/);
    expect(res.headers.get("access-control-allow-methods")).toMatch(/DELETE/);
    const headers = res.headers.get("access-control-allow-headers")!.toLowerCase();
    expect(headers).toContain("x-base-rev");
    expect(headers).toContain("x-rev");
    const got = await fetch(`${base}/api/files/changes`, {
      headers: { ...auth(tokenA), origin: "tauri://localhost" },
    });
    expect(got.headers.get("access-control-expose-headers")!.toLowerCase()).toContain("x-rev");
  });

  it("limits to 600 requests per hour per token across the file and key routes", async () => {
    for (let i = 0; i < 600; i++) {
      const r = await fetch(`${base}/api/files/changes`, { headers: auth(tokenA) });
      if (r.status !== 200) throw new Error(`request ${i} got ${r.status}`);
    }
    expect((await changes(tokenA, 0)).status).toBe(429);
    expect((await changes(tokenB, 0)).status).toBe(200);
  });
});
