import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { PlusIcon } from "@heroicons/react/20/solid";
import { parseQuickAdd, type QuickAdd as Parsed } from "../lib/quickAdd";
import { fmtDateShort, fmtMinutes } from "../lib/time";
import "../quickadd.css";

interface Props {
  date: string;
  isToday: boolean;
  /** The day file is still loading: nothing can be added yet. */
  loaded: boolean;
  /** Tags that already exist; any other tag in the text is flagged "new tag" in the preview. */
  knownTags?: string[];
  /** Called whenever the parsed time range changes: `null` when the text doesn't parse and when the bar closes. */
  onPreview?: (range: { start: number; end: number } | null) => void;
  onAdd: (block: Parsed) => void;
  onClose: () => void;
}

/** The bar at the top of the timeline: type `9-10:30 Deep work #tag`, Enter adds the block to the shown day. */
export function QuickAdd({
  date,
  isToday,
  loaded,
  knownTags,
  onPreview,
  onAdd,
  onClose,
}: Props) {
  const [text, setText] = useState("");
  const [tried, setTried] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const previous = document.activeElement;
    input.current?.focus();
    return () => {
      if (
        previous instanceof HTMLElement &&
        document.activeElement === document.body
      )
        previous.focus();
    };
  }, []);

  const parsed = text.trim() === "" ? null : parseQuickAdd(text);
  const parseError = parsed && "error" in parsed ? parsed.error : null;
  const error = tried ? parseError : null;
  const ok = parsed && !("error" in parsed) ? parsed : null;
  const start = ok?.start ?? null;
  const end = ok?.end ?? null;

  const previewRef = useRef(onPreview);
  previewRef.current = onPreview;
  useEffect(() => {
    previewRef.current?.(
      start !== null && end !== null ? { start, end } : null,
    );
  }, [start, end]);
  useEffect(() => () => previewRef.current?.(null), []);

  const submit = () => {
    setTried(true);
    if (!loaded || !parsed || "error" in parsed) return;
    onAdd(parsed);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <div className="quickadd" role="group" aria-label="Quick-add">
      <PlusIcon className="app__icon quickadd__icon" aria-hidden="true" />
      <div
        className={
          error ? "quickadd__body quickadd__body--error" : "quickadd__body"
        }
      >
        <input
          ref={input}
          className="quickadd__input"
          aria-label="Quick-add a block"
          placeholder="9-10:30 Deep work #tag"
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            setTried(false);
          }}
          onKeyDown={onKeyDown}
          spellCheck={false}
        />
        <div className="quickadd__line" aria-live="polite">
          {error ? (
            <span className="quickadd__error" role="alert">
              {error}
            </span>
          ) : tried && !loaded ? (
            <span className="quickadd__error" role="alert">
              The day is still loading.
            </span>
          ) : ok ? (
            <span className="quickadd__preview">
              <span className="quickadd__range">
                {isToday ? "" : `${fmtDateShort(date)} · `}
                {fmtMinutes(ok.start)}–{fmtMinutes(ok.end)}
              </span>
              <span className="quickadd__title">{ok.title}</span>
              {ok.tags.map((t) => (
                <span key={t} className="quickadd__tag">
                  #{t}
                  {knownTags && !knownTags.includes(t) && (
                    <span className="quickadd__new"> · new tag</span>
                  )}
                </span>
              ))}
            </span>
          ) : parseError ? (
            <span className="quickadd__hint">{parseError}</span>
          ) : (
            <span className="quickadd__hint">
              Type a time range, a title and #tags
            </span>
          )}
        </div>
        <div className="quickadd__foot">Enter add · Esc close</div>
      </div>
    </div>
  );
}
