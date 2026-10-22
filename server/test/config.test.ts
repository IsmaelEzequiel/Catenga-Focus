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
    expect(() => loadConfig(prod)).toThrow(/BASE_URL/);
    expect(() => loadConfig({ ...prod, BASE_URL: "https://x.example", ...mail })).toThrow(/D1/);
    expect(() => loadConfig({ ...prod, BASE_URL: "https://x.example", ...d1 })).toThrow(/RESEND/);
    expect(loadConfig({ ...prod, BASE_URL: "https://x.example", ...d1, ...mail }).d1).not.toBeNull();
    expect(() => loadConfig({ ...prod, ...d1, CLOUDFLARE_API_TOKEN: "SECRET" })).not.toThrow(/SECRET/);
  });
});
