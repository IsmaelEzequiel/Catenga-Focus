import type { Db } from "./db.js";
import { sha256Hex } from "./auth.js";

export const SYNC_KINDS = ["tag", "calendar", "project", "goal"] as const;
export type SyncKind = (typeof SYNC_KINDS)[number];

export const MAX_CHANGES = 500;
export const MAX_KEY = 300;
export const MAX_DATA = 16 * 1024;

export type SyncItem = {
  kind: SyncKind;
  key: string;
  data: Record<string, unknown> | null;
  changedAt: number;
  deleted: boolean;
};

export type SyncRequest = { cursor: number; changes: SyncItem[] };

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/** Validates the request body; returns the parsed request or a message for the 400. */
export function parseSyncRequest(body: unknown): SyncRequest | string {
  if (!isObject(body)) return "Body must be a JSON object";
  const { cursor, changes } = body;
  if (typeof cursor !== "number" || !Number.isSafeInteger(cursor) || cursor < 0) return "Invalid cursor";
  if (!Array.isArray(changes)) return "changes must be an array";
  if (changes.length > MAX_CHANGES) return `At most ${MAX_CHANGES} changes per request`;
  const out: SyncItem[] = [];
  for (const [i, c] of changes.entries()) {
    const at = `change ${i}`;
    if (!isObject(c)) return `${at}: must be an object`;
    const { kind, key, data, changedAt, deleted } = c;
    if (typeof kind !== "string" || !(SYNC_KINDS as readonly string[]).includes(kind)) return `${at}: unknown kind`;
    if (typeof key !== "string" || key.length < 1 || key.length > MAX_KEY) return `${at}: invalid key`;
    if (typeof changedAt !== "number" || !Number.isSafeInteger(changedAt) || changedAt < 0) {
      return `${at}: invalid changedAt`;
    }
    if (typeof deleted !== "boolean") return `${at}: deleted must be a boolean`;
    if (deleted) {
      if (data !== null && data !== undefined) return `${at}: data must be null when deleted`;
    } else {
      if (!isObject(data)) return `${at}: data must be an object`;
      if (Buffer.byteLength(JSON.stringify(data)) > MAX_DATA) return `${at}: data too large`;
    }
    out.push({
      kind: kind as SyncKind,
      key,
      data: deleted ? null : (data as Record<string, unknown>),
      changedAt,
      deleted,
    });
  }
  return { cursor, changes: out };
}

type Row = { kind: SyncKind; key: string; data: string | null; changed_at: number; deleted: number; seq: number };

export type SyncResponse = {
  cursor: number;
  changes: SyncItem[];
  hasMore: boolean;
  accepted: { kind: SyncKind; key: string }[];
};

/** The user id behind a bearer token, or null. */
export async function sessionUserId(db: Db, token: string, now: number): Promise<string | null> {
  const rows = await db.all<{ user_id: string }>(
    "SELECT user_id FROM sessions WHERE token_hash = ? AND expires_at > ?",
    [sha256Hex(token), now],
  );
  return rows[0]?.user_id ?? null;
}

export async function nextSeq(db: Db, userId: string): Promise<number> {
  const rows = await db.all<{ value: number }>(
    `INSERT INTO sync_counters (user_id, value) VALUES (?, 1)
     ON CONFLICT(user_id) DO UPDATE SET value = value + 1 RETURNING value`,
    [userId],
  );
  return Number(rows[0]!.value);
}

/** Applies one change with last-write-wins; true when it was accepted. */
async function applyChange(db: Db, userId: string, c: SyncItem): Promise<boolean> {
  const existing = await db.all<{ changed_at: number }>(
    "SELECT changed_at FROM sync_items WHERE user_id = ? AND kind = ? AND key = ?",
    [userId, c.kind, c.key],
  );
  if (existing[0] && c.changedAt <= Number(existing[0].changed_at)) return false;
  const seq = await nextSeq(db, userId);
  // The WHERE keeps LWW atomic if two requests race on the same row.
  await db.run(
    `INSERT INTO sync_items (user_id, kind, key, data, changed_at, deleted, seq)
     VALUES (?, ?, ?, NULLIF(?, ''), ?, ?, ?)
     ON CONFLICT(user_id, kind, key) DO UPDATE SET
       data = excluded.data, changed_at = excluded.changed_at, deleted = excluded.deleted, seq = excluded.seq
     WHERE excluded.changed_at > sync_items.changed_at`,
    [userId, c.kind, c.key, c.data === null ? "" : JSON.stringify(c.data), c.changedAt, c.deleted ? 1 : 0, seq],
  );
  return true;
}

export async function runSync(db: Db, userId: string, req: SyncRequest): Promise<SyncResponse> {
  const accepted: SyncResponse["accepted"] = [];
  for (const c of req.changes) {
    if (await applyChange(db, userId, c)) accepted.push({ kind: c.kind, key: c.key });
  }
  const rows = await db.all<Row>(
    `SELECT kind, key, data, changed_at, deleted, seq FROM sync_items
     WHERE user_id = ? AND seq > ? ORDER BY seq LIMIT ?`,
    [userId, req.cursor, MAX_CHANGES + 1],
  );
  const page = rows.slice(0, MAX_CHANGES);
  return {
    cursor: page.length ? Number(page[page.length - 1]!.seq) : req.cursor,
    changes: page.map((r) => ({
      kind: r.kind,
      key: r.key,
      data: r.data === null ? null : (JSON.parse(r.data) as Record<string, unknown>),
      changedAt: Number(r.changed_at),
      deleted: Number(r.deleted) === 1,
    })),
    hasMore: rows.length > MAX_CHANGES,
    accepted,
  };
}

export async function syncStatus(db: Db, userId: string): Promise<{ items: number; lastSeq: number }> {
  const items = await db.all<{ n: number }>("SELECT COUNT(*) AS n FROM sync_items WHERE user_id = ?", [userId]);
  const seq = await db.all<{ value: number }>("SELECT value FROM sync_counters WHERE user_id = ?", [userId]);
  return { items: Number(items[0]?.n ?? 0), lastSeq: Number(seq[0]?.value ?? 0) };
}
