import { useCallback, useMemo, useState } from "react";
import type { PomodoroPlan, TimerMode } from "../api";
import { clampPlan, loadPlan, planKey, safeStorage, savePlan } from "../lib/focus";

const MODE_KEY = "catenga.timerMode";

function loadMode(): TimerMode {
  try {
    return localStorage.getItem(MODE_KEY) === "pomodoro" ? "pomodoro" : "free";
  } catch {
    return "free";
  }
}

function saveMode(mode: TimerMode) {
  try {
    localStorage.setItem(MODE_KEY, mode);
  } catch {
    // Storage unavailable: the mode just isn't remembered.
  }
}

/**
 * What the next Start will use: the mode (remembered globally) and the Pomodoro plan (remembered per task,
 * by trimmed lowercased title; `defaults` from config.json when the task has none). Shared by the timer bar and the focus screen.
 */
export function useTimerSetup(title: string, defaults: PomodoroPlan) {
  const [mode, setModeState] = useState<TimerMode>(loadMode);
  const [edits, setEdits] = useState<Record<string, PomodoroPlan>>({});
  const key = planKey(title);
  const plan = useMemo(() => edits[key] ?? loadPlan(safeStorage(), title, defaults), [edits, key, title, defaults]);

  const setMode = useCallback((next: TimerMode) => {
    setModeState(next);
    saveMode(next);
  }, []);
  const setPlan = useCallback(
    (next: PomodoroPlan) => {
      const clamped = clampPlan(next, defaults);
      setEdits((e) => ({ ...e, [key]: clamped }));
      savePlan(safeStorage(), title, clamped);
    },
    [defaults, key, title],
  );

  return { mode, setMode, plan, setPlan };
}
