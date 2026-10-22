import type { CalendarCell, DashRange, DayCell, TagStat } from "../api";

export const RANGES: { value: DashRange; label: string }[] = [
  { value: "week", label: "This week" },
  { value: "month", label: "This month" },
  { value: "30d", label: "30 days" },
  { value: "year", label: "This year" },
  { value: "all", label: "All time" },
];

export function isValidRange(v: string | null): v is DashRange {
  return RANGES.some((r) => r.value === v);
}

/** `12h 30m`, `2h`, `45m`, `0m`. */
export function fmtDuration(minutes: number): string {
  const total = Math.max(0, Math.round(minutes));
  const h = Math.floor(total / 60);
  const m = total % 60;
  if (h === 0) return `${m}m`;
  return m === 0 ? `${h}h` : `${h}h ${m}m`;
}

/** 0 none, 1 <30m, 2 <1h, 3 <2h, 4 >=2h. */
export function heatBucket(minutes: number): 0 | 1 | 2 | 3 | 4 {
  if (minutes <= 0) return 0;
  if (minutes < 30) return 1;
  if (minutes < 60) return 2;
  if (minutes < 120) return 3;
  return 4;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function parts(date: string): { y: number; m: number; d: number } {
  const [y, m, d] = date.split("-").map(Number);
  return { y, m, d };
}

/** Monday = 0 … Sunday = 6. */
function weekdayIndex(date: string): number {
  const { y, m, d } = parts(date);
  return (new Date(y, m - 1, d).getDay() + 6) % 7;
}

export interface HeatLayout {
  /** Weeks, oldest first; each has 7 slots Monday to Sunday (null = outside the data). */
  columns: (CalendarCell | null)[][];
  months: { column: number; label: string }[];
}

/** Lays the days out as Monday-first week columns, with a month label where a month starts. */
export function heatLayout(days: CalendarCell[]): HeatLayout {
  if (days.length === 0) return { columns: [], months: [] };
  const columns: (CalendarCell | null)[][] = [];
  let current: (CalendarCell | null)[] = Array.from({ length: weekdayIndex(days[0].date) }, () => null);
  for (const day of days) {
    current.push(day);
    if (current.length === 7) {
      columns.push(current);
      current = [];
    }
  }
  if (current.length > 0) {
    while (current.length < 7) current.push(null);
    columns.push(current);
  }

  const months: { column: number; label: string }[] = [];
  columns.forEach((col, i) => {
    const first = col.find((c): c is CalendarCell => c !== null);
    if (!first) return;
    const starter = col.find((c) => c !== null && parts(c.date).d === 1);
    const source = starter ?? (i === 0 ? first : null);
    if (!source) return;
    const last = months[months.length - 1];
    if (last && i - last.column < 3) return;
    months.push({ column: i, label: MONTHS[parts(source.date).m - 1] });
  });
  return { columns, months };
}

/** Width of a bar in percent of the track; any non-zero value stays visible. */
export function barPercent(value: number, max: number): number {
  if (value <= 0 || max <= 0) return 0;
  return Math.min(100, Math.max(2, (value / max) * 100));
}

/** `Wed 21 Oct`. */
export function fmtDayShort(date: string): string {
  const { m, d } = parts(date);
  return `${WEEKDAYS[weekdayIndex(date)]} ${d} ${MONTHS[m - 1]}`;
}

export function cellLabel(date: string, minutes: number): string {
  return `${fmtDayShort(date)} · ${minutes > 0 ? fmtDuration(minutes) : "no activity"}`;
}

export function habitCellLabel(cell: DayCell): string {
  const when = fmtDayShort(cell.date);
  if (cell.minutes > 0 && cell.note) return `${when} · ${fmtDuration(cell.minutes)} and a note`;
  if (cell.minutes > 0) return `${when} · ${fmtDuration(cell.minutes)}`;
  if (cell.note) return `${when} · note only`;
  return `${when} · not done`;
}

/** True when a session is counted under more than one group (it has several tags). */
export function countsSessionsTwice(tags: TagStat[], totalSessions: number): boolean {
  return tags.reduce((sum, t) => sum + t.sessions, 0) > totalSessions;
}

/** The tag expanded on first render: the first (largest) one. */
export function defaultOpenTag(tags: TagStat[]): string | null {
  return tags.length > 0 ? tags[0].key : null;
}
