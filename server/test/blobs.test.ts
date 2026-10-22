import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { localBlobStore, r2BlobStore } from "../src/blobs.js";

let dir: string;
beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "catenga-blobs-"));
});
afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("localBlobStore", () => {
  it("puts, gets, overwrites and deletes", async () => {
    const s = localBlobStore(dir);
    expect(await s.get("u/a/b")).toBeNull();
    await s.put("u/a/b", new Uint8Array([1, 2, 3]));
    expect([...(await s.get("u/a/b"))!]).toEqual([1, 2, 3]);
    await s.put("u/a/b", new Uint8Array([9]));
    expect([...(await s.get("u/a/b"))!]).toEqual([9]);
    await s.del("u/a/b");
    expect(await s.get("u/a/b")).toBeNull();
    await s.del("u/a/b"); // idempotent
  });

  it("refuses keys that could escape the directory", async () => {
    const s = localBlobStore(dir);
    await expect(s.put("../x", new Uint8Array([1]))).rejects.toThrow(/key/);
    await expect(s.get("/etc/passwd")).rejects.toThrow(/key/);
    await expect(s.del("u/../../x")).rejects.toThrow(/key/);
  });
});

describe("r2BlobStore", () => {
  const calls: { url: string; method: string; auth: string | null; body: number }[] = [];
  const store = (status: number, body = new Uint8Array([7, 8])) =>
    r2BlobStore(
      { accountId: "acct", bucket: "bkt", accessKeyId: "AKID", secretAccessKey: "SECRET" },
      async (req) => {
        calls.push({
          url: req.url,
          method: req.method,
          auth: req.headers.get("authorization"),
          body: (await req.clone().arrayBuffer()).byteLength,
        });
        return new Response(status === 404 ? null : body, { status });
      },
    );
  beforeEach(() => {
    calls.length = 0;
  });

  it("signs S3 requests against the account endpoint", async () => {
    const s = store(200);
    await s.put("u/a/b", new Uint8Array([1, 2, 3]));
    expect([...(await s.get("u/a/b"))!]).toEqual([7, 8]);
    await s.del("u/a/b");
    expect(calls.map((c) => c.method)).toEqual(["PUT", "GET", "DELETE"]);
    for (const c of calls) {
      expect(c.url).toBe("https://acct.r2.cloudflarestorage.com/bkt/u/a/b");
      expect(c.auth).toMatch(/^AWS4-HMAC-SHA256 Credential=AKID\//);
      expect(c.auth).not.toContain("SECRET");
    }
    expect(calls[0]!.body).toBe(3);
  });

  it("maps 404 to null on get, tolerates 404 on delete, throws on other errors", async () => {
    expect(await store(404).get("u/a/b")).toBeNull();
    await store(404).del("u/a/b");
    await expect(store(500).get("u/a/b")).rejects.toThrow(/500/);
    await expect(store(403).put("u/a/b", new Uint8Array([1]))).rejects.toThrow(/403/);
  });
});
