const PALETTE = ["sun", "clay", "leaf", "sky"] as const;
const TAG = /^#[\p{Alphabetic}\p{N}_-]+$/u;

/** Peels trailing `#tag` tokens off the input, same rule as the Rust parser. */
export function parseTitleInput(text: string): { title: string; tags: string[] } {
  let rest = text.trim();
  const tags: string[] = [];
  for (;;) {
    const m = /\s\S*$/.exec(rest);
    const token = m ? m[0].trim() : rest;
    if (!TAG.test(token)) break;
    tags.unshift(token.slice(1));
    rest = m ? rest.slice(0, m.index).trimEnd() : "";
  }
  return { title: rest, tags };
}

export function formatTitleInput(title: string, tags: string[]): string {
  return [title, ...tags.map((t) => `#${t}`)].filter((s) => s !== "").join(" ");
}

function hash(s: string): number {
  let h = 0;
  for (const ch of s) h = (h * 31 + ch.codePointAt(0)!) >>> 0;
  return h;
}

/** CSS color for a block: first mapped tag, else a stable pick from the first tag; null if untagged. */
export function tagColorVar(tags: string[], tagColors: Record<string, string>): string | null {
  if (tags.length === 0) return null;
  for (const tag of tags) {
    const name = tagColors[tag];
    if ((PALETTE as readonly string[]).includes(name)) return `var(--color-tag-${name})`;
  }
  return `var(--color-tag-${PALETTE[hash(tags[0]) % PALETTE.length]})`;
}

/** The text-safe variant of a `tagColorVar` result (`var(--color-tag-sun)` → `var(--color-tag-sun-ink)`). */
export const tagInkOf = (colorVar: string) => colorVar.replace(/^var\(--color-tag-([a-z]+)\)$/, "var(--color-tag-$1-ink)");

const TAG_NAME = /^[\p{Alphabetic}\p{N}_-]+$/u;
const MAX_SUGGESTIONS = 6;
const normName = (s: string) => s.toLowerCase();

/** Whether `name` (without `#`) can be written in a heading. */
export const isValidTagName = (name: string) => TAG_NAME.test(name);

/** Free text to tag names: split on spaces/commas, `#` stripped, invalid tokens dropped. */
export function parseTagChips(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((t) => t.replace(/^#+/, ""))
    .filter(isValidTagName);
}

/** Adds the tags typed in `text`, skipping ones already present (case-insensitive). */
export function addTagChips(current: string[], text: string): string[] {
  const seen = new Set(current.map(normName));
  const next = [...current];
  for (const name of parseTagChips(text)) {
    if (seen.has(normName(name))) continue;
    seen.add(normName(name));
    next.push(name);
  }
  return next;
}

/** Cleans a list of names: `#` stripped, blanks and duplicates (case-insensitive) dropped. */
export function cleanTagNames(names: string[]): string[] {
  return addTagChips([], names.join(" "));
}

/** Existing tags not yet chosen that match the query: prefix matches first, then the rest. */
export function suggestTags(all: string[], chosen: string[], query: string, limit = MAX_SUGGESTIONS): string[] {
  const taken = new Set(chosen.map(normName));
  const q = normName(query.trim().replace(/^#+/, ""));
  const free = all.filter((n) => !taken.has(normName(n)));
  const starts = free.filter((n) => normName(n).startsWith(q));
  const rest = free.filter((n) => !normName(n).startsWith(q) && normName(n).includes(q));
  return [...starts, ...rest].slice(0, limit);
}

/** CSS color of a saved tag's palette name (sun when unknown). */
export const tagVar = (color: string) => `var(--color-tag-${(PALETTE as readonly string[]).includes(color) ? color : "sun"})`;

export const TAG_PALETTE = PALETTE;

/** CSS color for a saved tag's name as text: sun and clay use their darker, readable variant. */
export const tagInkVar = (color: string) => `var(--color-tag-${(PALETTE as readonly string[]).includes(color) ? color : "sun"}-ink)`;
