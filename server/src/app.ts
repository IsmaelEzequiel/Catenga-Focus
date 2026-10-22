import express, { type Request, type RequestHandler, type Response } from "express";
import {
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
  SESSION_TTL,
} from "./auth.js";
import type { Db } from "./db.js";
import type { Mailer } from "./mail.js";
import {
  checkEmailPage,
  errorPage,
  expiredPage,
  homePage,
  loginPage,
  registerPage,
  verifyPage,
  type AppParams,
} from "./pages.js";
import { createRateLimiter } from "./rateLimit.js";

export type AppDeps = { db: Db; mailer: Mailer; baseUrl: string; now?: () => number };

const COOKIE = "catenga_session";
const HOUR = 3_600_000;
const CORS_ORIGINS = new Set([
  "tauri://localhost",
  "http://tauri.localhost",
  "https://tauri.localhost",
  "http://localhost:1420",
]);

/** A form/query value as a string ("" when absent or repeated). */
const str = (v: unknown): string => (typeof v === "string" ? v : "");

/** Reads the optional app handoff params; both or neither must be present and valid. */
function readAppParams(src: Record<string, unknown>): AppParams | "invalid" {
  const port = str(src.app_port);
  const challenge = str(src.code_challenge);
  if (!port && !challenge) return null;
  const appPort = parseAppPort(port);
  const codeChallenge = parseCodeChallenge(challenge);
  return appPort && codeChallenge ? { appPort, codeChallenge } : "invalid";
}

function cookieValue(req: Request, name: string): string | null {
  for (const part of (req.headers.cookie ?? "").split(";")) {
    const [k, ...rest] = part.trim().split("=");
    if (k === name) return rest.join("=");
  }
  return null;
}

const bearer = (req: Request): string | null => {
  const m = /^Bearer (.+)$/.exec(req.headers.authorization ?? "");
  return m?.[1] ?? null;
};

export function createApp({ db, mailer, baseUrl, now = Date.now }: AppDeps): express.Express {
  const app = express();
  app.set("trust proxy", 1);
  app.disable("x-powered-by");

  const origin = new URL(baseUrl).origin;
  const secure = baseUrl.startsWith("https://") ? "; Secure" : "";
  const emailLimit = createRateLimiter(5, HOUR, now);
  const ipEmailLimit = createRateLimiter(20, HOUR, now);
  const tokenLimit = createRateLimiter(30, HOUR, now);

  const html = (res: Response, status: number, body: string) => res.status(status).type("html").send(body);
  const badRequest = (res: Response) =>
    html(res, 400, errorPage("Bad request", "The app sign-in parameters are invalid. Start again from the app."));
  const setCookie = (res: Response, value: string, maxAge: number) =>
    res.append("Set-Cookie", `${COOKIE}=${value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=${maxAge}${secure}`);

  app.use((_req, res, next) => {
    res.set({
      "X-Content-Type-Options": "nosniff",
      // Keeps the verify URL (it holds a token) from leaking to other sites. Not "no-referrer":
      // that makes browsers send `Origin: null` on our own form POSTs, which the CSRF guard refuses.
      "Referrer-Policy": "same-origin",
      "Cache-Control": "no-store",
    });
    next();
  });

  // CORS for the desktop app (bearer auth, so no credentials mode).
  app.use("/api", (req, res, next) => {
    const o = req.headers.origin;
    if (o && CORS_ORIGINS.has(o)) {
      res.set({
        "Access-Control-Allow-Origin": o,
        "Access-Control-Allow-Methods": "GET, POST",
        "Access-Control-Allow-Headers": "Authorization, Content-Type",
        Vary: "Origin",
      });
    }
    if (req.method === "OPTIONS") {
      res.sendStatus(204);
      return;
    }
    next();
  });

  // Simple CSRF guard for the browser forms.
  const sameOrigin: RequestHandler = (req, res, next) => {
    const o = req.headers.origin;
    if (o && o !== origin) {
      html(res, 403, errorPage("Forbidden", "Cross-origin request refused."));
      return;
    }
    next();
  };
  const form = express.urlencoded({ extended: false, limit: "4kb" });
  const formRoutes = ["/register", "/login", "/auth/verify", "/logout"];
  app.post(formRoutes, sameOrigin, form, (req, _res, next) => {
    req.body ??= {}; // no body / wrong content type leaves it undefined in Express 5
    next();
  });

  app.get("/health", (_req, res) => {
    res.type("text").send("ok");
  });

  app.get("/", async (req, res) => {
    const session = cookieValue(req, COOKIE);
    html(res, 200, homePage(session ? await getSessionUser(db, session, now()) : null));
  });

  app.get("/register", (req, res) => {
    const appParams = readAppParams(req.query);
    if (appParams === "invalid") return badRequest(res);
    html(res, 200, registerPage(appParams));
  });

  app.get("/login", (req, res) => {
    const appParams = readAppParams(req.query);
    if (appParams === "invalid") return badRequest(res);
    html(res, 200, loginPage(appParams));
  });

  /** Counts the attempt; false means "over the limit, send nothing". */
  const allowLoginEmail = (req: Request, email: string): boolean => {
    const byEmail = emailLimit(email);
    const byIp = ipEmailLimit(req.ip ?? "unknown");
    return byEmail && byIp;
  };

  const sendLink = async (userId: string, name: string, email: string, appParams: AppParams) => {
    const token = await createLoginToken(db, {
      userId,
      appPort: appParams?.appPort,
      codeChallenge: appParams?.codeChallenge,
      now: now(),
    });
    try {
      await mailer.sendLoginLink(email, name, `${baseUrl}/auth/verify?token=${token}`);
    } catch (err) {
      // Same page either way, so a mail failure cannot be used to probe accounts.
      console.error("failed to send login link:", err instanceof Error ? err.message : err);
    }
  };

  app.post("/register", async (req, res) => {
    const appParams = readAppParams(req.body);
    if (appParams === "invalid") return badRequest(res);
    const rawName = str(req.body.name);
    const rawEmail = str(req.body.email);
    const name = parseName(rawName);
    const email = parseEmail(rawEmail);
    if (!name || !email) {
      const error = !name ? "Enter your name (1-100 characters)." : "Enter a valid email address.";
      return html(res, 400, registerPage(appParams, { name: rawName, email: rawEmail }, error));
    }
    if (allowLoginEmail(req, email)) {
      await createUser(db, name, email, now());
      const user = await findUserByEmail(db, email);
      if (user) await sendLink(user.id, user.name, email, appParams);
    }
    html(res, 200, checkEmailPage(email, false));
  });

  app.post("/login", async (req, res) => {
    const appParams = readAppParams(req.body);
    if (appParams === "invalid") return badRequest(res);
    const rawEmail = str(req.body.email);
    const email = parseEmail(rawEmail);
    if (!email) return html(res, 400, loginPage(appParams, rawEmail, "Enter a valid email address."));
    if (allowLoginEmail(req, email)) {
      const user = await findUserByEmail(db, email);
      if (user) await sendLink(user.id, user.name, email, appParams);
    }
    html(res, 200, checkEmailPage(email, true));
  });

  app.get("/auth/verify", (req, res) => {
    html(res, 200, verifyPage(str(req.query.token)));
  });

  app.post("/auth/verify", async (req, res) => {
    const grant = await consumeLoginToken(db, str(req.body.token), now());
    if (!grant) return html(res, 400, expiredPage());
    setCookie(res, await createSession(db, grant.userId, "web", now()), SESSION_TTL / 1000);
    if (grant.appPort !== null && grant.codeChallenge !== null) {
      const code = await createAppCode(db, grant.userId, grant.codeChallenge, now());
      return res.redirect(303, `http://127.0.0.1:${grant.appPort}/callback?code=${code}`);
    }
    res.redirect(303, "/");
  });

  app.post("/logout", async (req, res) => {
    const session = cookieValue(req, COOKIE);
    if (session) await deleteSession(db, session);
    setCookie(res, "", 0);
    res.redirect(303, "/");
  });

  // --- JSON API for the desktop app -----------------------------------------

  const json = express.json({ limit: "4kb" });

  app.post("/api/app/token", json, async (req, res) => {
    const fail = () => res.status(400).json({ error: "Invalid or expired sign-in code" });
    if (!tokenLimit(req.ip ?? "unknown")) return res.status(429).json({ error: "Too many attempts" });
    const body: unknown = req.body;
    const { code, code_verifier: verifier } = (body ?? {}) as Record<string, unknown>;
    if (typeof code !== "string" || typeof verifier !== "string") return fail();
    const userId = await consumeAppCode(db, code, verifier, now());
    if (!userId) return fail();
    const token = await createSession(db, userId, "app", now());
    const user = await getSessionUser(db, token, now());
    res.json({ token, user });
  });

  app.get("/api/me", async (req, res) => {
    const token = bearer(req);
    const user = token ? await getSessionUser(db, token, now()) : null;
    if (!user) return res.status(401).json({ error: "Not signed in" });
    res.json(user);
  });

  app.post("/api/logout", async (req, res) => {
    const token = bearer(req);
    if (token) await deleteSession(db, token);
    res.sendStatus(204);
  });

  app.use("/api", (err: unknown, _req: Request, res: Response, next: (e?: unknown) => void) => {
    if (res.headersSent) return next(err);
    const status = typeof err === "object" && err !== null && "status" in err && err.status === 413 ? 413 : 400;
    res.status(status).json({ error: "Invalid request" });
  });

  app.use((err: unknown, _req: Request, res: Response, next: (e?: unknown) => void) => {
    if (res.headersSent) return next(err);
    console.error(err);
    html(res, 500, errorPage("Something went wrong", "Please try again."));
  });

  return app;
}
