import { describe, expect, it } from "vitest";
import type { Block, SaveOutcome } from "../api";
import {
  bannerText,
  createRecoveryTracker,
  openDecision,
  retryAction,
  type Recovery,
  type RecoveryApi,
} from "./recovery";

const block = (body: string): Block => ({ start: 540, end: 600, title: "Study", tags: [], body });
const rec = (date: string, body: string, baseRev = "r1"): Recovery => ({
  date,
  baseRev,
  savedAt: 1,
  blocks: [block(body)],
  error: null,
});

/** In-memory stand-in for the Tauri commands; `saves` decides what `saveDay` does. */
function fakeApi(initial: Recovery[] = [], saves: (() => Promise<SaveOutcome>)[] = []) {
  const files = new Map(initial.map((r) => [r.date, r]));
  const calls: string[] = [];
  const api: RecoveryApi = {
    put: async (date, blocks, baseRev) => {
      calls.push(`put ${date}`);
      files.set(date, { date, baseRev, savedAt: 2, blocks, error: null });
    },
    list: async () => [...files.values()],
    drop: async (date) => {
      calls.push(`drop ${date}`);
      files.delete(date);
    },
    save: async (date) => {
      calls.push(`save ${date}`);
      const next = saves.shift();
      if (!next) throw new Error("no save scripted");
      return next();
    },
  };
  return { api, files, calls };
}

const ok = (rev = "r2") => async (): Promise<SaveOutcome> => ({ status: "saved", rev });
const conflict = async (): Promise<SaveOutcome> => ({ status: "conflict", blocks: [], rev: "disk" });
const boom = async (): Promise<SaveOutcome> => {
  throw new Error("read-only");
};

describe("retryAction", () => {
  it("maps outcomes to actions", () => {
    expect(retryAction({ status: "saved", rev: "x" })).toBe("drop");
    expect(retryAction({ status: "conflict", blocks: [], rev: "x" })).toBe("review");
    expect(retryAction(new Error("nope"))).toBe("keep");
  });
});

describe("openDecision", () => {
  it("applies the recovery when the file is still at its base revision", () => {
    expect(openDecision("r1", rec("2026-10-21", "x"))).toBe("apply");
  });
  it("goes to the conflict path when the file changed", () => {
    expect(openDecision("other", rec("2026-10-21", "x"))).toBe("conflict");
  });
});

describe("bannerText", () => {
  it("lists the dates", () => {
    expect(bannerText([{ date: "2026-10-21", needsReview: false, unreadable: false }, { date: "2026-10-22", needsReview: false, unreadable: false }])).toBe(
      "Unsaved notes for 21 Oct, 22 Oct — kept safely.",
    );
  });
  it("asks to open the day when it needs review", () => {
    const text = bannerText([
      { date: "2026-10-21", needsReview: false, unreadable: false },
      { date: "2026-01-05", needsReview: true, unreadable: false },
    ]);
    expect(text).toContain("Unsaved notes for 21 Oct, 5 Jan — kept safely.");
    expect(text).toContain("Open the day to resolve: 5 Jan.");
  });
  it("says so when a recovery file cannot be read", () => {
    const text = bannerText([{ date: "2026-10-21", needsReview: true, unreadable: true }]);
    expect(text).toContain("couldn't be read");
  });
  it("is empty without entries", () => {
    expect(bannerText([])).toBeNull();
  });
});

describe("recovery tracker", () => {
  it("reproduces the bug: a save that fails after leaving the day keeps the text", async () => {
    const { api, files } = fakeApi([], [boom]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    // The day was left while this snapshot was being saved; the save then fails.
    await expect(tracker.attempt("2026-10-21", [block("typed just before leaving\n")], "r1")).rejects.toThrow("read-only");
    expect(files.get("2026-10-21")?.blocks[0].body).toBe("typed just before leaving\n");
    expect(files.get("2026-10-21")?.baseRev).toBe("r1");
    expect(tracker.get("2026-10-21")?.blocks[0].body).toBe("typed just before leaving\n");
    expect(tracker.entries().map((e) => e.date)).toEqual(["2026-10-21"]);
  });

  it("a successful attempt leaves nothing behind", async () => {
    const { api, files } = fakeApi([], [ok()]);
    const tracker = createRecoveryTracker(api);
    await expect(tracker.attempt("2026-10-21", [block("x\n")], "r1")).resolves.toEqual({ status: "saved", rev: "r2" });
    expect(files.size).toBe(0);
    expect(tracker.entries()).toEqual([]);
  });

  it("loads existing files at startup", async () => {
    const { api } = fakeApi([rec("2026-10-20", "old\n"), { ...rec("2026-10-19", ""), error: "garbage" }]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    expect(tracker.entries()).toEqual([
      { date: "2026-10-19", needsReview: true, unreadable: true },
      { date: "2026-10-20", needsReview: false, unreadable: false },
    ]);
  });

  it("keeps the entry in memory even when the durable write fails", async () => {
    const { api } = fakeApi([], [boom]);
    api.put = async () => {
      throw new Error("disk full");
    };
    const tracker = createRecoveryTracker(api);
    await expect(tracker.attempt("2026-10-21", [block("x\n")], "r1")).rejects.toThrow("read-only");
    expect(tracker.get("2026-10-21")?.blocks[0].body).toBe("x\n");
  });

  it("retry saves other days, drops what was saved and never touches the open day", async () => {
    const { api, files, calls } = fakeApi([rec("2026-10-21", "a\n"), rec("2026-10-22", "b\n")], [ok()]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    await tracker.retry("2026-10-22");
    expect(calls).toEqual(["save 2026-10-21", "drop 2026-10-21"]);
    expect([...files.keys()]).toEqual(["2026-10-22"]);
    expect(tracker.entries().map((e) => e.date)).toEqual(["2026-10-22"]);
  });

  it("retry keeps the entry when the save fails and tries again next time", async () => {
    const { api, files } = fakeApi([rec("2026-10-21", "a\n")], [boom, ok()]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    await tracker.retry("2026-10-25");
    expect(files.size).toBe(1);
    expect(tracker.entries()[0].needsReview).toBe(false);
    await tracker.retry("2026-10-25");
    expect(files.size).toBe(0);
  });

  it("retry marks a conflict for review, keeps the file and stops retrying it", async () => {
    const { api, files, calls } = fakeApi([rec("2026-10-21", "a\n")], [conflict]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    await tracker.retry("2026-10-25");
    expect(files.size).toBe(1);
    expect(tracker.entries()).toEqual([{ date: "2026-10-21", needsReview: true, unreadable: false }]);
    await tracker.retry("2026-10-25");
    expect(calls.filter((c) => c.startsWith("save"))).toHaveLength(1);
  });

  it("never retries or drops an unreadable file", async () => {
    const { api, files, calls } = fakeApi([{ ...rec("2026-10-21", ""), error: "garbage" }]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    await tracker.retry("2026-10-25");
    expect(calls).toEqual([]);
    expect(files.size).toBe(1);
  });

  it("a newer failure for the same day replaces the entry and clears needsReview", async () => {
    const { api } = fakeApi([rec("2026-10-21", "a\n")], [conflict, boom]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    await tracker.retry("2026-10-25");
    await expect(tracker.attempt("2026-10-21", [block("newer\n")], "r9")).rejects.toThrow();
    expect(tracker.get("2026-10-21")?.blocks[0].body).toBe("newer\n");
    expect(tracker.entries()[0].needsReview).toBe(false);
  });

  it("drop removes the file and the entry, and notifies subscribers", async () => {
    const { api, files } = fakeApi([rec("2026-10-21", "a\n")]);
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    let notified = 0;
    tracker.subscribe(() => notified++);
    await tracker.drop("2026-10-21");
    expect(files.size).toBe(0);
    expect(tracker.entries()).toEqual([]);
    expect(notified).toBe(1);
  });

  it("a failed drop keeps the entry", async () => {
    const { api } = fakeApi([rec("2026-10-21", "a\n")]);
    api.drop = async () => {
      throw new Error("busy");
    };
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    await tracker.drop("2026-10-21");
    expect(tracker.entries()).toHaveLength(1);
  });

  it("starts empty when the list cannot be read", async () => {
    const { api } = fakeApi();
    api.list = async () => {
      throw new Error("nope");
    };
    const tracker = createRecoveryTracker(api);
    await tracker.ready;
    expect(tracker.entries()).toEqual([]);
  });
});
