import { describe, expect, it } from "vitest";
import { SETTINGS_TABS, moveTab } from "./settingsTabs";

describe("moveTab", () => {
  it("has_the_four_panes_in_order", () => {
    expect(SETTINGS_TABS.map((t) => t.id)).toEqual(["general", "calendars", "projects", "tags"]);
  });
  it("arrows_move_and_wrap", () => {
    expect(moveTab("general", "ArrowDown")).toBe("calendars");
    expect(moveTab("tags", "ArrowDown")).toBe("general");
    expect(moveTab("general", "ArrowUp")).toBe("tags");
    expect(moveTab("projects", "ArrowUp")).toBe("calendars");
  });
  it("home_and_end_jump", () => {
    expect(moveTab("projects", "Home")).toBe("general");
    expect(moveTab("projects", "End")).toBe("tags");
  });
  it("other_keys_do_not_move", () => {
    expect(moveTab("projects", "a")).toBeNull();
  });
});
