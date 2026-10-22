import { describe, expect, it } from "vitest";
import type { SearchHit, SearchResponse } from "../api";
import { buildPaletteItems, fmtRange, resolveBlockKey, splitHighlight, stepIndex } from "./search";

const S = "\u0002";
const E = "\u0003";

describe("splitHighlight", () => {
  it("returns_one_plain_segment_when_no_markers", () => {
    expect(splitHighlight("hello world")).toEqual([{ text: "hello world", hit: false }]);
  });

  it("returns_nothing_for_empty_string", () => {
    expect(splitHighlight("")).toEqual([]);
  });

  it("splits_start_middle_and_end_matches", () => {
    expect(splitHighlight(`${S}ab${E}cd`)).toEqual([
      { text: "ab", hit: true },
      { text: "cd", hit: false },
    ]);
    expect(splitHighlight(`ab${S}cd${E}ef`)).toEqual([
      { text: "ab", hit: false },
      { text: "cd", hit: true },
      { text: "ef", hit: false },
    ]);
    expect(splitHighlight(`ab${S}cd${E}`)).toEqual([
      { text: "ab", hit: false },
      { text: "cd", hit: true },
    ]);
  });

  it("keeps_adjacent_matches_separate", () => {
    expect(splitHighlight(`${S}a${E}${S}b${E}`)).toEqual([
      { text: "a", hit: true },
      { text: "b", hit: true },
    ]);
  });

  it("treats_rest_as_highlighted_after_unterminated_open_marker", () => {
    expect(splitHighlight(`ab${S}cd`)).toEqual([
      { text: "ab", hit: false },
      { text: "cd", hit: true },
    ]);
  });

  it("ignores_stray_close_marker", () => {
    expect(splitHighlight(`ab${E}cd`)).toEqual([
      { text: "ab", hit: false },
      { text: "cd", hit: false },
    ]);
  });

  it("never_turns_html_looking_text_into_anything_but_text", () => {
    expect(splitHighlight(`<b>${S}x${E}</b>`)).toEqual([
      { text: "<b>", hit: false },
      { text: "x", hit: true },
      { text: "</b>", hit: false },
    ]);
  });
});

describe("resolveBlockKey", () => {
  const blocks = [
    { key: "k0", start: 540, end: 600, title: "A" },
    { key: "k1", start: 600, end: 660, title: "B" },
    { key: "k2", start: 600, end: 660, title: "C" },
  ];

  it("matches_by_ord", () => {
    expect(resolveBlockKey(blocks, { ord: 1, start: 600, end: 660, title: "B" })).toBe("k1");
  });

  it("falls_back_to_start_end_when_ord_shifted", () => {
    expect(resolveBlockKey(blocks, { ord: 0, start: 540, end: 600, title: "A" })).toBe("k0");
    expect(resolveBlockKey(blocks, { ord: 7, start: 540, end: 600, title: "Renamed" })).toBe("k0");
  });

  it("picks_title_among_same_start_end", () => {
    expect(resolveBlockKey(blocks, { ord: 9, start: 600, end: 660, title: "C" })).toBe("k2");
  });

  it("falls_back_to_title", () => {
    expect(resolveBlockKey(blocks, { ord: 0, start: 1, end: 2, title: "C" })).toBe("k2");
  });

  it("returns_null_when_nothing_matches", () => {
    expect(resolveBlockKey(blocks, { ord: 0, start: 1, end: 2, title: "Z" })).toBeNull();
  });

  it("does_not_match_an_empty_title_by_title", () => {
    const untitled = [{ key: "u", start: 0, end: 30, title: "" }];
    expect(resolveBlockKey(untitled, { ord: 3, start: 60, end: 90, title: "" })).toBeNull();
  });

  it("handles_empty_block_list", () => {
    expect(resolveBlockKey([], { ord: 0, start: 0, end: 15, title: "A" })).toBeNull();
  });
});

describe("stepIndex", () => {
  it("wraps_down_and_up", () => {
    expect(stepIndex(0, 1, 3)).toBe(1);
    expect(stepIndex(2, 1, 3)).toBe(0);
    expect(stepIndex(0, -1, 3)).toBe(2);
    expect(stepIndex(1, -1, 3)).toBe(0);
  });

  it("returns_0_for_empty_list", () => {
    expect(stepIndex(0, 1, 0)).toBe(0);
    expect(stepIndex(0, -1, 0)).toBe(0);
  });

  it("stays_put_for_single_item", () => {
    expect(stepIndex(0, 1, 1)).toBe(0);
    expect(stepIndex(0, -1, 1)).toBe(0);
  });
});

describe("buildPaletteItems", () => {
  const hit: SearchHit = { date: "2026-10-20", start: 540, end: 600, ord: 0, title: "A", tags: [], titleHl: "A", snippet: "" };
  const base: SearchResponse = { hits: [], titles: [], tags: [], truncated: false, indexing: false };

  it("orders_titles_then_tags_then_hits", () => {
    const items = buildPaletteItems({
      ...base,
      hits: [hit],
      titles: [{ label: "T", count: 2 }],
      tags: [{ label: "g", count: 1 }],
    });
    expect(items.map((i) => i.kind)).toEqual(["title", "tag", "hit"]);
  });

  it("returns_empty_for_empty_or_missing_response", () => {
    expect(buildPaletteItems(base)).toEqual([]);
    expect(buildPaletteItems(null)).toEqual([]);
  });

  it("keeps_payload_with_kind_discriminator", () => {
    const [item] = buildPaletteItems({ ...base, hits: [hit] });
    expect(item).toEqual({ kind: "hit", hit });
  });
});

describe("fmtRange", () => {
  it("formats_minutes_as_hh_mm_en_dash_range", () => {
    expect(fmtRange(540, 630)).toBe("09:00–10:30");
    expect(fmtRange(0, 1440)).toBe("00:00–24:00");
  });
});
