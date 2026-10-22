import { describe, expect, it } from "vitest";
import { isDashboardShortcut, isFocusShortcut, isGoalsShortcut, isSearchShortcut } from "./keys";

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
