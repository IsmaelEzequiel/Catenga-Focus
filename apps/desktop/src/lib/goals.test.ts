import { describe, expect, it } from "vitest";
import type { GoalProgress, Project, Tag, TimerStatus } from "../api";
import {
  applyLive,
  buildDraft,
  cellFill,
  fromMinutes,
  goalSubjectText,
  goalSummary,
  groupGoals,
  liveFocusMinutes,
  matchesGoal,
  ringFraction,
  toMinutes,
} from "./goals";

const project = (over: Partial<Project> = {}): Project => ({
  id: 1,
  googleCalendarId: "mestrado@group.calendar.google.com",
  name: "Mestrado",
  color: "#3d6fb6",
  showOnTimeline: true,
  isPrimary: false,
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
    project: "mestrado@group.calendar.google.com",
    ...over,
  });
  const mestrado = project();
  it("project_filter_compares_the_google_calendar_id", () => {
    const p = goal({ project: mestrado }, { projectId: 1, title: null });
    expect(matchesGoal(p, block())).toBe(true);
    expect(matchesGoal(p, block({ project: "other" }))).toBe(false);
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
  it("all_present_filters_must_hold", () => {
    const p = goal({ project: mestrado, tags: [tag(1, "study")] }, { projectId: 1, tagIds: [1], title: null });
    expect(matchesGoal(p, block())).toBe(true);
    expect(matchesGoal(p, block({ tags: ["go"] }))).toBe(false);
    expect(matchesGoal(p, block({ project: null }))).toBe(false);
    const withTitle = goal({ project: mestrado, tags: [tag(1, "study")] }, { projectId: 1, tagIds: [1], title: "Rust book" });
    expect(matchesGoal(withTitle, block())).toBe(true);
    expect(matchesGoal(withTitle, block({ title: "x" }))).toBe(false);
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
    expect(goalSubjectText(p)).toBe("Mestrado · #rust #go · Book");
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
