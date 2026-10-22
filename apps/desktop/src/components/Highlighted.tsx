import { splitHighlight } from "../lib/search";

/** Renders backend-marked text as plain text with `<mark>` around matches; never as HTML. */
export function Highlighted({ text }: { text: string }) {
  return (
    <>
      {splitHighlight(text).map((s, i) => (s.hit ? <mark key={i}>{s.text}</mark> : s.text))}
    </>
  );
}
