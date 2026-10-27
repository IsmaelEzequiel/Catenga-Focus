import { useRef, type CSSProperties, type PointerEvent } from "react";
import { CheckCircleIcon, TrashIcon } from "@heroicons/react/20/solid";
import { fmtDuration } from "../lib/dashboard";
import { fmtMinutes, PX_PER_MIN } from "../lib/time";
import { formatTitleInput, tagInkOf } from "../lib/tags";
import { projectStyle } from "../lib/projects";

export interface TimelineBlockProps {
  start: number;
  end: number;
  title: string;
  tags: string[];
  color: string | null;
  /** Google color of the block's/event's calendar (project); drawn as a small left stripe. */
  projectColor?: string | null;
  lane: number;
  lanes: number;
  selected: boolean;
  editing: boolean;
  ghost?: boolean;
  /** A timer is running on this block. */
  timing?: boolean;
  /** The running timer's reading, shown as a small live pill. */
  live?: string | null;
  /** Minutes of finished sessions on this block. */
  logged?: number;
  /** Has a session or a note (the dashboard's "done"). */
  done?: boolean;
  /** "event": a Google Calendar event with no block yet (dashed, click to make a block); "synced": a block standing for one. */
  calendar?: "event" | "synced";
  onEventClick?: () => void;
  /** Event only: shows a trash button (hover/focus) that asks to delete the event in Google Calendar. */
  onEventDelete?: () => void;
  onBodyPointerDown?: (e: PointerEvent<HTMLElement>) => void;
  onHandlePointerDown?: (e: PointerEvent<HTMLElement>, edge: "start" | "end") => void;
  onDoubleClick?: () => void;
  /** Right-click (not while typing in the title): the parent pops up the native menu. */
  onContextMenu?: () => void;
  onEditCommit?: (text: string) => void;
  onEditCancel?: () => void;
}

export function TimelineBlock(p: TimelineBlockProps) {
  const done = useRef(false);
  const height = (p.end - p.start) * PX_PER_MIN;
  const style = {
    top: p.start * PX_PER_MIN,
    height,
    left: `${(p.lane / p.lanes) * 100}%`,
    width: `${100 / p.lanes}%`,
    "--block-color": p.color ?? undefined,
    "--tag-ink": p.color ? tagInkOf(p.color) : undefined,
    ...projectStyle(p.projectColor ?? null),
  } as CSSProperties;
  const classes = [
    "block",
    p.selected ? "block--selected" : "",
    p.ghost ? "block--ghost" : "",
    p.calendar === "event" ? "block--event" : "",
    height < 32 ? "block--compact" : height < 44 ? "block--short" : "",
  ]
    .filter(Boolean)
    .join(" ");

  const finish = (commit: boolean, text: string) => {
    if (done.current) return;
    done.current = true;
    if (commit) p.onEditCommit?.(text);
    else p.onEditCancel?.();
  };

  return (
    <div
      className={classes}
      style={style}
      data-block
      data-ghost={p.calendar === "event" ? "" : undefined}
      onContextMenu={(e) => {
        if (p.editing || !p.onContextMenu) return;
        e.preventDefault();
        p.onContextMenu();
      }}
      onPointerDown={p.editing ? (e) => e.stopPropagation() : p.onBodyPointerDown}
      onDoubleClick={p.onDoubleClick}
      onClick={p.calendar === "event" ? p.onEventClick : undefined}
      onKeyDown={
        p.calendar === "event"
          ? (e) => {
              if (e.key !== "Enter" && e.key !== " ") return;
              e.preventDefault();
              e.stopPropagation();
              p.onEventClick?.();
            }
          : undefined
      }
      role={p.calendar === "event" ? "button" : undefined}
      tabIndex={p.calendar === "event" ? 0 : undefined}
      aria-label={p.calendar === "event" ? `Google Calendar event ${p.title}, ${fmtMinutes(p.start)} to ${fmtMinutes(p.end)}` : undefined}
    >
      {p.projectColor && projectStyle(p.projectColor) && <span className="block__stripe" aria-hidden="true" />}
      {!p.editing && !p.ghost && (
        <>
          <div className="block__handle block__handle--top" onPointerDown={(e) => p.onHandlePointerDown?.(e, "start")} />
          <div className="block__handle block__handle--bottom" onPointerDown={(e) => p.onHandlePointerDown?.(e, "end")} />
        </>
      )}
      {p.calendar === "event" && p.onEventDelete && (
        <button
          type="button"
          className="block__trash"
          aria-label="Delete event"
          title="Delete event"
          onClick={(e) => {
            e.stopPropagation();
            p.onEventDelete?.();
          }}
          onKeyDown={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
        >
          <TrashIcon className="app__icon" aria-hidden="true" />
        </button>
      )}
      {p.editing ? (
        <input
          className="block__input"
          autoFocus
          defaultValue={formatTitleInput(p.title, p.tags)}
          placeholder="Title #tag"
          onFocus={(e) => e.currentTarget.select()}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter") finish(true, e.currentTarget.value);
            else if (e.key === "Escape") finish(false, "");
          }}
          onBlur={(e) => finish(true, e.currentTarget.value)}
        />
      ) : (
        <span className="block__head">
          <span className={p.title ? "block__title" : "block__title block__title--empty"}>{p.title || "Untitled"}</span>
          {p.done && !p.live && <CheckCircleIcon className="block__done" aria-label="Done" />}
          {p.live && (
            <span className="block__live" role="timer" aria-live="off">
              <i className="block__live-dot" aria-hidden="true" />
              {p.live}
            </span>
          )}
        </span>
      )}
      {!p.ghost && (
        <span className="block__meta">
          {[
            `${fmtMinutes(p.start)}–${fmtMinutes(p.end)}`,
            p.tags.length > 0 ? p.tags.map((t) => `#${t}`).join(" ") : p.calendar ? "Google Calendar" : null,
            p.logged ? `${fmtDuration(p.logged)} logged` : null,
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
      )}
    </div>
  );
}
