import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type MutableRefObject, type PointerEvent, type ReactNode } from "react";
import type { BlockRef, CalEvent, Session } from "../api";
import type { UiBlock } from "../hooks/useDay";
import { blockDeletePlan, ghostEvents, isSeriesInstance, matchBlocks } from "../lib/calendar";
import { layoutLanes } from "../lib/layout";
import { blockHasSessions, blockTotalMinutes, sameBlock, sessionBars, sessionsForBlock } from "../lib/timer";
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
import { blockMenu, ghostMenu } from "../lib/contextMenu";
import { popupMenu } from "../lib/nativeMenu";
import { TimelineBlock } from "./TimelineBlock";
import { Button } from "./ui/Button";
import "../day.css";
import "../timer.css";

const DRAG_THRESHOLD_PX = 4;
const HOURS = Array.from({ length: 24 }, (_, h) => h);

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
  /** The running timer's reading for the live pill on its block. */
  liveTime: string | null;
  /** Quick-add's parsed slot, shown as a ghost block. */
  preview?: Range | null;
  /** True while the focus screen covers the app: Enter/Delete must not touch blocks. */
  shortcutsOff?: boolean;
  /** Set while a pointer drag is active so global shortcuts can stand down. */
  dragActive: MutableRefObject<boolean>;
  selected: string | null;
  /** Changes when the `R` shortcut asks to rename the selected block (same inline edit as a double-click). */
  renameToken: number;
  /** Scrolls this minute into view when `token` changes (search results). */
  reveal: { minute: number; token: number } | null;
  onSelect: (key: string | null, opts?: { focusNote: boolean }) => void;
  onCommit: (next: UiBlock[]) => void;
  onNotice: (message: string) => void;
  /** Google is connected: events (and blocks standing for them) can be deleted from Google Calendar. */
  canDeleteEvents: boolean;
  /** Deletes the event in Google Calendar; false when it failed (the app already showed a notice). */
  onDeleteEvent: (eventId: string) => Promise<boolean>;
  /** Context menu "Start timer" / "Stop timer" (same as the timer bar). */
  onStartTimer: (key: string) => void;
  onStopTimer: () => void;
  /** Rendered first, sticky at the top of the timeline (the quick-add bar). */
  top?: ReactNode;
}

/** The inline confirm over the timeline. */
type Ask =
  | { kind: "event"; event: CalEvent }
  | { kind: "both"; key: string; event: CalEvent }
  | { kind: "refused"; text: string; event: CalEvent };

const isTypingTarget = (t: EventTarget | null) =>
  t instanceof HTMLElement && (t.closest("input, textarea, select, [contenteditable]") !== null);

export function DayView({ date, blocks, events, loaded, tagColors, now, sessions, timing, runningStart, clockMs, liveTime, preview, shortcutsOff, dragActive, selected, renameToken, reveal, onSelect, onCommit, onNotice, canDeleteEvents, onDeleteEvent, onStartTimer, onStopTimer, top }: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const grid = useRef<HTMLDivElement>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [pending, setPending] = useState<UiBlock | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [ask, setAsk] = useState<Ask | null>(null);
  const [asking, setAsking] = useState(false);
  const stopDrag = useRef<(() => void) | null>(null);

  // Latest props for the native pointer listeners, which outlive a render.
  const live = useRef({ blocks, loaded, onCommit, onSelect });
  live.current = { blocks, loaded, onCommit, onSelect };
  // Latest blocks and sessions for the async delete confirm.
  const fresh = useRef({ blocks, sessions });
  fresh.current = { blocks, sessions };

  const isToday = date === toDateString(now);
  const nowMinutes = now.getHours() * 60 + now.getMinutes();

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const top = isToday ? Math.max(0, nowMinutes - 60) : 7 * 60;
    el.scrollTop = top * PX_PER_MIN;
    // Only on mount: DayView is keyed by date.
  }, []);

  // A rename request that predates this mount (the view is keyed by date) was already handled.
  const renamed = useRef(renameToken);
  useEffect(() => {
    if (renameToken === renamed.current) return;
    renamed.current = renameToken;
    if (loaded && !pending && selected && blocks.some((b) => b.key === selected)) setEditing(selected);
  }, [renameToken, loaded, pending, selected, blocks]);

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

  /** The one delete path (Delete key and context menu): refuses blocks with notes/sessions, asks before dropping an event's block. */
  const deleteBlock = (block: UiBlock) => {
    const plan = blockDeletePlan({
      hasSessions: blockHasSessions(sessions, block),
      hasNote: block.body.trim() !== "",
      event: matchBlocks(blocks, events).get(block.key),
      canDeleteEvent: canDeleteEvents,
    });
    if (plan.kind === "refuse") {
      const text =
        plan.reason === "sessions" ? "This block has timer sessions and can't be deleted." : "This block has notes. Edit the file to remove it.";
      if (plan.event) setAsk({ kind: "refused", text, event: plan.event });
      else onNotice(text);
    } else if (plan.kind === "confirm") {
      setAsk({ kind: "both", key: block.key, event: plan.event });
    } else {
      onSelect(null);
      onCommit(blocks.filter((b) => b.key !== block.key));
    }
  };

  const blockContextMenu = (block: UiBlock) => {
    if (!loaded || pending || dragActive.current) return;
    onSelect(block.key);
    const event = matchBlocks(blocks, events).get(block.key);
    const entries = blockMenu({
      isToday,
      timingHere: timing !== null && sameBlock(timing, block),
      hasEvent: event !== undefined,
      canDeleteEvents,
    });
    void popupMenu(entries, (id) => {
      if (id === "open") onSelect(block.key, { focusNote: true });
      else if (id === "start") onStartTimer(block.key);
      else if (id === "stop") onStopTimer();
      else if (id === "delete-block") deleteBlock(block);
      else if (id === "delete-event" && event) setAsk({ kind: "event", event });
    });
  };

  const ghostContextMenu = (event: CalEvent) => {
    if (dragActive.current) return;
    void popupMenu(ghostMenu({ canDeleteEvents }), (id) => {
      if (id === "open") openEvent(event);
      else if (id === "delete-event") setAsk({ kind: "event", event });
    });
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (shortcutsOff || e.metaKey || e.ctrlKey || e.altKey || isTypingTarget(e.target) || dragActive.current || !selected) return;
      // Enter (open the note) belongs to the app-wide shortcuts in `lib/keys.ts`; renaming is a double-click.
      if (e.key === "Escape" && ask) {
        setAsk(null);
      } else if (e.key === "Delete" || e.key === "Backspace") {
        e.preventDefault();
        const block = blocks.find((b) => b.key === selected);
        if (block) deleteBlock(block);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shortcutsOff, selected, blocks, events, sessions, ask, canDeleteEvents, dragActive, onCommit, onSelect, onNotice]);

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

  /** Deletes the event in Google; for "both", the block goes too, but only if it is still empty. */
  const confirmAsk = async (withBlock: boolean) => {
    if (!ask || asking) return;
    setAsking(true);
    const ok = await onDeleteEvent(ask.event.id);
    setAsking(false);
    if (!ok) return;
    setAsk(null);
    if (!withBlock || ask.kind !== "both") return;
    const { blocks: latest, sessions: latestSessions } = fresh.current;
    const block = latest.find((b) => b.key === ask.key);
    if (!block || block.body.trim() !== "" || blockHasSessions(latestSessions, block)) return;
    live.current.onSelect(null);
    live.current.onCommit(latest.filter((b) => b.key !== ask.key));
  };

  const askText = () => {
    if (!ask) return null;
    const title = `“${ask.event.title}”`;
    const series = isSeriesInstance(ask.event) ? "Only this occurrence." : null;
    if (ask.kind === "event") return { main: `Delete ${title} from Google Calendar?`, hint: series };
    if (ask.kind === "both") return { main: `Also delete ${title} from Google Calendar?`, hint: series };
    return { main: ask.text, hint: [`The block and its note stay; only the Google event ${title} goes.`, series].filter(Boolean).join(" ") };
  };
  const text = askText();

  return (
    <div className="day" ref={scroller}>
      {top}
      {ask && text && (
        <div className="dayask" role="alertdialog" aria-label="Confirm delete">
          <span className="dayask__text">
            {text.main}
            {text.hint && <span className="dayask__hint">{text.hint}</span>}
          </span>
          <span className="dayask__actions">
            <Button size="sm" variant="danger" disabled={asking} onClick={() => void confirmAsk(ask.kind === "both")}>
              {ask.kind === "both" ? "Delete both" : ask.kind === "refused" ? "Delete the event from Google Calendar" : "Delete"}
            </Button>
            {ask.kind === "both" && (
              <Button
                size="sm"
                disabled={asking}
                onClick={() => {
                  const key = ask.key;
                  setAsk(null);
                  onSelect(null);
                  onCommit(blocks.filter((b) => b.key !== key));
                }}
              >
                Only the block
              </Button>
            )}
            <Button size="sm" variant="ghost" disabled={asking} onClick={() => setAsk(null)}>
              Cancel
            </Button>
          </span>
        </div>
      )}
      <div
        className={loaded ? "day__grid" : "day__grid day__grid--disabled"}
        ref={grid}
        style={{ height: 24 * PX_PER_HOUR }}
        onPointerDown={(e) => {
          if ((e.target as HTMLElement).closest("[data-block]")) return;
          startDrag(e, "create", "new");
        }}
      >
        {/* Hour lines and a faint half-hour line; the 15-min snap is math, not markup. */}
        {HOURS.map((h) => (
          <div key={h} className="day__line" style={{ top: h * PX_PER_HOUR }} />
        ))}
        {HOURS.map((h) => (
          <div key={`half-${h}`} className="day__line day__line--half" style={{ top: h * PX_PER_HOUR + PX_PER_HOUR / 2 }} />
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
              live={timing !== null && sameBlock(timing, b) ? liveTime : null}
              logged={blockTotalMinutes(sessionsForBlock(sessions, b).filter((s) => s.end !== null))}
              done={b.body.trim() !== "" || blockHasSessions(sessions, b)}
              editing={editing === b.key}
              calendar={matched.has(b.key) ? "synced" : undefined}
              onBodyPointerDown={(e) => startDrag(e, "move", b.key)}
              onHandlePointerDown={(e, edge) => startDrag(e, edge === "start" ? "resize-start" : "resize-end", b.key)}
              onDoubleClick={() => loaded && !pending && setEditing(b.key)}
              onContextMenu={pending?.key === b.key ? undefined : () => blockContextMenu(b)}
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
              onContextMenu={() => ghostContextMenu(ev)}
              onEventDelete={canDeleteEvents ? () => setAsk({ kind: "event", event: ev }) : undefined}
            />
          ))}
          {preview && !draft && (
            <TimelineBlock start={preview.start} end={preview.end} title="" tags={[]} color={null} lane={0} lanes={1} selected={false} editing={false} ghost />
          )}
          {draft && draft.key === "new" && (
            <TimelineBlock start={draft.start} end={draft.end} title="" tags={[]} color={null} lane={0} lanes={1} selected={false} editing={false} ghost />
          )}
        </div>
        {isToday && nowMinutes < DAY_MINUTES && <div className="day__now" style={{ top: nowMinutes * PX_PER_MIN }} />}
      </div>
    </div>
  );
}
