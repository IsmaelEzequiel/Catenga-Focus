import { useCallback, useEffect, useRef, useState } from "react";
import { dashboard, onIndexStatus, onSessionsChanged, onTimerChanged } from "../api";

/** Finished focus minutes logged today across all sessions (not a sum of goals); refreshed like the goals list. */
export function useLoggedToday(today: string): number {
  const [minutes, setMinutes] = useState(0);
  const token = useRef(0);

  const load = useCallback(async () => {
    const mine = ++token.current;
    try {
      const d = await dashboard("week");
      if (mine !== token.current) return;
      const last = d.calendar[d.calendar.length - 1];
      setMinutes(last && last.date === today ? last.minutes : 0);
    } catch {
      // Keep the last number; the goals list reports its own errors.
    }
  }, [today]);

  useEffect(() => {
    void load();
    let cancelled = false;
    const unlisten: (() => void)[] = [];
    const keep = (p: Promise<() => void>) =>
      p.then(
        (fn) => (cancelled ? fn() : unlisten.push(fn)),
        () => {}, // no event bridge (tests, plain browser)
      );
    keep(onSessionsChanged(() => void load()));
    keep(onTimerChanged(() => void load()));
    keep(onIndexStatus((s) => s.state === "ready" && void load()));
    return () => {
      cancelled = true;
      unlisten.forEach((fn) => fn());
    };
  }, [load]);

  return minutes;
}
