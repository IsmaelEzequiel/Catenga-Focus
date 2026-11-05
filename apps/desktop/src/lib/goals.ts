/** Pure helpers for goals (see `docs/plans/M8-goals.md` §1.4 and `docs/plans/M8b-projects.md` §1.4-§1.5). */
import type { BlockRef, Goal, GoalDay, GoalDraft, GoalKind, GoalProgress, TimerStatus } from "../api";
import { fmtDuration } from "./dashboard";
import { cleanTagNames } from "./tags";
import { addDays } from "./time";

export const MIN_PER_DAY = 5;
export const MAX_PER_DAY = 720;
export const MAX_COUNT = 100_000;
const MAX_NAME = 80;
/** `minutesPerDay` sent for a count goal (the backend ignores it). */
const COUNT_PLACEHOLDER_MINUTES = 60;

/** The daily target in the goal's own unit: minutes, or the count. */
export const goalTarget = (goal: Pick<Goal, "kind" | "minutesPerDay" | "targetCount">): number =>
  goal.kind === "count" ? Math.max(1, goal.targetCount ?? 1) : goal.minutesPerDay;

/** `5 pages`, `1 page`… a plain number when the goal has no unit. */
export function fmtCount(n: number, unit: string | null): string {
  const u = unit?.trim();
  if (!u) return String(n);
  // Crude singular for the common English case ("1 pages" reads wrong); other units stay as typed.
  const one = n === 1 && u.length > 1 && u.endsWith("s") && !u.endsWith("ss") ? u.slice(0, -1) : u;
  return `${n} ${one}`;
}

/** A day's amount in the goal's unit: `45 min` / `5 pages`. */
export const fmtGoalAmount = (goal: Pick<Goal, "kind" | "unit">, n: number): string =>
  goal.kind === "count" ? fmtCount(n, goal.unit) : fmtDuration(n);

export const toMinutes = (h: number, min: number) => h * 60 + min;
export const fromMinutes = (minutes: number) => ({ h: Math.floor(minutes / 60), min: minutes % 60 });

const normTag = (s: string) => s.trim().replace(/^#+/, "").trim().toLowerCase();
const normTitle = (s: string) => s.trim().toLowerCase();

export interface DraftInput {
  kind: GoalKind;
  name: string;
  /** Count goals. */
  count: number;
  unit: string;
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
  const name = input.name.trim();
  if ([...name].length > MAX_NAME) return { error: `Name can be at most ${MAX_NAME} characters` };
  const span = () => {
    if (!input.endless && (!Number.isInteger(input.days) || input.days < 1)) return "Days must be at least 1";
    if (!isDate(input.start)) return "Start must be a date";
    return null;
  };
  if (input.kind === "count") {
    if (name === "") return { error: "Give the goal a name" };
    if (!Number.isInteger(input.count) || input.count < 1 || input.count > MAX_COUNT) {
      return { error: `Amount per day must be 1–${MAX_COUNT}` };
    }
    const err = span();
    if (err) return { error: err };
    const unit = input.unit.trim();
    return {
      draft: {
        kind: "count",
        name,
        projectId: null,
        tagNames: [],
        title: null,
        minutesPerDay: COUNT_PLACEHOLDER_MINUTES,
        targetCount: input.count,
        unit: unit === "" ? null : unit,
        start: input.start,
        days: input.endless ? null : input.days,
      },
    };
  }
  const tagNames = cleanTagNames(input.tags);
  const title = input.title.trim();
  if (input.projectId === null && tagNames.length === 0 && title === "" && name === "") {
    return { error: "Give the goal a name, a project or a tag" };
  }
  const minutes = toMinutes(input.h, input.min);
  if (!Number.isInteger(minutes) || minutes < MIN_PER_DAY || minutes > MAX_PER_DAY) {
    return { error: `Minutes per day must be ${MIN_PER_DAY}–${MAX_PER_DAY}` };
  }
  const err = span();
  if (err) return { error: err };
  return {
    draft: {
      kind: "time",
      name: name === "" ? null : name,
      projectId: input.projectId,
      tagNames,
      title: title === "" ? null : title,
      minutesPerDay: minutes,
      targetCount: null,
      unit: null,
      start: input.start,
      days: input.endless ? null : input.days,
    },
  };
}

/**
 * Same matching as the backend. A goal with a project matches by the project only (project wins; compares project keys
 * `series:…` / `event:…`). Otherwise tags (any, lowercase, no `#`) AND the title (trimmed, case-insensitive) must hold;
 * a goal with only a name counts blocks with that title. Count goals never match blocks (they are checked in by hand).
 */
export function matchesGoal(p: Pick<GoalProgress, "goal" | "project" | "tags">, block: Pick<BlockRef, "title" | "tags" | "project">): boolean {
  const { goal } = p;
  if (goal.kind === "count") return false;
  const hasTags = goal.tagIds.length > 0 || p.tags.length > 0;
  const titleFilter = goal.title?.trim() || (!hasTags ? goal.name?.trim() : "") || "";
  const hasTitle = titleFilter !== "";
  if (goal.projectId === null && !hasTags && !hasTitle) return false;
  if (goal.projectId !== null) return p.project !== null && block.project === p.project.key;
  if (hasTags) {
    const have = new Set(block.tags.map(normTag));
    if (!p.tags.some((t) => have.has(normTag(t.name)))) return false;
  }
  if (hasTitle && normTitle(block.title) !== normTitle(titleFilter)) return false;
  return true;
}

/** `Mestrado · #rust #go · Book`, for labels and tooltips. */
export function goalSubjectText(p: GoalProgress): string {
  const parts: string[] = [];
  if (p.goal.name) parts.push(p.goal.name);
  if (p.project && p.project.name !== p.goal.name) parts.push(p.project.name);
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

/** A day's share of the target in thousandths, capped at 1000 (same as the backend's score). */
export const dayScore = (amount: number, target: number): number =>
  target <= 0 ? 0 : Math.min(1000, Math.floor((Math.max(0, amount) * 1000) / target));

/**
 * Adds running-timer minutes to today's numbers (active time goals only). The first live minute already counts for the
 * streak (any progress keeps it). Returns the same object when nothing changes.
 */
export function applyLive(p: GoalProgress, live: number, today: string): GoalProgress {
  if (live <= 0 || p.status !== "active" || p.goal.kind === "count") return p;
  const target = goalTarget(p.goal);
  const todayMinutes = p.todayMinutes + live;
  const newlyMet = p.todayMinutes < target && todayMinutes >= target;
  const newlyDone = p.todayMinutes <= 0;
  const hasToday = p.last14.some((d) => d.date === today);
  const last14: GoalDay[] = hasToday
    ? p.last14.map((d) => (d.date === today ? { ...d, minutes: d.minutes + live, met: d.minutes + live >= target } : d))
    : [...p.last14, { date: today, minutes: live, met: live >= target }];
  const currentStreak = newlyDone ? p.currentStreak + 1 : p.currentStreak;
  return {
    ...p,
    todayMinutes,
    totalMinutes: p.totalMinutes + live,
    metDays: newlyMet ? p.metDays + 1 : p.metDays,
    doneDays: newlyDone ? p.doneDays + 1 : p.doneDays,
    score: p.score - dayScore(p.todayMinutes, target) + dayScore(todayMinutes, target),
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

/** `1h a day · day 12 of 30`, `… · since 21 Oct`, `… · starts 22 Oct`, `… · 30 days · ended 20 Nov`; with `today`, "today" / "tomorrow" replace the date. */
export function goalSummary(p: GoalProgress, today?: string): string {
  const { goal } = p;
  const base = `${fmtGoalAmount(goal, goalTarget(goal))} a day`;
  const when = (date: string) =>
    today && date === today ? "today" : today && date === addDays(today, 1) ? "tomorrow" : fmtShort(date);
  if (p.status === "upcoming") return `${base} · starts ${when(goal.start)}`;
  if (goal.days === null) return `${base} · since ${when(goal.start)}`;
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

/** The one subject a card is titled with: its name, else the project, else the title filter, else the tags (`#a #b`). */
export function goalTitle(p: GoalProgress): string {
  const name = p.goal.name?.trim();
  if (name) return name;
  if (p.project) return p.project.name;
  const title = p.goal.title?.trim();
  if (title) return title;
  if (p.tags.length > 0) return p.tags.map((t) => `#${t.name}`).join(" ");
  return "Goal";
}

/**
 * What the card shows besides its headline: the project (when the name is the headline), the title filter (when it is
 * not the headline) and tag names (unless they are the headline).
 */
export function goalExtras(p: GoalProgress): { project: string | null; title: string | null; tags: string[] } {
  const headline = goalTitle(p);
  const title = p.goal.title?.trim() || null;
  const differs = (s: string | null) => (s !== null && normTitle(s) !== normTitle(headline) ? s : null);
  const tagsHeadline = p.tags.length > 0 && headline === p.tags.map((t) => `#${t.name}`).join(" ");
  return {
    project: differs(p.project?.name ?? null),
    title: differs(title),
    tags: tagsHeadline ? [] : p.tags.map((t) => t.name),
  };
}

/** `1h a day · day 2 of 30`, `1h a day · since today`. */
export const goalMetaLine = (p: GoalProgress, today: string): string => goalSummary(p, today);

export interface StripCell {
  date: string;
  minutes: number;
  met: boolean;
  /** Before the goal's start: drawn faint, not as a missed day. */
  placeholder: boolean;
}

export const STRIP_DAYS = 14;

/** Exactly 14 cells, oldest first, the last one `today`; days the backend did not report count as empty. */
export function stripCells(last14: GoalDay[], start: string, today: string): StripCell[] {
  const byDate = new Map(last14.map((d) => [d.date, d]));
  return Array.from({ length: STRIP_DAYS }, (_, i) => {
    const date = addDays(today, i - (STRIP_DAYS - 1));
    const d = byDate.get(date);
    return { date, minutes: d?.minutes ?? 0, met: d?.met ?? false, placeholder: date < start };
  });
}

export interface Kpi {
  value: string;
  sub: string;
}

/** Hours with up to one decimal ("1.5", "5"), or minutes when the target is under an hour. */
const fmtAmount = (minutes: number, unit: "hours" | "min") =>
  unit === "min" ? String(Math.round(minutes)) : String(Math.round((minutes / 60) * 10) / 10);

/**
 * Header numbers over the ACTIVE goals (`active` already has the running timer applied);
 * `logged` = all focus minutes today (every session, plus the live one).
 */
export function goalsSummary(active: GoalProgress[], logged: number): { today: Kpi; success: Kpi; streak: Kpi; logged: Kpi } {
  const timed = active.filter((p) => p.goal.kind !== "count");
  const done = timed.reduce((n, p) => n + p.todayMinutes, 0);
  const target = timed.reduce((n, p) => n + p.goal.minutesPerDay, 0);
  const met = active.reduce((n, p) => n + p.metDays, 0);
  const elapsed = active.reduce((n, p) => n + p.elapsedDays, 0);
  // Partial days count for their share: 5 of 10 pages is half a day.
  const score = active.reduce((n, p) => n + p.score, 0);
  const best = Math.max(0, ...active.map((p) => p.longestStreak));
  const current = Math.max(0, ...active.map((p) => p.currentStreak));
  const none = active.length === 0;
  const unit = target < 60 ? "min" : "hours";
  const metToday = active.filter((p) => p.todayMinutes >= goalTarget(p.goal)).length;
  return {
    today:
      timed.length === 0 && !none
        ? { value: `${metToday}/${active.length} goals`, sub: "met today" }
        : {
            value: `${fmtAmount(done, unit)}/${fmtAmount(target, unit)} ${unit}`,
            sub: none ? "no active goals" : `${Math.round((done / target) * 100)}% of target`,
          },
    success:
      score === 0
        ? { value: "—", sub: none ? "no active goals" : "nothing done yet" }
        : { value: `${Math.round(score / (elapsed * 10))}%`, sub: `${met} of ${elapsed} days met` },
    streak: { value: String(best), sub: none ? "no active goals" : `current ${current}` },
    logged: { value: fmtDuration(logged), sub: "all focus today" },
  };
}
