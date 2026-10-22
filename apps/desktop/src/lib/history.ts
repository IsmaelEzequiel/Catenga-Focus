import type { HistoryEntry } from "../api";

/** Entries arrive newest first; this applies the view's filter and order without touching the input. */
export function visibleEntries(
  entries: HistoryEntry[],
  opts: { onlyWithNotes: boolean; order: "newest" | "oldest" },
): HistoryEntry[] {
  const shown = opts.onlyWithNotes ? entries.filter((e) => e.body.trim() !== "") : [...entries];
  return opts.order === "oldest" ? shown.reverse() : shown;
}
