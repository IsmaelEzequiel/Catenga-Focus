import { useCallback, useEffect, useRef, useState } from "react";
import {
  calendarCreate,
  calendarDelete,
  calendarDay,
  calendarRefresh,
  calendarUpdate,
  onCalendarChanged,
  onCalendarError,
  onCalendarsChanged,
  onProjectsChanged,
  type CalEvent,
} from "../api";
import { calendarOps, type CalBlock } from "../lib/calendar";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

const upsert = (events: CalEvent[], event: CalEvent) =>
  events.some((e) => e.id === event.id) ? events.map((e) => (e.id === event.id ? event : e)) : [...events, event];

/**
 * The visible day's Google Calendar events. Does nothing at all unless `enabled`.
 * Failures only produce notices; they never touch note saving.
 */
export function useCalendar(
  date: string,
  enabled: boolean,
  refreshMinutes: number,
  onNotice: (message: string) => void,
  onRefreshFailed: () => void,
) {
  const [state, setState] = useState<{ date: string; events: CalEvent[] } | null>(null);
  const dateRef = useRef(date);
  dateRef.current = date;
  const enabledRef = useRef(enabled);
  enabledRef.current = enabled;
  const eventsRef = useRef<CalEvent[]>([]);
  const noticeRef = useRef(onNotice);
  noticeRef.current = onNotice;
  const failedRef = useRef(onRefreshFailed);
  failedRef.current = onRefreshFailed;
  const chain = useRef<Promise<void>>(Promise.resolve());
  /** Dates whose refresh error was already shown, so an offline laptop does not nag every cycle. */
  const warned = useRef(new Set<string>());

  const setEvents = useCallback((forDate: string, events: CalEvent[]) => {
    if (forDate !== dateRef.current) return;
    eventsRef.current = events;
    setState({ date: forDate, events });
  }, []);

  const read = useCallback(
    async (forDate: string) => {
      try {
        const day = await calendarDay(forDate);
        if (enabledRef.current) setEvents(forDate, day.events);
      } catch (e) {
        noticeRef.current(`Couldn't read Google Calendar: ${message(e)}`);
      }
    },
    [setEvents],
  );

  // Load the cache, then refresh now, on an interval, and when the window regains focus.
  useEffect(() => {
    eventsRef.current = [];
    if (!enabled) {
      setState(null);
      return;
    }
    let cancelled = false;
    const refresh = () => {
      calendarRefresh(date).catch((e) => !cancelled && noticeRef.current(`Couldn't refresh Google Calendar: ${message(e)}`));
    };
    void read(date).then(() => !cancelled && refresh());
    const ms = Math.max(1, refreshMinutes) * 60_000;
    const timer = window.setInterval(refresh, ms);
    window.addEventListener("focus", refresh);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", refresh);
    };
  }, [date, enabled, refreshMinutes, read]);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    const unlisten: Array<() => void> = [];
    const keep = (p: Promise<() => void>) =>
      p.then(
        (fn) => (cancelled ? fn() : unlisten.push(fn)),
        () => {
          // No event bridge (tests, plain browser): the interval refresh still works.
        },
      );
    keep(
      onCalendarChanged(({ date: changed }) => {
        warned.current.delete(changed);
        if (changed === dateRef.current) void read(changed);
      }),
    );
    // A calendar shown/hidden or newly listed: hidden ones drop out on re-read, shown ones need a fetch.
    keep(
      onCalendarsChanged(() => {
        const day = dateRef.current;
        void read(day).then(() => calendarRefresh(day).catch(() => {}));
      }),
    );
    // A project created or archived changes the events' project keys: the cache is enough (no network, no loop).
    keep(onProjectsChanged(() => void read(dateRef.current)));
    keep(
      onCalendarError(({ date: failed, message: text }) => {
        failedRef.current();
        if (failed !== dateRef.current || warned.current.has(failed)) return;
        warned.current.add(failed);
        noticeRef.current(`Couldn't refresh Google Calendar: ${text}`);
      }),
    );
    return () => {
      cancelled = true;
      unlisten.forEach((fn) => fn());
    };
  }, [enabled, read]);

  /** Mirrors a block edit into Google (create/update); writes run one at a time. */
  const sync = useCallback(
    (prev: CalBlock[], next: CalBlock[]) => {
      if (!enabledRef.current) return;
      const forDate = dateRef.current;
      const ops = calendarOps(prev, next, eventsRef.current);
      for (const op of ops) {
        if (op.kind === "update") {
          // Optimistic, so the event does not flash as a ghost while the request is in flight.
          const { eventId, block } = op;
          setEvents(
            forDate,
            eventsRef.current.map((e) =>
              e.id === eventId ? { ...e, start: block.start, end: block.end, title: block.title.trim() } : e,
            ),
          );
        }
        chain.current = chain.current.then(async () => {
          try {
            const event =
              op.kind === "create"
                ? await calendarCreate(forDate, op.block.start, op.block.end, op.block.title.trim())
                : await calendarUpdate(forDate, op.eventId, op.block.start, op.block.end, op.block.title.trim());
            setEvents(forDate, upsert(eventsRef.current, event));
          } catch (e) {
            noticeRef.current(`Couldn't update Google Calendar: ${message(e)}`);
            // Drop the optimistic change.
            if (op.kind === "update") await read(forDate);
          }
        });
      }
    },
    [read, setEvents],
  );

  /** Deletes an event in Google (after the pending writes); the timeline drops it right away. False on failure (with a notice). */
  const remove = useCallback(
    (eventId: string): Promise<boolean> => {
      const forDate = dateRef.current;
      const run = chain.current.then(async () => {
        try {
          await calendarDelete(forDate, eventId);
          setEvents(
            forDate,
            eventsRef.current.filter((e) => e.id !== eventId),
          );
          return true;
        } catch (e) {
          noticeRef.current(`Couldn't delete the event: ${message(e)}`);
          return false;
        }
      });
      chain.current = run.then(() => undefined);
      return run;
    },
    [setEvents],
  );

  /** Re-reads the visible day from the cache (e.g. after an event became a project). */
  const reload = useCallback(() => read(dateRef.current), [read]);

  /** False until the cached events of `date` are read (always true when Google Calendar is off). */
  const loaded = !enabled || state?.date === date;
  return { events: enabled && state?.date === date ? state.events : [], loaded, sync, remove, reload };
}
