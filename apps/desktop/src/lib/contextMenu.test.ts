import { describe, expect, it } from "vitest";
import { blockMenu, ghostMenu, menuTargetKind, shouldSuppressDefault, type MenuEntry } from "./contextMenu";

const ids = (m: MenuEntry[]) => m.map((e) => ("separator" in e ? "---" : e.id));
const texts = (m: MenuEntry[]) => m.map((e) => ("separator" in e ? "---" : e.text));
const base = { isToday: true, timingHere: false, hasEvent: true, canDeleteEvents: true };

describe("blockMenu", () => {
  it("full_menu_today_with_event", () => {
    const m = blockMenu(base);
    expect(ids(m)).toEqual(["open", "start", "---", "delete-block", "delete-event"]);
    expect(texts(m)).toEqual(["Open note", "Start timer", "---", "Delete block", "Delete event from Google Calendar"]);
  });

  it("stop_replaces_start_when_running_here", () => {
    expect(texts(blockMenu({ ...base, timingHere: true }))[1]).toBe("Stop timer");
    expect(ids(blockMenu({ ...base, timingHere: true }))[1]).toBe("stop");
  });

  it("no_start_on_other_days", () => {
    expect(ids(blockMenu({ ...base, isToday: false }))).toEqual(["open", "---", "delete-block", "delete-event"]);
  });

  it("stop_still_offered_when_running_on_a_non_today_view", () => {
    expect(ids(blockMenu({ ...base, isToday: false, timingHere: true }))).toContain("stop");
  });

  it("event_delete_needs_google_and_a_matching_event", () => {
    expect(ids(blockMenu({ ...base, hasEvent: false }))).toEqual(["open", "start", "---", "delete-block"]);
    expect(ids(blockMenu({ ...base, canDeleteEvents: false }))).toEqual(["open", "start", "---", "delete-block"]);
  });

  it("destructive_items_come_last_after_a_separator", () => {
    const m = ids(blockMenu(base));
    expect(m.indexOf("---")).toBeLessThan(m.indexOf("delete-block"));
    expect(m.slice(-2)).toEqual(["delete-block", "delete-event"]);
  });
});

describe("ghostMenu", () => {
  it("open_and_delete_when_google_connected", () => {
    const m = ghostMenu({ canDeleteEvents: true });
    expect(ids(m)).toEqual(["open", "---", "delete-event"]);
    expect(texts(m)).toEqual(["Open as block", "---", "Delete event from Google Calendar"]);
  });

  it("only_open_without_google", () => {
    expect(ids(ghostMenu({ canDeleteEvents: false }))).toEqual(["open"]);
  });
});

describe("shouldSuppressDefault", () => {
  it("never_for_typing_targets", () => {
    expect(shouldSuppressDefault("typing", true)).toBe(false);
    expect(shouldSuppressDefault("typing", false)).toBe(false);
  });

  it("blocks_and_ghosts_always_use_ours", () => {
    for (const prod of [true, false]) {
      expect(shouldSuppressDefault("block", prod)).toBe(true);
      expect(shouldSuppressDefault("ghost", prod)).toBe(true);
    }
  });

  it("elsewhere_only_in_production", () => {
    expect(shouldSuppressDefault("other", true)).toBe(true);
    expect(shouldSuppressDefault("other", false)).toBe(false);
  });
});

describe("menuTargetKind", () => {
  /** A target whose `closest` answers true only for the selectors it was built with. */
  const target = (...matching: string[]) => ({
    closest: (sel: string) => (matching.some((m) => sel.includes(m)) ? {} : null),
  });
  const TYPING = "textarea";

  it("typing_wins_even_inside_a_block", () => {
    expect(menuTargetKind(target(TYPING, "[data-block]"))).toBe("typing");
  });

  it("ghost_before_block", () => {
    expect(menuTargetKind(target("[data-block]", "[data-ghost]"))).toBe("ghost");
    expect(menuTargetKind(target("[data-block]"))).toBe("block");
  });

  it("other_and_null", () => {
    expect(menuTargetKind(target())).toBe("other");
    expect(menuTargetKind(null)).toBe("other");
  });

  it("selectors_cover_inputs_textareas_selects_and_contenteditable", () => {
    const seen: string[] = [];
    menuTargetKind({ closest: (s) => (seen.push(s), null) });
    const typing = seen.find((s) => s.includes("textarea")) ?? "";
    for (const part of ["input", "textarea", "select", "[contenteditable]"]) expect(typing).toContain(part);
  });
});
