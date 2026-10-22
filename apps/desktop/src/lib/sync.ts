import type { SyncStatus } from "../api";

/** "just now", "2 min ago", "3 h ago", "2 d ago". */
export function ago(thenMs: number, nowMs: number): string {
  const s = Math.max(0, Math.floor((nowMs - thenMs) / 1000));
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} h ago`;
  return `${Math.floor(h / 24)} d ago`;
}

const pendingText = (n: number) => (n === 1 ? "1 change pending" : `${n} changes pending`);

/** The one line under the account: what sync is doing, or why it is not. */
export function syncLine(status: SyncStatus, nowMs: number): { text: string; error: boolean } {
  switch (status.state) {
    case "off":
      return { text: "", error: false };
    case "syncing":
      return { text: "Syncing…", error: false };
    case "offline":
      return { text: "Offline — will retry", error: false };
    case "error":
      return { text: status.error ?? "Sync failed", error: true };
    case "idle": {
      const when = status.lastSyncedAt === null ? "Not synced yet" : `Synced ${ago(status.lastSyncedAt, nowMs)}`;
      return { text: status.pending > 0 ? `${when} · ${pendingText(status.pending)}` : when, error: false };
    }
  }
}

/** Extra lines about the notes folder: what waits, what was kept twice, what is too big. */
export function notesLines(status: SyncStatus): { text: string; error: boolean }[] {
  const { files } = status;
  if (files.unavailable) return [{ text: "Notes sync isn't available on the server yet", error: false }];
  const lines: { text: string; error: boolean }[] = [];
  if (files.pending > 0) {
    lines.push({
      text: files.pending === 1 ? "1 notes file waiting to sync" : `${files.pending} notes files waiting to sync`,
      error: false,
    });
  }
  for (const path of files.conflicts) lines.push({ text: `Both versions kept: ${path}`, error: false });
  for (const path of files.skipped) lines.push({ text: `Too big to sync (over 5 MB): ${path}`, error: false });
  return lines;
}
