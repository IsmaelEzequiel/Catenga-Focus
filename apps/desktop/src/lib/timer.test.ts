import { describe, expect, it } from "vitest";
import type { BlockRef, Session } from "../api";
import {
  blockHasSessions,
  blockTotalMinutes,
  fmtElapsed,
  fmtRemaining,
  listedSessions,
  sessionBars,
  sessionLine,
  sessionRelinks,
  sessionsForBlock,
  toBlockRef,
} from "./timer";

const ref = (start: number, end: number, title: string, tags: string[] = []): BlockRef => ({ start, end, title, tags, project: null });
const local = (h: number, m: number, s = 0, dayOffset = 0) => new Date(2026, 9, 20 + dayOffset, h, m, s).toISOString();
const session = (over: Partial<Session> = {}): Session => ({
  start: local(9, 5),
  end: local(9, 50),
  mode: "pomodoro",
  block: ref(540, 630, "Deep work"),
  minutes: 45,
  ...over,
});

describe("fmtElapsed", () => {
  it("formats_h_mm_ss", () => {
    expect(fmtElapsed(0)).toBe("0:00:00");
    expect(fmtElapsed(65_000)).toBe("0:01:05");
    expect(fmtElapsed(3_725_000)).toBe("1:02:05");
  });
  it("never_goes_negative_and_floors_ms", () => {
    expect(fmtElapsed(-5000)).toBe("0:00:00");
    expect(fmtElapsed(1999)).toBe("0:00:01");
  });
});

describe("fmtRemaining", () => {
  it("formats_mm_ss_rounding_up_partial_seconds", () => {
    expect(fmtRemaining(25 * 60_000)).toBe("25:00");
    expect(fmtRemaining(59_001)).toBe("01:00");
    expect(fmtRemaining(1)).toBe("00:01");
  });
  it("clamps_at_zero", () => {
    expect(fmtRemaining(-1000)).toBe("00:00");
  });
  it("handles_more_than_99_minutes", () => {
    expect(fmtRemaining(100 * 60_000)).toBe("100:00");
  });
});

describe("sessionLine", () => {
  it("shows_local_times_minutes_and_mode", () => {
    expect(sessionLine(session())).toBe("09:05–09:50 · 45 min · pomodoro");
  });
  it("open_session_is_marked_not_stopped", () => {
    expect(sessionLine(session({ end: null, mode: "free", minutes: 3 }))).toBe("09:05– · 3 min · free · not stopped");
  });
});

describe("sessionsForBlock", () => {
  const s1 = session();
  const s2 = session({ block: ref(540, 630, "  Deep work ", ["x"]), start: local(10, 0) });
  const other = session({ block: ref(540, 600, "Deep work") });
  it("matches_on_start_end_and_trimmed_title_ignoring_tags", () => {
    const got = sessionsForBlock([s1, s2, other], { start: 540, end: 630, title: "Deep work" });
    expect(got).toEqual([s1, s2]);
  });
  it("title_is_case_sensitive", () => {
    expect(sessionsForBlock([s1], { start: 540, end: 630, title: "deep work" })).toEqual([]);
  });
  it("hasSessions_agrees", () => {
    expect(blockHasSessions([s1], { start: 540, end: 630, title: "Deep work" })).toBe(true);
    expect(blockHasSessions([s1], { start: 0, end: 30, title: "Deep work" })).toBe(false);
  });
});

describe("blockTotalMinutes", () => {
  it("sums_finished_sessions_only", () => {
    const total = blockTotalMinutes([session({ minutes: 45 }), session({ minutes: 25 }), session({ end: null, minutes: 7 })]);
    expect(total).toBe(70);
  });
  it("empty_is_zero", () => expect(blockTotalMinutes([])).toBe(0));
});

describe("sessionBars", () => {
  const date = "2026-10-20";
  const now = new Date(2026, 9, 20, 12, 0).getTime();
  it("maps_to_minutes_of_the_day", () => {
    expect(sessionBars([session()], date, now, null)).toEqual([{ start: 545, end: 590, running: false }]);
  });
  it("running_ends_at_now", () => {
    const s = session({ end: null });
    expect(sessionBars([s], date, now, s.start)).toEqual([{ start: 545, end: 720, running: true }]);
  });
  it("an_open_session_that_is_not_running_does_not_grow", () => {
    // Orphan left by a crash: drawn with the minutes it had, not up to now.
    const s = session({ end: null, minutes: 10 });
    expect(sessionBars([s], date, now, null)).toEqual([{ start: 545, end: 555, running: false }]);
  });
  it("clamps_to_midnight_when_crossing_it", () => {
    const s = session({ start: local(23, 30), end: local(0, 30, 0, 1) });
    expect(sessionBars([s], date, now, null)).toEqual([{ start: 1410, end: 1440, running: false }]);
  });
  it("skips_sessions_of_other_days", () => {
    const s = session({ start: local(9, 0, 0, 1), end: local(10, 0, 0, 1) });
    expect(sessionBars([s], date, now, null)).toEqual([]);
  });
  it("gives_very_short_bars_a_minimum_length", () => {
    const s = session({ start: local(9, 0), end: local(9, 1) });
    const [bar] = sessionBars([s], date, now, null);
    expect(bar.end - bar.start).toBeGreaterThanOrEqual(2);
  });
});

describe("listedSessions", () => {
  it("lists_finished_and_orphan_open_sessions_but_not_the_running_one", () => {
    const done = session();
    const running = session({ start: local(11, 0), end: null });
    const orphan = session({ start: local(8, 0), end: null });
    expect(listedSessions([orphan, done, running], running.start)).toEqual([orphan, done]);
    expect(listedSessions([orphan, done], null)).toEqual([orphan, done]);
  });
});

describe("toBlockRef", () => {
  it("copies_only_the_snapshot_fields", () => {
    const b = { key: "k", start: 1, end: 2, title: "t", tags: ["a"], body: "x" };
    expect(toBlockRef(b)).toEqual({ start: 1, end: 2, title: "t", tags: ["a"], project: null });
  });
  it("carries_the_google_calendar_id_of_the_blocks_project", () => {
    const b = { key: "k", start: 1, end: 2, title: " t ", tags: [] };
    expect(toBlockRef(b, "cal-1")).toEqual({ start: 1, end: 2, title: "t", tags: [], project: "cal-1" });
  });
});

describe("sessionRelinks", () => {
  const b = (key: string, start: number, end: number, title: string, tags: string[] = []) => ({ key, start, end, title, tags });
  it("no_change_no_links", () => {
    expect(sessionRelinks([b("a", 1, 2, "x")], [b("a", 1, 2, "x")])).toEqual([]);
  });
  it("moved_block_links_from_old_to_new", () => {
    expect(sessionRelinks([b("a", 540, 600, "x")], [b("a", 600, 660, "x")])).toEqual([
      { from: ref(540, 600, "x"), to: ref(600, 660, "x") },
    ]);
  });
  it("rename_and_tag_changes_are_links", () => {
    const links = sessionRelinks([b("a", 1, 2, "x"), b("c", 5, 6, "y", ["t"])], [b("a", 1, 2, "z"), b("c", 5, 6, "y", ["u"])]);
    expect(links).toHaveLength(2);
  });
  it("new_and_deleted_blocks_are_ignored", () => {
    expect(sessionRelinks([b("a", 1, 2, "x")], [b("n", 1, 2, "x")])).toEqual([]);
    expect(sessionRelinks([b("a", 1, 2, "x")], [])).toEqual([]);
  });
  it("untitled_to_titled_is_a_link", () => {
    expect(sessionRelinks([b("a", 1, 2, "")], [b("a", 1, 2, "x")])).toHaveLength(1);
  });
});
