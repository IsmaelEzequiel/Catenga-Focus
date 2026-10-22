import { describe, expect, it } from "vitest";
import { blockDeletePlan, calendarOps, ghostEvents, isSeriesInstance, matchBlocks, type CalBlock, type CalEvent } from "./calendar";

const block = (key: string, start: number, end: number, title: string): CalBlock => ({ key, start, end, title });
const event = (id: string, start: number, end: number, title: string): CalEvent => ({ id, start, end, title, projectKey: null, projectId: null, color: "#3d6fb6" });

describe("matchBlocks", () => {
  it("matches_on_start_end_and_trimmed_title", () => {
    const m = matchBlocks([block("a", 540, 600, "Deep work")], [event("e1", 540, 600, "  Deep work ")]);
    expect(m.get("a")?.id).toBe("e1");
  });

  it("title_is_case_sensitive", () => {
    expect(matchBlocks([block("a", 540, 600, "deep work")], [event("e1", 540, 600, "Deep work")]).size).toBe(0);
  });

  it("different_times_do_not_match", () => {
    expect(matchBlocks([block("a", 540, 600, "x")], [event("e1", 540, 615, "x")]).size).toBe(0);
  });

  it("one_event_matches_only_the_first_block", () => {
    const m = matchBlocks([block("a", 540, 600, "x"), block("b", 540, 600, "x")], [event("e1", 540, 600, "x")]);
    expect([...m.keys()]).toEqual(["a"]);
  });

  it("two_events_two_blocks_pair_one_to_one", () => {
    const m = matchBlocks(
      [block("a", 540, 600, "x"), block("b", 540, 600, "x")],
      [event("e1", 540, 600, "x"), event("e2", 540, 600, "x")],
    );
    expect(m.get("a")?.id).toBe("e1");
    expect(m.get("b")?.id).toBe("e2");
  });
});

describe("ghostEvents", () => {
  it("returns_events_without_a_matching_block", () => {
    const events = [event("e1", 540, 600, "x"), event("e2", 700, 760, "y")];
    expect(ghostEvents([block("a", 540, 600, "x")], events).map((e) => e.id)).toEqual(["e2"]);
  });

  it("second_event_stays_ghost_when_two_blocks_share_times_and_one_event_matches", () => {
    const events = [event("e1", 540, 600, "x"), event("e2", 540, 600, "x")];
    expect(ghostEvents([block("a", 540, 600, "x")], events).map((e) => e.id)).toEqual(["e2"]);
  });

  it("no_blocks_means_all_ghosts", () => {
    expect(ghostEvents([], [event("e1", 1, 2, "x")])).toHaveLength(1);
  });
});

describe("calendarOps", () => {
  const events = [event("e1", 540, 600, "Study")];

  it("new_titled_block_creates", () => {
    const next = block("n", 700, 760, "Gym");
    expect(calendarOps([], [next], events)).toEqual([{ kind: "create", block: next }]);
  });

  it("untitled_new_block_does_nothing", () => {
    expect(calendarOps([], [block("n", 700, 760, "")], events)).toEqual([]);
    expect(calendarOps([], [block("n", 700, 760, "   ")], events)).toEqual([]);
  });

  it("titling_an_empty_block_creates", () => {
    const prev = [block("n", 700, 760, "")];
    const next = [block("n", 700, 760, "Gym")];
    expect(calendarOps(prev, next, events)).toEqual([{ kind: "create", block: next[0] }]);
  });

  it("new_block_that_matches_an_event_does_nothing", () => {
    expect(calendarOps([], [block("n", 540, 600, "Study")], events)).toEqual([]);
  });

  it("rename_of_matched_block_updates", () => {
    const prev = [block("a", 540, 600, "Study")];
    const next = [block("a", 540, 600, "Study more")];
    expect(calendarOps(prev, next, events)).toEqual([{ kind: "update", eventId: "e1", block: next[0] }]);
  });

  it("move_of_matched_block_updates", () => {
    const prev = [block("a", 540, 600, "Study")];
    const next = [block("a", 570, 630, "Study")];
    expect(calendarOps(prev, next, events)).toEqual([{ kind: "update", eventId: "e1", block: next[0] }]);
  });

  it("resize_of_matched_block_updates", () => {
    const prev = [block("a", 540, 600, "Study")];
    const next = [block("a", 540, 660, "Study")];
    expect(calendarOps(prev, next, events)).toHaveLength(1);
  });

  it("unchanged_matched_block_does_nothing", () => {
    const prev = [block("a", 540, 600, "Study")];
    expect(calendarOps(prev, [{ ...prev[0] }], events)).toEqual([]);
  });

  it("matched_block_renamed_to_empty_does_nothing", () => {
    expect(calendarOps([block("a", 540, 600, "Study")], [block("a", 540, 600, "")], events)).toEqual([]);
  });

  it("delete_does_nothing", () => {
    expect(calendarOps([block("a", 540, 600, "Study")], [], events)).toEqual([]);
  });

  it("old_app_only_block_moved_or_renamed_does_nothing", () => {
    const prev = [block("o", 800, 860, "Old")];
    expect(calendarOps(prev, [block("o", 830, 890, "Old")], events)).toEqual([]);
    expect(calendarOps(prev, [block("o", 800, 860, "Older")], events)).toEqual([]);
  });

  it("two_blocks_same_times_only_the_matched_one_updates", () => {
    const prev = [block("a", 540, 600, "Study"), block("b", 540, 600, "Study")];
    const next = [block("a", 540, 600, "Study"), block("b", 600, 660, "Study")];
    // "a" holds the event; moving "b" (app-only) must not touch Google.
    expect(calendarOps(prev, next, events)).toEqual([]);
    const next2 = [block("a", 600, 660, "Study"), block("b", 540, 600, "Study")];
    expect(calendarOps(prev, next2, events)).toEqual([{ kind: "update", eventId: "e1", block: next2[0] }]);
  });

  it("unrelated_blocks_are_ignored", () => {
    const prev = [block("a", 540, 600, "Study"), block("o", 800, 860, "Old")];
    const next = [block("a", 540, 600, "Study"), block("o", 800, 860, "Old"), block("n", 900, 960, "New")];
    expect(calendarOps(prev, next, events)).toEqual([{ kind: "create", block: next[2] }]);
  });
});

describe("isSeriesInstance", () => {
  it("is_true_only_for_series_projects", () => {
    expect(isSeriesInstance({ ...event("e", 1, 2, "x"), projectKey: "series:abc" })).toBe(true);
    expect(isSeriesInstance({ ...event("e", 1, 2, "x"), projectKey: "event:abc" })).toBe(false);
    expect(isSeriesInstance(event("e", 1, 2, "x"))).toBe(false);
  });
});

describe("blockDeletePlan", () => {
  const e = event("e1", 540, 600, "x");
  it("an_empty_block_without_an_event_is_deleted_straight_away", () => {
    expect(blockDeletePlan({ hasSessions: false, hasNote: false, event: undefined, canDeleteEvent: true })).toEqual({ kind: "delete" });
    expect(blockDeletePlan({ hasSessions: false, hasNote: false, event: e, canDeleteEvent: false })).toEqual({ kind: "delete" });
  });
  it("an_empty_block_with_an_event_asks_which_to_delete", () => {
    expect(blockDeletePlan({ hasSessions: false, hasNote: false, event: e, canDeleteEvent: true })).toEqual({ kind: "confirm", event: e });
  });
  it("a_block_with_sessions_or_a_note_is_refused_and_may_offer_deleting_the_event", () => {
    expect(blockDeletePlan({ hasSessions: true, hasNote: true, event: e, canDeleteEvent: true })).toEqual({ kind: "refuse", reason: "sessions", event: e });
    expect(blockDeletePlan({ hasSessions: false, hasNote: true, event: e, canDeleteEvent: true })).toEqual({ kind: "refuse", reason: "notes", event: e });
    expect(blockDeletePlan({ hasSessions: false, hasNote: true, event: undefined, canDeleteEvent: true })).toEqual({ kind: "refuse", reason: "notes", event: null });
    expect(blockDeletePlan({ hasSessions: true, hasNote: false, event: e, canDeleteEvent: false })).toEqual({ kind: "refuse", reason: "sessions", event: null });
  });
});
