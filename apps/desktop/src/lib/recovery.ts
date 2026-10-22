import type { Block, SaveOutcome } from "../api";

/** A snapshot of a day whose save failed, kept on disk by the Rust side. */
export interface Recovery {
  date: string;
  /** revision of the day file the snapshot was based on */
  baseRev: string;
  savedAt: number;
  blocks: Block[];
  /** set when the recovery file could not be read (it is kept, never deleted) */
  error: string | null;
}

export interface RecoveryApi {
  put(date: string, blocks: Block[], baseRev: string): Promise<void>;
  list(): Promise<Recovery[]>;
  drop(date: string): Promise<void>;
  save(date: string, blocks: Block[], baseRev: string): Promise<SaveOutcome>;
}

export interface PendingEntry {
  date: string;
  /** the file changed (or the recovery file is unreadable): only the user can resolve it */
  needsReview: boolean;
  unreadable: boolean;
}

export type RetryAction = "drop" | "review" | "keep";

/** What to do with a pending day after a background save attempt (`Error` = the save threw). */
export function retryAction(outcome: SaveOutcome | Error): RetryAction {
  if (outcome instanceof Error) return "keep";
  return outcome.status === "saved" ? "drop" : "review";
}

/** Opening a day that has a recovery: same base file -> apply as unsaved edits; else the conflict UI. */
export function openDecision(loadedRev: string, recovery: Recovery): "apply" | "conflict" {
  return loadedRev === recovery.baseRev ? "apply" : "conflict";
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const shortDate = (date: string) => `${Number(date.slice(8, 10))} ${MONTHS[Number(date.slice(5, 7)) - 1] ?? ""}`;

export function bannerText(entries: PendingEntry[]): string | null {
  if (entries.length === 0) return null;
  let text = `Unsaved notes for ${entries.map((e) => shortDate(e.date)).join(", ")} — kept safely.`;
  const unreadable = entries.filter((e) => e.unreadable);
  const review = entries.filter((e) => e.needsReview && !e.unreadable);
  if (review.length > 0) text += ` Open the day to resolve: ${review.map((e) => shortDate(e.date)).join(", ")}.`;
  if (unreadable.length > 0) {
    text += ` A recovery file for ${unreadable.map((e) => shortDate(e.date)).join(", ")} couldn't be read; it was left in the recovery folder.`;
  }
  return text;
}

interface Held {
  recovery: Recovery;
  needsReview: boolean;
}

/**
 * The days whose latest save failed, mirrored in memory and in the recovery folder.
 * Nothing here ever deletes a file it could not read.
 */
export function createRecoveryTracker(api: RecoveryApi) {
  const held = new Map<string, Held>();
  const listeners = new Set<() => void>();
  let cache: PendingEntry[] = [];
  const changed = () => {
    cache = [...held.values()]
      .map((h) => ({ date: h.recovery.date, needsReview: h.needsReview || h.recovery.error !== null, unreadable: h.recovery.error !== null }))
      .sort((a, b) => a.date.localeCompare(b.date));
    listeners.forEach((fn) => fn());
  };

  const ready = api.list().then(
    (list) => {
      for (const recovery of list) held.set(recovery.date, { recovery, needsReview: false });
      changed();
    },
    () => {},
  );

  /** Remembers a snapshot that could not be saved; the in-memory copy survives a failed durable write. */
  const record = async (date: string, blocks: Block[], baseRev: string): Promise<void> => {
    held.set(date, { recovery: { date, baseRev, savedAt: Date.now(), blocks, error: null }, needsReview: false });
    changed();
    try {
      await api.put(date, blocks, baseRev);
    } catch {
      // Kept in memory; the next failure or retry writes it again.
    }
  };

  const drop = async (date: string): Promise<void> => {
    const h = held.get(date);
    if (!h) return;
    try {
      await api.drop(date);
    } catch {
      return;
    }
    // A newer snapshot may have been recorded while the file was being removed.
    if (held.get(date) !== h) return;
    held.delete(date);
    changed();
  };

  /** Saves a snapshot; if the save throws, the snapshot is kept before the error is passed on. */
  const attempt = async (date: string, blocks: Block[], baseRev: string): Promise<SaveOutcome> => {
    try {
      return await api.save(date, blocks, baseRev);
    } catch (e) {
      await record(date, blocks, baseRev);
      throw e;
    }
  };

  /** Tries to save every pending day except `currentDate` (that one is owned by the open editor). */
  const retry = async (currentDate: string): Promise<void> => {
    await ready;
    for (const [date, h] of [...held]) {
      if (date === currentDate || h.needsReview || h.recovery.error !== null) continue;
      let outcome: SaveOutcome | Error;
      try {
        outcome = await api.save(date, h.recovery.blocks, h.recovery.baseRev);
      } catch (e) {
        outcome = e instanceof Error ? e : new Error(String(e));
      }
      // A newer snapshot may have replaced this one while the save was running.
      if (held.get(date) !== h) continue;
      const action = retryAction(outcome);
      if (action === "drop") await drop(date);
      else if (action === "review") {
        h.needsReview = true;
        changed();
      }
    }
  };

  return {
    ready,
    record,
    drop,
    attempt,
    retry,
    get: (date: string): Recovery | undefined => held.get(date)?.recovery,
    entries: (): PendingEntry[] => cache,
    subscribe: (fn: () => void) => {
      listeners.add(fn);
      return () => void listeners.delete(fn);
    },
  };
}

export type RecoveryTracker = ReturnType<typeof createRecoveryTracker>;
