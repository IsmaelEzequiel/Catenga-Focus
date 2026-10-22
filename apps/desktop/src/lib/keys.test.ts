import { describe, expect, it } from "vitest";
import {
  isDashboardShortcut,
  isFocusShortcut,
  isGoalsShortcut,
  isSearchShortcut,
  shortcutFor,
  shortcutRows,
  stepBlock,
  type KeyContext,
} from "./keys";

const key = (k: string, mods: Partial<{ metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean }> = {}) => ({
  key: k,
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...mods,
});

describe("isSearchShortcut", () => {
  it("accepts_cmd_k_on_mac", () => {
    expect(isSearchShortcut(key("k", { metaKey: true }), true)).toBe(true);
  });

  it("accepts_ctrl_k_off_mac", () => {
    expect(isSearchShortcut(key("k", { ctrlKey: true }), false)).toBe(true);
  });

  it("is_case_insensitive", () => {
    expect(isSearchShortcut(key("K", { metaKey: true }), true)).toBe(true);
    expect(isSearchShortcut(key("K", { ctrlKey: true }), false)).toBe(true);
  });

  it("rejects_the_other_platforms_modifier", () => {
    expect(isSearchShortcut(key("k", { ctrlKey: true }), true)).toBe(false);
    expect(isSearchShortcut(key("k", { metaKey: true }), false)).toBe(false);
  });

  it("rejects_plain_k_other_keys_and_extra_modifiers", () => {
    expect(isSearchShortcut(key("k"), true)).toBe(false);
    expect(isSearchShortcut(key("j", { metaKey: true }), true)).toBe(false);
    expect(isSearchShortcut(key("k", { metaKey: true, shiftKey: true }), true)).toBe(false);
    expect(isSearchShortcut(key("k", { ctrlKey: true, altKey: true }), false)).toBe(false);
  });
});

describe("isFocusShortcut", () => {
  it("accepts_cmd_shift_f_on_mac_and_ctrl_shift_f_elsewhere", () => {
    expect(isFocusShortcut(key("F", { metaKey: true, shiftKey: true }), true)).toBe(true);
    expect(isFocusShortcut(key("f", { ctrlKey: true, shiftKey: true }), false)).toBe(true);
  });

  it("rejects_missing_shift_wrong_modifier_alt_and_other_keys", () => {
    expect(isFocusShortcut(key("f", { metaKey: true }), true)).toBe(false);
    expect(isFocusShortcut(key("F", { ctrlKey: true, shiftKey: true }), true)).toBe(false);
    expect(isFocusShortcut(key("F", { metaKey: true, shiftKey: true }), false)).toBe(false);
    expect(isFocusShortcut(key("F", { metaKey: true, shiftKey: true, altKey: true }), true)).toBe(false);
    expect(isFocusShortcut(key("G", { metaKey: true, shiftKey: true }), true)).toBe(false);
  });
});

describe("isDashboardShortcut", () => {
  it("accepts_cmd_d_on_mac_and_ctrl_d_elsewhere", () => {
    expect(isDashboardShortcut(key("d", { metaKey: true }), true)).toBe(true);
    expect(isDashboardShortcut(key("D", { ctrlKey: true }), false)).toBe(true);
  });

  it("rejects_wrong_modifier_extra_modifiers_and_other_keys", () => {
    expect(isDashboardShortcut(key("d", { ctrlKey: true }), true)).toBe(false);
    expect(isDashboardShortcut(key("d", { metaKey: true }), false)).toBe(false);
    expect(isDashboardShortcut(key("d", { metaKey: true, shiftKey: true }), true)).toBe(false);
    expect(isDashboardShortcut(key("d", { metaKey: true, altKey: true }), true)).toBe(false);
    expect(isDashboardShortcut(key("d"), true)).toBe(false);
    expect(isDashboardShortcut(key("k", { metaKey: true }), true)).toBe(false);
  });
});

describe("isGoalsShortcut", () => {
  it("accepts_cmd_g_on_mac_and_ctrl_g_elsewhere", () => {
    expect(isGoalsShortcut(key("g", { metaKey: true }), true)).toBe(true);
    expect(isGoalsShortcut(key("G", { ctrlKey: true }), false)).toBe(true);
  });
  it("rejects_the_other_modifier_shift_alt_and_plain_g", () => {
    expect(isGoalsShortcut(key("g", { ctrlKey: true }), true)).toBe(false);
    expect(isGoalsShortcut(key("g", { metaKey: true }), false)).toBe(false);
    expect(isGoalsShortcut(key("g", { metaKey: true, shiftKey: true }), true)).toBe(false);
    expect(isGoalsShortcut(key("g", { metaKey: true, altKey: true }), true)).toBe(false);
    expect(isGoalsShortcut(key("g"), true)).toBe(false);
  });
});

describe("shortcutFor", () => {
  const ctx = (over: Partial<KeyContext> = {}): KeyContext => ({
    typing: false,
    onControl: false,
    dragging: false,
    focusOpen: false,
    paletteOpen: false,
    modalOpen: false,
    view: "day",
    isMac: true,
    ...over,
  });
  const f = (k: string, mods: Parameters<typeof key>[1] = {}, c: Partial<KeyContext> = {}) => shortcutFor(key(k, mods), ctx(c));

  it("maps_the_plain_keys", () => {
    expect(f("n")).toBe("quickAdd");
    expect(f("N")).toBe("quickAdd");
    expect(f("t")).toBe("today");
    expect(f("Enter")).toBe("openNote");
    expect(f(" ")).toBe("toggleTimer");
    expect(f("ArrowUp")).toBe("prevBlock");
    expect(f("ArrowDown")).toBe("nextBlock");
    expect(f("f")).toBe("focus");
    expect(f("?", { shiftKey: true })).toBe("help");
    expect(f("?")).toBe("help");
    expect(f("r")).toBe("rename");
    expect(f("R")).toBe("rename");
  });

  it("rename_only_in_the_plain_day_view", () => {
    expect(f("r", {}, { typing: true })).toBeNull();
    expect(f("r", {}, { focusOpen: true })).toBeNull();
    expect(f("r", {}, { modalOpen: true })).toBeNull();
    expect(f("r", {}, { paletteOpen: true })).toBeNull();
    expect(f("r", { metaKey: true })).toBeNull();
    expect(f("r", { ctrlKey: true })).toBeNull();
    expect(f("r", { shiftKey: true })).toBeNull();
    for (const view of ["history", "dashboard", "goals"] as const) expect(f("r", {}, { view })).toBeNull();
    expect(shortcutFor({ ...key("r"), repeat: true }, ctx())).toBeNull();
  });

  it("ignores_unmapped_keys_and_left_right", () => {
    expect(f("x")).toBeNull();
    expect(f("ArrowLeft")).toBeNull();
    expect(f("ArrowRight")).toBeNull();
    expect(f("Escape")).toBeNull();
    expect(f("Delete")).toBeNull();
  });

  it("never_fires_plain_keys_while_typing", () => {
    for (const k of ["n", "t", "Enter", " ", "ArrowUp", "ArrowDown", "f", "?", "r"]) {
      expect(f(k, {}, { typing: true })).toBeNull();
    }
  });

  it("allows_cmd_n_while_typing_but_not_plain_n", () => {
    expect(f("n", { metaKey: true }, { typing: true })).toBe("quickAdd");
    expect(f("n", {}, { typing: true })).toBeNull();
  });

  it("cmd_n_uses_the_platform_modifier", () => {
    expect(f("n", { ctrlKey: true }, { isMac: false })).toBe("quickAdd");
    expect(f("n", { metaKey: true }, { isMac: false })).toBeNull();
    expect(f("n", { ctrlKey: true }, { isMac: true })).toBeNull();
    expect(f("n", { metaKey: true, shiftKey: true })).toBeNull();
    expect(f("n", { metaKey: true, altKey: true })).toBeNull();
  });

  it("ignores_plain_keys_with_modifiers", () => {
    for (const mods of [{ metaKey: true }, { ctrlKey: true }, { altKey: true }]) {
      expect(f("t", mods)).toBeNull();
      expect(f("f", mods)).toBeNull();
      expect(f("ArrowUp", mods)).toBeNull();
      expect(f(" ", mods)).toBeNull();
    }
    expect(f("t", { shiftKey: true })).toBeNull();
    expect(f("ArrowDown", { shiftKey: true })).toBeNull();
  });

  it("stands_down_while_dragging_or_with_the_palette_or_a_modal_open", () => {
    for (const c of [{ dragging: true }, { paletteOpen: true }, { modalOpen: true }]) {
      expect(f("n", {}, c)).toBeNull();
      expect(f("n", { metaKey: true }, c)).toBeNull();
      expect(f(" ", {}, c)).toBeNull();
      expect(f("?", {}, c)).toBeNull();
    }
  });

  it("on_the_focus_screen_only_space_works", () => {
    expect(f(" ", {}, { focusOpen: true })).toBe("toggleTimer");
    for (const k of ["n", "t", "Enter", "ArrowUp", "ArrowDown", "f", "?"]) expect(f(k, {}, { focusOpen: true })).toBeNull();
    expect(f("n", { metaKey: true }, { focusOpen: true })).toBeNull();
    expect(f(" ", {}, { focusOpen: true, typing: true })).toBeNull();
  });

  it("enter_and_space_leave_focused_controls_alone", () => {
    expect(f("Enter", {}, { onControl: true })).toBeNull();
    expect(f(" ", {}, { onControl: true })).toBeNull();
    expect(f("t", {}, { onControl: true })).toBe("today");
  });

  it("block_keys_only_work_in_the_day_view", () => {
    for (const view of ["history", "dashboard", "goals"] as const) {
      for (const k of ["Enter", " ", "ArrowUp", "ArrowDown", "f", "r"]) expect(f(k, {}, { view })).toBeNull();
      expect(f("n", {}, { view })).toBe("quickAdd");
      expect(f("t", {}, { view })).toBe("today");
      expect(f("?", {}, { view })).toBe("help");
    }
  });

  it("ignores_auto_repeat", () => {
    expect(shortcutFor({ ...key(" "), repeat: true }, ctx())).toBeNull();
    expect(shortcutFor({ ...key("n"), repeat: true }, ctx())).toBeNull();
  });
});

describe("stepBlock", () => {
  const blocks = [
    { key: "b", start: 600, end: 660 },
    { key: "a", start: 540, end: 600 },
    { key: "c", start: 900, end: 960 },
  ];
  it("steps_by_start_time", () => {
    expect(stepBlock(blocks, "a", 1)).toBe("b");
    expect(stepBlock(blocks, "b", 1)).toBe("c");
    expect(stepBlock(blocks, "c", -1)).toBe("b");
  });
  it("stays_at_the_ends", () => {
    expect(stepBlock(blocks, "c", 1)).toBe("c");
    expect(stepBlock(blocks, "a", -1)).toBe("a");
  });
  it("starts_from_the_edge_when_nothing_is_selected", () => {
    expect(stepBlock(blocks, null, 1)).toBe("a");
    expect(stepBlock(blocks, null, -1)).toBe("c");
    expect(stepBlock(blocks, "gone", 1)).toBe("a");
  });
  it("returns_null_without_blocks", () => {
    expect(stepBlock([], null, 1)).toBeNull();
  });
});

describe("shortcutRows", () => {
  it("uses_cmd_on_mac_and_ctrl_elsewhere", () => {
    const mac = shortcutRows(true).flatMap((r) => r.keys);
    const other = shortcutRows(false).flatMap((r) => r.keys);
    expect(mac).toContain("⌘");
    expect(mac).not.toContain("Ctrl");
    expect(other).toContain("Ctrl");
    expect(other).not.toContain("⌘");
  });
  it("lists_the_new_shortcuts", () => {
    const labels = shortcutRows(true).map((r) => r.label).join("|");
    for (const word of ["Quick-add", "today", "timer", "focus screen", "shortcuts", "Rename"]) expect(labels.toLowerCase()).toContain(word.toLowerCase());
  });
});
