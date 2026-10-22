/** Quick-add: `9-10:30 deep work #tag` -> a block (see `docs/plans/M10-speed.md` §1.1). Pure. */
import { isValidTagName } from "./tags";
import { DAY_MINUTES, snap } from "./time";

export type QuickAdd = { start: number; end: number; title: string; tags: string[] };
export type QuickAddResult = QuickAdd | { error: string };

export const NO_RANGE = "Start with a time range, e.g. 9-10:30";
const BAD_TIME = "That isn't a valid time";
const BAD_ORDER = "The end must be after the start";
const NO_TITLE = "Add a title after the time range";

/** `9`, `09`, `9:30`, `9h`, `9h30`, `9.30` (hour, then optional minutes). */
const TIME = String.raw`(\d{1,2})(?:[:.](\d{2})|h(\d{2})?)?`;
const SEP = String.raw`\s*(?:-|–|—|to|até)\s*`;
const RANGE = new RegExp(String.raw`^\s*${TIME}${SEP}${TIME}(?=\s|·|$)`, "iu");

/** Minutes since 00:00, or null when out of range. 24 is only valid as an exact end (24:00). */
function toMinutes(hour: string, minute: string | undefined, isEnd: boolean): number | null {
  const h = Number(hour);
  const m = minute === undefined ? 0 : Number(minute);
  if (m > 59) return null;
  if (h === 24) return isEnd && m === 0 ? DAY_MINUTES : null;
  return h > 23 ? null : h * 60 + m;
}

export function parseQuickAdd(text: string): QuickAddResult {
  const match = RANGE.exec(text);
  if (!match) return { error: NO_RANGE };
  const rawStart = toMinutes(match[1], match[2] ?? match[3], false);
  const rawEnd = toMinutes(match[4], match[5] ?? match[6], true);
  if (rawStart === null || rawEnd === null) return { error: BAD_TIME };
  const start = snap(rawStart);
  const end = snap(rawEnd);
  if (end <= start) return { error: BAD_ORDER };

  const rest = text.slice(match[0].length).replace(/^\s*·/u, "");
  const words: string[] = [];
  const tags: string[] = [];
  for (const token of rest.split(/\s+/).filter((t) => t !== "")) {
    if (token.startsWith("#") && isValidTagName(token.slice(1))) tags.push(token.slice(1));
    else words.push(token);
  }
  const title = words.join(" ");
  if (title === "") return { error: NO_TITLE };
  return { start, end, title, tags };
}
