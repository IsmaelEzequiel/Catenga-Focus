import { describe, expect, it } from "vitest";
import { defaultRepeat, describeRepeat, repeatProblem, repeatRequest, toggleDay, weekdayOf } from "./repeat";

describe("weekdayOf", () => {
  it("monday_is_zero_and_sunday_is_six", () => {
    expect(weekdayOf("2026-11-09")).toBe(0);
    expect(weekdayOf("2026-11-15")).toBe(6);
  });
});

describe("defaultRepeat", () => {
  it("daily_forever_with_the_blocks_weekday_ready_for_weekly", () => {
    expect(defaultRepeat("2026-11-11")).toEqual({ freq: "daily", days: [2], until: null });
  });
});

describe("toggleDay", () => {
  it("adds_in_order_and_removes", () => {
    expect(toggleDay([4, 0], 2)).toEqual([0, 2, 4]);
    expect(toggleDay([0, 2], 2)).toEqual([0]);
  });
});

describe("repeatProblem", () => {
  const date = "2026-11-09";
  it("weekly_needs_a_day", () => {
    expect(repeatProblem({ freq: "weekly", days: [], until: null }, date)).toBe("Pick at least one day.");
    expect(repeatProblem({ freq: "daily", days: [], until: null }, date)).toBeNull();
  });
  it("the_last_day_must_be_picked_and_not_before_the_block", () => {
    expect(repeatProblem({ freq: "daily", days: [], until: "" }, date)).toBe("Pick the last day.");
    expect(repeatProblem({ freq: "daily", days: [], until: "2026-11-08" }, date)).toBe("The last day is before this block.");
    expect(repeatProblem({ freq: "daily", days: [], until: "2026-11-09" }, date)).toBeNull();
  });
});

describe("repeatRequest", () => {
  it("drops_days_unless_weekly", () => {
    expect(repeatRequest({ freq: "daily", days: [3], until: null }).days).toEqual([]);
    expect(repeatRequest({ freq: "weekly", days: [3], until: null }).days).toEqual([3]);
  });
});

describe("describeRepeat", () => {
  it("names_the_rule_and_its_end", () => {
    expect(describeRepeat({ freq: "daily", days: [], until: null })).toBe("Daily, forever");
    expect(describeRepeat({ freq: "weekdays", days: [], until: null })).toBe("Every weekday, forever");
    expect(describeRepeat({ freq: "weekly", days: [0, 2], until: null })).toBe("Weekly on Mon, Wed, forever");
    expect(describeRepeat({ freq: "daily", days: [], until: "2026-12-31" })).toMatch(/^Daily, until .*2026$/);
  });
});
