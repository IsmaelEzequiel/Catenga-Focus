import { fileURLToPath } from "node:url";
import Cloudflare from "cloudflare";
import { createApp } from "./app.js";
import { loadConfig } from "./config.js";
import { applySchema, d1Db, sqliteDb, type Db } from "./db.js";
import { consoleMailer, resendMailer } from "./mail.js";

let config;
try {
  config = loadConfig(process.env);
} catch (err) {
  console.error(`config error: ${err instanceof Error ? err.message : err}`);
  process.exit(1);
}

let db: Db;
if (config.d1) {
  db = d1Db(new Cloudflare({ apiToken: config.d1.apiToken }), config.d1.accountId, config.d1.databaseId);
} else {
  console.log("using local SQLite (dev)");
  db = sqliteDb(fileURLToPath(new URL("../dev.sqlite", import.meta.url)));
}
const mailer = config.resend ? resendMailer(config.resend.apiKey, config.resend.from) : consoleMailer();

await applySchema(db);
createApp({ db, mailer, baseUrl: config.baseUrl }).listen(config.port, () => {
  console.log(`catenga server listening on :${config.port}`);
});
