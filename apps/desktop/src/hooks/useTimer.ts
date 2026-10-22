import { useCallback, useEffect, useRef, useState } from "react";
import {
  onSessionsChanged,
  onTimerChanged,
  timerDeleteSession,
  timerResolveStale,
  timerSessions,
  timerSkipBreak,
  timerStart,
  timerStatus,
  timerStop,
  type BlockRef,
  type PomodoroPlan,
  type Session,
  type TimerMode,
  type TimerStatus,
} from "../api";
import { runFinished, wasDiscarded } from "../lib/focus";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

const IDLE: TimerStatus = { state: "idle" };

export const DISCARDED = "Under a minute — that session wasn't saved.";

/** Subscribes without failing when there is no event bridge (tests, plain browser). */
function useListeners(subscribe: () => Array<Promise<() => void>>, deps: unknown[]) {
  useEffect(() => {
    let cancelled = false;
    const unlisten: Array<() => void> = [];
    for (const p of subscribe()) {
      p.then(
        (fn) => (cancelled ? fn() : unlisten.push(fn)),
        () => {},
      );
    }
    return () => {
      cancelled = true;
      unlisten.forEach((fn) => fn());
    };
  }, deps);
}

/**
 * The app's single timer. The engine lives in Rust; this only mirrors its status and ticks a clock once
 * a second while running (display only: elapsed/remaining are always computed from timestamps).
 */
export function useTimer(onNotice: (message: string) => void) {
  const [status, setStatusState] = useState<TimerStatus>(IDLE);
  const [finished, setFinished] = useState<{ cycles: number; id: number } | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const noticeRef = useRef(onNotice);
  noticeRef.current = onNotice;
  const statusRef = useRef<TimerStatus>(IDLE);
  /** True while a user-requested stop is in flight, so the resulting idle isn't taken for a finished run. */
  const stopping = useRef(false);
  const finishedId = useRef(0);

  /** Every status change goes through here (events and command results alike). */
  const apply = useCallback((next: TimerStatus) => {
    const cycles = runFinished(statusRef.current, next, stopping.current);
    statusRef.current = next;
    if (cycles !== null) setFinished({ cycles, id: ++finishedId.current });
    else if (next.state === "running") setFinished(null);
    setStatusState(next);
  }, []);

  useEffect(() => {
    let cancelled = false;
    timerStatus().then(
      (s) => !cancelled && apply(s),
      (e) => !cancelled && noticeRef.current(`Couldn't read the timer: ${message(e)}`),
    );
    return () => {
      cancelled = true;
    };
  }, [apply]);

  useListeners(() => [onTimerChanged(apply)], [apply]);

  const running = status.state === "running";
  useEffect(() => {
    if (!running) return;
    setNowMs(Date.now());
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [running]);

  const run = useCallback(
    async (what: string, call: () => Promise<TimerStatus>, userStop = false): Promise<boolean> => {
      const before = statusRef.current;
      const at = Date.now();
      stopping.current = userStop;
      try {
        apply(await call());
        // Stopping (or replacing) a session under a minute discards it; say so.
        if ((userStop || before.state === "running") && wasDiscarded(before, at) && statusRef.current !== before) {
          noticeRef.current(DISCARDED);
        }
        return true;
      } catch (e) {
        noticeRef.current(`Couldn't ${what}: ${message(e)}`);
        return false;
      } finally {
        stopping.current = false;
      }
    },
    [apply],
  );

  const start = useCallback(
    (date: string, block: BlockRef, mode: TimerMode, plan: PomodoroPlan | null) =>
      run("start the timer", () => timerStart(date, block, mode, mode === "pomodoro" ? plan : null)),
    [run],
  );
  const stop = useCallback(() => run("stop the timer", timerStop, true), [run]);
  const skipBreak = useCallback(() => run("skip the break", timerSkipBreak), [run]);
  const resolveStale = useCallback(
    (keepRunning: boolean) => run("resolve the timer", () => timerResolveStale(keepRunning)),
    [run],
  );

  return { status, nowMs, finished, start, stop, skipBreak, resolveStale };
}

/** The sessions of one day, reloaded whenever the timer or the day's sidecar changes. */
export function useSessions(date: string, onNotice: (message: string) => void) {
  const [state, setState] = useState<{ date: string; sessions: Session[] } | null>(null);
  const dateRef = useRef(date);
  dateRef.current = date;
  const noticeRef = useRef(onNotice);
  noticeRef.current = onNotice;
  const token = useRef(0);

  const load = useCallback(async (forDate: string) => {
    const mine = ++token.current;
    try {
      const sessions = await timerSessions(forDate);
      if (mine === token.current && forDate === dateRef.current) setState({ date: forDate, sessions });
    } catch (e) {
      if (mine === token.current) noticeRef.current(`Couldn't read the timer sessions: ${message(e)}`);
    }
  }, []);

  useEffect(() => {
    void load(date);
  }, [date, load]);

  useListeners(
    () => [
      onSessionsChanged(({ date: changed }) => {
        if (changed === dateRef.current) void load(changed);
      }),
      onTimerChanged(() => void load(dateRef.current)),
    ],
    [load],
  );

  const remove = useCallback(async (start: string) => {
    const forDate = dateRef.current;
    try {
      const sessions = await timerDeleteSession(forDate, start);
      if (forDate === dateRef.current) setState({ date: forDate, sessions });
    } catch (e) {
      noticeRef.current(`Couldn't delete the session: ${message(e)}`);
    }
  }, []);

  return { sessions: state?.date === date ? state.sessions : [], remove };
}
