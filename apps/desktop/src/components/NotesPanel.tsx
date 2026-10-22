import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import type { HistoryKind } from "../api";
import type { Conflict, SaveState, UiBlock } from "../hooks/useDay";
import { bodyToNote } from "../lib/note";
import { fmtMinutes } from "../lib/time";
import { ProjectChip, type ProjectPanel } from "./ProjectChip";
import { TimerBar, type TimerPanel } from "./TimerBar";
import "../notes.css";

interface Props {
  block: UiBlock | null;
  /** Set when this block's note changed on disk while it was edited here. */
  conflict: Conflict | undefined;
  /** Conflicts on other blocks, so none is unreachable. */
  otherConflicts: Conflict[];
  saveState: SaveState;
  /** Bumped when the text must be re-read from the block (external reload, "Use file's"). */
  noteVersion: number;
  /** Changes whenever a click on a block asks for the textarea to take focus. */
  focusToken: number;
  onNote: (key: string, note: string) => void;
  onBlur: () => void;
  onRetry: () => void;
  onSelect: (key: string) => void;
  onResolve: (key: string, choice: "mine" | "file") => void;
  onOpenHistory: (by: HistoryKind, value: string) => void;
  timer: TimerPanel;
  project: ProjectPanel;
}

/** Shown in an empty note editor only; never written into the file. */
export const NOTE_PROMPT = "What does a good outcome for this block look like?";

const STATUS: Partial<Record<SaveState, string>> = {
  saving: "Saving…",
  saved: "Saved",
  conflict: "File changed on disk",
};

export function NotesPanel(p: Props) {
  const { block, conflict } = p;
  if (!block) {
    return (
      <aside className="notes notes--empty">
        <p>Select a block to write its note.</p>
        <OtherConflicts conflicts={p.otherConflicts} onSelect={p.onSelect} />
      </aside>
    );
  }
  return (
    <aside className="notes">
      <header className="notes__header">
        <h2 className="notes__title">
          {fmtMinutes(block.start)}–{fmtMinutes(block.end)}
          {block.title !== "" && (
            <>
              {" · "}
              <button
                className="notes__history"
                title="Show the history of this title"
                onClick={() => p.onOpenHistory("title", block.title)}
              >
                {block.title}
              </button>
            </>
          )}
          {block.tags.map((tag) => (
            <button
              key={tag}
              className="notes__history"
              title={`Show the history of #${tag}`}
              onClick={() => p.onOpenHistory("tag", tag)}
            >
              {" "}
              #{tag}
            </button>
          ))}
        </h2>
        <span className="notes__status" role="status" aria-live="polite">
          {p.saveState === "error" ? (
            <>
              Not saved ·{" "}
              <button className="notes__link" onClick={p.onRetry}>
                Retry
              </button>
            </>
          ) : (
            <span key={p.saveState} className={p.saveState === "saved" ? "notes__saved" : undefined}>
              {STATUS[p.saveState]}
            </span>
          )}
        </span>
      </header>
      <ProjectChip panel={p.project} />
      <TimerBar block={block} timer={p.timer} />
      {conflict && (
        <div className="notes__conflict" role="alert">
          <span>This note changed on disk.</span>
          <span className="notes__actions">
            <button className="app__btn" onClick={() => p.onResolve(block.key, "mine")}>
              Keep mine
            </button>
            <button className="app__btn" onClick={() => p.onResolve(block.key, "file")}>
              Use file's
            </button>
          </span>
        </div>
      )}
      <NoteEditor
        key={`${block.key}:${p.noteVersion}`}
        initial={bodyToNote(conflict ? conflict.mine.body : block.body)}
        focusToken={p.focusToken}
        onChange={(text) => p.onNote(block.key, text)}
        onBlur={p.onBlur}
      />
      <OtherConflicts conflicts={p.otherConflicts.filter((c) => c.key !== block.key)} onSelect={p.onSelect} />
    </aside>
  );
}

function OtherConflicts({ conflicts, onSelect }: { conflicts: Conflict[]; onSelect: (key: string) => void }) {
  if (conflicts.length === 0) return null;
  return (
    <div className="notes__conflict" role="alert">
      <span>{conflicts.length === 1 ? "Another note changed on disk." : `${conflicts.length} other notes changed on disk.`}</span>
      <button className="app__btn" onClick={() => onSelect(conflicts[0].key)}>
        Review
      </button>
    </div>
  );
}

/**
 * Owns the text while mounted: it is never re-derived from the block on a keystroke.
 * Shared by the notes panel and the focus screen (`autoGrow`: sized to its content, the page scrolls instead).
 */
export function NoteEditor(p: {
  initial: string;
  focusToken: number;
  onChange: (text: string) => void;
  onBlur: () => void;
  className?: string;
  autoGrow?: boolean;
}) {
  const [text, setText] = useState(p.initial);
  const ref = useRef<HTMLTextAreaElement>(null);
  const handled = useRef(p.focusToken);

  useEffect(() => {
    if (p.focusToken !== handled.current) {
      handled.current = p.focusToken;
      ref.current?.focus();
    }
  }, [p.focusToken]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!p.autoGrow || !el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text, p.autoGrow]);

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.currentTarget.blur();
    }
  };

  return (
    <textarea
      ref={ref}
      className={p.className ?? "notes__editor"}
      aria-label="Block note"
      placeholder={NOTE_PROMPT}
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        p.onChange(e.target.value);
      }}
      onBlur={p.onBlur}
      onKeyDown={onKeyDown}
    />
  );
}
