/** Pure helpers for Pomodoro plans and the focus screen (see `docs/plans/M6b-focus.md` §1.2-§1.3). */
import type { PomodoroPlan, TimerStatus } from "../api";

type Running = Extract<TimerStatus, { state: "running" }>;

export type { PomodoroPlan };

export const PLAN_LIMITS = {
  focusMinutes: { min: 1, max: 180 },
  breakMinutes: { min: 1, max: 60 },
  cycles: { min: 1, max: 12 },
} as const;

const FALLBACK: PomodoroPlan = { focusMinutes: 25, breakMinutes: 5, cycles: 4 };

function clampField(value: unknown, field: keyof PomodoroPlan, fallback: number): number {
  const { min, max } = PLAN_LIMITS[field];
  const n = typeof value === "number" && Number.isFinite(value) ? Math.round(value) : fallback;
  return Math.min(max, Math.max(min, n));
}

/** Rounds and clamps every field to the backend limits; missing or non-finite ones take the fallback. */
export function clampPlan(plan: Partial<PomodoroPlan>, fallback: PomodoroPlan): PomodoroPlan {
  return {
    focusMinutes: clampField(plan.focusMinutes, "focusMinutes", fallback.focusMinutes),
    breakMinutes: clampField(plan.breakMinutes, "breakMinutes", fallback.breakMinutes),
    cycles: clampField(plan.cycles, "cycles", fallback.cycles),
  };
}

interface ConfigPlan {
  pomodoroFocusMinutes?: number;
  pomodoroBreakMinutes?: number;
  pomodoroCycles?: number;
}

/** The plan from `config.json` (25 / 5 / 4 when absent). */
export function defaultPlan(config: ConfigPlan | null | undefined): PomodoroPlan {
  return clampPlan(
    {
      focusMinutes: config?.pomodoroFocusMinutes,
      breakMinutes: config?.pomodoroBreakMinutes,
      cycles: config?.pomodoroCycles,
    },
    FALLBACK,
  );
}

/** Storage key of the plan remembered for a task (trimmed, lowercased title). */
export const planKey = (title: string) => `catenga.plan.${title.trim().toLowerCase()}`;

type PlanStorage = Pick<Storage, "getItem" | "setItem"> | null;

/** The plan last used for this title, or `defaults`. Never throws. */
export function loadPlan(storage: PlanStorage, title: string, defaults: PomodoroPlan): PomodoroPlan {
  try {
    const raw = storage?.getItem(planKey(title));
    if (!raw) return defaults;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return defaults;
    return clampPlan(parsed as Partial<PomodoroPlan>, defaults);
  } catch {
    return defaults;
  }
}

/** Remembers the plan for this title. Never throws. */
export function savePlan(storage: PlanStorage, title: string, plan: PomodoroPlan): void {
  try {
    storage?.setItem(planKey(title), JSON.stringify(plan));
  } catch {
    // Storage unavailable: the plan just isn't remembered.
  }
}

/** `localStorage`, or null when the browser refuses it. */
export function safeStorage(): Storage | null {
  try {
    return localStorage;
  } catch {
    return null;
  }
}

/** A whole number typed into a stepper; null when it isn't one. */
export function parseCount(text: string): number | null {
  const t = text.trim();
  return /^\d+$/.test(t) ? Number(t) : null;
}

const MIN_SESSION_MS = 60_000;

/** Whether stopping now throws the session away (same 60 s rule as the backend). */
export function wasDiscarded(status: TimerStatus, nowMs: number): boolean {
  if (status.state !== "running") return false;
  if (status.mode === "pomodoro" && status.phase !== "focus") return false;
  return nowMs - new Date(status.sessionStart).getTime() < MIN_SESSION_MS;
}

const HOUR_MS = 3_600_000;

/** Ring fill, 0..1: the share of the current Pomodoro phase, or the share of the hour for a free timer. */
export function ringProgress(status: TimerStatus, nowMs: number): number {
  if (status.state !== "running") return 0;
  if (status.mode === "pomodoro") {
    if (!status.phaseEndsAt) return 0;
    const start = new Date(status.phaseStart).getTime();
    const span = new Date(status.phaseEndsAt).getTime() - start;
    if (!(span > 0)) return 0;
    return Math.min(1, Math.max(0, (nowMs - start) / span));
  }
  const elapsed = Math.max(0, nowMs - new Date(status.sessionStart).getTime());
  return (elapsed % HOUR_MS) / HOUR_MS;
}

/** Which characters of `next` differ from `prev`, aligned to the right (so a longer string marks its new leading chars). */
export function digitDiff(prev: string | null, next: string): boolean[] {
  return [...next].map((ch, i) => {
    if (prev === null) return false;
    const j = prev.length - (next.length - i);
    if (j < 0) return true;
    return prev[j] !== ch;
  });
}

export type DotState = "done" | "current" | "upcoming";

/** One dot per Pomodoro of the run. While idle, `plannedCycles` of them, all upcoming. */
export function cycleDots(status: TimerStatus, plannedCycles = 0): DotState[] {
  if (status.state !== "running") return Array.from({ length: plannedCycles }, () => "upcoming");
  const { cycle, cycles } = status;
  if (status.mode !== "pomodoro" || cycle === null || cycles === null) return [];
  return Array.from({ length: cycles }, (_, i): DotState => {
    const n = i + 1;
    if (status.phase === "break") return n <= cycle ? "done" : "upcoming";
    return n < cycle ? "done" : n === cycle ? "current" : "upcoming";
  });
}

/** The number of pomodoros when a run just ended by itself (last break over or skipped); otherwise null. */
export function runFinished(prev: TimerStatus, next: TimerStatus, userStopped: boolean): number | null {
  if (userStopped || next.state !== "idle" || prev.state !== "running") return null;
  const { mode, phase, cycle, cycles } = prev as Running;
  if (mode !== "pomodoro" || phase !== "break" || cycle === null || cycles === null || cycle < cycles) return null;
  return cycles;
}

export const finishedMessage = (cycles: number) => `${cycles} pomodoro${cycles === 1 ? "" : "s"} done`;
