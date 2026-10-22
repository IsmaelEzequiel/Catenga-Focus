import { useCallback, useEffect, useRef, useState } from "react";
import {
  calendars,
  calendarSetShown,
  onCalendarsChanged,
  onProjectsChanged,
  projectFromEvent,
  projects,
  projectSetArchived,
  projectsRefresh,
  type Calendar,
  type Project,
} from "../api";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** The series list refreshes at most this often when the Goals page asks. */
const REFRESH_EVERY_MS = 10 * 60_000;

/**
 * Projects (recurring event series) and calendars from the app database. Local only: works offline and without Google.
 * `refresh` talks to Google; failures only set `error`.
 */
export function useProjects() {
  const [list, setList] = useState<Project[]>([]);
  const [calendarList, setCalendarList] = useState<Calendar[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const lastRefresh = useRef(0);

  const load = useCallback(async () => {
    try {
      const [p, c] = await Promise.all([projects(), calendars()]);
      setList(p);
      setCalendarList(c);
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);

  useEffect(() => {
    void load();
    let cancelled = false;
    const unlisten: Array<() => void> = [];
    for (const on of [onProjectsChanged, onCalendarsChanged]) {
      on(() => void load()).then(
        (fn) => (cancelled ? fn() : unlisten.push(fn)),
        () => {}, // no live refresh; reopening the view still loads
      );
    }
    return () => {
      cancelled = true;
      unlisten.forEach((fn) => fn());
    };
  }, [load]);

  /** Re-reads the calendars and series from Google. `force: false` skips it if it ran in the last 10 minutes. */
  const refresh = useCallback(
    async (force = true) => {
      if (!force && Date.now() - lastRefresh.current < REFRESH_EVERY_MS) return;
      lastRefresh.current = Date.now();
      setRefreshing(true);
      try {
        setList(await projectsRefresh());
        setCalendarList(await calendars());
        setError(null);
      } catch (e) {
        lastRefresh.current = 0;
        setError(message(e));
      } finally {
        setRefreshing(false);
      }
    },
    [],
  );

  const setShown = useCallback(async (id: number, shown: boolean) => {
    try {
      setCalendarList(await calendarSetShown(id, shown));
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);

  const setArchived = useCallback(async (id: number, archived: boolean) => {
    try {
      setList(await projectSetArchived(id, archived));
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);

  /** Makes a one-off event a project. Resolves to an error text or null. */
  const fromEvent = useCallback(async (date: string, eventId: string): Promise<string | null> => {
    try {
      setList(await projectFromEvent(date, eventId));
      return null;
    } catch (e) {
      return message(e);
    }
  }, []);

  return { list, calendars: calendarList, error, refreshing, refresh, setShown, setArchived, fromEvent };
}
