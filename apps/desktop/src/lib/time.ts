export const PX_PER_HOUR = 64;
export const PX_PER_MIN = PX_PER_HOUR / 60;
export const SNAP = 15;
export const DAY_MINUTES = 24 * 60;

export interface Range {
  start: number;
  end: number;
}

const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), hi);

export const snap = (minutes: number) => Math.round(minutes / SNAP) * SNAP;

export const minuteFromY = (y: number) => y / PX_PER_MIN;

export function fmtMinutes(minutes: number): string {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

/** Shifts a block by `delta` minutes, keeping its duration inside [0, 24:00]. */
export function clampMove(start: number, end: number, delta: number): Range {
  const duration = end - start;
  const s = clamp(start + delta, 0, DAY_MINUTES - duration);
  return { start: s, end: s + duration };
}

/** Moves one edge to `minute`, keeping at least SNAP minutes and the day bounds. */
export function clampResize(
  edge: "start" | "end",
  start: number,
  end: number,
  minute: number,
): Range {
  if (edge === "start") return { start: clamp(minute, 0, end - SNAP), end };
  return { start, end: clamp(minute, start + SNAP, DAY_MINUTES) };
}

/** Range for a create-drag: earlier edge floors, later edge ceils, min SNAP minutes. */
export function createRange(a: number, b: number): Range {
  const lo = clamp(Math.floor(Math.min(a, b) / SNAP) * SNAP, 0, DAY_MINUTES - SNAP);
  const hi = clamp(Math.ceil(Math.max(a, b) / SNAP) * SNAP, lo + SNAP, DAY_MINUTES);
  return { start: lo, end: hi };
}

/** Local-time YYYY-MM-DD (never UTC). */
export function toDateString(d: Date): string {
  const y = String(d.getFullYear()).padStart(4, "0");
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export function addDays(date: string, n: number): string {
  const [y, m, d] = date.split("-").map(Number);
  return toDateString(new Date(y, m - 1, d + n));
}

export function fmtDateLabel(date: string): string {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: "numeric",
  });
}

/** Compact label for lists, e.g. "Tue, Oct 20, 2026". */
export function fmtDateShort(date: string): string {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

/** Header date, short so it never wraps: "Tue, Oct 27" (the year only when it isn't this year). */
export function fmtDateHeader(date: string, today: string): string {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    year: today.slice(0, 4) === date.slice(0, 4) ? undefined : "numeric",
  });
}
