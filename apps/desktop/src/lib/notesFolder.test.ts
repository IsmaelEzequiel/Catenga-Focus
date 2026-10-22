import { describe, expect, it, vi } from "vitest";
import { applyNotesFolder } from "./notesFolder";

const guard = (over: Partial<{ flush: () => Promise<void>; busy: boolean }> = {}) => ({
  flushAndDrain: over.flush ?? (() => Promise.resolve()),
  isBusy: () => over.busy ?? false,
});

describe("applyNotesFolder", () => {
  it("flushes_then_saves_then_restarts_in_that_order", async () => {
    const calls: string[] = [];
    const notes = guard({ flush: async () => void calls.push("flush") });
    const save = vi.fn(async (p: string | null) => void calls.push(`save ${p}`));
    const restart = vi.fn(async () => void calls.push("restart"));
    expect(await applyNotesFolder(notes, "/x", save, restart)).toEqual({ ok: true });
    expect(calls).toEqual(["flush", "save /x", "restart"]);
  });

  it("null_means_the_default_folder", async () => {
    const save = vi.fn(async () => {});
    await applyNotesFolder(guard(), null, save, async () => {});
    expect(save).toHaveBeenCalledWith(null);
  });

  it("a_failed_flush_blocks_everything_and_says_so", async () => {
    const save = vi.fn(async () => {});
    const restart = vi.fn(async () => {});
    const notes = guard({ flush: () => Promise.reject(new Error("disk")) });
    const out = await applyNotesFolder(notes, "/x", save, restart);
    expect(out.ok).toBe(false);
    expect(save).not.toHaveBeenCalled();
    expect(restart).not.toHaveBeenCalled();
  });

  it("a_note_still_being_saved_blocks_the_restart", async () => {
    const restart = vi.fn(async () => {});
    const out = await applyNotesFolder(guard({ busy: true }), "/x", async () => {}, restart);
    expect(out.ok).toBe(false);
    expect(restart).not.toHaveBeenCalled();
  });

  it("a_refused_folder_does_not_restart_and_shows_the_reason", async () => {
    const restart = vi.fn(async () => {});
    const out = await applyNotesFolder(guard(), "/x", () => Promise.reject("/x isn't writable"), restart);
    expect(out).toEqual({ ok: false, error: "/x isn't writable" });
    expect(restart).not.toHaveBeenCalled();
  });
});
