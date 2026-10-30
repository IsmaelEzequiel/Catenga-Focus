/** Pure helpers grouping Google calendars by account (see `docs/plans/M15-calendars-settings.md` §1.5). */
import type { Calendar, GoogleAccount } from "../api";

export interface CalendarGroup {
  /** Email of the owning account; null for calendars whose account is unknown (until the next refresh). */
  account: string | null;
  calendars: Calendar[];
}

const byPrimaryThenName = (a: Calendar, b: Calendar) =>
  Number(b.isPrimary) - Number(a.isPrimary) || a.name.localeCompare(b.name, undefined, { sensitivity: "base" });

/** Live calendars grouped by account: connection order, then other accounts, then the unknown (null) one last. */
export function calendarGroups(calendars: Calendar[], accounts: GoogleAccount[]): CalendarGroup[] {
  const live = calendars.filter((c) => !c.archived);
  const order = accounts.map((a) => a.email);
  const others = [...new Set(live.map((c) => c.account).filter((a): a is string => a !== null && !order.includes(a)))];
  const keys: Array<string | null> = [...order, ...others, null];
  return keys
    .map((account) => ({ account, calendars: live.filter((c) => c.account === account).sort(byPrimaryThenName) }))
    .filter((g) => g.calendars.length > 0);
}

/** The "New blocks go to" options: live calendars of connected accounts, grouped by account. */
export function newEventGroups(calendars: Calendar[], accounts: GoogleAccount[]): CalendarGroup[] {
  const connected = new Set(accounts.map((a) => a.email));
  return calendarGroups(calendars, accounts).filter((g) => g.account !== null && connected.has(g.account));
}
