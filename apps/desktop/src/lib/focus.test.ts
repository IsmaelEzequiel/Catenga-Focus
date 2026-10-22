import { describe, expect, it } from "vitest";
import type { TimerStatus } from "../api";
import {
  clampPlan,
  cycleDots,
  defaultPlan,
  digitDiff,
  finishedMessage,
  loadPlan,
  parseCount,
  planKey,
  ringProgress,
  runFinished,
  savePlan,
  wasDiscarded,
} from "./focus";

type Running = Extract<TimerStatus, { state: "running" }>;

const T0 = Date.UTC(2026, 9, 21, 12, 0, 0);
const iso = (ms: number) => new Date(ms).toISOString();
const block = { start: 540, end: 630, title: "Deep work", tags: [], project: null };

const running = (over: Partial<Running> = {}): Running => ({
  state: "running",
  date: "2026-10-21",
  block,
  mode: "pomodoro",
  phase: "focus",
  sessionStart: iso(T0),
  phaseStart: iso(T0),
  phaseEndsAt: iso(T0 + 25 * 60_000),
  cycle: 1,
  cycles: 4,
  focusMinutes: 25,
  breakMinutes: 5,
  ...over,
});

const free = (over: Partial<Running> = {}): Running =>
  running({ mode: "free", phaseEndsAt: null, cycle: null, cycles: null, focusMinutes: null, breakMinutes: null, ...over });

const store = (initial: Record<string, string> = {}) => {
  const data = { ...initial };
  return {
    data,
    getItem: (k: string) => data[k] ?? null,
    setItem: (k: string, v: string) => {
      data[k] = v;
    },
  };
};

describe("clampPlan", () => {
  const fallback = { focusMinutes: 25, breakMinutes: 5, cycles: 4 };
  it("keeps_values_in_range", () => {
    expect(clampPlan({ focusMinutes: 50, breakMinutes: 10, cycles: 3 }, fallback)).toEqual({ focusMinutes: 50, breakMinutes: 10, cycles: 3 });
  });
  it("clamps_to_the_limits", () => {
    expect(clampPlan({ focusMinutes: 0, breakMinutes: 999, cycles: 50 }, fallback)).toEqual({ focusMinutes: 1, breakMinutes: 60, cycles: 12 });
    expect(clampPlan({ focusMinutes: 500, breakMinutes: -3, cycles: -1 }, fallback)).toEqual({ focusMinutes: 180, breakMinutes: 1, cycles: 1 });
  });
  it("rounds_and_replaces_missing_or_non_finite_with_the_fallback", () => {
    expect(clampPlan({ focusMinutes: 24.6, breakMinutes: Number.NaN }, fallback)).toEqual({ focusMinutes: 25, breakMinutes: 5, cycles: 4 });
  });
});

describe("defaultPlan", () => {
  it("uses_config_values", () => {
    expect(defaultPlan({ pomodoroFocusMinutes: 50, pomodoroBreakMinutes: 10, pomodoroCycles: 2 })).toEqual({
      focusMinutes: 50,
      breakMinutes: 10,
      cycles: 2,
    });
  });
  it("falls_back_to_25_5_4_and_clamps", () => {
    expect(defaultPlan(null)).toEqual({ focusMinutes: 25, breakMinutes: 5, cycles: 4 });
    expect(defaultPlan({ pomodoroFocusMinutes: 400 })).toEqual({ focusMinutes: 180, breakMinutes: 5, cycles: 4 });
  });
});

describe("planKey / loadPlan / savePlan", () => {
  const defaults = { focusMinutes: 25, breakMinutes: 5, cycles: 4 };
  it("key_is_trimmed_and_lowercased", () => {
    expect(planKey("  Deep Work ")).toBe(planKey("deep work"));
    expect(planKey("a")).not.toBe(planKey("b"));
  });
  it("round_trips_per_title", () => {
    const s = store();
    savePlan(s, "Deep work", { focusMinutes: 50, breakMinutes: 10, cycles: 2 });
    expect(loadPlan(s, " deep WORK", defaults)).toEqual({ focusMinutes: 50, breakMinutes: 10, cycles: 2 });
    expect(loadPlan(s, "Other", defaults)).toEqual(defaults);
  });
  it("garbage_or_missing_storage_gives_defaults", () => {
    expect(loadPlan(store({ [planKey("x")]: "{nope" }), "x", defaults)).toEqual(defaults);
    expect(loadPlan(null, "x", defaults)).toEqual(defaults);
    expect(() => savePlan(null, "x", defaults)).not.toThrow();
  });
  it("out_of_range_stored_values_are_clamped", () => {
    const s = store({ [planKey("x")]: JSON.stringify({ focusMinutes: 9999, breakMinutes: 5, cycles: 4 }) });
    expect(loadPlan(s, "x", defaults).focusMinutes).toBe(180);
  });
  it("storage_that_throws_is_tolerated", () => {
    const bad = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(loadPlan(bad, "x", defaults)).toEqual(defaults);
    expect(() => savePlan(bad, "x", defaults)).not.toThrow();
  });
});

describe("parseCount", () => {
  it("parses_digits_only", () => {
    expect(parseCount("25")).toBe(25);
    expect(parseCount(" 7 ")).toBe(7);
    expect(parseCount("")).toBeNull();
    expect(parseCount("2a")).toBeNull();
    expect(parseCount("-3")).toBeNull();
  });
});

describe("wasDiscarded", () => {
  it("free_or_focus_stopped_under_60s_is_discarded", () => {
    expect(wasDiscarded(free(), T0 + 59_999)).toBe(true);
    expect(wasDiscarded(running(), T0 + 30_000)).toBe(true);
  });
  it("60s_or_more_is_kept", () => {
    expect(wasDiscarded(free(), T0 + 60_000)).toBe(false);
    expect(wasDiscarded(running(), T0 + 120_000)).toBe(false);
  });
  it("breaks_idle_and_stale_never_announce", () => {
    expect(wasDiscarded(running({ phase: "break", sessionStart: iso(T0 - 1_500_000) }), T0)).toBe(false);
    expect(wasDiscarded({ state: "idle" }, T0)).toBe(false);
  });
  it("uses_the_session_start_not_the_phase_start", () => {
    expect(wasDiscarded(running({ sessionStart: iso(T0 - 90_000), phaseStart: iso(T0 - 10_000) }), T0)).toBe(false);
  });
});

describe("ringProgress", () => {
  it("pomodoro_is_the_fraction_of_the_phase", () => {
    expect(ringProgress(running(), T0)).toBe(0);
    expect(ringProgress(running(), T0 + 12.5 * 60_000)).toBeCloseTo(0.5);
    expect(ringProgress(running(), T0 + 25 * 60_000)).toBe(1);
  });
  it("pomodoro_is_clamped", () => {
    expect(ringProgress(running(), T0 - 5000)).toBe(0);
    expect(ringProgress(running(), T0 + 99 * 60_000)).toBe(1);
  });
  it("pomodoro_without_a_valid_span_is_zero", () => {
    expect(ringProgress(running({ phaseEndsAt: iso(T0) }), T0 + 1000)).toBe(0);
    expect(ringProgress(running({ phaseEndsAt: null }), T0 + 1000)).toBe(0);
  });
  it("free_makes_one_lap_per_hour", () => {
    expect(ringProgress(free(), T0 + 15 * 60_000)).toBeCloseTo(0.25);
    expect(ringProgress(free(), T0 + 3_600_000 + 30 * 60_000)).toBeCloseTo(0.5);
    expect(ringProgress(free(), T0 + 3_600_000)).toBe(0);
  });
  it("idle_is_empty", () => {
    expect(ringProgress({ state: "idle" }, T0)).toBe(0);
  });
});

describe("digitDiff", () => {
  it("marks_changed_positions_aligned_to_the_right", () => {
    expect(digitDiff("24:59", "24:58")).toEqual([false, false, false, false, true]);
    expect(digitDiff("24:00", "23:59")).toEqual([false, true, false, true, true]);
  });
  it("first_render_and_equal_strings_animate_nothing", () => {
    expect(digitDiff(null, "10:00")).toEqual([false, false, false, false, false]);
    expect(digitDiff("10:00", "10:00")).toEqual([false, false, false, false, false]);
  });
  it("a_longer_string_marks_the_new_leading_chars", () => {
    expect(digitDiff("99:59", "100:00")).toEqual([true, true, true, false, true, true]);
  });
});

describe("cycleDots", () => {
  it("focus_marks_earlier_cycles_done_the_current_one_and_the_rest_upcoming", () => {
    expect(cycleDots(running({ cycle: 2 }))).toEqual(["done", "current", "upcoming", "upcoming"]);
  });
  it("break_counts_its_cycle_as_done_with_no_current_dot", () => {
    expect(cycleDots(running({ cycle: 2, phase: "break" }))).toEqual(["done", "done", "upcoming", "upcoming"]);
  });
  it("idle_uses_the_planned_cycles_all_upcoming", () => {
    expect(cycleDots({ state: "idle" }, 3)).toEqual(["upcoming", "upcoming", "upcoming"]);
  });
  it("free_has_no_dots", () => {
    expect(cycleDots(free())).toEqual([]);
  });
});

describe("runFinished / finishedMessage", () => {
  const last = running({ phase: "break", cycle: 4, cycles: 4 });
  it("last_break_ending_on_its_own_finishes_the_run", () => {
    expect(runFinished(last, { state: "idle" }, false)).toBe(4);
  });
  it("a_user_stop_never_counts", () => {
    expect(runFinished(last, { state: "idle" }, true)).toBeNull();
    expect(runFinished(running({ cycle: 4 }), { state: "idle" }, true)).toBeNull();
  });
  it("mid_run_transitions_and_free_timers_do_not_finish", () => {
    expect(runFinished(running({ phase: "break", cycle: 2 }), running({ cycle: 3 }), false)).toBeNull();
    expect(runFinished(running({ phase: "break", cycle: 2 }), { state: "idle" }, false)).toBeNull();
    expect(runFinished(free(), { state: "idle" }, false)).toBeNull();
    expect(runFinished({ state: "idle" }, { state: "idle" }, false)).toBeNull();
  });
  it("message_pluralises", () => {
    expect(finishedMessage(4)).toBe("4 pomodoros done");
    expect(finishedMessage(1)).toBe("1 pomodoro done");
  });
});
