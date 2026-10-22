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

/** An occurrence of a repeating series: deleting it removes only that occurrence in Google. */
export const isSeriesInstance = (e: CalEvent): boolean => e.projectKey?.startsWith("series:") ?? false;

export type BlockDeletePlan =
  | { kind: "delete" }
  | { kind: "confirm"; event: CalEvent }
  | { kind: "refuse"; reason: "sessions" | "notes"; event: CalEvent | null };

/**
 * What deleting a block does (`docs/plans/M8e-play-delete.md` §1.4). A block with sessions or a note is never deleted
 * (`event` then says whether the Google event can be offered for deletion instead); an empty one backed by an event asks.
 */
export function blockDeletePlan(p: {
  hasSessions: boolean;
  hasNote: boolean;
  event: CalEvent | undefined;
  canDeleteEvent: boolean;
}): BlockDeletePlan {
  const event = p.canDeleteEvent && p.event ? p.event : null;
  if (p.hasSessions) return { kind: "refuse", reason: "sessions", event };
  if (p.hasNote) return { kind: "refuse", reason: "notes", event };
  return event ? { kind: "confirm", event } : { kind: "delete" };
}
