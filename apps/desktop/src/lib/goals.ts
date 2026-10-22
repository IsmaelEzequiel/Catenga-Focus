/** Pure helpers for goals (see `docs/plans/M8-goals.md` §1.4 and `docs/plans/M8b-projects.md` §1.4-§1.5). */
import type { BlockRef, GoalDay, GoalDraft, GoalProgress, TimerStatus } from "../api";
import { fmtDuration } from "./dashboard";
import { cleanTagNames } from "./tags";

export const MIN_PER_DAY = 5;
export const MAX_PER_DAY = 720;

export const toMinutes = (h: number, min: number) => h * 60 + min;
export const fromMinutes = (minutes: number) => ({ h: Math.floor(minutes / 60), min: minutes % 60 });

const normTag = (s: string) => s.trim().replace(/^#+/, "").trim().toLowerCase();
const normTitle = (s: string) => s.trim().toLowerCase();

export interface DraftInput {
  projectId: number | null;
  /** Tag names, with or without `#`. */
  tags: string[];
  title: string;
  h: number;
  min: number;
  start: string;
  endless: boolean;
  days: number;
}

const isDate = (s: string) => {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  if (!m) return false;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const dt = new Date(y, mo - 1, d);
  return dt.getFullYear() === y && dt.getMonth() === mo - 1 && dt.getDate() === d;
};

/** Form values to a draft, or the same plain-text error the backend would give. */
export function buildDraft(input: DraftInput): { draft: GoalDraft } | { error: string } {
  const tagNames = cleanTagNames(input.tags);
  const title = input.title.trim();
  if (input.projectId === null && tagNames.length === 0 && title === "") return { error: "Pick a project, a tag or a title" };
  const minutes = toMinutes(input.h, input.min);
  if (!Number.isInteger(minutes) || minutes < MIN_PER_DAY || minutes > MAX_PER_DAY) {
    return { error: `Minutes per day must be ${MIN_PER_DAY}–${MAX_PER_DAY}` };
  }
  if (!input.endless && (!Number.isInteger(input.days) || input.days < 1)) return { error: "Days must be at least 1" };
  if (!isDate(input.start)) return { error: "Start must be a date" };
  return {
    draft: {
      projectId: input.projectId,
      tagNames,
      title: title === "" ? null : title,
      minutesPerDay: minutes,
      start: input.start,
      days: input.endless ? null : input.days,
    },
  };
}

/**
 * Same matching as the backend: every filter the goal has must hold. The project compares Google calendar ids,
 * the tags match if the block has any of them (lowercase, no `#`), the title is trimmed and case-insensitive.
 */
export function matchesGoal(p: Pick<GoalProgress, "goal" | "project" | "tags">, block: Pick<BlockRef, "title" | "tags" | "project">): boolean {
  const { goal } = p;
  const hasTags = goal.tagIds.length > 0 || p.tags.length > 0;
  const hasTitle = goal.title !== null && goal.title.trim() !== "";
  if (goal.projectId === null && !hasTags && !hasTitle) return false;
  if (goal.projectId !== null && (p.project === null || block.project !== p.project.googleCalendarId)) return false;
  if (hasTags) {
    const have = new Set(block.tags.map(normTag));
    if (!p.tags.some((t) => have.has(normTag(t.name)))) return false;
  }
  if (hasTitle && normTitle(block.title) !== normTitle(goal.title ?? "")) return false;
  return true;
}

/** `Mestrado · #rust #go · Book`, for labels and tooltips. */
export function goalSubjectText(p: GoalProgress): string {
  const parts: string[] = [];
  if (p.project) parts.push(p.project.name);
  if (p.tags.length > 0) parts.push(p.tags.map((t) => `#${t.name}`).join(" "));
  if (p.goal.title) parts.push(p.goal.title);
  return parts.length > 0 ? parts.join(" · ") : "Goal";
}

/** Focus minutes of the running session so far (finished sessions are already in the backend numbers). */
export function liveFocusMinutes(status: TimerStatus, nowMs: number): number {
  // Each Pomodoro focus is saved as its own session when it ends, so only the current focus is live.
  if (status.state !== "running" || status.phase === "break") return 0;
  const minutes = Math.max(0, Math.floor((nowMs - new Date(status.phaseStart).getTime()) / 60_000));
  return status.focusMinutes ? Math.min(minutes, status.focusMinutes) : minutes;
}

/** Adds running-timer minutes to today's numbers (active goals only). Returns the same object when nothing changes. */
export function applyLive(p: GoalProgress, live: number, today: string): GoalProgress {
  if (live <= 0 || p.status !== "active") return p;
  const target = p.goal.minutesPerDay;
  const todayMinutes = p.todayMinutes + live;
  const newlyMet = p.todayMinutes < target && todayMinutes >= target;
  const hasToday = p.last14.some((d) => d.date === today);
  const last14: GoalDay[] = hasToday
    ? p.last14.map((d) => (d.date === today ? { ...d, minutes: d.minutes + live, met: d.minutes + live >= target } : d))
    : [...p.last14, { date: today, minutes: live, met: live >= target }];
  const currentStreak = newlyMet ? p.currentStreak + 1 : p.currentStreak;
  return {
    ...p,
    todayMinutes,
    totalMinutes: p.totalMinutes + live,
    metDays: newlyMet ? p.metDays + 1 : p.metDays,
    currentStreak,
    longestStreak: Math.max(p.longestStreak, currentStreak),
    last14,
  };
}

export const ringFraction = (minutes: number, target: number) =>
  target <= 0 ? 0 : Math.min(1, Math.max(0, minutes / target));

const MIN_VISIBLE_FILL = 0.1;

/** Strip cell: met = filled, partial = bottom-up fill, none = empty. */
export function cellFill(d: GoalDay, target: number): { state: "met" | "partial" | "none"; fill: number } {
  if (d.met) return { state: "met", fill: 1 };
  if (d.minutes <= 0) return { state: "none", fill: 0 };
  return { state: "partial", fill: Math.max(MIN_VISIBLE_FILL, Math.min(0.95, ringFraction(d.minutes, target))) };
}

const fmtShort = (date: string) => {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, { day: "numeric", month: "short" });
};

/** `1h a day · day 12 of 30`, `… · since 21 Oct`, `… · starts 22 Oct`, `… · 30 days · ended 20 Nov`. */
export function goalSummary(p: GoalProgress): string {
  const { goal } = p;
  const base = `${fmtDuration(goal.minutesPerDay)} a day`;
  if (p.status === "upcoming") return `${base} · starts ${fmtShort(goal.start)}`;
  if (goal.days === null) return `${base} · since ${fmtShort(goal.start)}`;
  if (p.status === "finished") return `${base} · ${goal.days} ${goal.days === 1 ? "day" : "days"} · ended ${fmtShort(p.end ?? goal.start)}`;
  return `${base} · day ${p.elapsedDays} of ${goal.days}`;
}

export function groupGoals(list: GoalProgress[]): { active: GoalProgress[]; upcoming: GoalProgress[]; past: GoalProgress[] } {
  return {
    active: list.filter((g) => g.status === "active"),
    upcoming: list.filter((g) => g.status === "upcoming"),
    past: list.filter((g) => g.status === "finished").sort((a, b) => (b.end ?? "").localeCompare(a.end ?? "")),
  };
}
