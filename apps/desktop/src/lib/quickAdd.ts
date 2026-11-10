/** Quick-add: `9-10:30 deep work #tag` -> a block (see `docs/plans/M10-speed.md` §1.1). Pure. */
import type { RepeatFreq } from "../api";
import { isValidTagName } from "./tags";
import { DAY_MINUTES, snap } from "./time";

/** A trailing `daily` / `weekdays` / `weekly [mon wed]` (M16). `days` empty for "weekly" means the block's own weekday. */
export type QuickRepeat = { freq: RepeatFreq; days: number[] };
export type QuickAdd = { start: number; end: number; title: string; tags: string[]; repeat?: QuickRepeat };
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

const FREQ_WORDS: Record<string, RepeatFreq> = {
  daily: "daily",
  diario: "daily",
  diário: "daily",
  weekdays: "weekdays",
  uteis: "weekdays",
  úteis: "weekdays",
  weekly: "weekly",
  semanal: "weekly",
};

/** Day names in English and Portuguese, 0 = Monday … 6 = Sunday. */
const DAY_WORDS: Record<string, number> = {
  mon: 0, monday: 0, seg: 0, segunda: 0,
  tue: 1, tuesday: 1, ter: 1, terça: 1, terca: 1,
  wed: 2, wednesday: 2, qua: 2, quarta: 2,
  thu: 3, thursday: 3, qui: 3, quinta: 3,
  fri: 4, friday: 4, sex: 4, sexta: 4,
  sat: 5, saturday: 5, sab: 5, sáb: 5, sabado: 5, sábado: 5,
  sun: 6, sunday: 6, dom: 6, domingo: 6,
};

/** Splits a trailing repeat off the title words; the title must keep at least one word. */
function takeRepeat(words: string[]): { words: string[]; repeat?: QuickRepeat } {
  const lower = words.map((w) => w.toLowerCase().replace(/,$/u, ""));
  let i = lower.length;
  const days: number[] = [];
  while (i > 0 && DAY_WORDS[lower[i - 1]] !== undefined) {
    days.unshift(DAY_WORDS[lower[i - 1]]);
    i -= 1;
  }
  const freq = i > 1 ? FREQ_WORDS[lower[i - 1]] : undefined;
  if (!freq || (days.length > 0 && freq !== "weekly")) return { words };
  return {
    words: words.slice(0, i - 1),
    repeat: { freq, days: freq === "weekly" ? [...new Set(days)].sort((a, b) => a - b) : [] },
  };
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
  const { words: titleWords, repeat } = takeRepeat(words);
  const title = titleWords.join(" ");
  if (title === "") return { error: NO_TITLE };
  return repeat ? { start, end, title, tags, repeat } : { start, end, title, tags };
}
