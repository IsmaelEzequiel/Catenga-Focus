export type Config = {
  port: number;
  baseUrl: string;
  d1: { apiToken: string; accountId: string; databaseId: string } | null;
  resend: { apiKey: string; from: string } | null;
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

  return { port, baseUrl, d1, resend };
}
