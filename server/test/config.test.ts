import { describe, expect, it } from "vitest";
import { loadConfig } from "../src/config.js";

const d1 = { CLOUDFLARE_API_TOKEN: "t", CLOUDFLARE_ACCOUNT_ID: "a", D1_DATABASE_ID: "d" };
const mail = { RESEND_API_KEY: "k", MAIL_FROM: "Catenga <login@example.com>" };

describe("loadConfig", () => {
  it("defaults to local dev: SQLite, console mailer, localhost base URL", () => {
    const c = loadConfig({});
    expect(c).toMatchObject({ port: 8080, baseUrl: "http://localhost:8080", d1: null, resend: null });
  });

  it("uses PORT and BASE_URL (trailing slash removed)", () => {
    const c = loadConfig({ PORT: "3000", BASE_URL: "https://x.example/" });
    expect(c).toMatchObject({ port: 3000, baseUrl: "https://x.example" });
  });

  it("reads D1 and Resend settings when complete", () => {
    const c = loadConfig({ ...d1, ...mail });
    expect(c.d1).toEqual({ apiToken: "t", accountId: "a", databaseId: "d" });
    expect(c.resend).toEqual({ apiKey: "k", from: "Catenga <login@example.com>" });
  });

  it("refuses partial D1 config and Resend without MAIL_FROM", () => {
    expect(() => loadConfig({ CLOUDFLARE_ACCOUNT_ID: "a" })).toThrow(/D1/);
    expect(() => loadConfig({ RESEND_API_KEY: "k" })).toThrow(/MAIL_FROM/);
  });

  it("refuses an invalid PORT", () => {
    expect(() => loadConfig({ PORT: "abc" })).toThrow(/PORT/);
  });

  it("production requires D1, Resend and BASE_URL; the error never shows values", () => {
    const prod = { NODE_ENV: "production" };
    const notes = {
      R2_ACCESS_KEY_ID: "i",
      R2_SECRET_ACCESS_KEY: "s",
      R2_BUCKET: "b",
      CLOUDFLARE_ACCOUNT_ID: "a",
      NOTES_KEK: Buffer.alloc(32).toString("base64"),
    };
    expect(() => loadConfig(prod)).toThrow(/BASE_URL/);
    expect(() => loadConfig({ ...prod, BASE_URL: "https://x.example", ...mail, ...notes })).toThrow(/D1/);
    expect(() => loadConfig({ ...prod, BASE_URL: "https://x.example", ...d1, ...notes })).toThrow(/RESEND/);
    expect(loadConfig({ ...prod, BASE_URL: "https://x.example", ...d1, ...mail, ...notes }).d1).not.toBeNull();
    expect(() => loadConfig({ ...prod, ...d1, CLOUDFLARE_API_TOKEN: "SECRET" })).not.toThrow(/SECRET/);
  });
});

describe("loadConfig notes storage", () => {
  const r2 = { R2_ACCESS_KEY_ID: "id", R2_SECRET_ACCESS_KEY: "sk", R2_BUCKET: "b", CLOUDFLARE_ACCOUNT_ID: "a" };
  const kek = Buffer.alloc(32, 1).toString("base64");

  it("defaults to a local blob dir and the dev KEK", () => {
    const c = loadConfig({});
    expect(c.r2).toBeNull();
    expect(c.notesKek).toBeNull();
  });

  it("reads R2 and the KEK when complete", () => {
    const c = loadConfig({ ...d1, ...r2, NOTES_KEK: kek });
    expect(c.r2).toEqual({ accountId: "a", bucket: "b", accessKeyId: "id", secretAccessKey: "sk" });
    expect(c.notesKek).toEqual(Buffer.alloc(32, 1));
  });

  it("refuses partial R2 config, a bad KEK, and never prints values", () => {
    expect(() => loadConfig({ R2_BUCKET: "b" })).toThrow(/R2/);
    expect(() => loadConfig({ R2_ACCESS_KEY_ID: "id", R2_SECRET_ACCESS_KEY: "sk", R2_BUCKET: "b" })).toThrow(/CLOUDFLARE_ACCOUNT_ID/);
    expect(() => loadConfig({ NOTES_KEK: "c2hvcnQ=" })).toThrow(/NOTES_KEK/);
    expect(() => loadConfig({ NOTES_KEK: "SECRETVALUE" })).not.toThrow(/SECRETVALUE/);
    try {
      loadConfig({ NOTES_KEK: "SECRETVALUE" });
    } catch (e) {
      expect(String(e)).not.toContain("SECRETVALUE");
    }
  });

  it("production without R2 or NOTES_KEK still loads, with notes sync disabled", () => {
    const prod = { NODE_ENV: "production", BASE_URL: "https://x.example", ...d1, ...mail };
    const c = loadConfig(prod);
    expect(c.notesDisabled).toMatch(/R2_ACCESS_KEY_ID/);
    expect(c.notesDisabled).toMatch(/NOTES_KEK/);
    expect(loadConfig({ ...prod, ...r2 }).notesDisabled).toMatch(/^(?!.*R2_).*NOTES_KEK/);
    expect(loadConfig({ ...prod, NOTES_KEK: kek }).notesDisabled).toMatch(/R2_/);
    expect(loadConfig({ ...prod, ...r2, NOTES_KEK: kek }).notesDisabled).toBeNull();
    // partial R2 config is still a startup error
    expect(() => loadConfig({ ...prod, R2_BUCKET: "b" })).toThrow(/R2/);
  });

  it("outside production notes sync is always enabled (dev fallbacks)", () => {
    expect(loadConfig({}).notesDisabled).toBeNull();
  });
});
