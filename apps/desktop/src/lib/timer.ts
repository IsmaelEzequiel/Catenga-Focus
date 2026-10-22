/** Pure helpers for the timer UI (see `docs/plans/M6-timer.md` §1.5-§1.7). */
import type { BlockRef, Session } from "../api";
import { DAY_MINUTES, fmtMinutes } from "./time";

export type { BlockRef, Session };

/** The block fields a session snapshot is matched on. */
interface Slot {
  start: number;
  end: number;
  title: string;
}

interface RefLike extends Slot {
  tags: string[];
}

/** `project` = project key (`series:…` / `event:…`) of the block's matched event. */
export const toBlockRef = (b: RefLike, project: string | null = null): BlockRef => ({
  start: b.start,
  end: b.end,
  title: b.title.trim(),
  tags: b.tags,
  project,
});

/** Free timer display: H:MM:SS. */
export function fmtElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

/** Pomodoro display: MM:SS, partial seconds count as a full one. */
export function fmtRemaining(ms: number): string {
  const total = Math.max(0, Math.ceil(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

const minuteOfDay = (d: Date) => d.getHours() * 60 + d.getMinutes();
const clock = (iso: string) => fmtMinutes(minuteOfDay(new Date(iso)));

/** `09:05–09:50 · 45 min · pomodoro` (local times); an open session is marked `· not stopped`. */
export function sessionLine(s: Session): string {
  const line = `${clock(s.start)}–${s.end ? clock(s.end) : ""} · ${s.minutes} min · ${s.mode}`;
  return s.end ? line : `${line} · not stopped`;
}

/** Sessions to list in the panel: all but the running one (an open one that isn't running was left by a crash and can be deleted). */
export const listedSessions = (sessions: Session[], runningStart: string | null): Session[] =>
  sessions.filter((s) => s.end !== null || s.start !== runningStart);

/** Whether a session snapshot (or timer block) stands for this block: start, end and trimmed title. */
export const sameBlock = (a: Slot, b: Slot) => a.start === b.start && a.end === b.end && a.title.trim() === b.title.trim();

/** Sessions whose snapshot equals the block (start, end, trimmed title), in the given order. */
export const sessionsForBlock = (sessions: Session[], block: Slot): Session[] =>
  sessions.filter((s) => sameBlock(s.block, block));

export const blockHasSessions = (sessions: Session[], block: Slot): boolean => sessions.some((s) => sameBlock(s.block, block));

/** Total of the finished sessions. */
export const blockTotalMinutes = (sessions: Session[]): number =>
  sessions.reduce((sum, s) => (s.end ? sum + s.minutes : sum), 0);

export interface SessionBar {
  start: number;
  end: number;
  running: boolean;
}

const MIN_BAR = 2;

/** Sessions of `date` as minute ranges for the timeline gutter; the running one ends at `nowMs`, other open ones after their `minutes`. */
export function sessionBars(sessions: Session[], date: string, nowMs: number, runningStart: string | null): SessionBar[] {
  const [y, m, d] = date.split("-").map(Number);
  const dayStart = new Date(y, m - 1, d).getTime();
  const bars: SessionBar[] = [];
  for (const s of sessions) {
    const startMs = new Date(s.start).getTime();
    const running = s.end === null && s.start === runningStart;
    const endMs = s.end ? new Date(s.end).getTime() : running ? nowMs : startMs + s.minutes * 60_000;
    const start = (startMs - dayStart) / 60_000;
    if (start < 0 || start >= DAY_MINUTES) continue;
    const end = Math.min(DAY_MINUTES, Math.max((endMs - dayStart) / 60_000, start + MIN_BAR));
    bars.push({ start: Math.floor(start), end: Math.ceil(end), running });
  }
  return bars;
}

interface Keyed extends RefLike {
  key: string;
}

const sameRef = (a: BlockRef, b: BlockRef) =>
  a.start === b.start && a.end === b.end && a.title === b.title && a.tags.length === b.tags.length && a.tags.every((t, i) => t === b.tags[i]);

/** Blocks present in both lists (by key) whose start/end/title/tags changed. */
export function sessionRelinks(prev: Keyed[], next: Keyed[]): { from: BlockRef; to: BlockRef }[] {
  const before = new Map(prev.map((b) => [b.key, b]));
  const links: { from: BlockRef; to: BlockRef }[] = [];
  for (const b of next) {
    const old = before.get(b.key);
    if (!old) continue;
    const from = toBlockRef(old);
    const to = toBlockRef(b);
    if (!sameRef(from, to)) links.push({ from, to });
  }
  return links;
}
