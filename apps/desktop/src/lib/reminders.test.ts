import { describe, expect, it } from "vitest";
import { DEFAULT_POPUP_MINUTES, reminderMinutes, reminderOfKind } from "./reminders";

describe("reminderOfKind", () => {
  it("picking_a_notification_starts_at_the_usual_minutes", () => {
    expect(reminderOfKind("popup", { kind: "default" })).toEqual({ kind: "popup", minutes: DEFAULT_POPUP_MINUTES });
  });

  it("picking_a_notification_again_keeps_its_minutes", () => {
    expect(reminderOfKind("popup", { kind: "popup", minutes: 30 })).toEqual({ kind: "popup", minutes: 30 });
  });

  it("none_and_googles_default_carry_no_minutes", () => {
    expect(reminderOfKind("none", { kind: "popup", minutes: 30 })).toEqual({ kind: "none" });
    expect(reminderOfKind("default", { kind: "none" })).toEqual({ kind: "default" });
  });

  it("an_unknown_choice_falls_back_to_googles_default", () => {
    expect(reminderOfKind("email", { kind: "none" })).toEqual({ kind: "default" });
  });
});

describe("reminderMinutes", () => {
  it("reads_whole_minutes_including_zero_for_at_the_start", () => {
    expect(reminderMinutes("15")).toBe(15);
    expect(reminderMinutes(" 0 ")).toBe(0);
  });

  it("refuses_what_is_not_a_whole_number_of_minutes", () => {
    for (const text of ["", "abc", "-5", "1.5"]) expect(reminderMinutes(text)).toBeNull();
  });

  it("stops_at_googles_limit_of_four_weeks", () => {
    expect(reminderMinutes("99999")).toBe(40320);
  });
});
