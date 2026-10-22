import { describe, expect, it } from "vitest";
import type { CalendarCell, TagStat } from "../api";
import { barPercent, cellLabel, countsSessionsTwice, defaultOpenTag, fmtDuration, habitCellLabel, heatBucket, heatLayout, isValidRange, RANGES } from "./dashboard";

const cal = (start: string, days: number): CalendarCell[] => {
  const [y, m, d] = start.split("-").map(Number);
  return Array.from({ length: days }, (_, i) => {
    const dt = new Date(y, m - 1, d + i);
    const date = `${dt.getFullYear()}-${String(dt.getMonth() + 1).padStart(2, "0")}-${String(dt.getDate()).padStart(2, "0")}`;
    return { date, minutes: 0, done: false };
  });
};

describe("fmtDuration", () => {
  it("formats_hours_minutes_and_zero", () => {
    expect(fmtDuration(750)).toBe("12h 30m");
    expect(fmtDuration(45)).toBe("45m");
    expect(fmtDuration(0)).toBe("0m");
    expect(fmtDuration(120)).toBe("2h");
    expect(fmtDuration(61)).toBe("1h 1m");
  });
  it("clamps_negative_and_rounds_fractions", () => {
    expect(fmtDuration(-5)).toBe("0m");
    expect(fmtDuration(59.6)).toBe("1h");
  });
});

describe("heatBucket", () => {
  it("buckets_by_minutes", () => {
    expect([0, 1, 29, 30, 59, 60, 119, 120, 600].map(heatBucket)).toEqual([0, 1, 1, 2, 2, 3, 3, 4, 4]);
  });
});

describe("heatLayout", () => {
  it("is_monday_first_and_pads_the_first_week", () => {
    // 2026-10-21 is a Wednesday.
    const { columns } = heatLayout(cal("2026-10-21", 10));
    expect(columns[0].map((c) => c?.date ?? null)).toEqual([null, null, "2026-10-21", "2026-10-22", "2026-10-23", "2026-10-24", "2026-10-25"]);
    expect(columns[1][0]?.date).toBe("2026-10-26");
    expect(columns[1][2]?.date).toBe("2026-10-28");
    expect(columns[1][4]?.date).toBe("2026-10-30");
    expect(columns[1][5]).toBeNull();
  });

  it("makes_53_columns_for_371_days_ending_on_a_sunday", () => {
    const { columns } = heatLayout(cal("2025-10-27", 371));
    expect(columns).toHaveLength(53);
    expect(columns.every((c) => c.length === 7)).toBe(true);
  });

  it("empty_input_gives_no_columns", () => {
    expect(heatLayout([])).toEqual({ columns: [], months: [] });
  });

  it("labels_the_column_where_a_month_starts", () => {
    const { months } = heatLayout(cal("2026-09-28", 60));
    // Sep 28 column 0; Oct 1 (Thu) in column 0 too -> "Oct"; Nov 1 (Sun) in column 5
    expect(months).toEqual([
      { column: 0, label: "Oct" },
      { column: 4, label: "Nov" },
    ]);
  });

  it("labels_the_first_column_with_its_month_and_skips_crowded_labels", () => {
    // starts Sat 2026-10-31: col 0 = Oct 31 (Sat, Sun 1 Nov) ... Nov starts in col 0 as well
    const { months } = heatLayout(cal("2026-10-31", 20));
    expect(months[0]).toEqual({ column: 0, label: "Nov" });
    expect(months).toHaveLength(1);
  });

  it("labels_first_column_with_its_own_month_when_no_month_start_inside", () => {
    const { months } = heatLayout(cal("2026-10-14", 7));
    expect(months).toEqual([{ column: 0, label: "Oct" }]);
  });
});

describe("barPercent", () => {
  it("is_relative_to_max_with_a_visible_floor", () => {
    expect(barPercent(50, 100)).toBe(50);
    expect(barPercent(100, 100)).toBe(100);
    expect(barPercent(1, 1000)).toBe(2);
  });
  it("is_zero_for_empty", () => {
    expect(barPercent(0, 100)).toBe(0);
    expect(barPercent(5, 0)).toBe(0);
  });
});

describe("labels", () => {
  it("cellLabel_shows_weekday_date_and_duration", () => {
    expect(cellLabel("2026-10-21", 135)).toBe("Wed 21 Oct · 2h 15m");
    expect(cellLabel("2026-10-21", 0)).toBe("Wed 21 Oct · no activity");
  });
  it("habitCellLabel_distinguishes_timer_note_and_none", () => {
    const d = { date: "2026-10-20", minutes: 30, done: true, note: false };
    expect(habitCellLabel(d)).toBe("Tue 20 Oct · 30m");
    expect(habitCellLabel({ ...d, minutes: 0, note: true })).toBe("Tue 20 Oct · note only");
    expect(habitCellLabel({ ...d, minutes: 30, note: true })).toBe("Tue 20 Oct · 30m and a note");
    expect(habitCellLabel({ ...d, minutes: 0, done: false })).toBe("Tue 20 Oct · not done");
  });
});

describe("ranges", () => {
  it("has_the_five_ranges_in_order_with_labels", () => {
    expect(RANGES.map((r) => r.value)).toEqual(["week", "month", "30d", "year", "all"]);
    expect(RANGES.map((r) => r.label)).toEqual(["This week", "This month", "30 days", "This year", "All time"]);
  });
  it("validates_stored_values", () => {
    expect(isValidRange("30d")).toBe(true);
    expect(isValidRange("nope")).toBe(false);
    expect(isValidRange(null)).toBe(false);
  });
});

const tagStat = (key: string, sessions: number): TagStat => ({ label: key, key, minutes: 10, sessions, titles: [] });

describe("countsSessionsTwice", () => {
  it("is_true_when_tag_sessions_exceed_the_total", () => {
    expect(countsSessionsTwice([tagStat("a", 2), tagStat("b", 1)], 2)).toBe(true);
  });
  it("is_false_when_each_session_is_in_one_group", () => {
    expect(countsSessionsTwice([tagStat("a", 2), tagStat("", 1)], 3)).toBe(false);
    expect(countsSessionsTwice([], 0)).toBe(false);
  });
});

describe("defaultOpenTag", () => {
  it("is_the_first_tag_key", () => {
    expect(defaultOpenTag([tagStat("a", 1), tagStat("b", 1)])).toBe("a");
  });
  it("is_null_without_tags", () => {
    expect(defaultOpenTag([])).toBeNull();
  });
});
