import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createApp } from "../src/app.js";
import type { BlobStore } from "../src/blobs.js";
import { codeChallengeFor } from "../src/auth.js";
import { applySchema, sqliteDb, type Db } from "../src/db.js";
import type { Mailer } from "../src/mail.js";

type Sent = { to: string; name: string; link: string };

const nullBlobs: BlobStore = { put: async () => {}, get: async () => null, del: async () => {} };
let db: Db;
let sent: Sent[];
let server: Server;
let base: string;
let clock: number;

async function freePort(): Promise<number> {
  const s = createServer();
  await new Promise<void>((r) => s.listen(0, "127.0.0.1", r));
  const { port } = s.address() as AddressInfo;
  await new Promise((r) => s.close(r));
  return port;
}

beforeEach(async () => {
  db = sqliteDb(":memory:");
  await applySchema(db);
  sent = [];
  clock = 1_000_000;
  const mailer: Mailer = {
    sendLoginLink: async (to, name, link) => {
      sent.push({ to, name, link });
    },
  };
  const port = await freePort();
  base = `http://127.0.0.1:${port}`;
  const app = createApp({ db, mailer, baseUrl: base, blobs: nullBlobs, notesKek: Buffer.alloc(32), now: () => clock });
  server = await new Promise<Server>((resolve) => {
    const s = app.listen(port, "127.0.0.1", () => resolve(s));
  });
});

afterEach(async () => {
  await new Promise((r) => server.close(r));
});

const form = (fields: Record<string, string>) => ({
  method: "POST",
  redirect: "manual" as const,
  headers: { "content-type": "application/x-www-form-urlencoded" },
  body: new URLSearchParams(fields).toString(),
});

const tokenOf = (link: string) => new URL(link).searchParams.get("token") ?? "";
const cookieOf = (res: Response) => (res.headers.get("set-cookie") ?? "").split(";")[0] ?? "";

async function register(name: string, email: string, extra: Record<string, string> = {}) {
  return fetch(`${base}/register`, form({ name, email, ...extra }));
}

async function signInWeb(name = "Ana", email = "ana@example.com") {
  await register(name, email);
  const res = await fetch(`${base}/auth/verify`, form({ token: tokenOf(sent.at(-1)!.link) }));
  return cookieOf(res);
}

describe("basics", () => {
  it("answers /health and sets security headers", async () => {
    const res = await fetch(`${base}/health`);
    expect(await res.text()).toBe("ok");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    // Not "no-referrer": with it browsers send `Origin: null` on our own form POSTs and the CSRF guard refuses them.
    expect(res.headers.get("referrer-policy")).toBe("same-origin");
    expect(res.headers.get("cache-control")).toBe("no-store");
  });

  it("shows the logo and uses it as the tab icon", async () => {
    const page = await (await fetch(`${base}/login`)).text();
    expect(page).toMatch(/<p class="brand"><svg [^>]*viewBox="0 0 120 120"/);
    expect(page).toContain('<link rel="icon" href="data:image/svg+xml,');
  });

  it("serves the forms and a signed-out home", async () => {
    expect(await (await fetch(`${base}/`)).text()).toContain('href="/login"');
    expect(await (await fetch(`${base}/register`)).text()).toContain('name="email"');
    expect(await (await fetch(`${base}/login`)).text()).toContain('name="email"');
  });
});

describe("register and login (web)", () => {
  it("register new: creates the user and mails one working link", async () => {
    const res = await register("Ana", "Ana@Example.com");
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("Check your email");
    const users = await db.all<{ name: string; email: string }>("SELECT name, email FROM users", []);
    expect(users).toEqual([{ name: "Ana", email: "ana@example.com" }]);
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ to: "ana@example.com", name: "Ana" });
    expect(sent[0]!.link.startsWith(`${base}/auth/verify?token=`)).toBe(true);
  });

  it("register existing: no new user, name unchanged, mail sent, same page", async () => {
    const first = await (await register("Ana", "ana@example.com")).text();
    const second = await register("Someone Else", "ana@example.com");
    expect(await second.text()).toBe(first);
    const users = await db.all<{ name: string }>("SELECT name FROM users", []);
    expect(users).toEqual([{ name: "Ana" }]);
    expect(sent).toHaveLength(2);
    expect(sent[1]!.name).toBe("Ana");
  });

  it("register with invalid input shows the form with the error and kept values", async () => {
    const res = await register("Ana", "not-an-email");
    expect(res.status).toBe(400);
    const html = await res.text();
    expect(html).toContain('value="Ana"');
    expect(html).toContain('value="not-an-email"');
    expect(html).toContain("valid email");
    expect(sent).toHaveLength(0);
  });

  it("login unknown email: same page, no mail", async () => {
    await register("Ana", "ana@example.com");
    const known = await (await fetch(`${base}/login`, form({ email: "ana@example.com" }))).text();
    sent.length = 0;
    const unknown = await fetch(`${base}/login`, form({ email: "ghost@example.com" }));
    expect(unknown.status).toBe(200);
    const unknownHtml = await unknown.text();
    expect(unknownHtml).toContain("If an account exists");
    expect(known).toContain("If an account exists");
    expect(sent).toHaveLength(0);
  });

  it("GET verify does not consume the token", async () => {
    await register("Ana", "ana@example.com");
    const token = tokenOf(sent[0]!.link);
    const page = await fetch(`${base}/auth/verify?token=${token}`);
    expect(page.status).toBe(200);
    expect(await page.text()).toContain('<button');
    const res = await fetch(`${base}/auth/verify`, form({ token }));
    expect(res.status).toBe(303);
  });

  it("POST verify sets the cookie, home shows the name, second POST is expired", async () => {
    await register("Ana", "ana@example.com");
    const token = tokenOf(sent[0]!.link);
    const res = await fetch(`${base}/auth/verify`, form({ token }));
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/");
    const setCookie = res.headers.get("set-cookie") ?? "";
    expect(setCookie).toContain("catenga_session=");
    expect(setCookie).toContain("HttpOnly");
    expect(setCookie).toContain("SameSite=Lax");
    expect(setCookie).toContain("Max-Age=31536000");
    expect(setCookie).not.toContain("Secure"); // http base URL

    const home = await fetch(`${base}/`, { headers: { cookie: cookieOf(res) } });
    expect(await home.text()).toContain("ana@example.com");

    const again = await fetch(`${base}/auth/verify`, form({ token }));
    expect(again.status).toBe(400);
    expect(await again.text()).toContain("expired or was already used");
  });

  it("an expired token is refused", async () => {
    await register("Ana", "ana@example.com");
    clock += 16 * 60_000;
    const res = await fetch(`${base}/auth/verify`, form({ token: tokenOf(sent[0]!.link) }));
    expect(res.status).toBe(400);
  });

  it("logout clears the session", async () => {
    const cookie = await signInWeb();
    const out = await fetch(`${base}/logout`, { ...form({}), headers: { ...form({}).headers, cookie } });
    expect(out.status).toBe(303);
    expect(out.headers.get("set-cookie")).toContain("Max-Age=0");
    const home = await fetch(`${base}/`, { headers: { cookie } });
    expect(await home.text()).toContain('href="/login"');
  });

  it("escapes HTML in the name", async () => {
    const cookie = await signInWeb("<script>alert(1)</script>");
    const html = await (await fetch(`${base}/`, { headers: { cookie } })).text();
    expect(html).not.toContain("<script>alert(1)");
    expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
  });
});

describe("app flow (loopback + PKCE)", () => {
  const verifier = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
  const challenge = codeChallengeFor(verifier);
  const appParams = { app_port: "45678", code_challenge: challenge };

  async function loginForApp() {
    await register("Ana", "ana@example.com");
    sent.length = 0;
    await fetch(`${base}/login`, form({ email: "ana@example.com", ...appParams }));
    const res = await fetch(`${base}/auth/verify`, form({ token: tokenOf(sent[0]!.link) }));
    const code = new URL(res.headers.get("location")!).searchParams.get("code")!;
    return { res, code };
  }

  const exchange = (code: string, code_verifier: string) =>
    fetch(`${base}/api/app/token`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code, code_verifier }),
    });

  it("works end to end", async () => {
    const { res, code } = await loginForApp();
    expect(res.status).toBe(303);
    expect(res.headers.get("location")!.startsWith("http://127.0.0.1:45678/callback?code=")).toBe(true);
    expect(res.headers.get("set-cookie")).toContain("catenga_session=");

    const tokenRes = await exchange(code, verifier);
    expect(tokenRes.status).toBe(200);
    const body = (await tokenRes.json()) as { token: string; user: { name: string; email: string } };
    expect(body.user).toEqual({ name: "Ana", email: "ana@example.com" });

    const auth = { authorization: `Bearer ${body.token}` };
    const me = await fetch(`${base}/api/me`, { headers: auth });
    expect(me.status).toBe(200);
    expect(await me.json()).toEqual({ name: "Ana", email: "ana@example.com" });

    expect((await fetch(`${base}/api/logout`, { method: "POST", headers: auth })).status).toBe(204);
    expect((await fetch(`${base}/api/me`, { headers: auth })).status).toBe(401);
    expect((await fetch(`${base}/api/logout`, { method: "POST", headers: auth })).status).toBe(204);
  });

  it("a code cannot be used twice", async () => {
    const { code } = await loginForApp();
    expect((await exchange(code, verifier)).status).toBe(200);
    expect((await exchange(code, verifier)).status).toBe(400);
  });

  it("wrong verifier gives 400 and burns the code", async () => {
    const { code } = await loginForApp();
    const bad = await exchange(code, "w".repeat(43));
    expect(bad.status).toBe(400);
    const badBody = (await bad.json()) as { error: string };
    expect(badBody.error).toBeTruthy();
    expect((await exchange(code, verifier)).status).toBe(400);
    expect(((await (await exchange("nope", verifier)).json()) as { error: string }).error).toBe(badBody.error);
  });

  it("/api/me without a valid bearer is 401", async () => {
    expect((await fetch(`${base}/api/me`)).status).toBe(401);
    expect((await fetch(`${base}/api/me`, { headers: { authorization: "Bearer nope" } })).status).toBe(401);
  });

  it("malformed JSON on /api/app/token is a 400 json error", async () => {
    const res = await fetch(`${base}/api/app/token`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{oops",
    });
    expect(res.status).toBe(400);
    expect(await res.json()).toHaveProperty("error");
  });

  it("carries the app params through the register and login forms", async () => {
    const q = `app_port=45678&code_challenge=${challenge}`;
    for (const path of ["login", "register"]) {
      const html = await (await fetch(`${base}/${path}?${q}`)).text();
      expect(html).toContain('name="app_port" value="45678"');
      expect(html).toContain(`name="code_challenge" value="${challenge}"`);
    }
  });

  it("rejects bad app params with a 400 page", async () => {
    const good = { email: "ana@example.com" };
    const bads: Record<string, string>[] = [
      { app_port: "80", code_challenge: challenge },
      { app_port: "45678", code_challenge: "short" },
      { app_port: "45678" },
      { code_challenge: challenge },
    ];
    for (const bad of bads) {
      const post = await fetch(`${base}/login`, form({ ...good, ...bad }));
      expect(post.status).toBe(400);
      const qs = new URLSearchParams(bad).toString();
      expect((await fetch(`${base}/login?${qs}`)).status).toBe(400);
      expect((await fetch(`${base}/register?${qs}`)).status).toBe(400);
    }
    expect(sent).toHaveLength(0);
  });
});

describe("limits and guards", () => {
  it("per-email rate limit: the 6th link is not sent but the page is the same", async () => {
    await register("Ana", "ana@example.com"); // 1
    const pages: string[] = [];
    for (let i = 0; i < 5; i++) {
      pages.push(await (await fetch(`${base}/login`, form({ email: "ana@example.com" }))).text());
    }
    expect(sent).toHaveLength(5);
    expect(new Set(pages).size).toBe(1);
    clock += 3_600_001;
    await fetch(`${base}/login`, form({ email: "ana@example.com" }));
    expect(sent).toHaveLength(6);
  });

  it("per-IP rate limit: at most 20 emails per hour", async () => {
    for (let i = 0; i < 22; i++) await register("U", `u${i}@example.com`);
    expect(sent).toHaveLength(20);
  });

  it("CORS preflight is allowed for the desktop origins only", async () => {
    const ok = await fetch(`${base}/api/me`, {
      method: "OPTIONS",
      headers: { origin: "tauri://localhost", "access-control-request-method": "GET" },
    });
    expect(ok.status).toBe(204);
    expect(ok.headers.get("access-control-allow-origin")).toBe("tauri://localhost");
    expect(ok.headers.get("access-control-allow-headers")).toContain("Authorization");

    const bad = await fetch(`${base}/api/me`, {
      method: "OPTIONS",
      headers: { origin: "https://evil.example", "access-control-request-method": "GET" },
    });
    expect(bad.headers.get("access-control-allow-origin")).toBeNull();

    const real = await fetch(`${base}/api/me`, { headers: { origin: "http://localhost:1420" } });
    expect(real.headers.get("access-control-allow-origin")).toBe("http://localhost:1420");
  });

  it("rejects browser POSTs with a foreign Origin", async () => {
    const withOrigin = (origin: string) => {
      const f = form({ email: "ana@example.com" });
      return { ...f, headers: { ...f.headers, origin } };
    };
    expect((await fetch(`${base}/login`, withOrigin("https://evil.example"))).status).toBe(403);
    expect((await fetch(`${base}/login`, withOrigin(base))).status).toBe(200);
    expect(sent).toHaveLength(0);
  });
});

describe("robustness", () => {
  it("a form POST without a body is a 400 form, not a crash", async () => {
    const res = await fetch(`${base}/login`, { method: "POST", redirect: "manual" });
    expect(res.status).toBe(400);
  });
});
