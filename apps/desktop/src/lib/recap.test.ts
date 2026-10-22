import { describe, expect, it } from "vitest";
import type { Session, TimerStatus } from "../api";
import { buildRecap, firstLine, toMarkdown } from "./recap";

const DATE = "2026-10-22";
const IDLE: TimerStatus = { state: "idle" };

const block = (key: string, start: number, end: number, title: string, body = "", tags: string[] = []) => ({ key, start, end, title, tags, body });
const session = (b: { start: number; end: number; title: string; tags?: string[] }, minutes: number, finished = true): Session => ({
  start: "2026-10-22T09:00:00+00:00",
  end: finished ? "2026-10-22T10:00:00+00:00" : null,
  mode: "free",
  block: { start: b.start, end: b.end, title: b.title, tags: b.tags ?? [], project: null },
  minutes,
});
const running = (b: { start: number; end: number; title: string }, phaseStart: string, over: Partial<Extract<TimerStatus, { state: "running" }>> = {}): TimerStatus => ({
  state: "running",
  date: DATE,
  block: { start: b.start, end: b.end, title: b.title, tags: [], project: null },
  mode: "free",
  phase: "focus",
  sessionStart: phaseStart,
  phaseEndsAt: null,
  phaseStart,
  cycle: null,
  cycles: null,
  focusMinutes: null,
  breakMinutes: null,
  ...over,
});

describe("firstLine", () => {
  it("skips_blank_lines", () => expect(firstLine("\n\n  hello \nworld")).toBe("hello"));
  it("strips_markdown_prefixes", () => {
    expect(firstLine("# Title")).toBe("Title");
    expect(firstLine("### Deep")).toBe("Deep");
    expect(firstLine("- item")).toBe("item");
    expect(firstLine("* item")).toBe("item");
    expect(firstLine("> quote")).toBe("quote");
    expect(firstLine("1. first")).toBe("first");
    expect(firstLine("- [ ] todo")).toBe("todo");
    expect(firstLine("- [x] done")).toBe("done");
    expect(firstLine("[X] done")).toBe("done");
  });
  it("is_empty_without_text", () => {
    expect(firstLine("")).toBe("");
    expect(firstLine("  \n\n")).toBe("");
  });
  it("caps_at_140_with_ellipsis", () => {
    const out = firstLine("a".repeat(300));
    expect(out).toBe(`${"a".repeat(139)}…`);
    expect([...out]).toHaveLength(140);
    expect(firstLine("b".repeat(140))).toBe("b".repeat(140));
  });
});

describe("buildRecap", () => {
  it("is_empty_for_no_activity", () => {
    const r = buildRecap([block("a", 540, 600, "Plan")], [], IDLE, 0, DATE);
    expect(r.done).toEqual([]);
    expect(r.totalMinutes).toBe(0);
    expect(r.doneCount).toBe(0);
    expect(r.otherCount).toBe(1);
  });

  it("done_by_finished_session_or_note_sorted_by_start", () => {
    const late = block("l", 840, 900, "Late", "wrote things", ["x"]);
    const early = block("e", 540, 600, "Early");
    const none = block("n", 700, 760, "Nothing", "   \n");
    const r = buildRecap([late, none, early], [session(early, 30)], IDLE, 0, DATE);
    expect(r.done.map((d) => d.key)).toEqual(["e", "l"]);
    expect(r.doneCount).toBe(2);
    expect(r.otherCount).toBe(1);
    expect(r.done[0]).toMatchObject({ start: 540, end: 600, title: "Early", minutes: 30, firstLine: "" });
    expect(r.done[1]).toMatchObject({ minutes: 0, firstLine: "wrote things", tags: ["x"] });
  });

  it("ignores_open_sessions_unless_running", () => {
    const b = block("a", 540, 600, "Deep");
    const r = buildRecap([b], [session(b, 12, false)], IDLE, 0, DATE);
    expect(r.doneCount).toBe(0);
    expect(r.totalMinutes).toBe(0);
  });

  it("sums_sessions_per_block_and_total_including_unmatched", () => {
    const b = block("a", 540, 600, "Deep");
    const other = { start: 1, end: 2, title: "Deleted block" };
    const r = buildRecap([b], [session(b, 20), session(b, 25), session(other, 10)], IDLE, 0, DATE);
    expect(r.done[0].minutes).toBe(45);
    expect(r.totalMinutes).toBe(55);
  });

  it("adds_live_minutes_to_the_running_block_and_total", () => {
    const b = block("a", 540, 600, "Deep");
    const t0 = new Date("2026-10-22T09:00:00Z").getTime();
    const status = running(b, "2026-10-22T09:00:00Z");
    const r = buildRecap([b], [session(b, 30)], status, t0 + 7 * 60_000 + 30_000, DATE);
    expect(r.done[0].minutes).toBe(37);
    expect(r.totalMinutes).toBe(37);
  });

  it("running_block_with_live_minutes_counts_as_done", () => {
    const b = block("a", 540, 600, "Deep");
    const t0 = new Date("2026-10-22T09:00:00Z").getTime();
    const r = buildRecap([b], [], running(b, "2026-10-22T09:00:00Z"), t0 + 3 * 60_000, DATE);
    expect(r.doneCount).toBe(1);
    expect(r.done[0].minutes).toBe(3);
  });

  it("live_counts_only_for_this_date_and_focus_phase", () => {
    const b = block("a", 540, 600, "Deep", "note");
    const t0 = new Date("2026-10-22T09:00:00Z").getTime();
    const now = t0 + 10 * 60_000;
    const elsewhere = running(b, "2026-10-22T09:00:00Z", { date: "2026-10-21" });
    expect(buildRecap([b], [], elsewhere, now, DATE).totalMinutes).toBe(0);
    const onBreak = running(b, "2026-10-22T09:00:00Z", { phase: "break" });
    expect(buildRecap([b], [], onBreak, now, DATE).totalMinutes).toBe(0);
  });

  it("live_on_a_block_that_is_not_listed_still_counts_in_total", () => {
    const b = block("a", 540, 600, "Deep", "note");
    const t0 = new Date("2026-10-22T09:00:00Z").getTime();
    const gone = { start: 1, end: 2, title: "Gone" };
    const r = buildRecap([b], [], running(gone, "2026-10-22T09:00:00Z"), t0 + 5 * 60_000, DATE);
    expect(r.totalMinutes).toBe(5);
    expect(r.done[0].minutes).toBe(0);
  });

  it("matches_title_trimmed", () => {
    const b = block("a", 540, 600, " Deep ");
    const r = buildRecap([b], [session({ start: 540, end: 600, title: "Deep" }, 15)], IDLE, 0, DATE);
    expect(r.done[0].minutes).toBe(15);
  });
});

describe("toMarkdown", () => {
  it("formats_header_rows_and_total", () => {
    const a = block("a", 540, 630, "Deep work", "# Chapter 3 draft\nmore", ["mestrado"]);
    const c = block("c", 700, 760, "Read");
    const r = buildRecap([a, c], [session(a, 75), session(c, 20)], IDLE, 0, DATE);
    expect(toMarkdown(r, "22 Oct")).toBe(
      ["## Today — 22 Oct", "", "- 09:00–10:30 Deep work #mestrado · 1h 15m — Chapter 3 draft", "- 11:40–12:40 Read · 20m", "", "Total: 1h 35m"].join("\n") + "\n",
    );
  });

  it("omits_zero_minutes_and_untitled_gets_a_name", () => {
    const a = block("a", 540, 600, "", "just a note");
    const r = buildRecap([a], [], IDLE, 0, DATE);
    expect(toMarkdown(r, "22 Oct")).toContain("- 09:00–10:00 Untitled — just a note\n");
  });

  it("empty_recap_has_no_total_line", () => {
    const r = buildRecap([], [], IDLE, 0, DATE);
    expect(toMarkdown(r, "22 Oct")).toBe("## Today — 22 Oct\n\nNothing done.\n");
  });
});
