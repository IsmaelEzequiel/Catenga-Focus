export type Config = {
  port: number;
  baseUrl: string;
  d1: { apiToken: string; accountId: string; databaseId: string } | null;
  resend: { apiKey: string; from: string } | null;
  /** R2 object storage for notes; null means the local blob directory (dev). */
  r2: { accountId: string; bucket: string; accessKeyId: string; secretAccessKey: string } | null;
  /** Master key that wraps each user's notes key; null means the fixed dev key (dev only). */
  notesKek: Buffer | null;
  /** Why notes sync is off (names variables, never values); null when it is available. Production only. */
  notesDisabled: string | null;
};

type Env = Record<string, string | undefined>;

/** Reads settings from the environment. Errors name variables, never values. */
export function loadConfig(env: Env): Config {
  const production = env.NODE_ENV === "production";
  const port = env.PORT === undefined ? 8080 : Number(env.PORT);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("PORT must be a valid port number");

  const baseUrl = (env.BASE_URL ?? `http://localhost:${port}`).replace(/\/+$/, "");
  if (production && !env.BASE_URL) throw new Error("BASE_URL is required in production");

  const d1Vars = [env.CLOUDFLARE_API_TOKEN, env.CLOUDFLARE_ACCOUNT_ID, env.D1_DATABASE_ID];
  const d1Count = d1Vars.filter(Boolean).length;
  if (d1Count !== 0 && d1Count !== 3) {
    throw new Error("D1 config is partial: set all of CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, D1_DATABASE_ID or none");
  }
  if (production && d1Count === 0) throw new Error("D1 config is required in production");
  const d1 =
    d1Count === 3
      ? { apiToken: d1Vars[0]!, accountId: d1Vars[1]!, databaseId: d1Vars[2]! }
      : null;

  if (env.RESEND_API_KEY && !env.MAIL_FROM) throw new Error("MAIL_FROM is required when RESEND_API_KEY is set");
  if (production && !env.RESEND_API_KEY) throw new Error("RESEND_API_KEY is required in production");
  const resend = env.RESEND_API_KEY ? { apiKey: env.RESEND_API_KEY, from: env.MAIL_FROM! } : null;

  const r2Vars = [env.R2_ACCESS_KEY_ID, env.R2_SECRET_ACCESS_KEY, env.R2_BUCKET];
  const r2Count = r2Vars.filter(Boolean).length;
  if (r2Count !== 0 && r2Count !== 3) {
    throw new Error("R2 config is partial: set all of R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_BUCKET or none");
  }
  if (r2Count === 3 && !env.CLOUDFLARE_ACCOUNT_ID) throw new Error("CLOUDFLARE_ACCOUNT_ID is required for R2");
  const r2 =
    r2Count === 3
      ? { accountId: env.CLOUDFLARE_ACCOUNT_ID!, bucket: r2Vars[2]!, accessKeyId: r2Vars[0]!, secretAccessKey: r2Vars[1]! }
      : null;

  let notesKek: Buffer | null = null;
  if (env.NOTES_KEK) {
    notesKek = Buffer.from(env.NOTES_KEK, "base64");
    if (notesKek.length !== 32) throw new Error("NOTES_KEK must be base64 of exactly 32 bytes");
  }

  // A deployed server must keep serving login and /api/sync before the owner creates the R2 secrets.
  const missing = [
    ...(r2 ? [] : ["R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY", "R2_BUCKET"]),
    ...(notesKek ? [] : ["NOTES_KEK"]),
  ];
  const notesDisabled = production && missing.length ? `missing ${missing.join(", ")}` : null;

  return { port, baseUrl, d1, resend, r2, notesKek, notesDisabled };
}
