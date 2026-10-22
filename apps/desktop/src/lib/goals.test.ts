import { describe, expect, it } from "vitest";
import type { GoalProgress, Project, Tag, TimerStatus } from "../api";
import {
  applyLive,
  buildDraft,
  cellFill,
  fromMinutes,
  goalExtras,
  goalMetaLine,
  goalSubjectText,
  goalSummary,
  goalTitle,
  goalsSummary,
  groupGoals,
  liveFocusMinutes,
  matchesGoal,
  ringFraction,
  stripCells,
  toMinutes,
} from "./goals";

const project = (over: Partial<Project> = {}): Project => ({
  id: 1,
  kind: "series",
  key: "series:rust",
  name: "Learn Rust",
  color: "#3d6fb6",
  calendarName: "Mestrado",
  archived: false,
  ...over,
});
const tag = (id: number, name: string): Tag => ({ id, name, color: "sun", saved: true, goalCount: 1 });

const goal = (over: Partial<GoalProgress> = {}, g: Partial<GoalProgress["goal"]> = {}): GoalProgress => ({
  goal: { id: 1, projectId: null, tagIds: [], title: "Rust", minutesPerDay: 60, start: "2026-10-10", days: 30, archived: false, ...g },
  project: null,
  tags: [],
  status: "active",
  todayMinutes: 0,
  elapsedDays: 12,
  metDays: 9,
  totalMinutes: 600,
  currentStreak: 2,
  longestStreak: 4,
  end: "2026-11-08",
  last14: [
    { date: "2026-10-20", minutes: 60, met: true },
    { date: "2026-10-21", minutes: 40, met: false },
  ],
  ...over,
});

const running = (over: Record<string, unknown> = {}): Extract<TimerStatus, { state: "running" }> => ({
  state: "running",
  date: "2026-10-21",
  block: { start: 540, end: 600, title: "  rust ", tags: ["Study"], project: null },
  mode: "free",
  phase: "focus",
  sessionStart: "2026-10-21T10:00:00.000Z",
  phaseEndsAt: null,
  phaseStart: "2026-10-21T10:00:00.000Z",
  cycle: null,
  cycles: null,
  focusMinutes: null,
  breakMinutes: null,
  ...over,
});
const T0 = new Date("2026-10-21T10:00:00.000Z").getTime();

describe("h+min conversion", () => {
  it("round_trips", () => {
    expect(toMinutes(1, 30)).toBe(90);
    expect(fromMinutes(90)).toEqual({ h: 1, min: 30 });
    expect(fromMinutes(45)).toEqual({ h: 0, min: 45 });
  });
});

describe("buildDraft", () => {
  const ok = { projectId: null, tags: ["rust"], title: "", h: 1, min: 0, start: "2026-10-21", endless: false, days: 30 };
  it("builds_a_tag_draft_with_null_title", () => {
    expect(buildDraft(ok)).toEqual({
      draft: { projectId: null, tagNames: ["rust"], title: null, minutesPerDay: 60, start: "2026-10-21", days: 30 },
    });
  });
  it("accepts_only_a_project", () => {
    const r = buildDraft({ ...ok, projectId: 3, tags: [] });
    expect("draft" in r && r.draft.projectId).toBe(3);
  });
  it("accepts_only_a_title_trimmed", () => {
    const r = buildDraft({ ...ok, tags: [], title: "  Rust " });
    expect("draft" in r && r.draft.title).toBe("Rust");
  });
  it("cleans_tag_names", () => {
    const r = buildDraft({ ...ok, tags: ["#Rust", " rust ", "#", "Go"] });
    expect("draft" in r && r.draft.tagNames).toEqual(["Rust", "Go"]);
  });
  it("rejects_no_project_tag_or_title", () => {
    const msg = "Pick a project, a tag or a title";
    expect(buildDraft({ ...ok, tags: [] })).toEqual({ error: msg });
    expect(buildDraft({ ...ok, tags: ["#"], title: "   " })).toEqual({ error: msg });
  });
  it("endless_means_null_days", () => {
    const r = buildDraft({ ...ok, endless: true });
    expect("draft" in r && r.draft.days).toBeNull();
  });
  it("rejects_minutes_out_of_range", () => {
    const msg = "Minutes per day must be 5–720";
    expect(buildDraft({ ...ok, h: 0, min: 4 })).toEqual({ error: msg });
    expect(buildDraft({ ...ok, h: 12, min: 1 })).toEqual({ error: msg });
    expect("draft" in buildDraft({ ...ok, h: 12, min: 0 })).toBe(true);
    expect("draft" in buildDraft({ ...ok, h: 0, min: 5 })).toBe(true);
  });
  it("rejects_bad_days_and_start", () => {
    expect(buildDraft({ ...ok, days: 0 })).toEqual({ error: "Days must be at least 1" });
    expect(buildDraft({ ...ok, days: 1.5 })).toEqual({ error: "Days must be at least 1" });
    expect(buildDraft({ ...ok, days: Number.NaN })).toEqual({ error: "Days must be at least 1" });
    expect(buildDraft({ ...ok, start: "" })).toEqual({ error: "Start must be a date" });
    expect(buildDraft({ ...ok, start: "2026-13-40" })).toEqual({ error: "Start must be a date" });
  });
  it("ignores_days_when_endless", () => {
    expect("draft" in buildDraft({ ...ok, endless: true, days: 0 })).toBe(true);
  });
});

describe("matchesGoal", () => {
  const block = (over: Partial<{ title: string; tags: string[]; project: string | null }> = {}) => ({
    title: "Rust book",
    tags: ["Study"],
    project: "series:rust",
    ...over,
  });
  const mestrado = project();
  it("project_filter_compares_the_project_key", () => {
    const p = goal({ project: mestrado }, { projectId: 1, title: null });
    expect(matchesGoal(p, block())).toBe(true);
    expect(matchesGoal(p, block({ project: "series:other" }))).toBe(false);
    expect(matchesGoal(p, block({ project: null }))).toBe(false);
  });
  it("tags_match_any_ignoring_hash_and_case", () => {
    const p = goal({ tags: [tag(1, "rust"), tag(2, "study")] }, { title: null, tagIds: [1, 2] });
    expect(matchesGoal(p, block({ tags: ["#STUDY"] }))).toBe(true);
    expect(matchesGoal(p, block({ tags: ["go"] }))).toBe(false);
    expect(matchesGoal(p, block({ tags: [] }))).toBe(false);
  });
  it("title_is_trimmed_and_case_insensitive", () => {
    const p = goal();
    expect(matchesGoal(p, block({ title: "  RUST " }))).toBe(true);
    expect(matchesGoal(p, block({ title: "Rust book" }))).toBe(false);
  });
  it("a_project_goal_matches_by_the_project_only", () => {
    const p = goal({ project: mestrado, tags: [tag(1, "study")] }, { projectId: 1, tagIds: [1], title: "Rust book" });
    expect(matchesGoal(p, block())).toBe(true);
    expect(matchesGoal(p, block({ tags: ["go"], title: "x" }))).toBe(true);
    expect(matchesGoal(p, block({ tags: [], title: "" }))).toBe(true);
    expect(matchesGoal(p, block({ project: null }))).toBe(false);
    expect(matchesGoal(p, block({ project: "series:other" }))).toBe(false);
  });
  it("tags_and_title_both_hold_without_a_project", () => {
    const p = goal({ tags: [tag(1, "study")] }, { tagIds: [1], title: "Rust book" });
    expect(matchesGoal(p, block())).toBe(true);
    expect(matchesGoal(p, block({ title: "x" }))).toBe(false);
    expect(matchesGoal(p, block({ tags: ["go"] }))).toBe(false);
  });
  it("a_goal_without_any_filter_matches_nothing", () => {
    expect(matchesGoal(goal({}, { title: null }), block())).toBe(false);
  });
  it("a_project_goal_whose_project_is_missing_matches_nothing", () => {
    expect(matchesGoal(goal({ project: null }, { projectId: 9, title: null }), block())).toBe(false);
  });
});

describe("goalSubjectText", () => {
  it("joins_project_tags_and_title", () => {
    const p = goal({ project: project(), tags: [tag(1, "rust"), tag(2, "go")] }, { projectId: 1, tagIds: [1, 2], title: "Book" });
    expect(goalSubjectText(p)).toBe("Learn Rust · #rust #go · Book");
  });
  it("skips_what_is_absent", () => {
    expect(goalSubjectText(goal())).toBe("Rust");
    expect(goalSubjectText(goal({ tags: [tag(1, "rust")] }, { title: null }))).toBe("#rust");
    expect(goalSubjectText(goal({}, { title: null }))).toBe("Goal");
  });
});

describe("liveFocusMinutes", () => {
  it("free_timer_counts_whole_minutes_since_start", () => {
    expect(liveFocusMinutes(running(), T0 + 5 * 60_000 + 59_000)).toBe(5);
  });
  it("zero_when_not_running_or_in_break", () => {
    expect(liveFocusMinutes({ state: "idle" }, T0)).toBe(0);
    // Each focus is saved as its own session when it ends, so a break adds nothing live.
    expect(liveFocusMinutes(running({ mode: "pomodoro", phase: "break", cycle: 1, cycles: 2, focusMinutes: 25 }), T0)).toBe(0);
  });
  it("pomodoro_counts_only_the_current_focus", () => {
    // Earlier cycles are finished sessions, already in the backend numbers.
    const s = running({ mode: "pomodoro", cycle: 2, cycles: 4, focusMinutes: 25, breakMinutes: 5, phaseStart: "2026-10-21T10:30:00.000Z" });
    expect(liveFocusMinutes(s, new Date("2026-10-21T10:40:00.000Z").getTime())).toBe(10);
  });
  it("never_negative", () => {
    expect(liveFocusMinutes(running(), T0 - 10_000)).toBe(0);
  });
});

describe("applyLive", () => {
  const today = "2026-10-21";
  it("adds_live_minutes_to_today", () => {
    const r = applyLive(goal(), 10, today);
    expect(r.todayMinutes).toBe(10);
    expect(r.last14[1].minutes).toBe(50);
    expect(r.metDays).toBe(9);
  });
  it("crossing_target_marks_met_and_bumps_streaks", () => {
    const r = applyLive(goal({ todayMinutes: 40, currentStreak: 4, longestStreak: 4 }), 25, today);
    expect(r.todayMinutes).toBe(65);
    expect(r.metDays).toBe(10);
    expect(r.currentStreak).toBe(5);
    expect(r.longestStreak).toBe(5);
    expect(r.last14[1].met).toBe(true);
  });
  it("already_met_is_not_counted_twice", () => {
    const r = applyLive(goal({ todayMinutes: 70, metDays: 10, last14: [{ date: today, minutes: 70, met: true }] }), 5, today);
    expect(r.metDays).toBe(10);
    expect(r.currentStreak).toBe(2);
  });
  it("noop_when_zero_or_not_active", () => {
    const g = goal();
    expect(applyLive(g, 0, today)).toBe(g);
    const up = goal({ status: "upcoming" });
    expect(applyLive(up, 30, today)).toBe(up);
    const done = goal({ status: "finished" });
    expect(applyLive(done, 30, today)).toBe(done);
  });
  it("adds_a_today_cell_when_missing", () => {
    const r = applyLive(goal({ last14: [] }), 7, today);
    expect(r.last14).toEqual([{ date: today, minutes: 7, met: false }]);
  });
});

describe("ringFraction and cellFill", () => {
  it("clamps_between_0_and_1", () => {
    expect(ringFraction(36, 60)).toBeCloseTo(0.6);
    expect(ringFraction(90, 60)).toBe(1);
    expect(ringFraction(-3, 60)).toBe(0);
    expect(ringFraction(10, 0)).toBe(0);
  });
  it("cell_states", () => {
    expect(cellFill({ date: "d", minutes: 60, met: true }, 60)).toEqual({ state: "met", fill: 1 });
    expect(cellFill({ date: "d", minutes: 30, met: false }, 60)).toEqual({ state: "partial", fill: 0.5 });
    expect(cellFill({ date: "d", minutes: 0, met: false }, 60)).toEqual({ state: "none", fill: 0 });
  });
  it("partial_fill_has_a_visible_floor", () => {
    expect(cellFill({ date: "d", minutes: 1, met: false }, 600).fill).toBeGreaterThanOrEqual(0.1);
  });
});

describe("goalSummary", () => {
  it("active_with_end", () => {
    expect(goalSummary(goal())).toMatch(/^1h a day · day 12 of 30$/);
  });
  it("minutes_and_mixed_targets", () => {
    const p = goal();
    p.goal.minutesPerDay = 30;
    expect(goalSummary(p)).toMatch(/^30m a day/);
    p.goal.minutesPerDay = 90;
    expect(goalSummary(p)).toMatch(/^1h 30m a day/);
  });
  it("indefinite_says_since", () => {
    const p = goal();
    p.goal.days = null;
    p.end = null;
    expect(goalSummary(p)).toMatch(/^1h a day · since .*21|^1h a day · since .*10/);
  });
  it("upcoming_and_finished", () => {
    expect(goalSummary(goal({ status: "upcoming", elapsedDays: 0 }))).toMatch(/^1h a day · starts /);
    expect(goalSummary(goal({ status: "finished", elapsedDays: 30 }))).toMatch(/^1h a day · 30 days · ended /);
  });
});

describe("groupGoals", () => {
  it("splits_by_status_keeping_order_and_sorting_past_by_end_desc", () => {
    const a = goal({ status: "active" });
    const u = goal({ status: "upcoming" });
    const f1 = goal({ status: "finished", end: "2026-09-01" });
    const f2 = goal({ status: "finished", end: "2026-10-01" });
    expect(groupGoals([f1, a, u, f2])).toEqual({ active: [a], upcoming: [u], past: [f2, f1] });
  });
});

describe("goalTitle", () => {
  it("shows the project once, even when the title filter repeats it", () => {
    const p = goal({ project: project({ name: "Teoria da Computação" }) }, { projectId: 1, title: "Teoria da Computação" });
    expect(goalTitle(p)).toBe("Teoria da Computação");
  });
  it("falls back to the title, then to the tags", () => {
    expect(goalTitle(goal({}, { title: "Rust" }))).toBe("Rust");
    expect(goalTitle(goal({ tags: [tag(1, "rust"), tag(2, "go")] }, { title: null, tagIds: [1, 2] }))).toBe("#rust #go");
    expect(goalTitle(goal({}, { title: null }))).toBe("Goal");
  });
});

describe("goalExtras", () => {
  it("lists the title filter only when it differs from the project name", () => {
    const base = { project: project({ name: "Mestrado" }), tags: [tag(1, "rust")] };
    expect(goalExtras(goal(base, { projectId: 1, title: "Mestrado" }))).toEqual({ title: null, tags: ["rust"] });
    expect(goalExtras(goal(base, { projectId: 1, title: " book " }))).toEqual({ title: "book", tags: ["rust"] });
  });
  it("does not repeat what the headline already shows", () => {
    expect(goalExtras(goal({}, { title: "Rust" }))).toEqual({ title: null, tags: [] });
    expect(goalExtras(goal({ tags: [tag(1, "rust")] }, { title: null }))).toEqual({ title: null, tags: [] });
    expect(goalExtras(goal({ tags: [tag(1, "rust")] }, { title: "Book" }))).toEqual({ title: null, tags: ["rust"] });
  });
});

describe("goalMetaLine", () => {
  it("reads like the summary", () => {
    expect(goalMetaLine(goal(), "2026-10-21")).toBe("1h a day · day 12 of 30");
  });
  it("uses relative words for today and tomorrow", () => {
    const open = goal({ elapsedDays: 1 }, { days: null, start: "2026-10-21" });
    expect(goalMetaLine(open, "2026-10-21")).toBe("1h a day · since today");
    const later = goal({ status: "upcoming" }, { start: "2026-10-22" });
    expect(goalMetaLine(later, "2026-10-21")).toBe("1h a day · starts tomorrow");
  });
});

describe("stripCells", () => {
  it("always returns 14 cells ending today, with placeholders before the start", () => {
    const cells = stripCells(
      [
        { date: "2026-10-20", minutes: 60, met: true },
        { date: "2026-10-21", minutes: 10, met: false },
      ],
      "2026-10-20",
      "2026-10-21",
    );
    expect(cells).toHaveLength(14);
    expect(cells[13]).toEqual({ date: "2026-10-21", minutes: 10, met: false, placeholder: false });
    expect(cells[12]).toEqual({ date: "2026-10-20", minutes: 60, met: true, placeholder: false });
    expect(cells.slice(0, 12).every((c) => c.placeholder && c.minutes === 0)).toBe(true);
    expect(cells[0].date).toBe("2026-10-08");
  });
  it("keeps missed days after the start as real, empty cells", () => {
    const cells = stripCells([], "2026-10-01", "2026-10-21");
    expect(cells.every((c) => !c.placeholder && c.minutes === 0 && !c.met)).toBe(true);
  });
});

describe("goalsSummary", () => {
  const g = (today: number, perDay: number, met: number, elapsed: number, cur: number, best: number) =>
    goal({ todayMinutes: today, metDays: met, elapsedDays: elapsed, currentStreak: cur, longestStreak: best }, { minutesPerDay: perDay });

  it("sums today over the goals", () => {
    const s = goalsSummary([g(90, 180, 3, 4, 2, 5), g(0, 120, 1, 4, 1, 3)], 80);
    expect(s.today).toEqual({ value: "1.5/5 hours", sub: "30% of target" });
    expect(s.success).toEqual({ value: "50%", sub: "4 of 8 days met" });
    expect(s.streak).toEqual({ value: "5", sub: "current 2" });
    expect(s.logged).toEqual({ value: "1h 20m", sub: "all focus today" });
  });
  it("uses minutes under an hour and allows more than 100%", () => {
    expect(goalsSummary([g(54, 45, 0, 1, 0, 0)], 0).today).toEqual({ value: "54/45 min", sub: "120% of target" });
  });
  it("has no success rate before a complete day", () => {
    expect(goalsSummary([g(0, 60, 0, 3, 0, 0)], 0).success).toEqual({ value: "—", sub: "no complete days yet" });
  });
  it("copes with no active goals", () => {
    const s = goalsSummary([], 0);
    expect(s.today).toEqual({ value: "0/0 min", sub: "no active goals" });
    expect(s.success).toEqual({ value: "—", sub: "no active goals" });
    expect(s.streak).toEqual({ value: "0", sub: "no active goals" });
    expect(s.logged).toEqual({ value: "0m", sub: "all focus today" });
  });
});
