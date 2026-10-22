import { createCipheriv, createDecipheriv, createHash, randomBytes } from "node:crypto";
import type { BlobStore } from "./blobs.js";
import type { Db } from "./db.js";
import { nextSeq } from "./sync.js";

export const MAX_FILE_BYTES = 6 * 1024 * 1024;
export const MAX_FILE_CHANGES = 500;

// --- per-user notes key, wrapped with the server master key (KEK) ----------------

/** AES-256-GCM(KEK, key) with the user id as AAD; stored as base64(iv 12 | tag 16 | ciphertext). */
export function wrapKey(kek: Buffer, userId: string, key: Buffer): string {
  const iv = randomBytes(12);
  const c = createCipheriv("aes-256-gcm", kek, iv);
  c.setAAD(Buffer.from(userId));
  const ct = Buffer.concat([c.update(key), c.final()]);
  return Buffer.concat([iv, c.getAuthTag(), ct]).toString("base64");
}

/** Throws when the KEK or the user id is wrong, or the value was tampered with. */
export function unwrapKey(kek: Buffer, userId: string, wrapped: string): Buffer {
  const raw = Buffer.from(wrapped, "base64");
  const d = createDecipheriv("aes-256-gcm", kek, raw.subarray(0, 12));
  d.setAAD(Buffer.from(userId));
  d.setAuthTag(raw.subarray(12, 28));
  return Buffer.concat([d.update(raw.subarray(28)), d.final()]);
}

/** The user's notes key (base64), created on first use. */
export async function getOrCreateNotesKey(db: Db, kek: Buffer, userId: string, now: number): Promise<string> {
  const read = () => db.all<{ wrapped: string }>("SELECT wrapped FROM user_keys WHERE user_id = ?", [userId]);
  let rows = await read();
  if (!rows[0]) {
    await db.run(
      "INSERT INTO user_keys (user_id, wrapped, created_at) VALUES (?, ?, ?) ON CONFLICT(user_id) DO NOTHING",
      [userId, wrapKey(kek, userId, randomBytes(32)), now],
    );
    rows = await read(); // also covers a concurrent first call: the stored key wins
  }
  return unwrapKey(kek, userId, rows[0]!.wrapped).toString("base64");
}

// --- synced files ----------------------------------------------------------------

const DAY = "(?:0[1-9]|[12]\\d|3[01])";
const CONFLICT = " \\(conflict \\d{4}-\\d{2}-\\d{2} \\d{4}\\)(?: \\d+)?";
/**
 * `YYYY/MM/YYYY-MM-DD.md`, `YYYY/MM/YYYY-MM-DD.sessions.json` (folders match the name), `config.json`,
 * and conflict copies: `YYYY/MM/YYYY-MM-DD (conflict YYYY-MM-DD HHMM)[ N].md`, `config (conflict …)[ N].json`.
 */
const PATH_RE = new RegExp(
  "^(?:config(?:\\.json|" + CONFLICT + "\\.json)|" +
    "(\\d{4})/(0[1-9]|1[0-2])/\\1-\\2-" + DAY + "(?:\\.md|\\.sessions\\.json|" + CONFLICT + "\\.md))$",
);
const REV_RE = /^[0-9a-f]{64}$/;

export const isSyncPath = (p: string): boolean => PATH_RE.test(p);
export const isRev = (r: string): boolean => REV_RE.test(r);

/** One object per version, so a losing upload can never overwrite the winner's content. */
export const blobKey = (userId: string, path: string, rev: string): string =>
  `u/${userId}/${createHash("sha256").update(path).digest("hex")}/${rev}`;

/** Cleanup that must never fail the request: a stray object is harmless, an error here is not. */
const bestEffortDel = async (blobs: BlobStore, key: string): Promise<void> => {
  try {
    await blobs.del(key);
  } catch {
    /* ignored on purpose */
  }
};

type FileRow = { rev: string; size: number; deleted: number; seq: number };

const findRow = async (db: Db, userId: string, path: string): Promise<FileRow | undefined> =>
  (
    await db.all<FileRow>("SELECT rev, size, deleted, seq FROM note_files WHERE user_id = ? AND path = ?", [
      userId,
      path,
    ])
  )[0];

export type FileChange = { path: string; rev: string; size: number; deleted: boolean; seq: number };

export async function fileChanges(
  db: Db,
  userId: string,
  cursor: number,
): Promise<{ cursor: number; hasMore: boolean; changes: FileChange[] }> {
  const rows = await db.all<FileRow & { path: string }>(
    `SELECT path, rev, size, deleted, seq FROM note_files
     WHERE user_id = ? AND seq > ? ORDER BY seq LIMIT ?`,
    [userId, cursor, MAX_FILE_CHANGES + 1],
  );
  const page = rows.slice(0, MAX_FILE_CHANGES);
  return {
    cursor: page.length ? Number(page[page.length - 1]!.seq) : cursor,
    hasMore: rows.length > MAX_FILE_CHANGES,
    changes: page.map((r) => ({
      path: r.path,
      rev: r.rev,
      size: Number(r.size),
      deleted: Number(r.deleted) === 1,
      seq: Number(r.seq),
    })),
  };
}

export async function readFile(
  db: Db,
  blobs: BlobStore,
  userId: string,
  path: string,
): Promise<{ rev: string; bytes: Uint8Array } | null> {
  const row = await findRow(db, userId, path);
  if (!row || Number(row.deleted) === 1) return null;
  const bytes = await blobs.get(blobKey(userId, path, row.rev));
  return bytes ? { rev: row.rev, bytes } : null;
}

export type WriteResult = { status: 200; seq: number } | { status: 409; rev: string };

/** Stores a blob unless the server copy moved on from `baseRev`. Same `rev` again is a no-op. */
export async function writeFile(
  db: Db,
  blobs: BlobStore,
  userId: string,
  path: string,
  bytes: Uint8Array,
  rev: string,
  baseRev: string,
  now: number,
): Promise<WriteResult> {
  const row = await findRow(db, userId, path);
  if (row && Number(row.deleted) === 0) {
    if (row.rev === rev) return { status: 200, seq: Number(row.seq) };
    if (row.rev !== baseRev) return { status: 409, rev: row.rev };
  }
  const newKey = blobKey(userId, path, rev);
  await blobs.put(newKey, bytes);
  const seq = await nextSeq(db, userId);
  // The WHERE keeps the base-rev check atomic if two uploads race on the same path.
  const done = await db.all<{ seq: number }>(
    `INSERT INTO note_files (user_id, path, rev, size, deleted, changed_at, seq)
     VALUES (?, ?, ?, ?, 0, ?, ?)
     ON CONFLICT(user_id, path) DO UPDATE SET
       rev = excluded.rev, size = excluded.size, deleted = 0, changed_at = excluded.changed_at, seq = excluded.seq
     WHERE note_files.deleted = 1 OR note_files.rev = ?
     RETURNING seq`,
    [userId, path, rev, bytes.length, now, seq, baseRev],
  );
  if (done[0]) {
    if (row && Number(row.deleted) === 0 && row.rev !== rev) {
      await bestEffortDel(blobs, blobKey(userId, path, row.rev));
    }
    return { status: 200, seq: Number(done[0].seq) };
  }
  const current = await findRow(db, userId, path);
  // Same rev means the object is the winner's own (identical content): keep it.
  if (!current || current.rev !== rev) await bestEffortDel(blobs, newKey);
  return { status: 409, rev: current?.rev ?? "" };
}

/** Tombstones a file unless the server copy moved on from `baseRev`. Missing/deleted files are a no-op. */
export async function removeFile(
  db: Db,
  blobs: BlobStore,
  userId: string,
  path: string,
  baseRev: string,
  now: number,
): Promise<WriteResult> {
  const row = await findRow(db, userId, path);
  if (!row || Number(row.deleted) === 1) return { status: 200, seq: Number(row?.seq ?? 0) };
  if (row.rev !== baseRev) return { status: 409, rev: row.rev };
  const seq = await nextSeq(db, userId);
  const done = await db.all<{ seq: number }>(
    `UPDATE note_files SET deleted = 1, changed_at = ?, seq = ?
     WHERE user_id = ? AND path = ? AND deleted = 0 AND rev = ? RETURNING seq`,
    [now, seq, userId, path, baseRev],
  );
  if (!done[0]) return { status: 409, rev: (await findRow(db, userId, path))?.rev ?? "" };
  await bestEffortDel(blobs, blobKey(userId, path, row.rev));
  return { status: 200, seq: Number(done[0].seq) };
}
