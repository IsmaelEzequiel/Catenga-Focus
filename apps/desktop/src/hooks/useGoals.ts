import { useCallback, useEffect, useRef, useState } from "react";
import {
  goalCreate,
  goalDelete,
  goalSetArchived,
  goalUpdate,
  goals,
  goalsArchived,
  onGoalsChanged,
  onIndexStatus,
  onSessionsChanged,
  onTimerChanged,
  type GoalDraft,
  type GoalProgress,
} from "../api";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Goals (active/upcoming/past and archived), refreshed on every event that can change their numbers. */
export function useGoals() {
  const [list, setList] = useState<GoalProgress[] | null>(null);
  const [archived, setArchived] = useState<GoalProgress[]>([]);
  const [error, setError] = useState<string | null>(null);
  const token = useRef(0);

  const load = useCallback(async () => {
    const mine = ++token.current;
    try {
      const [a, b] = await Promise.all([goals(), goalsArchived()]);
      if (mine !== token.current) return;
      setList(a);
      setArchived(b);
      setError(null);
    } catch (e) {
      if (mine === token.current) setError(message(e));
    }
  }, []);

  useEffect(() => {
    void load();
    let cancelled = false;
    const unlisten: (() => void)[] = [];
    const keep = (p: Promise<() => void>) =>
      p.then(
        (fn) => (cancelled ? fn() : unlisten.push(fn)),
        () => {}, // no live refresh; reopening the view still loads
      );
    keep(onGoalsChanged(() => void load()));
    keep(onSessionsChanged(() => void load()));
    keep(onTimerChanged(() => void load()));
    keep(onIndexStatus((s) => s.state === "ready" && void load()));
    return () => {
      cancelled = true;
      unlisten.forEach((fn) => fn());
    };
  }, [load]);

  /** Runs a write; the command returns the fresh active list, archived is reloaded. Resolves to an error text or null. */
  const write = useCallback(
    async (call: () => Promise<GoalProgress[]>): Promise<string | null> => {
      try {
        const next = await call();
        token.current++;
        setList(next);
        setError(null);
        void goalsArchived().then(setArchived, () => {});
        return null;
      } catch (e) {
        return message(e);
      }
    },
    [],
  );

  return {
    list,
    archived,
    error,
    create: (d: GoalDraft) => write(() => goalCreate(d)),
    update: (id: number, d: GoalDraft) => write(() => goalUpdate(id, d)),
    setArchived: (id: number, v: boolean) => write(() => goalSetArchived(id, v)),
    remove: (id: number) => write(() => goalDelete(id)),
  };
}
