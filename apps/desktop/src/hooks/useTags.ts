import { useCallback, useEffect, useState } from "react";
import { goalsChangedOrTags, tagDelete, tagSave, tags, type Tag, type TagColor } from "../api";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Saved tags plus the ones seen in notes. Refreshed when tags or goals change (goal counts). */
export function useTags() {
  const [list, setList] = useState<Tag[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setList(await tags());
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);

  useEffect(() => {
    void load();
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    goalsChangedOrTags(() => void load()).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no live refresh; reopening the view still loads
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [load]);

  /** Runs a write; resolves to an error text or null. */
  const write = useCallback(async (call: () => Promise<Tag[]>): Promise<string | null> => {
    try {
      setList(await call());
      setError(null);
      return null;
    } catch (e) {
      return message(e);
    }
  }, []);

  return {
    list,
    error,
    save: (name: string, color: TagColor) => write(() => tagSave(name, color)),
    remove: (id: number) => write(() => tagDelete(id)),
  };
}
