/** Pure helpers for making a block's Google event repeat (`docs/plans/M16-repeat.md`). */
import type { Repeat, RepeatFreq } from "../api";
import { fmtDateShort } from "./time";

export type { Repeat, RepeatFreq };

/** Monday first, like the rule sent to Google (0 = Monday … 6 = Sunday). */
export const WEEKDAYS = [
  { short: "M", name: "Mon" },
  { short: "T", name: "Tue" },
  { short: "W", name: "Wed" },
  { short: "T", name: "Thu" },
  { short: "F", name: "Fri" },
  { short: "S", name: "Sat" },
  { short: "S", name: "Sun" },
] as const;

/** 0 = Monday … 6 = Sunday, for a `YYYY-MM-DD` date. */
export function weekdayOf(date: string): number {
  const [y, m, d] = date.split("-").map(Number);
  return (new Date(y, m - 1, d).getDay() + 6) % 7;
}

/** Daily, forever; "weekly" starts on the block's own weekday. */
export const defaultRepeat = (date: string): Repeat => ({ freq: "daily", days: [weekdayOf(date)], until: null });

/** Turns a day on or off for "weekly", keeping Monday-first order. */
export function toggleDay(days: number[], day: number): number[] {
  return days.includes(day) ? days.filter((d) => d !== day) : [...days, day].sort((a, b) => a - b);
}

/** Why the rule can't be saved yet, or null. `until === ""` means "On" with no date picked. */
export function repeatProblem(repeat: Repeat, date: string): string | null {
  if (repeat.freq === "weekly" && repeat.days.length === 0) return "Pick at least one day.";
  if (repeat.until === "") return "Pick the last day.";
  if (repeat.until !== null && repeat.until < date) return "The last day is before this block.";
  return null;
}

/** What is sent: `days` only matter for "weekly". */
export const repeatRequest = (repeat: Repeat): Repeat => ({ ...repeat, days: repeat.freq === "weekly" ? repeat.days : [] });

/** "Daily", "Every weekday", "Weekly on Mon, Wed" + ", until Tue, Dec 31, 2026" or ", forever". */
export function describeRepeat(repeat: Repeat): string {
  const how =
    repeat.freq === "daily"
      ? "Daily"
      : repeat.freq === "weekdays"
        ? "Every weekday"
        : `Weekly on ${repeat.days.map((d) => WEEKDAYS[d].name).join(", ")}`;
  return repeat.until ? `${how}, until ${fmtDateShort(repeat.until)}` : `${how}, forever`;
}
