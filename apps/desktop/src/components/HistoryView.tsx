import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronLeftIcon } from "@heroicons/react/20/solid";
import { history, onIndexStatus, type HistoryEntry, type HistoryKind, type HistoryResult } from "../api";
import { visibleEntries } from "../lib/history";
import { bodyToNote } from "../lib/note";
import { fmtRange } from "../lib/search";
import { tagColorVar } from "../lib/tags";
import { fmtDateShort } from "../lib/time";
import "../history.css";

interface Props {
  by: HistoryKind;
  value: string;
  tagColors: Record<string, string>;
  onBack: () => void;
  onOpenBlock: (entry: HistoryEntry) => void;
  onOpenHistory: (by: HistoryKind, value: string) => void;
}

/** Every note of one title or tag as a single chronological list. */
export function HistoryView({ by, value, tagColors, onBack, onOpenBlock, onOpenHistory }: Props) {
  const [result, setResult] = useState<HistoryResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [onlyWithNotes, setOnlyWithNotes] = useState(true);
  const [order, setOrder] = useState<"newest" | "oldest">("newest");
  const [readyTick, setReadyTick] = useState(0);
  const latest = useRef(0);
  const indexing = useRef(false);
  indexing.current = result?.indexing ?? false;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onIndexStatus((status) => {
      if (status.state === "ready" && indexing.current) setReadyTick((t) => t + 1);
    }).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no live refresh; reopening the view still loads
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const id = ++latest.current;
    history(by, value).then(
      (r) => {
        if (id !== latest.current) return;
        setResult(r);
        setError(null);
      },
      (e) => {
        if (id !== latest.current) return;
        setResult(null);
        setError(String(e));
      },
    );
  }, [by, value, readyTick]);

  const entries = useMemo(
    () => (result ? visibleEntries(result.entries, { onlyWithNotes, order }) : []),
    [result, onlyWithNotes, order],
  );
  const label = result?.label ?? value;

  return (
    <section className="history" aria-label="History">
      <header className="history__header">
        <button className="app__btn" onClick={onBack}>
          <ChevronLeftIcon className="app__icon" aria-hidden="true" />
          Day
        </button>
        <h2 className="history__title">
          <span className="history__kind">{by === "tag" ? "Tag" : "Title"}</span> {by === "tag" ? `#${label}` : label}
        </h2>
        <div className="history__controls">
          <label className="history__check">
            <input type="checkbox" checked={onlyWithNotes} onChange={(e) => setOnlyWithNotes(e.target.checked)} />
            Only blocks with notes
          </label>
          <button className="app__btn" onClick={() => setOrder((o) => (o === "newest" ? "oldest" : "newest"))}>
            {order === "newest" ? "Newest first" : "Oldest first"}
          </button>
        </div>
      </header>
      <div className="history__status" role="status" aria-live="polite">
        {result && <span>{entries.length === 1 ? "1 entry" : `${entries.length} entries`}</span>}
        {result?.truncated && <span>Showing latest 500</span>}
        {result?.indexing && <span>Indexing… the list may be incomplete</span>}
      </div>
      <div className="history__list">
        {error && <p className="history__message history__message--error">{error}</p>}
        {result && entries.length === 0 && (
          <p className="history__message">
            {result.entries.length === 0 ? "No blocks found." : "No blocks with notes. Turn off the filter to see them all."}
          </p>
        )}
        {entries.map((e) => (
          <article key={`${e.date}:${e.ord}`} className="history__entry">
            <div className="history__entry-head">
              <button className="history__date" onClick={() => onOpenBlock(e)} aria-label={`Open ${fmtDateShort(e.date)}`}>
                {fmtDateShort(e.date)}
              </button>
              <span className="history__time">{fmtRange(e.start, e.end)}</span>
              {by === "tag" && e.title !== "" && (
                <button className="history__chip" onClick={() => onOpenHistory("title", e.title)}>
                  {e.title}
                </button>
              )}
              {e.tags.map((tag) => (
                <button key={tag} className="history__chip" onClick={() => onOpenHistory("tag", tag)}>
                  <span className="history__dot" style={{ background: tagColorVar([tag], tagColors) ?? "var(--color-line)" }} />#{tag}
                </button>
              ))}
            </div>
            <div className="history__note">{bodyToNote(e.body)}</div>
          </article>
        ))}
      </div>
    </section>
  );
}
