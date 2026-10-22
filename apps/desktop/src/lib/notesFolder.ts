import { settleNotes, type NoteGuard } from "./lifecycle";

export type ApplyResult = { ok: true } | { ok: false; error: string };

/**
 * Switches the notes folder: everything typed is flushed to the current folder first, then the choice is saved
 * (the backend validates it) and the app restarts on it. Nothing restarts unless the flush and the save both worked.
 */
export async function applyNotesFolder(
  notes: NoteGuard,
  path: string | null,
  save: (path: string | null) => Promise<void>,
  restart: () => Promise<void>,
): Promise<ApplyResult> {
  if ((await settleNotes(notes, false)) === "blocked") {
    return { ok: false, error: "Couldn't save your note, so the folder wasn't changed. Try again." };
  }
  try {
    await save(path);
  } catch (e) {
    return { ok: false, error: String(e) };
  }
  try {
    await restart();
  } catch (e) {
    return { ok: false, error: String(e) };
  }
  return { ok: true };
}
