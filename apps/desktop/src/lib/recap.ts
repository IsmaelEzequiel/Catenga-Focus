/** Pure helpers for the "Today" recap: what got done on one day (finished session or note), with time and a note teaser. */
import type { Block, TimerStatus } from "../api";
import { fmtDuration } from "./dashboard";
import { liveFocusMinutes } from "./goals";
import { bodyToNote } from "./note";
import { fmtRange } from "./search";
import { blockTotalMinutes, sameBlock, sessionsForBlock, type Session } from "./timer";

export const FIRST_LINE_MAX = 140;

export interface RecapItem {
  key: string;
  start: number;
  end: number;
  title: string;
  tags: string[];
  minutes: number;
  firstLine: string;
}

export interface Recap {
  done: RecapItem[];
  totalMinutes: number;
  doneCount: number;
  otherCount: number;
}

/** `#`, `-`, `*`, `>`, `1.` and a task checkbox, in that order of nesting (`- [x] ` -> text). */
const PREFIX = /^(?:#{1,6}\s+|[-*>]\s+|\d+[.)]\s+)*(?:\[[ xX]\]\s+)?/;

/** First non-empty line of a note, markdown prefixes stripped, at most 140 characters. */
export function firstLine(note: string): string {
  for (const raw of note.split("\n")) {
    const line = raw.trim().replace(PREFIX, "").trim();
    if (line === "") continue;
    const chars = [...line];
    return chars.length > FIRST_LINE_MAX ? `${chars.slice(0, FIRST_LINE_MAX - 1).join("")}…` : line;
  }
  return "";
}

/** The running timer's focus minutes so far, only when it runs on `date`. */
const liveFor = (status: TimerStatus, nowMs: number, date: string): number =>
  status.state === "running" && status.date === date ? liveFocusMinutes(status, nowMs) : 0;

export function buildRecap(blocks: (Block & { key: string })[], sessions: Session[], status: TimerStatus, nowMs: number, date: string): Recap {
  const live = liveFor(status, nowMs, date);
  const done: RecapItem[] = [];
  for (const b of blocks) {
    const mine = sessionsForBlock(sessions, b);
    const note = bodyToNote(b.body);
    const liveHere = status.state === "running" && live > 0 && sameBlock(status.block, b) ? live : 0;
    const hasSession = mine.some((s) => s.end !== null);
    if (!hasSession && note.trim() === "" && liveHere === 0) continue;
    done.push({
      key: b.key,
      start: b.start,
      end: b.end,
      title: b.title,
      tags: b.tags,
      minutes: blockTotalMinutes(mine) + liveHere,
      firstLine: firstLine(note),
    });
  }
  done.sort((a, b) => a.start - b.start || a.end - b.end);
  return {
    done,
    totalMinutes: blockTotalMinutes(sessions) + live,
    doneCount: done.length,
    otherCount: blocks.length - done.length,
  };
}

/** Markdown to paste elsewhere: a heading, one bullet per done block, a total line. */
export function toMarkdown(recap: Recap, dateLabel: string): string {
  const head = `## Today — ${dateLabel}\n\n`;
  if (recap.done.length === 0) return `${head}Nothing done.\n`;
  const rows = recap.done.map((d) => {
    const tags = d.tags.map((t) => ` #${t}`).join("");
    const time = d.minutes > 0 ? ` · ${fmtDuration(d.minutes)}` : "";
    const line = d.firstLine ? ` — ${d.firstLine}` : "";
    return `- ${fmtRange(d.start, d.end)} ${d.title.trim() || "Untitled"}${tags}${time}${line}`;
  });
  return `${head}${rows.join("\n")}\n\nTotal: ${fmtDuration(recap.totalMinutes)}\n`;
}
