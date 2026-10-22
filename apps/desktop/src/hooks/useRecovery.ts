import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { recoveryDrop, recoveryList, recoveryPut, saveDay } from "../api";
import { createRecoveryTracker, type PendingEntry, type RecoveryTracker } from "../lib/recovery";

const RETRY_MS = 30_000;

/**
 * The days whose save failed. Retries the ones not on screen every 30 s, when the window regains
 * focus, and on demand. `currentDate` is the day the editor owns; it is never retried from here.
 */
export function useRecovery(currentDate: string): { tracker: RecoveryTracker; entries: PendingEntry[]; retry: () => void } {
  const [tracker] = useState(() =>
    createRecoveryTracker({ put: recoveryPut, list: recoveryList, drop: recoveryDrop, save: saveDay }),
  );
  const entries = useSyncExternalStore(tracker.subscribe, tracker.entries);
  const dateRef = useRef(currentDate);
  dateRef.current = currentDate;

  const retry = useCallback(() => void tracker.retry(dateRef.current), [tracker]);

  useEffect(() => {
    const id = setInterval(retry, RETRY_MS);
    window.addEventListener("focus", retry);
    retry();
    return () => {
      clearInterval(id);
      window.removeEventListener("focus", retry);
    };
  }, [retry]);

  return { tracker, entries, retry };
}
