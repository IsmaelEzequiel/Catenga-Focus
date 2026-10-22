/** Pure helpers tying blocks to Google Calendar events (see `docs/plans/M5-calendar.md` §1.6). */
import type { CalEvent } from "../api";
import type { Range } from "./time";

export type { CalEvent };

/** What matching and syncing need from a block. */
export interface CalBlock extends Range {
  key: string;
  title: string;
}

export type CalendarOp =
  | { kind: "create"; block: CalBlock }
  | { kind: "update"; eventId: string; block: CalBlock };

const sameSlot = (b: CalBlock, e: CalEvent) =>
  b.start === e.start && b.end === e.end && b.title.trim() === e.title.trim();

/** block key -> its event. One event matches at most one block (the first by order). */
export function matchBlocks<B extends CalBlock>(blocks: B[], events: CalEvent[]): Map<string, CalEvent> {
  const result = new Map<string, CalEvent>();
  const taken = new Set<string>();
  for (const b of blocks) {
    const event = events.find((e) => !taken.has(e.id) && sameSlot(b, e));
    if (!event) continue;
    taken.add(event.id);
    result.set(b.key, event);
  }
  return result;
}

/** Events not stood for by a block; these are drawn as ghosts. */
export function ghostEvents<B extends CalBlock>(blocks: B[], events: CalEvent[]): CalEvent[] {
  const matched = new Set([...matchBlocks(blocks, events).values()].map((e) => e.id));
  return events.filter((e) => !matched.has(e.id));
}

/** Google writes implied by a block edit; `events` are the day's events before the edit. */
export function calendarOps<B extends CalBlock>(prev: B[], next: B[], events: CalEvent[]): CalendarOp[] {
  const prevMatch = matchBlocks(prev, events);
  const nextMatch = matchBlocks(next, events);
  const before = new Map(prev.map((b) => [b.key, b]));
  const ops: CalendarOp[] = [];
  for (const block of next) {
    if (block.title.trim() === "") continue;
    const old = before.get(block.key);
    const event = prevMatch.get(block.key);
    if (old && event) {
      if (old.start !== block.start || old.end !== block.end || old.title.trim() !== block.title.trim()) {
        ops.push({ kind: "update", eventId: event.id, block });
      }
    } else if ((!old || old.title.trim() === "") && !nextMatch.has(block.key)) {
      ops.push({ kind: "create", block });
    }
  }
  return ops;
}
