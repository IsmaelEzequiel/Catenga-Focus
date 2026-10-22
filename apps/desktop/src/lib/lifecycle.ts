/** What closing or quitting needs from the notes (see `docs/plans/M12-background-tray.md` §1.1). */
export interface NoteGuard {
  flushAndDrain(): Promise<void>;
  isBusy(): boolean;
}

/**
 * Flushes everything typed. `proceed` when it is on disk (or the user chose to discard what could not be
 * saved), `blocked` when the save failed and the window must stay as it is.
 */
export async function settleNotes(notes: NoteGuard, discard: boolean): Promise<"proceed" | "blocked"> {
  try {
    await notes.flushAndDrain();
  } catch {
    return discard ? "proceed" : "blocked";
  }
  return notes.isBusy() && !discard ? "blocked" : "proceed";
}
