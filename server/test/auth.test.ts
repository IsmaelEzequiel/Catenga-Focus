import { beforeEach, describe, expect, it } from "vitest";
import {
  codeChallengeFor,
  consumeAppCode,
  consumeLoginToken,
  createAppCode,
  createLoginToken,
  createSession,
  createUser,
  deleteSession,
  findUserByEmail,
  getSessionUser,
  parseAppPort,
  parseCodeChallenge,
  parseEmail,
  parseName,
  verifyPkce,
} from "../src/auth.js";
import { applySchema, sqliteDb, type Db } from "../src/db.js";

const MIN = 60_000;
const challenge = codeChallengeFor("v".repeat(43));

describe("validation", () => {
  it.each([
    ["ana@example.com", "ana@example.com"],
    ["  Ana@Example.COM ", "ana@example.com"],
    ["a.b+c@sub.example.co", "a.b+c@sub.example.co"],
  ])("accepts email %j", (raw, expected) => {
    expect(parseEmail(raw)).toBe(expected);
  });

  it.each([
    "",
    "ana",
    "ana@",
    "@example.com",
    "ana@example",
    "a@b@example.com",
    "ana @example.com",
    `${"a".repeat(250)}@example.com`,
  ])("rejects email %j", (raw) => {
    expect(parseEmail(raw)).toBeNull();
  });

  it("validates names (trimmed, 1-100 chars)", () => {
    expect(parseName("  Ana  ")).toBe("Ana");
    expect(parseName("")).toBeNull();
    expect(parseName("   ")).toBeNull();
    expect(parseName("x".repeat(100))).toBe("x".repeat(100));
    expect(parseName("x".repeat(101))).toBeNull();
  });

  it("validates app ports (integer 1024-65535)", () => {
    expect(parseAppPort("1024")).toBe(1024);
    expect(parseAppPort("65535")).toBe(65535);
    for (const bad of ["1023", "65536", "abc", "80.5", "", "1e4", "-5", " 4000"]) {
      expect(parseAppPort(bad)).toBeNull();
    }
  });

  it("validates code challenges (43 base64url chars)", () => {
    expect(parseCodeChallenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")).not.toBeNull();
    expect(parseCodeChallenge("short")).toBeNull();
    expect(parseCodeChallenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM=")).toBeNull();
    expect(parseCodeChallenge("E9Melhoa2OwvFrEMTJguCHoeRZt8hn+a8/6sCjgJELE")).toBeNull();
  });
});

describe("PKCE", () => {
  it("matches the RFC 7636 appendix B vector", () => {
    const verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    expect(codeChallengeFor(verifier)).toBe(expected);
    expect(verifyPkce(verifier, expected)).toBe(true);
    expect(verifyPkce("wrong", expected)).toBe(false);
  });
});

describe("tokens and sessions", () => {
  let db: Db;
  let userId: string;
  beforeEach(async () => {
    db = sqliteDb(":memory:");
    await applySchema(db);
    userId = await createUser(db, "Ana", "ana@example.com", 0);
  });

  it("finds users by email", async () => {
    expect(await findUserByEmail(db, "ana@example.com")).toMatchObject({ id: userId, name: "Ana" });
    expect(await findUserByEmail(db, "nobody@example.com")).toBeNull();
  });

  it("consumes a login token once", async () => {
    const token = await createLoginToken(db, { userId, now: 0 });
    expect(await consumeLoginToken(db, token, 1000)).toEqual({
      userId,
      appPort: null,
      codeChallenge: null,
    });
    expect(await consumeLoginToken(db, token, 2000)).toBeNull();
  });

  it("rejects an expired login token (15 min) and an unknown one", async () => {
    const token = await createLoginToken(db, { userId, now: 0 });
    expect(await consumeLoginToken(db, token, 15 * MIN)).toBeNull();
    expect(await consumeLoginToken(db, "nope", 0)).toBeNull();
  });

  it("keeps app params on the login token and stores only hashes", async () => {
    const token = await createLoginToken(db, { userId, appPort: 4321, codeChallenge: challenge, now: 0 });
    const stored = await db.all<{ token_hash: string }>("SELECT token_hash FROM login_tokens", []);
    expect(stored[0]?.token_hash).not.toBe(token);
    expect(await consumeLoginToken(db, token, 1)).toEqual({
      userId,
      appPort: 4321,
      codeChallenge: challenge,
    });
  });

  it("deletes expired login tokens when a new one is created", async () => {
    await createLoginToken(db, { userId, now: 0 });
    await createLoginToken(db, { userId, now: 20 * MIN });
    const rows = await db.all("SELECT 1 FROM login_tokens", []);
    expect(rows).toHaveLength(1);
  });

  it("binds an app code to its challenge and burns it on a wrong verifier", async () => {
    const verifier = "v".repeat(43);
    const code = await createAppCode(db, userId, challenge, 0);
    expect(await consumeAppCode(db, code, "w".repeat(43), 1)).toBeNull();
    expect(await consumeAppCode(db, code, verifier, 2)).toBeNull(); // burned
    const code2 = await createAppCode(db, userId, challenge, 0);
    expect(await consumeAppCode(db, code2, verifier, 1)).toBe(userId);
    expect(await consumeAppCode(db, code2, verifier, 2)).toBeNull(); // used
  });

  it("expires app codes after 5 minutes", async () => {
    const code = await createAppCode(db, userId, challenge, 0);
    expect(await consumeAppCode(db, code, "v".repeat(43), 5 * MIN)).toBeNull();
  });

  it("creates, reads, expires and deletes sessions", async () => {
    const token = await createSession(db, userId, "web", 0);
    expect(await getSessionUser(db, token, 1000)).toEqual({ name: "Ana", email: "ana@example.com" });
    expect(await getSessionUser(db, "bogus", 1000)).toBeNull();
    expect(await getSessionUser(db, token, 366 * 24 * 60 * MIN)).toBeNull();
    await deleteSession(db, token);
    expect(await getSessionUser(db, token, 1000)).toBeNull();
    await deleteSession(db, token); // already gone: fine
  });
});
