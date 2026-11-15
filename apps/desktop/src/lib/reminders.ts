import type { Reminder } from "../api";

/** Minutes a notification starts with when it is first picked. */
export const DEFAULT_POPUP_MINUTES = 10;
/** The longest reminder Google accepts: 4 weeks. */
export const MAX_REMINDER_MINUTES = 40320;

/** The reminder for the kind picked in Settings; a notification keeps the minutes it already had. */
export function reminderOfKind(kind: string, current: Reminder): Reminder {
  if (kind === "none") return { kind: "none" };
  if (kind === "popup") {
    return { kind: "popup", minutes: current.kind === "popup" ? current.minutes : DEFAULT_POPUP_MINUTES };
  }
  return { kind: "default" };
}

/** The minutes typed in Settings: a whole number from 0 (at the start) up to Google's limit; null when it isn't one. */
export function reminderMinutes(text: string): number | null {
  const t = text.trim();
  if (!/^\d+$/.test(t)) return null;
  return Math.min(Number(t), MAX_REMINDER_MINUTES);
}
