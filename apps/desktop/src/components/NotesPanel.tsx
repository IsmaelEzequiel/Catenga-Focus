import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { CheckIcon } from "@heroicons/react/20/solid";
import type { HistoryKind } from "../api";
import type { Conflict, SaveState, UiBlock } from "../hooks/useDay";
import { bodyToNote } from "../lib/note";
import { tagColorVar } from "../lib/tags";
import { fmtMinutes } from "../lib/time";
import { ProjectChip, type ProjectPanel } from "./ProjectChip";
import { SessionsSummary, TimerBar, type TimerPanel } from "./TimerBar";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import "../notes.css";
import "../tagchip.css";

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
  tagColors: Record<string, string>;
  /** The block stands for a Google Calendar event. */
  fromCalendar: boolean;
  /** Nothing selected: today's current (or next) block or event, the same one "Start now" would use. */
  nowTarget: { title: string; start: number; end: number; upcoming: boolean } | null;
  onOpenNow: () => void;
}

/** Shown in an empty note editor only; never written into the file. */
export const NOTE_PROMPT = "What does a good outcome for this block look like?";

const IS_MAC = navigator.userAgent.includes("Mac");

const STATUS: Partial<Record<SaveState, string>> = {
  editing: "Editing…",
  saving: "Saving…",
  saved: "Saved",
  conflict: "File changed on disk",
};

export function NotesPanel(p: Props) {
  const { block, conflict } = p;
  if (!block) {
    return (
      <aside className="notes notes--empty">
        <div className="notes__empty">
          {p.nowTarget && (
            <>
              <span className="notes__meta">{p.nowTarget.upcoming ? "Up next" : "Happening now"}</span>
              <button className="notes__now" onClick={p.onOpenNow}>
                <span className="notes__now-title">{p.nowTarget.title}</span>
                <span className="notes__meta">
                  {fmtMinutes(p.nowTarget.start)}–{fmtMinutes(p.nowTarget.end)} · Enter to open
                </span>
              </button>
            </>
          )}
          <p className="notes__hint">
            {p.nowTarget ? "Or click any block. " : "Click a block to write its note. "}
            <kbd>N</kbd> adds one · <kbd>{IS_MAC ? "⌘K" : "Ctrl+K"}</kbd> searches notes.
          </p>
          <OtherConflicts conflicts={p.otherConflicts} onSelect={p.onSelect} />
        </div>
      </aside>
    );
  }
  const title = block.title.trim();
  return (
    <aside className="notes">
      <header className="notes__head">
        <span className="notes__meta">
          {fmtMinutes(block.start)}–{fmtMinutes(block.end)}
          {p.fromCalendar && " · Google Calendar"}
        </span>
        <h2 className={title ? "notes__title" : "notes__title notes__title--empty"}>{title || "Untitled"}</h2>
        {(block.tags.length > 0 || p.project.current || p.project.canMake) && (
          <div className="notes__chips">
            {block.tags.map((tag) => (
              <button
                key={tag}
                className="tagchip"
                style={{ ["--tag" as string]: tagColorVar([tag], p.tagColors) ?? undefined }}
                title={`All #${tag} notes`}
                onClick={() => p.onOpenHistory("tag", tag)}
              >
                <span className="tagchip__dot" aria-hidden="true" />#{tag}
              </button>
            ))}
            <ProjectChip panel={p.project} />
          </div>
        )}
      </header>
      <TimerBar block={block} timer={p.timer} />
      {conflict && (
        <div className="notes__banner">
          <Banner
            variant="inline"
            actions={
              <>
                <Button size="sm" onClick={() => p.onResolve(block.key, "mine")}>
                  Keep mine
                </Button>
                <Button size="sm" onClick={() => p.onResolve(block.key, "file")}>
                  Use file's
                </Button>
              </>
            }
          >
            This note changed on disk.
          </Banner>
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
      <footer className="notes__foot">
        <span className="notes__status" role="status" aria-live="polite">
          {p.saveState === "error" ? (
            <span className="notes__error">
              Not saved ·{" "}
              <button className="notes__link" onClick={p.onRetry}>
                Try again
              </button>
            </span>
          ) : (
            STATUS[p.saveState] && (
              <span key={p.saveState} className="notes__state">
                {p.saveState === "saved" && <CheckIcon className="notes__check" aria-hidden="true" />}
                {STATUS[p.saveState]}
              </span>
            )
          )}
        </span>
        <SessionsSummary block={block} timer={p.timer} />
        {title && (
          <button className="notes__link notes__history" onClick={() => p.onOpenHistory("title", block.title)}>
            All “{title}” notes →
          </button>
        )}
      </footer>
    </aside>
  );
}

function OtherConflicts({ conflicts, onSelect }: { conflicts: Conflict[]; onSelect: (key: string) => void }) {
  if (conflicts.length === 0) return null;
  return (
    <div className="notes__banner">
      <Banner
        variant="inline"
        actions={
          <Button size="sm" onClick={() => onSelect(conflicts[0].key)}>
            Review
          </Button>
        }
      >
        {conflicts.length === 1 ? "Another note changed on disk." : `${conflicts.length} other notes changed on disk.`}
      </Banner>
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
