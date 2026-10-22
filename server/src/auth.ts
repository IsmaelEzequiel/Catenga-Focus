import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import type { Db } from "./db.js";

const MIN = 60_000;
export const LOGIN_TOKEN_TTL = 15 * MIN;
export const APP_CODE_TTL = 5 * MIN;
export const SESSION_TTL = 365 * 24 * 60 * MIN;

// --- secrets -------------------------------------------------------------

export const newSecret = (): string => randomBytes(32).toString("base64url");
export const sha256Hex = (s: string): string => createHash("sha256").update(s).digest("hex");

export const codeChallengeFor = (verifier: string): string =>
  createHash("sha256").update(verifier).digest("base64url");

export function verifyPkce(verifier: string, challenge: string): boolean {
  const a = Buffer.from(codeChallengeFor(verifier));
  const b = Buffer.from(challenge);
  return a.length === b.length && timingSafeEqual(a, b);
}

// --- validation (each returns the cleaned value, or null when invalid) ----

export function parseEmail(raw: string): string | null {
  const email = raw.trim().toLowerCase();
  if (email.length > 254 || /\s/.test(email)) return null;
  const parts = email.split("@");
  if (parts.length !== 2) return null;
  const [local, domain] = parts as [string, string];
  if (!local || !domain.includes(".") || domain.startsWith(".") || domain.endsWith(".")) return null;
  return email;
}

export function parseName(raw: string): string | null {
  const name = raw.trim();
  return name.length >= 1 && name.length <= 100 ? name : null;
}

export function parseAppPort(raw: string): number | null {
  if (!/^\d{4,5}$/.test(raw)) return null;
  const port = Number(raw);
  return port >= 1024 && port <= 65535 ? port : null;
}

export function parseCodeChallenge(raw: string): string | null {
  return /^[A-Za-z0-9_-]{43}$/.test(raw) ? raw : null;
}

// --- users ----------------------------------------------------------------

export type User = { id: string; name: string; email: string };

export async function findUserByEmail(db: Db, email: string): Promise<User | null> {
  const rows = await db.all<User>("SELECT id, name, email FROM users WHERE email = ?", [email]);
  return rows[0] ?? null;
}

/** Inserts the user unless the email already exists; returns the new id. */
export async function createUser(db: Db, name: string, email: string, now: number): Promise<string> {
  const id = randomBytes(16).toString("hex");
  await db.run(
    "INSERT INTO users (id, name, email, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(email) DO NOTHING",
    [id, name, email, now],
  );
  return id;
}

// --- login tokens (emailed) -------------------------------------------------

export type LoginGrant = { userId: string; appPort: number | null; codeChallenge: string | null };

export async function createLoginToken(
  db: Db,
  opts: { userId: string; appPort?: number; codeChallenge?: string; now: number },
): Promise<string> {
  await db.run("DELETE FROM login_tokens WHERE expires_at < ? OR used_at IS NOT NULL", [opts.now]);
  await db.run("DELETE FROM app_codes WHERE expires_at < ? OR used_at IS NOT NULL", [opts.now]);
  await db.run("DELETE FROM sessions WHERE expires_at < ?", [opts.now]);
  const token = newSecret();
  // '' stands for NULL because D1 params are strings only.
  await db.run(
    `INSERT INTO login_tokens (token_hash, user_id, app_port, code_challenge, expires_at)
     VALUES (?, ?, NULLIF(?, ''), NULLIF(?, ''), ?)`,
    [
      sha256Hex(token),
      opts.userId,
      opts.appPort ?? "",
      opts.codeChallenge ?? "",
      opts.now + LOGIN_TOKEN_TTL,
    ],
  );
  return token;
}

export async function consumeLoginToken(db: Db, token: string, now: number): Promise<LoginGrant | null> {
  const rows = await db.all<{ user_id: string; app_port: number | null; code_challenge: string | null }>(
    `UPDATE login_tokens SET used_at = ?
     WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?
     RETURNING user_id, app_port, code_challenge`,
    [now, sha256Hex(token), now],
  );
  const row = rows[0];
  if (!row) return null;
  return {
    userId: row.user_id,
    appPort: row.app_port === null ? null : Number(row.app_port),
    codeChallenge: row.code_challenge,
  };
}

// --- app codes (loopback handoff) ------------------------------------------

export async function createAppCode(db: Db, userId: string, codeChallenge: string, now: number): Promise<string> {
  const code = newSecret();
  await db.run(
    "INSERT INTO app_codes (code_hash, user_id, code_challenge, expires_at) VALUES (?, ?, ?, ?)",
    [sha256Hex(code), userId, codeChallenge, now + APP_CODE_TTL],
  );
  return code;
}

/** Burns the code first, then checks the verifier: a wrong guess kills the code. */
export async function consumeAppCode(db: Db, code: string, verifier: string, now: number): Promise<string | null> {
  const rows = await db.all<{ user_id: string; code_challenge: string }>(
    `UPDATE app_codes SET used_at = ?
     WHERE code_hash = ? AND used_at IS NULL AND expires_at > ?
     RETURNING user_id, code_challenge`,
    [now, sha256Hex(code), now],
  );
  const row = rows[0];
  return row && verifyPkce(verifier, row.code_challenge) ? row.user_id : null;
}

// --- sessions ---------------------------------------------------------------

export async function createSession(db: Db, userId: string, kind: "web" | "app", now: number): Promise<string> {
  const token = newSecret();
  await db.run(
    "INSERT INTO sessions (token_hash, user_id, kind, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
    [sha256Hex(token), userId, kind, now, now + SESSION_TTL],
  );
  return token;
}

export async function getSessionUser(
  db: Db,
  token: string,
  now: number,
): Promise<{ name: string; email: string } | null> {
  const rows = await db.all<{ name: string; email: string }>(
    `SELECT u.name, u.email FROM sessions s JOIN users u ON u.id = s.user_id
     WHERE s.token_hash = ? AND s.expires_at > ?`,
    [sha256Hex(token), now],
  );
  return rows[0] ?? null;
}

export async function deleteSession(db: Db, token: string): Promise<void> {
  await db.run("DELETE FROM sessions WHERE token_hash = ?", [sha256Hex(token)]);
}
