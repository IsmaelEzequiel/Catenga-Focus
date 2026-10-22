import { describe, expect, it } from "vitest";
import type { SyncStatus } from "../api";
import { ago, notesLines, syncLine } from "./sync";

const status = (over: Partial<SyncStatus>): SyncStatus => ({
  state: "idle",
  lastSyncedAt: null,
  error: null,
  pending: 0,
  files: { pending: 0, conflicts: [], skipped: [], unavailable: false },
  ...over,
});

describe("ago", () => {
  it("rounds down to the largest unit", () => {
    expect(ago(1000, 1000)).toBe("just now");
    expect(ago(0, 59_000)).toBe("just now");
    expect(ago(0, 120_000)).toBe("2 min ago");
    expect(ago(0, 3 * 3_600_000)).toBe("3 h ago");
    expect(ago(0, 49 * 3_600_000)).toBe("2 d ago");
    expect(ago(10_000, 0)).toBe("just now");
  });
});

describe("syncLine", () => {
  it("describes every state", () => {
    expect(syncLine(status({ state: "off" }), 0).text).toBe("");
    expect(syncLine(status({ state: "syncing" }), 0).text).toBe("Syncing…");
    expect(syncLine(status({ state: "offline" }), 0).text).toBe("Offline — will retry");
    expect(syncLine(status({ state: "error", error: "Sign in again" }), 0)).toEqual({
      text: "Sign in again",
      error: true,
    });
    expect(syncLine(status({ state: "error" }), 0).text).toBe("Sync failed");
    expect(syncLine(status({ lastSyncedAt: 0 }), 120_000).text).toBe("Synced 2 min ago");
    expect(syncLine(status({ lastSyncedAt: 0, pending: 1 }), 120_000).text).toBe("Synced 2 min ago · 1 change pending");
    expect(syncLine(status({ lastSyncedAt: 0, pending: 3 }), 0).text).toBe("Synced just now · 3 changes pending");
    expect(syncLine(status({}), 0).text).toBe("Not synced yet");
  });
});

describe("notesLines", () => {
  const files = (over: Partial<SyncStatus["files"]>) => status({ files: { pending: 0, conflicts: [], skipped: [], unavailable: false, ...over } });

  it("is empty when everything is in sync", () => {
    expect(notesLines(files({}))).toEqual([]);
  });

  it("says when the server cannot sync notes yet, and nothing else", () => {
    expect(notesLines(files({ unavailable: true, pending: 4 }))).toEqual([
      { text: "Notes sync isn't available on the server yet", error: false },
    ]);
  });

  it("counts pending files, lists conflict copies and files that are too big", () => {
    const lines = notesLines(
      files({
        pending: 2,
        conflicts: ["2026/10/2026-10-20 (conflict 2026-10-22 1530).md"],
        skipped: ["2026/10/2026-10-21.md"],
      }),
    );
    expect(lines.map((l) => l.text)).toEqual([
      "2 notes files waiting to sync",
      "Both versions kept: 2026/10/2026-10-20 (conflict 2026-10-22 1530).md",
      "Too big to sync (over 5 MB): 2026/10/2026-10-21.md",
    ]);
    expect(notesLines(files({ pending: 1 }))[0].text).toBe("1 notes file waiting to sync");
  });
});
