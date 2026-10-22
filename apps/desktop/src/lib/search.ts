import type { SearchHit, SearchResponse, Suggestion } from "../api";
import { fmtMinutes } from "./time";

const MARK_START = "\u0002";
const MARK_END = "\u0003";

export interface Segment {
  text: string;
  hit: boolean;
}

/** Splits text on the highlight markers sent by the backend; the result is always rendered as plain text. */
export function splitHighlight(s: string): Segment[] {
  const out: Segment[] = [];
  let hit = false;
  let buf = "";
  const flush = () => {
    if (buf !== "") out.push({ text: buf, hit });
    buf = "";
  };
  for (const ch of s) {
    if (ch === MARK_START) {
      flush();
      hit = true;
    } else if (ch === MARK_END) {
      flush();
      hit = false;
    } else {
      buf += ch;
    }
  }
  flush();
  return out;
}

export interface BlockTarget {
  start: number;
  end: number;
  title: string;
  ord: number;
}

type Resolvable = Omit<BlockTarget, "ord"> & { key: string };

/** Finds the block a search result pointed at, tolerating edits made since it was indexed. */
export function resolveBlockKey(blocks: Resolvable[], t: BlockTarget): string | null {
  const at = blocks[t.ord];
  if (at && at.start === t.start && at.end === t.end) return at.key;
  const sameRange = blocks.filter((b) => b.start === t.start && b.end === t.end);
  if (sameRange.length > 0) return (sameRange.find((b) => b.title === t.title) ?? sameRange[0]).key;
  if (t.title === "") return null;
  return blocks.find((b) => b.title === t.title)?.key ?? null;
}

/** Moves the active row by `delta`, wrapping around. */
export function stepIndex(current: number, delta: number, length: number): number {
  if (length <= 0) return 0;
  return (((current + delta) % length) + length) % length;
}

export type PaletteItem =
  | { kind: "title"; suggestion: Suggestion }
  | { kind: "tag"; suggestion: Suggestion }
  | { kind: "hit"; hit: SearchHit };

export function buildPaletteItems(r: SearchResponse | null): PaletteItem[] {
  if (!r) return [];
  return [
    ...r.titles.map((suggestion): PaletteItem => ({ kind: "title", suggestion })),
    ...r.tags.map((suggestion): PaletteItem => ({ kind: "tag", suggestion })),
    ...r.hits.map((hit): PaletteItem => ({ kind: "hit", hit })),
  ];
}

export const fmtRange = (start: number, end: number) => `${fmtMinutes(start)}–${fmtMinutes(end)}`;
