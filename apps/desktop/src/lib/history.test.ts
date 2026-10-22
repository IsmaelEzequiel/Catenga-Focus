import { describe, expect, it } from "vitest";
import type { HistoryEntry } from "../api";
import { visibleEntries } from "./history";

const e = (date: string, body: string, ord = 0): HistoryEntry => ({ date, start: 540, end: 600, ord, title: "T", tags: [], body });
const entries = [e("2026-10-22", "\nnote\n"), e("2026-10-21", "\n  \n"), e("2026-10-20", ""), e("2026-10-19", "x\n")];

describe("visibleEntries", () => {
  it("filters_empty_and_whitespace_only_bodies_when_only_with_notes", () => {
    const got = visibleEntries(entries, { onlyWithNotes: true, order: "newest" });
    expect(got.map((x) => x.date)).toEqual(["2026-10-22", "2026-10-19"]);
  });

  it("keeps_all_when_not_filtering", () => {
    expect(visibleEntries(entries, { onlyWithNotes: false, order: "newest" })).toEqual(entries);
  });

  it("reverses_for_oldest_without_mutating_input", () => {
    const copy = [...entries];
    const got = visibleEntries(entries, { onlyWithNotes: false, order: "oldest" });
    expect(got.map((x) => x.date)).toEqual(["2026-10-19", "2026-10-20", "2026-10-21", "2026-10-22"]);
    expect(entries).toEqual(copy);
  });

  it("is_stable_for_equal_dates", () => {
    const same = [e("2026-10-20", "a\n", 2), e("2026-10-20", "b\n", 1), e("2026-10-20", "c\n", 0)];
    expect(visibleEntries(same, { onlyWithNotes: true, order: "newest" }).map((x) => x.ord)).toEqual([2, 1, 0]);
    expect(visibleEntries(same, { onlyWithNotes: true, order: "oldest" }).map((x) => x.ord)).toEqual([0, 1, 2]);
  });
});
