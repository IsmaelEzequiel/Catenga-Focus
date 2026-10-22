import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { AwsClient } from "aws4fetch";

/** Where the (already encrypted) note blobs live. Keys look like `u/<userId>/<sha256hex>`. */
export type BlobStore = {
  put(key: string, bytes: Uint8Array): Promise<void>;
  get(key: string): Promise<Uint8Array | null>;
  del(key: string): Promise<void>;
};

const SAFE_KEY = /^[A-Za-z0-9_-]+(\/[A-Za-z0-9_-]+)*$/;

function checkKey(key: string): void {
  if (!SAFE_KEY.test(key)) throw new Error("invalid blob key");
}

/** Dev/test store: one file per key under `dir`, written atomically. */
export function localBlobStore(dir: string): BlobStore {
  const file = (key: string) => {
    checkKey(key);
    return join(dir, key);
  };
  return {
    put: async (key, bytes) => {
      const target = file(key);
      await mkdir(dirname(target), { recursive: true });
      const tmp = `${target}.${process.pid}.${Date.now()}.tmp`;
      await writeFile(tmp, bytes);
      await rename(tmp, target);
    },
    get: async (key) => {
      try {
        return new Uint8Array(await readFile(file(key)));
      } catch (err) {
        if ((err as NodeJS.ErrnoException).code === "ENOENT") return null;
        throw err;
      }
    },
    del: async (key) => {
      await rm(file(key), { force: true });
    },
  };
}

export type R2Options = { accountId: string; bucket: string; accessKeyId: string; secretAccessKey: string };

/**
 * Cloudflare R2 through its S3 API, signed with SigV4 (aws4fetch). The official `cloudflare` SDK can
 * move object bytes too, but only with an API token; R2's S3 credentials are a separate key pair.
 * Errors name the operation and status, never credentials.
 */
export function r2BlobStore(opts: R2Options, send: (req: Request) => Promise<Response> = fetch): BlobStore {
  const aws = new AwsClient({
    accessKeyId: opts.accessKeyId,
    secretAccessKey: opts.secretAccessKey,
    service: "s3",
    region: "auto",
  });
  const url = (key: string) => {
    checkKey(key);
    return `https://${opts.accountId}.r2.cloudflarestorage.com/${opts.bucket}/${key}`;
  };
  const call = async (method: string, key: string, body?: Uint8Array) =>
    send(await aws.sign(url(key), { method, body: body as BodyInit | undefined }));
  const fail = (op: string, res: Response) => new Error(`R2 ${op} failed with status ${res.status}`);
  return {
    put: async (key, bytes) => {
      const res = await call("PUT", key, bytes);
      if (!res.ok) throw fail("put", res);
    },
    get: async (key) => {
      const res = await call("GET", key);
      if (res.status === 404) return null;
      if (!res.ok) throw fail("get", res);
      return new Uint8Array(await res.arrayBuffer());
    },
    del: async (key) => {
      const res = await call("DELETE", key);
      if (!res.ok && res.status !== 404) throw fail("delete", res);
    },
  };
}
