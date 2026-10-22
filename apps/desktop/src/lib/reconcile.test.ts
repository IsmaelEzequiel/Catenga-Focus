import { describe, expect, it } from "vitest";
import { mergeConflict, overlayNotes, reconcileKeys, type UiBlock } from "./reconcile";

let n = 0;
const ui = (start: number, end: number, title: string, body = "", tags: string[] = []): UiBlock => ({
  key: `k${n++}`,
  start,
  end,
  title,
  tags,
  body,
});
const raw = ({ key: _key, ...b }: UiBlock) => b;
const withBody = (b: UiBlock, body: string): UiBlock => ({ ...b, body });

describe("reconcileKeys", () => {
  it("reconcileKeys_keeps_key_for_identical_block", () => {
    const prev = [ui(540, 600, "A", "x\n", ["t"])];
    const next = reconcileKeys(prev, [{ ...raw(prev[0]), body: "changed\n" }]);
    expect(next[0].key).toBe(prev[0].key);
  });

  it("reconcileKeys_matches_duplicates_in_order", () => {
    const prev = [ui(540, 600, "A"), ui(540, 600, "A")];
    const next = reconcileKeys(prev, [raw(prev[0]), raw(prev[1])]);
    expect(next.map((b) => b.key)).toEqual([prev[0].key, prev[1].key]);
  });

  it("reconcileKeys_falls_back_to_start_end_match", () => {
    const prev = [ui(540, 600, "Old")];
    const next = reconcileKeys(prev, [{ ...raw(prev[0]), title: "Renamed" }]);
    expect(next[0].key).toBe(prev[0].key);
  });

  it("reconcileKeys_new_block_gets_fresh_key", () => {
    const prev = [ui(540, 600, "A")];
    const next = reconcileKeys(prev, [raw(prev[0]), { start: 600, end: 660, title: "B", tags: [], body: "" }]);
    expect(next[0].key).toBe(prev[0].key);
    expect(next[1].key).not.toBe(prev[0].key);
    expect(next[1].key).toBeTruthy();
  });
});

describe("mergeConflict", () => {
  it("mergeConflict_no_local_note_edits_returns_disk", () => {
    const a = ui(540, 600, "A", "a\n");
    const r = mergeConflict([a], [a], [{ ...raw(a), body: "external\n" }]);
    expect(r.blocks.map(raw)).toEqual([{ ...raw(a), body: "external\n" }]);
    expect(r.conflicts).toEqual([]);
    expect(r.changed).toBe(false);
    expect(r.structureDropped).toBe(false);
    expect(r.blocks[0].key).toBe(a.key);
  });

  it("mergeConflict_disk_changed_other_block_applies_my_note", () => {
    const a = ui(540, 600, "A", "a\n");
    const b = ui(600, 660, "B", "b\n");
    const mine = withBody(a, "a mine\n");
    const r = mergeConflict([a, b], [mine, b], [raw(a), { ...raw(b), body: "b external\n" }]);
    expect(r.blocks.map((x) => x.body)).toEqual(["a mine\n", "b external\n"]);
    expect(r.conflicts).toEqual([]);
    expect(r.changed).toBe(true);
  });

  it("mergeConflict_disk_body_equal_to_base_applies_my_note", () => {
    const a = ui(540, 600, "A", "a\n");
    const r = mergeConflict([a], [withBody(a, "mine\n")], [raw(a)]);
    expect(r.blocks[0].body).toBe("mine\n");
    expect(r.changed).toBe(true);
    expect(r.conflicts).toEqual([]);
  });

  it("mergeConflict_identical_edits_are_not_a_conflict", () => {
    const a = ui(540, 600, "A", "a\n");
    const r = mergeConflict([a], [withBody(a, "same\n")], [{ ...raw(a), body: "same\n" }]);
    expect(r.conflicts).toEqual([]);
    expect(r.changed).toBe(false);
    expect(r.blocks[0].body).toBe("same\n");
  });

  it("mergeConflict_same_note_edited_both_sides_is_conflict", () => {
    const a = ui(540, 600, "A", "a\n");
    const mine = withBody(a, "mine\n");
    const r = mergeConflict([a], [mine], [{ ...raw(a), body: "theirs\n" }]);
    expect(r.conflicts).toEqual([{ key: a.key, mine }]);
    expect(r.blocks[0].body).toBe("theirs\n");
    expect(r.blocks[0].key).toBe(a.key);
  });

  it("mergeConflict_block_missing_on_disk_is_conflict", () => {
    const a = ui(540, 600, "A", "a\n");
    const b = ui(600, 660, "B", "b\n");
    const mine = withBody(b, "b mine\n");
    const r = mergeConflict([a, b], [a, mine], [raw(a), { ...raw(b), start: 700, end: 760 }]);
    expect(r.conflicts).toEqual([{ key: b.key, mine }]);
    expect(r.blocks.map((x) => x.title)).toEqual(["A", "B"]);
    expect(r.blocks[1].body).toBe("b\n");
  });

  it("mergeConflict_disk_empty_makes_every_edited_block_a_conflict", () => {
    const a = ui(540, 600, "A", "a\n");
    const b = ui(600, 660, "B", "b\n");
    const c = ui(700, 760, "C", "c\n");
    const r = mergeConflict([a, b, c], [withBody(a, "1\n"), withBody(b, "2\n"), c], []);
    expect(r.blocks).toEqual([]);
    expect(r.conflicts.map((x) => x.key)).toEqual([a.key, b.key]);
  });

  it("mergeConflict_local_structure_change_is_dropped_and_flagged", () => {
    const a = ui(540, 600, "A", "a\n");
    const moved = { ...a, start: 600, end: 660 };
    const r = mergeConflict([a], [moved], [{ ...raw(a), body: "external\n" }]);
    expect(r.structureDropped).toBe(true);
    expect(r.blocks.map((x) => [x.start, x.end])).toEqual([[540, 600]]);
  });

  it("mergeConflict_conflict_keeps_mine_text_verbatim", () => {
    const a = ui(540, 600, "A", "a\n");
    const text = "\n  mine\n\n```\nopen\n## 10:00–11:00 · X\n";
    const r = mergeConflict([a], [withBody(a, text)], [{ ...raw(a), body: "theirs\n" }]);
    expect(r.conflicts[0].mine.body).toBe(text);
  });

  it("mergeConflict_unsaved_new_block_with_text_is_kept_as_conflict", () => {
    const a = ui(540, 600, "A", "a\n");
    const fresh = ui(700, 760, "New", "typed\n");
    const r = mergeConflict([a], [a, fresh], [raw(a)]);
    expect(r.conflicts).toEqual([{ key: fresh.key, mine: fresh }]);
    expect(r.structureDropped).toBe(true);
  });
});

describe("overlayNotes", () => {
  it("overlayNotes_keeps_typed_text_over_rolled_back_structure", () => {
    const a = ui(540, 600, "A", "old\n");
    const savedA = { ...a };
    const localA = { ...a, start: 600, end: 660, body: "typed\n" };
    const extra = ui(700, 760, "Extra", "x\n");
    const out = overlayNotes([savedA], [localA, extra]);
    expect(out).toEqual([{ ...savedA, body: "typed\n" }]);
  });
});
