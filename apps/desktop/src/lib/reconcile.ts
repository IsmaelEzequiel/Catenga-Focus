import type { Block } from "../api";

export type UiBlock = Block & { key: string };

/** A note edited here whose file version diverged; `mine` is the text the user typed. */
export interface Conflict {
  key: string;
  mine: UiBlock;
}

export interface MergeResult {
  /** The on-disk blocks as they are, keyed; the new "last saved" state. */
  disk: UiBlock[];
  /** `disk` with my note edits applied where that is safe. */
  blocks: UiBlock[];
  conflicts: Conflict[];
  /** Unsaved local move/resize/retitle/add/delete that the disk layout overrode. */
  structureDropped: boolean;
  /** `blocks` differs from the disk blocks, so a follow-up save is needed. */
  changed: boolean;
}

const identity = (b: Block) => JSON.stringify([b.start, b.end, b.title, b.tags]);
const span = (b: Block) => `${b.start}-${b.end}`;

/**
 * Picks a key for each `next` block from `prev`: `preset` (index -> key) first, then exact
 * `(start, end, title, tags)` matches in order (duplicates one-to-one), then `(start, end)`;
 * the rest get fresh keys.
 */
function matchKeys(prev: UiBlock[], next: Block[], preset: Map<number, string>): UiBlock[] {
  const keys: (string | undefined)[] = next.map((_, i) => preset.get(i));
  const used = new Set<string>(preset.values());
  const pass = (same: (a: Block, b: Block) => boolean) => {
    next.forEach((n, i) => {
      if (keys[i] !== undefined) return;
      const hit = prev.find((p) => !used.has(p.key) && same(p, n));
      if (hit) {
        keys[i] = hit.key;
        used.add(hit.key);
      }
    });
  };
  pass((a, b) => identity(a) === identity(b));
  pass((a, b) => span(a) === span(b));
  return next.map((n, i) => ({ ...n, key: keys[i] ?? crypto.randomUUID() }));
}

export function reconcileKeys(prev: UiBlock[], next: Block[]): UiBlock[] {
  return matchKeys(prev, next, new Map());
}

/** Keeps the typed note text of `local` on top of `saved`'s structure (matched by key). */
export function overlayNotes(saved: UiBlock[], local: UiBlock[]): UiBlock[] {
  const bodies = new Map(local.map((b) => [b.key, b.body]));
  return saved.map((b) => (bodies.has(b.key) ? { ...b, body: bodies.get(b.key) as string } : b));
}

/**
 * Three-way merge of note edits after the file changed under us.
 * `base` = last state known to match the disk, `local` = what the user has now, `disk` = file now.
 * Never drops text: a note that cannot be merged cleanly becomes a conflict carrying my version.
 */
export function mergeConflict(base: UiBlock[], local: UiBlock[], disk: Block[]): MergeResult {
  const baseByKey = new Map(base.map((b) => [b.key, b]));

  // Disk counterpart of every local block that existed in base, by base identity, one-to-one.
  const counterpart = new Map<string, number>();
  const taken = new Set<number>();
  for (const l of local) {
    const b = baseByKey.get(l.key);
    if (!b) continue;
    const idx = disk.findIndex((d, i) => !taken.has(i) && identity(d) === identity(b));
    if (idx >= 0) {
      counterpart.set(l.key, idx);
      taken.add(idx);
    }
  }
  const preset = new Map<number, string>([...counterpart].map(([key, idx]) => [idx, key]));
  const diskBlocks = matchKeys(local, disk, preset);
  const blocks = diskBlocks.map((b) => ({ ...b }));

  const conflicts: Conflict[] = [];
  let changed = false;
  for (const l of local) {
    const b = baseByKey.get(l.key);
    if (!b) {
      // Not saved yet; its layout is dropped, but typed text must not be.
      if (l.body.trim() !== "") conflicts.push({ key: l.key, mine: l });
      continue;
    }
    if (l.body === b.body) continue;
    const idx = counterpart.get(l.key);
    if (idx === undefined) {
      conflicts.push({ key: l.key, mine: l });
      continue;
    }
    const theirs = diskBlocks[idx];
    if (theirs.body === l.body) continue;
    if (theirs.body === b.body) {
      blocks[idx] = { ...theirs, body: l.body };
      changed = true;
    } else {
      conflicts.push({ key: l.key, mine: l });
    }
  }

  const shape = (list: Block[]) => list.map(identity).sort().join("|");
  return { disk: diskBlocks, blocks, conflicts, structureDropped: shape(local) !== shape(base), changed };
}
