import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type MutableRefObject, type PointerEvent } from "react";
import type { BlockRef, CalEvent, Session } from "../api";
import type { UiBlock } from "../hooks/useDay";
import { ghostEvents, matchBlocks } from "../lib/calendar";
import { layoutLanes } from "../lib/layout";
import { blockHasSessions, sameBlock, sessionBars } from "../lib/timer";
import { parseTitleInput, tagColorVar } from "../lib/tags";
import {
  DAY_MINUTES,
  PX_PER_HOUR,
  PX_PER_MIN,
  clampMove,
  clampResize,
  createRange,
  fmtMinutes,
  minuteFromY,
  snap,
  toDateString,
  type Range,
} from "../lib/time";
import { TimelineBlock } from "./TimelineBlock";
import "../day.css";
import "../timer.css";

const DRAG_THRESHOLD_PX = 4;
const HOURS = Array.from({ length: 24 }, (_, h) => h);
const QUARTERS = Array.from({ length: 96 }, (_, q) => q);

type DragMode = "create" | "move" | "resize-start" | "resize-end";

interface Draft extends Range {
  key: string;
}

interface Props {
  date: string;
  blocks: UiBlock[];
  /** Google Calendar events of this day; empty when not connected. */
  events: CalEvent[];
  loaded: boolean;
  tagColors: Record<string, string>;
  now: Date;
  /** Sessions of this day (drawn in the gutter; a block that has some can't be deleted). */
  sessions: Session[];
  /** The running timer's block when it belongs to this day. */
  timing: BlockRef | null;
  /** Start of the running session on this day; other open sessions are crash leftovers and don't grow. */
  runningStart: string | null;
  /** Wall clock (ms) ticking every second while a timer runs, to grow the running session. */
  clockMs: number;
  /** True while the focus screen covers the app: Enter/Delete must not touch blocks. */
  shortcutsOff?: boolean;
  /** Set while a pointer drag is active so global shortcuts can stand down. */
  dragActive: MutableRefObject<boolean>;
  selected: string | null;
  /** Scrolls this minute into view when `token` changes (search results). */
  reveal: { minute: number; token: number } | null;
  onSelect: (key: string | null, opts?: { focusNote: boolean }) => void;
  onCommit: (next: UiBlock[]) => void;
  onNotice: (message: string) => void;
}

const isTypingTarget = (t: EventTarget | null) =>
  t instanceof HTMLElement && (t.closest("input, textarea, select, [contenteditable]") !== null);

export function DayView({ date, blocks, events, loaded, tagColors, now, sessions, timing, runningStart, clockMs, shortcutsOff, dragActive, selected, reveal, onSelect, onCommit, onNotice }: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const grid = useRef<HTMLDivElement>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [pending, setPending] = useState<UiBlock | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const stopDrag = useRef<(() => void) | null>(null);

  // Latest props for the native pointer listeners, which outlive a render.
  const live = useRef({ blocks, loaded, onCommit, onSelect });
  live.current = { blocks, loaded, onCommit, onSelect };

  const isToday = date === toDateString(now);
  const nowMinutes = now.getHours() * 60 + now.getMinutes();

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const top = isToday ? Math.max(0, nowMinutes - 60) : 7 * 60;
    el.scrollTop = top * PX_PER_MIN;
    // Only on mount: DayView is keyed by date.
  }, []);

  // A reveal that predates this mount (the view is keyed by date) was already handled.
  const revealed = useRef(reveal?.token);
  useEffect(() => {
    const el = scroller.current;
    if (!el || !reveal || reveal.token === revealed.current) return;
    revealed.current = reveal.token;
    const y = reveal.minute * PX_PER_MIN;
    if (y < el.scrollTop || y > el.scrollTop + el.clientHeight - PX_PER_HOUR) {
      el.scrollTop = Math.max(0, y - el.clientHeight / 3);
    }
  }, [reveal]);

  useEffect(
    () => () => {
      stopDrag.current?.();
    },
    [],
  );

  const gridTop = () => grid.current?.getBoundingClientRect().top ?? 0;

  const startDrag = useCallback(
    (e: PointerEvent<HTMLElement>, mode: DragMode, key: string) => {
      if (e.button !== 0 || !live.current.loaded) return;
      e.stopPropagation();
      const target = e.currentTarget;
      const pointerId = e.pointerId;
      const startX = e.clientX;
      const startY = e.clientY;
      const orig = live.current.blocks.find((b) => b.key === key);
      if (mode !== "create" && !orig) return;
      const anchor = minuteFromY(startY - gridTop());
      let moved = false;
      let latest: Range | null = null;

      target.setPointerCapture(pointerId);
      dragActive.current = true;

      const compute = (clientY: number): Range => {
        if (mode === "create") return createRange(anchor, minuteFromY(clientY - gridTop()));
        const o = orig!;
        const deltaMin = (clientY - startY) / PX_PER_MIN;
        if (mode === "move") return clampMove(o.start, o.end, snap(deltaMin));
        if (mode === "resize-start") return clampResize("start", o.start, o.end, snap(o.start + deltaMin));
        return clampResize("end", o.start, o.end, snap(o.end + deltaMin));
      };

      const cleanup = () => {
        target.removeEventListener("pointermove", onMove);
        target.removeEventListener("pointerup", onUp);
        target.removeEventListener("pointercancel", cancel);
        window.removeEventListener("keydown", onKey, true);
        if (target.hasPointerCapture(pointerId)) target.releasePointerCapture(pointerId);
        dragActive.current = false;
        stopDrag.current = null;
        setDraft(null);
      };
      const cancel = () => cleanup();
      const onKey = (ev: KeyboardEvent) => {
        if (ev.key !== "Escape") return;
        ev.preventDefault();
        cleanup();
      };
      const onMove = (ev: globalThis.PointerEvent) => {
        if (!moved && Math.hypot(ev.clientX - startX, ev.clientY - startY) < DRAG_THRESHOLD_PX) return;
        moved = true;
        latest = compute(ev.clientY);
        setDraft({ key, ...latest });
      };
      const onUp = () => {
        cleanup();
        if (!moved || !latest) {
          live.current.onSelect(mode === "create" ? null : key, { focusNote: mode !== "create" });
          return;
        }
        if (mode === "create") {
          const block: UiBlock = { key: crypto.randomUUID(), ...latest, title: "", tags: [], body: "" };
          live.current.onSelect(null);
          setPending(block);
          setEditing(block.key);
          return;
        }
        live.current.onSelect(key);
        const o = orig!;
        if (latest.start === o.start && latest.end === o.end) return;
        const range = latest;
        live.current.onCommit(live.current.blocks.map((b) => (b.key === key ? { ...b, ...range } : b)));
      };

      target.addEventListener("pointermove", onMove);
      target.addEventListener("pointerup", onUp);
      target.addEventListener("pointercancel", cancel);
      window.addEventListener("keydown", onKey, true);
      stopDrag.current = cleanup;
    },
    [dragActive],
  );

  const commitEdit = (key: string, text: string) => {
    const { title, tags } = parseTitleInput(text);
    setEditing(null);
    if (pending && pending.key === key) {
      setPending(null);
      onCommit([...blocks, { ...pending, title, tags }]);
      onSelect(key);
      return;
    }
    const current = blocks.find((b) => b.key === key);
    if (!current) return;
    const same = current.title === title && current.tags.length === tags.length && current.tags.every((t, i) => t === tags[i]);
    if (same) return;
    onCommit(blocks.map((b) => (b.key === key ? { ...b, title, tags } : b)));
  };

  const cancelEdit = (key: string) => {
    setEditing(null);
    if (pending && pending.key === key) setPending(null);
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (shortcutsOff || e.metaKey || e.ctrlKey || e.altKey || isTypingTarget(e.target) || dragActive.current || !selected) return;
      if (e.key === "Enter") {
        e.preventDefault();
        setEditing(selected);
      } else if (e.key === "Delete" || e.key === "Backspace") {
        e.preventDefault();
        const block = blocks.find((b) => b.key === selected);
        if (!block) return;
        if (blockHasSessions(sessions, block)) {
          onNotice("This block has timer sessions and can't be deleted.");
          return;
        }
        if (block.body.trim() !== "") {
          onNotice("This block has notes. Edit the file to remove it.");
          return;
        }
        onSelect(null);
        onCommit(blocks.filter((b) => b.key !== selected));
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shortcutsOff, selected, blocks, sessions, dragActive, onCommit, onSelect, onNotice]);

  const display = useMemo(() => {
    const all = pending ? [...blocks, pending] : blocks;
    return all.map((b) => (draft && draft.key === b.key ? { ...b, start: draft.start, end: draft.end } : b));
  }, [blocks, pending, draft]);
  const matched = useMemo(() => matchBlocks(blocks, events), [blocks, events]);
  const bars = useMemo(() => sessionBars(sessions, date, clockMs, runningStart), [sessions, date, clockMs, runningStart]);
  const ghosts = useMemo(() => ghostEvents(blocks, events), [blocks, events]);
  const lanes = useMemo(() => layoutLanes([...display, ...ghosts]), [display, ghosts]);

  /** Clicking an event makes its block (no Google write: it already matches) and opens the note. */
  const openEvent = (event: CalEvent) => {
    if (!loaded || pending) return;
    const key = crypto.randomUUID();
    onCommit([...blocks, { key, start: event.start, end: event.end, title: event.title, tags: [], body: "" }]);
    onSelect(key, { focusNote: true });
  };

  return (
    <div className="day" ref={scroller}>
      <div
        className={loaded ? "day__grid" : "day__grid day__grid--disabled"}
        ref={grid}
        style={{ height: 24 * PX_PER_HOUR }}
        onPointerDown={(e) => {
          if ((e.target as HTMLElement).closest("[data-block]")) return;
          startDrag(e, "create", "new");
        }}
      >
        {QUARTERS.map((q) => (
          <div key={q} className={q % 4 === 0 ? "day__line day__line--hour" : "day__line"} style={{ top: q * 15 * PX_PER_MIN }} />
        ))}
        {HOURS.map((h) => (
          <span key={h} className="day__label" style={{ top: h * PX_PER_HOUR }}>
            {fmtMinutes(h * 60)}
          </span>
        ))}
        <div className="day__gutter" aria-hidden="true">
          {bars.map((bar, i) => (
            <div key={i} className="day__session" style={{ top: bar.start * PX_PER_MIN, height: (bar.end - bar.start) * PX_PER_MIN }} />
          ))}
        </div>
        <div className="day__track">
          {display.map((b, i) => (
            <TimelineBlock
              key={b.key}
              start={b.start}
              end={b.end}
              title={b.title}
              tags={b.tags}
              color={tagColorVar(b.tags, tagColors)}
              projectColor={matched.get(b.key)?.color ?? null}
              lane={lanes[i].lane}
              lanes={lanes[i].lanes}
              selected={selected === b.key}
              timing={timing !== null && sameBlock(timing, b)}
              editing={editing === b.key}
              calendar={matched.has(b.key) ? "synced" : undefined}
              onBodyPointerDown={(e) => startDrag(e, "move", b.key)}
              onHandlePointerDown={(e, edge) => startDrag(e, edge === "start" ? "resize-start" : "resize-end", b.key)}
              onDoubleClick={() => loaded && !pending && setEditing(b.key)}
              onEditCommit={(text) => commitEdit(b.key, text)}
              onEditCancel={() => cancelEdit(b.key)}
            />
          ))}
          {ghosts.map((ev, i) => (
            <TimelineBlock
              key={`event-${ev.id}`}
              start={ev.start}
              end={ev.end}
              title={ev.title}
              tags={[]}
              color={null}
              projectColor={ev.color}
              lane={lanes[display.length + i].lane}
              lanes={lanes[display.length + i].lanes}
              selected={false}
              editing={false}
              calendar="event"
              onEventClick={() => openEvent(ev)}
            />
          ))}
          {draft && draft.key === "new" && (
            <TimelineBlock start={draft.start} end={draft.end} title="" tags={[]} color={null} lane={0} lanes={1} selected={false} editing={false} ghost />
          )}
        </div>
        {isToday && nowMinutes < DAY_MINUTES && <div className="day__now" style={{ top: nowMinutes * PX_PER_MIN }} />}
      </div>
    </div>
  );
}
