import { useCallback, useEffect, useRef, useState } from "react";
import { onProjectsChanged, projectSetShown, projects, projectsRefresh, type Project } from "../api";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** The Google calendars refresh at most this often when the Goals page asks. */
const REFRESH_EVERY_MS = 10 * 60_000;

/**
 * Projects (the user's Google calendars) from the app database. Local only: works offline and without Google.
 * `refresh` talks to Google; failures only set `error`.
 */
export function useProjects() {
  const [list, setList] = useState<Project[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const lastRefresh = useRef(0);

  const load = useCallback(async () => {
    try {
      setList(await projects());
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);

  useEffect(() => {
    void load();
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onProjectsChanged(() => void load()).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no live refresh; reopening the view still loads
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [load]);

  /** Re-reads the calendar list from Google. `force: false` skips it if it ran in the last 10 minutes. */
  const refresh = useCallback(async (force = true) => {
    if (!force && Date.now() - lastRefresh.current < REFRESH_EVERY_MS) return;
    lastRefresh.current = Date.now();
    setRefreshing(true);
    try {
      setList(await projectsRefresh());
      setError(null);
    } catch (e) {
      lastRefresh.current = 0;
      setError(message(e));
    } finally {
      setRefreshing(false);
    }
  }, []);

  const setShown = useCallback(async (id: number, shown: boolean) => {
    try {
      setList(await projectSetShown(id, shown));
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);

  return { list, error, refreshing, refresh, setShown };
}
