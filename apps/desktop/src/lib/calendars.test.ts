import { describe, expect, it } from "vitest";
import type { Calendar, GoogleAccount } from "../api";
import { calendarGroups, newEventGroups } from "./calendars";

const cal = (id: number, name: string, account: string | null, over: Partial<Calendar> = {}): Calendar => ({
  id,
  googleCalendarId: `g${id}`,
  name,
  color: "#3d6fb6",
  showOnTimeline: true,
  isPrimary: false,
  account,
  archived: false,
  ...over,
});
const acc = (email: string): GoogleAccount => ({ email, needsReconnect: false });
const accounts = [acc("me@home.com"), acc("me@work.com")];

describe("calendarGroups", () => {
  it("groups_by_account_in_connection_order_primary_first_then_by_name", () => {
    const groups = calendarGroups(
      [cal(1, "Work B", "me@work.com"), cal(2, "Home B", "me@home.com"), cal(3, "Work", "me@work.com", { isPrimary: true }), cal(4, "Home A", "me@home.com")],
      accounts,
    );
    expect(groups.map((g) => g.account)).toEqual(["me@home.com", "me@work.com"]);
    expect(groups[0].calendars.map((c) => c.name)).toEqual(["Home A", "Home B"]);
    expect(groups[1].calendars.map((c) => c.name)).toEqual(["Work", "Work B"]);
  });

  it("hides_archived_calendars_and_empty_groups", () => {
    const groups = calendarGroups([cal(1, "Old", "me@home.com", { archived: true }), cal(2, "Work", "me@work.com")], accounts);
    expect(groups.map((g) => g.account)).toEqual(["me@work.com"]);
  });

  it("disconnected_accounts_follow_the_connected_ones_and_unknown_comes_last", () => {
    const groups = calendarGroups([cal(1, "Loose", null), cal(2, "Gone", "x@y.com"), cal(3, "Home", "me@home.com")], accounts);
    expect(groups.map((g) => g.account)).toEqual(["me@home.com", "x@y.com", null]);
  });
});

describe("newEventGroups", () => {
  it("offers_live_calendars_of_connected_accounts_only", () => {
    const groups = newEventGroups(
      [
        cal(1, "Home", "me@home.com", { isPrimary: true }),
        cal(2, "Old", "me@home.com", { archived: true }),
        cal(3, "Gone", "x@y.com"),
        cal(4, "Loose", null),
        cal(5, "Work", "me@work.com"),
      ],
      accounts,
    );
    expect(groups.map((g) => [g.account, g.calendars.map((c) => c.googleCalendarId)])).toEqual([
      ["me@home.com", ["g1"]],
      ["me@work.com", ["g5"]],
    ]);
  });

  it("is_empty_without_accounts", () => {
    expect(newEventGroups([cal(1, "Home", "me@home.com")], [])).toEqual([]);
  });
});
