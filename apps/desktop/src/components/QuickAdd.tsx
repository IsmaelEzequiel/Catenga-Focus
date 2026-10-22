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
  onAdd: (block: Parsed) => void;
  onClose: () => void;
}

/** The bar at the top of the timeline: type `9-10:30 Deep work #tag`, Enter adds the block to the shown day. */
export function QuickAdd({ date, isToday, loaded, onAdd, onClose }: Props) {
  const [text, setText] = useState("");
  const [tried, setTried] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const previous = document.activeElement;
    input.current?.focus();
    return () => {
      if (previous instanceof HTMLElement && document.activeElement === document.body) previous.focus();
    };
  }, []);

  const parsed = text.trim() === "" ? null : parseQuickAdd(text);
  const error = tried && parsed && "error" in parsed ? parsed.error : null;
  const preview =
    parsed && !("error" in parsed)
      ? `${isToday ? "Today" : fmtDateShort(date)} ${fmtMinutes(parsed.start)}–${fmtMinutes(parsed.end)} · ${parsed.title}${parsed.tags.map((t) => ` #${t}`).join("")}`
      : null;

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
      <div className="quickadd__body">
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
          ) : preview ? (
            <span className="quickadd__preview">{preview}</span>
          ) : (
            <span className="quickadd__hint">Enter to add · Esc to close</span>
          )}
        </div>
      </div>
    </div>
  );
}
