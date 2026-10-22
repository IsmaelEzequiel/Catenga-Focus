import { describe, expect, it, vi } from "vitest";
import { settleNotes, type NoteGuard } from "./lifecycle";

function guard(busyAfterFlush: boolean): NoteGuard & { flushAndDrain: ReturnType<typeof vi.fn> } {
  return { flushAndDrain: vi.fn(async () => {}), isBusy: () => busyAfterFlush };
}

describe("settleNotes", () => {
  it("proceeds_after_flushing_clean_notes", async () => {
    const g = guard(false);
    expect(await settleNotes(g, false)).toBe("proceed");
    expect(g.flushAndDrain).toHaveBeenCalledTimes(1);
  });

  it("always_flushes_even_when_nothing_looks_pending", async () => {
    const g = guard(false);
    await settleNotes(g, true);
    expect(g.flushAndDrain).toHaveBeenCalledTimes(1);
  });

  it("blocks_when_the_flush_leaves_notes_unsaved", async () => {
    const g = guard(true);
    expect(await settleNotes(g, false)).toBe("blocked");
  });

  it("proceeds_with_unsaved_notes_only_when_the_user_chose_to_discard", async () => {
    const g = guard(true);
    expect(await settleNotes(g, true)).toBe("proceed");
    expect(g.flushAndDrain).toHaveBeenCalledTimes(1);
  });

  it("blocks_when_the_flush_itself_throws", async () => {
    const g: NoteGuard = {
      flushAndDrain: async () => {
        throw new Error("disk full");
      },
      isBusy: () => true,
    };
    expect(await settleNotes(g, false)).toBe("blocked");
  });
});
