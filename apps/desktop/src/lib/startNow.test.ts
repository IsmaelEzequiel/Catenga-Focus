import { describe, expect, it } from "vitest";
import type { CalEvent } from "../api";
import { startNowTarget } from "./startNow";

const block = (key: string, start: number, end: number, title: string) => ({ key, start, end, title, tags: [], body: "" });
const event = (id: string, start: number, end: number, title: string): CalEvent => ({
  id,
  start,
  end,
  title,
  projectKey: null,
  projectId: null,
  color: "#3d6fb6",
});

describe("startNowTarget", () => {
  it("is null with nothing left today", () => {
    expect(startNowTarget([], [], 600)).toBeNull();
    expect(startNowTarget([block("a", 540, 600, "Rust")], [], 600)).toBeNull();
  });

  it("picks the block containing now", () => {
    const blocks = [block("a", 540, 600, "Rust"), block("b", 660, 720, "Deep")];
    expect(startNowTarget(blocks, [], 570)).toEqual({ kind: "block", key: "a" });
  });

  it("falls back to the soonest block starting later", () => {
    const blocks = [block("late", 900, 960, "Late"), block("soon", 660, 720, "Soon")];
    expect(startNowTarget(blocks, [], 600)).toEqual({ kind: "block", key: "soon" });
  });

  it("start is inclusive and end exclusive", () => {
    const blocks = [block("a", 540, 600, "A"), block("b", 600, 660, "B")];
    expect(startNowTarget(blocks, [], 600)).toEqual({ kind: "block", key: "b" });
  });

  it("uses a calendar ghost the same way", () => {
    const e = event("e1", 540, 600, "Standup");
    expect(startNowTarget([], [e], 560)).toEqual({ kind: "event", event: e });
    expect(startNowTarget([], [e], 500)).toEqual({ kind: "event", event: e });
  });

  it("ignores an event a block already stands for", () => {
    const e = event("e1", 540, 600, "Rust");
    expect(startNowTarget([block("a", 540, 600, "Rust")], [e], 560)).toEqual({ kind: "block", key: "a" });
    expect(startNowTarget([block("a", 540, 600, "Rust")], [e], 500)).toEqual({ kind: "block", key: "a" });
  });

  it("a block wins over a ghost at the same time", () => {
    const e = event("e1", 540, 600, "Other");
    const b = block("a", 540, 600, "Rust");
    expect(startNowTarget([b], [e], 560)).toEqual({ kind: "block", key: "a" });
    expect(startNowTarget([b], [e], 500)).toEqual({ kind: "block", key: "a" });
  });

  it("a current ghost beats a later block; a sooner ghost beats a later block", () => {
    const e = event("e1", 540, 600, "Cal");
    expect(startNowTarget([block("a", 660, 720, "Later")], [e], 560)).toEqual({ kind: "event", event: e });
    expect(startNowTarget([block("a", 660, 720, "Later")], [e], 500)).toEqual({ kind: "event", event: e });
  });

  it("a current item beats a sooner-or-later upcoming one", () => {
    const blocks = [block("now", 540, 700, "Now"), block("next", 600, 620, "Next")];
    expect(startNowTarget(blocks, [], 570)).toEqual({ kind: "block", key: "now" });
  });
});
