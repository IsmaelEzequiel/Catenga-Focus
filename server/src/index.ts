import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import Cloudflare from "cloudflare";
import { createApp } from "./app.js";
import { localBlobStore, r2BlobStore, type BlobStore } from "./blobs.js";
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

let notes: { blobs: BlobStore; notesKek: Buffer } | null = null;
if (config.notesDisabled) {
  console.log(`notes sync disabled: ${config.notesDisabled}`);
} else {
  let blobs: BlobStore;
  if (config.r2) {
    blobs = r2BlobStore(config.r2);
  } else {
    console.log("using local blob directory server/dev-blobs (dev)");
    blobs = localBlobStore(fileURLToPath(new URL("../dev-blobs", import.meta.url)));
  }
  let notesKek = config.notesKek;
  if (!notesKek) {
    console.log("NOTES_KEK not set: using the fixed dev key (dev only)");
    notesKek = createHash("sha256").update("catenga-dev-notes-kek").digest();
  }
  notes = { blobs, notesKek };
}

await applySchema(db);
createApp({ db, mailer, baseUrl: config.baseUrl, ...notes }).listen(config.port, () => {
  console.log(`catenga server listening on :${config.port}`);
});
