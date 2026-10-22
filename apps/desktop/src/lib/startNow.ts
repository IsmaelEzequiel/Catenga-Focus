/** What "Start now" starts today (`CLAUDE.md` feature 15). Pure; the caller passes today's blocks and events. */
import type { CalEvent } from "../api";
import type { UiBlock } from "../hooks/useDay";
import { ghostEvents } from "./calendar";

export type StartNowTarget = { kind: "block"; key: string } | { kind: "event"; event: CalEvent };

/**
 * The block (or calendar ghost) containing `nowMinute`, else the soonest one starting later today.
 * Ghosts count like blocks; a block wins over a ghost starting at the same minute.
 */
export function startNowTarget(blocks: UiBlock[], events: CalEvent[], nowMinute: number): StartNowTarget | null {
  const items: { start: number; end: number; target: StartNowTarget; rank: number }[] = [
    ...blocks.map((b) => ({ start: b.start, end: b.end, target: { kind: "block", key: b.key } as StartNowTarget, rank: 0 })),
    ...ghostEvents(blocks, events).map((e) => ({ start: e.start, end: e.end, target: { kind: "event", event: e } as StartNowTarget, rank: 1 })),
  ];
  const order = (a: (typeof items)[number], b: (typeof items)[number]) => a.start - b.start || a.rank - b.rank;
  const current = items.filter((i) => i.start <= nowMinute && nowMinute < i.end).sort(order);
  if (current.length > 0) return current[0].target;
  const upcoming = items.filter((i) => i.start > nowMinute).sort(order);
  return upcoming[0]?.target ?? null;
}
