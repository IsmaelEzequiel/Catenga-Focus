/** Mapping between the textarea text and `Block.body` (the raw markdown under the heading). */

interface Positioned {
  start: number;
  end: number;
  key: string;
}

const isBlank = (line: string) => line.trim() === "";

/** Lines with their `\n` kept, so joining gives the input back. */
const splitLines = (text: string): string[] => text.match(/[^\n]*\n|[^\n]+$/g) ?? [];

/** `lead` and `trail` are whole blank lines, verbatim; indentation on the first content line stays in `core`. */
export function splitBody(body: string): { lead: string; core: string; trail: string } {
  const lines = splitLines(body);
  let from = 0;
  while (from < lines.length && isBlank(lines[from])) from++;
  let to = lines.length;
  while (to > from && isBlank(lines[to - 1])) to--;
  return {
    lead: lines.slice(0, from).join(""),
    core: lines.slice(from, to).join(""),
    trail: lines.slice(to).join(""),
  };
}

export function bodyToNote(body: string): string {
  const { core } = splitBody(body);
  return core.endsWith("\n") ? core.slice(0, -1) : core;
}

/** New body for `note`, keeping the old body's surrounding blank lines so unrelated bytes stay put. */
export function applyNote(oldBody: string, note: string, isLast: boolean): string {
  const lines = note.replace(/\r\n/g, "\n").split("\n");
  while (lines.length > 0 && isBlank(lines[lines.length - 1])) lines.pop();
  if (lines.length === 0) return "";
  const old = splitBody(oldBody);
  const lead = oldBody !== "" ? old.lead : "\n";
  const trail = old.trail !== "" ? old.trail : isLast ? "" : "\n";
  return `${lead}${lines.join("\n")}\n${trail}`;
}

/** The block the Rust side sorts last: greatest `(start, end)`, ties go to the later index. */
export function isLastBlock(blocks: Positioned[], key: string): boolean {
  let last = -1;
  blocks.forEach((b, i) => {
    if (last < 0) {
      last = i;
      return;
    }
    const l = blocks[last];
    if (b.start > l.start || (b.start === l.start && b.end >= l.end)) last = i;
  });
  return last >= 0 && blocks[last].key === key;
}
