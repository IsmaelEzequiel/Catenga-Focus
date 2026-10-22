/** Pure helpers for the goal Play button (see `docs/plans/M8d-goal-play.md` §1.2). */
import type { CalEvent, GoalProgress } from "../api";
import type { UiBlock } from "../hooks/useDay";
import { ghostEvents, matchBlocks } from "./calendar";
import { matchesGoal } from "./goals";

export type GoalTarget =
  | { kind: "block"; key: string }
  | { kind: "event"; event: CalEvent }
  | { kind: "none" };

interface Span {
  start: number;
  end: number;
}

/** The one containing `now`, else the latest started, else the earliest upcoming. */
function pickNearest<T extends Span>(items: T[], now: number): T | undefined {
  const byStartDesc = (a: T, b: T) => b.start - a.start;
  const current = items.filter((i) => i.start <= now && now < i.end).sort(byStartDesc);
  if (current.length > 0) return current[0];
  const started = items.filter((i) => i.start <= now).sort(byStartDesc);
  if (started.length > 0) return started[0];
  return [...items].sort((a, b) => a.start - b.start)[0];
}

/**
 * What Play should use today: a block that counts for the goal; else, for a project goal, an event of that project
 * with no block yet (a ghost); else nothing (Play never creates a block from nothing).
 */
export function findGoalBlock(
  blocks: UiBlock[],
  events: CalEvent[],
  p: Pick<GoalProgress, "goal" | "project" | "tags">,
  nowMinute: number,
): GoalTarget {
  const matched = matchBlocks(blocks, events);
  const norm = (s: string) => s.trim().toLowerCase();
  const projectOf = (b: UiBlock): string | null => {
    const key = matched.get(b.key)?.projectKey ?? null;
    // A block Play made on a day without the project's event has no project of its own;
    // it carries the project's name, so a second Play reuses it instead of adding another.
    if (key === null && p.project && norm(b.title) === norm(p.project.name)) return p.project.key;
    return key;
  };
  const candidates = blocks.filter((b) => matchesGoal(p, { title: b.title, tags: b.tags, project: projectOf(b) }));
  const block = pickNearest(candidates, nowMinute);
  if (block) return { kind: "block", key: block.key };
  if (p.goal.projectId !== null && p.project) {
    const key = p.project.key;
    const event = pickNearest(
      ghostEvents(blocks, events).filter((e) => e.projectKey === key),
      nowMinute,
    );
    if (event) return { kind: "event", event };
  }
  return { kind: "none" };
}
