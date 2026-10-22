import { describe, expect, it } from "vitest";
import { layoutLanes } from "./layout";

const r = (start: number, end: number) => ({ start, end });

describe("layoutLanes", () => {
  it("no_overlap_single_lane", () => {
    expect(layoutLanes([r(0, 60), r(120, 180)])).toEqual([
      { lane: 0, lanes: 1 },
      { lane: 0, lanes: 1 },
    ]);
  });

  it("two_overlapping_two_lanes", () => {
    expect(layoutLanes([r(0, 120), r(60, 180)])).toEqual([
      { lane: 0, lanes: 2 },
      { lane: 1, lanes: 2 },
    ]);
  });

  it("chain_overlap_reuses_lane", () => {
    // A overlaps B, B overlaps C, A and C do not overlap: C reuses A's lane.
    expect(layoutLanes([r(0, 60), r(30, 90), r(60, 120)])).toEqual([
      { lane: 0, lanes: 2 },
      { lane: 1, lanes: 2 },
      { lane: 0, lanes: 2 },
    ]);
  });

  it("adjacent_blocks_do_not_overlap", () => {
    expect(layoutLanes([r(0, 60), r(60, 120)])).toEqual([
      { lane: 0, lanes: 1 },
      { lane: 0, lanes: 1 },
    ]);
  });

  it("identical_blocks_split_lanes", () => {
    expect(layoutLanes([r(0, 60), r(0, 60), r(0, 60)])).toEqual([
      { lane: 0, lanes: 3 },
      { lane: 1, lanes: 3 },
      { lane: 2, lanes: 3 },
    ]);
  });

  it("keeps_input_order", () => {
    expect(layoutLanes([r(60, 120), r(0, 90)])).toEqual([
      { lane: 1, lanes: 2 },
      { lane: 0, lanes: 2 },
    ]);
  });
});
