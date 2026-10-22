import { useCallback, useEffect, useRef, useState } from "react";
import { loadDay, onDayChanged, type DayChanged, type LoadedDay } from "../api";
import { createAutosaveTimer } from "../lib/autosaveTimer";
import { applyNote, isLastBlock } from "../lib/note";
import { mergeConflict, overlayNotes, reconcileKeys, recoveryConflicts, type Conflict, type UiBlock } from "../lib/reconcile";
import { openDecision, type RecoveryTracker } from "../lib/recovery";

export type { Conflict, UiBlock };

export type SaveState = "idle" | "editing" | "saving" | "saved" | "error" | "conflict";

const MAX_CONFLICT_RETRIES = 3;

const withKey = (b: Omit<UiBlock, "key">): UiBlock => ({ ...b, key: crypto.randomUUID() });
const stripKey = ({ key: _key, ...block }: UiBlock) => block;

/**
 * Single owner of a day's blocks and of every write to its file. Saves run one at a time;
 * each carries the revision it was based on, so edits made outside the app are never overwritten.
 * A save that fails (or conflicts after the day was left) is kept in `recovery`, never dropped.
 */
export function useDay(date: string, onNotice: (message: string) => void, recovery: RecoveryTracker) {
  const [blocks, setBlocks] = useState<UiBlock[]>([]);
  const [loadedDate, setLoadedDate] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [noteVersion, setNoteVersion] = useState(0);
  const [conflicts, setConflicts] = useState<Conflict[]>([]);

  const dateRef = useRef(date);
  // Identifies the current load; stale loads and saves compare against it.
  const loadId = useRef(0);
  const blocksRef = useRef<UiBlock[]>([]);
  const loadedRef = useRef(false);
  // Last state known to match the file, the base for three-way merges.
  const saved = useRef<UiBlock[]>([]);
  const revs = useRef(new Map<string, string>());
  const conflictsRef = useRef<Conflict[]>([]);
  const dirty = useRef(false);
  const noteEdited = useRef(false);
  const errored = useRef(false);
  const inflight = useRef(0);
  const retries = useRef(0);
  const chain = useRef<Promise<void>>(Promise.resolve());
  const seq = useRef(0);
  const failed = useRef({ loadId: -1, upToSeq: -1 });
  const onNoticeRef = useRef(onNotice);
  onNoticeRef.current = onNotice;
  const recoveryRef = useRef(recovery);
  recoveryRef.current = recovery;

  const refreshSaveState = useCallback(() => {
    let state: SaveState;
    if (conflictsRef.current.length > 0) state = "conflict";
    else if (errored.current) state = "error";
    else if (!noteEdited.current) state = "idle";
    else if (inflight.current > 0) state = "saving";
    else if (dirty.current) state = "editing";
    else state = "saved";
    setSaveState(state);
  }, []);

  const setBlocksBoth = useCallback((next: UiBlock[]) => {
    blocksRef.current = next;
    setBlocks(next);
  }, []);

  const setConflictsBoth = useCallback((next: Conflict[]) => {
    conflictsRef.current = next;
    setConflicts(next);
  }, []);

  /**
   * Keeps what a failed save tried to write. For the day on screen that is the structure on disk plus
   * the typed text (what the editor still holds); for a day already left, the snapshot itself.
   */
  const keepUnsaved = useCallback(async (id: number, saveDate: string, snapshot: UiBlock[], baseRev: string) => {
    const keep = id === loadId.current ? overlayNotes(saved.current, blocksRef.current) : snapshot;
    await recoveryRef.current.record(saveDate, keep.map(stripKey), baseRev);
  }, []);

  const handleConflict = useCallback(
    async (id: number, saveDate: string, diskBlocks: LoadedDay, snapshot: UiBlock[], baseRev: string) => {
      revs.current.set(saveDate, diskBlocks.rev);
      if (id !== loadId.current) {
        await keepUnsaved(id, saveDate, snapshot, baseRev);
        setError("A day file you edited changed on disk; your last note edit there was kept safely and needs review.");
        return;
      }
      const merged = mergeConflict(saved.current, blocksRef.current, diskBlocks.blocks);
      saved.current = merged.disk;
      setBlocksBoth(merged.blocks);
      const fresh = new Set(merged.conflicts.map((c) => c.key));
      setConflictsBoth([...conflictsRef.current.filter((c) => !fresh.has(c.key)), ...merged.conflicts]);
      if (merged.structureDropped) {
        onNoticeRef.current("The file changed on disk; your last layout change wasn't applied.");
      }
      if (merged.changed) {
        if (++retries.current > MAX_CONFLICT_RETRIES) {
          errored.current = true;
          setError("The file keeps changing on disk, so your note could not be saved. Try again in a moment.");
        } else {
          dirty.current = false;
          enqueueRef.current(saveDate, merged.blocks);
        }
      }
    },
    [keepUnsaved, setBlocksBoth, setConflictsBoth],
  );

  const enqueueRef = useRef<(saveDate: string, snapshot: UiBlock[]) => void>(() => {});
  const enqueue = useCallback(
    (saveDate: string, snapshot: UiBlock[]) => {
      const id = loadId.current;
      const mySeq = ++seq.current;
      inflight.current++;
      refreshSaveState();
      chain.current = chain.current.then(async () => {
        try {
          const baseRev = revs.current.get(saveDate) ?? "";
          if (failed.current.loadId === id && mySeq <= failed.current.upToSeq) {
            // Skipped after an earlier failure; this snapshot may be newer than the one kept.
            await keepUnsaved(id, saveDate, snapshot, baseRev);
            return;
          }
          const out = await recoveryRef.current.attempt(saveDate, snapshot.map(stripKey), baseRev);
          if (out.status === "saved") {
            revs.current.set(saveDate, out.rev);
            retries.current = 0;
            if (errored.current) setError(null);
            errored.current = false;
            if (id === loadId.current) saved.current = snapshot;
            // Saved: any kept copy of this day is superseded (unless conflicts still hold my text).
            if (id !== loadId.current || conflictsRef.current.length === 0) await recoveryRef.current.drop(saveDate);
          } else {
            await handleConflict(id, saveDate, out, snapshot, baseRev);
          }
        } catch (e) {
          failed.current = { loadId: id, upToSeq: seq.current };
          errored.current = noteEdited.current;
          setError(String(e));
          if (id === loadId.current) {
            // Structure goes back to what is on disk, but typed text is never rolled back.
            setBlocksBoth(overlayNotes(saved.current, blocksRef.current));
            dirty.current = noteEdited.current;
            // What was kept must match what the editor is left with, not the rolled-back layout.
            await recoveryRef.current.record(
              saveDate,
              blocksRef.current.map(stripKey),
              revs.current.get(saveDate) ?? "",
            );
          }
        } finally {
          inflight.current--;
          refreshSaveState();
        }
      });
    },
    [handleConflict, keepUnsaved, refreshSaveState, setBlocksBoth],
  );
  enqueueRef.current = enqueue;

  /** Saves the current notes now if there are unsaved edits. */
  const flushNow = useCallback(() => {
    if (!dirty.current || !loadedRef.current) return;
    dirty.current = false;
    enqueue(dateRef.current, blocksRef.current);
  }, [enqueue]);

  const timer = useRef<ReturnType<typeof createAutosaveTimer> | null>(null);
  if (timer.current === null) timer.current = createAutosaveTimer(() => flushNow());

  const flush = useCallback(() => {
    timer.current?.cancel();
    flushNow();
  }, [flushNow]);

  const flushAndDrain = useCallback(async () => {
    flush();
    let last: Promise<void>;
    do {
      last = chain.current;
      await last;
    } while (last !== chain.current);
  }, [flush]);

  const isBusy = useCallback(
    () => dirty.current || inflight.current > 0 || conflictsRef.current.length > 0 || errored.current,
    [],
  );

  /** Why leaving this day now could lose typed text, or `null` when it is safe. */
  const leaveBlocker = useCallback((): string | null => {
    if (conflictsRef.current.length > 0) {
      return "A note changed on disk. Choose Keep mine or Use file's before leaving this day.";
    }
    if (errored.current && dirty.current) return "Your note isn't saved yet. Retry saving before leaving this day.";
    return null;
  }, []);

  useEffect(() => {
    const id = ++loadId.current;
    dateRef.current = date;
    loadedRef.current = false;
    setBlocksBoth([]);
    setConflictsBoth([]);
    setLoadedDate(null);
    dirty.current = false;
    noteEdited.current = false;
    errored.current = false;
    retries.current = 0;
    refreshSaveState();
    // Wait for the previous day's flush so a quick back-and-forth can't load a stale file.
    chain.current
      .then(() => recoveryRef.current.ready)
      .then(() => loadDay(date))
      .then(
        (loaded) => {
          if (id !== loadId.current) return;
          const next = loaded.blocks.map(withKey);
          revs.current.set(date, loaded.rev);
          saved.current = next;
          loadedRef.current = true;
          setBlocksBoth(next);
          setLoadedDate(date);
          const kept = recoveryRef.current.get(date);
          if (!kept || kept.error !== null) return;
          if (openDecision(loaded.rev, kept) === "apply") {
            // The file is as it was when the save failed: the kept notes become unsaved edits again.
            setBlocksBoth(reconcileKeys(next, kept.blocks));
            setNoteVersion((v) => v + 1);
            dirty.current = true;
            noteEdited.current = true;
            timer.current?.touch();
            refreshSaveState();
            onNoticeRef.current("Recovered notes that could not be saved earlier; saving them now.");
          } else {
            const found = recoveryConflicts(next, kept.blocks);
            if (found.length === 0) {
              void recoveryRef.current.drop(date);
            } else {
              setConflictsBoth(found);
              onNoticeRef.current("This day changed on disk after a failed save. Choose Keep mine or Use file's for each note.");
            }
          }
        },
        (e) => {
          if (id !== loadId.current) return;
          saved.current = [];
          setError(String(e));
        },
      );
    return () => {
      // Runs before the next date's effect: refs still describe the day being left.
      timer.current?.cancel();
      flushNow();
    };
  }, [date, flushNow, refreshSaveState, setBlocksBoth, setConflictsBoth]);

  /** Replaces the day with what is on disk (only when nothing local is unsaved). */
  const applyDisk = useCallback(
    (disk: LoadedDay) => {
      const prev = blocksRef.current;
      const next = reconcileKeys(prev, disk.blocks);
      const oldBodies = new Map(prev.map((b) => [b.key, b.body]));
      const bodiesChanged = next.length !== prev.length || next.some((b) => oldBodies.get(b.key) !== b.body);
      revs.current.set(dateRef.current, disk.rev);
      saved.current = next;
      setBlocksBoth(next);
      if (bodiesChanged) setNoteVersion((v) => v + 1);
    },
    [setBlocksBoth],
  );

  const syncWithDisk = useCallback(
    async (changedDate: string) => {
      const id = loadId.current;
      const stale = () => id !== loadId.current || dateRef.current !== changedDate;
      // A dirty day is saved first; Rust refuses it against the new file and the conflict path merges.
      flush();
      let last: Promise<void>;
      do {
        last = chain.current;
        await last;
      } while (last !== chain.current);
      if (stale() || dirty.current || inflight.current > 0 || conflictsRef.current.length > 0) return;
      try {
        const disk = await loadDay(changedDate);
        if (stale() || disk.rev === revs.current.get(changedDate)) return;
        // Typing that started during the load will hit the conflict path when it saves.
        if (dirty.current || inflight.current > 0) return;
        applyDisk(disk);
      } catch (e) {
        if (!stale()) setError(String(e));
      }
    },
    [applyDisk, flush],
  );

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const handler = ({ date: changedDate, rev }: DayChanged) => {
      if (changedDate !== dateRef.current || !loadedRef.current) return;
      if (rev === revs.current.get(changedDate)) return;
      if (conflictsRef.current.length > 0) return;
      void syncWithDisk(changedDate);
    };
    onDayChanged(handler).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      (e) => setError(String(e)),
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [syncWithDisk]);

  /** Optimistic update of the whole day (geometry, titles, deletes); includes any typed notes. */
  const commit = useCallback(
    (next: UiBlock[]) => {
      timer.current?.cancel();
      dirty.current = false;
      setBlocksBoth(next);
      enqueue(dateRef.current, next);
    },
    [enqueue, setBlocksBoth],
  );

  const setNote = useCallback(
    (key: string, note: string) => {
      const current = blocksRef.current;
      const conflict = conflictsRef.current.find((c) => c.key === key);
      if (conflict) {
        // Unresolved: keep the typed text aside, never write over the file's version.
        const all = current.some((b) => b.key === key) ? current : [...current, conflict.mine];
        const body = applyNote(conflict.mine.body, note, isLastBlock(all, key));
        setConflictsBoth(conflictsRef.current.map((c) => (c.key === key ? { ...c, mine: { ...c.mine, body } } : c)));
        return;
      }
      const block = current.find((b) => b.key === key);
      if (!block) return;
      const body = applyNote(block.body, note, isLastBlock(current, key));
      if (body === block.body) return;
      setBlocksBoth(current.map((b) => (b.key === key ? { ...b, body } : b)));
      dirty.current = true;
      noteEdited.current = true;
      timer.current?.touch();
      refreshSaveState();
    },
    [refreshSaveState, setBlocksBoth, setConflictsBoth],
  );

  const resolveConflict = useCallback(
    (key: string, choice: "mine" | "file") => {
      const conflict = conflictsRef.current.find((c) => c.key === key);
      if (!conflict) return;
      setConflictsBoth(conflictsRef.current.filter((c) => c.key !== key));
      if (choice === "file") {
        setNoteVersion((v) => v + 1);
        refreshSaveState();
        // Nothing of mine is left to save: the kept copy is no longer needed.
        if (conflictsRef.current.length === 0 && !dirty.current && inflight.current === 0) {
          void recoveryRef.current.drop(dateRef.current);
        }
        return;
      }
      const current = blocksRef.current;
      const next = current.some((b) => b.key === key)
        ? current.map((b) => (b.key === key ? { ...b, body: conflict.mine.body } : b))
        : [...current, conflict.mine];
      noteEdited.current = true;
      commit(next);
    },
    [commit, refreshSaveState, setConflictsBoth],
  );

  // Until this date's load lands, show nothing (avoids a frame of the previous day).
  const loaded = loadedDate === date;
  return {
    blocks: loaded ? blocks : [],
    loaded,
    error,
    setError,
    commit,
    setNote,
    flush,
    flushAndDrain,
    saveState,
    noteVersion,
    conflicts,
    resolveConflict,
    isBusy,
    leaveBlocker,
  };
}
