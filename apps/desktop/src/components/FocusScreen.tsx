import { useEffect, useRef, useState } from "react";
import {
  ArrowsPointingInIcon,
  ForwardIcon,
  PlayIcon,
  StopIcon,
} from "@heroicons/react/20/solid";
import type { PomodoroPlan, TimerMode, TimerStatus } from "../api";
import type { Conflict, SaveState, UiBlock } from "../hooks/useDay";
import {
  cycleDots,
  digitDiff,
  finishedMessage,
  ringProgress,
} from "../lib/focus";
import { bodyToNote } from "../lib/note";
import { tagColorVar } from "../lib/tags";
import { fmtMinutes } from "../lib/time";
import { sameBlock } from "../lib/timer";
import { NoteEditor } from "./NotesPanel";
import { PlanRow } from "./PlanRow";
import { timerReadout } from "./TimerBar";
import { Button } from "./ui/Button";
import "../focus.css";

const LEAVE_MS = 250;

const IS_MAC = navigator.userAgent.includes("Mac");

const reducedMotion = () =>
  typeof window.matchMedia === "function" &&
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

const SAVE_TEXT: Partial<Record<SaveState, string>> = {
  saving: "Saving…",
  saved: "Saved",
  conflict: "File changed on disk",
};

// Ring geometry (SVG user units).
const R = 92;
const CIRCUMFERENCE = 2 * Math.PI * R;

function Ring({ progress, active }: { progress: number; active: boolean }) {
  const previous = useRef(0);
  // A phase change or a new lap sends the ring back to empty: do that instantly instead of unwinding it.
  const jump = progress < previous.current - 0.02;
  useEffect(() => {
    previous.current = progress;
  });
  return (
    <svg
      className={jump ? "ring ring--jump" : "ring"}
      viewBox="0 0 200 200"
      aria-hidden="true"
    >
      <circle className="ring__disc" cx="100" cy="100" r={R - 4} />
      <circle
        className="ring__ticks"
        cx="100"
        cy="100"
        r="82"
        strokeDasharray={`1 ${(2 * Math.PI * 82) / 4 - 1}`}
      />
      <circle className="ring__track" cx="100" cy="100" r={R} />
      <circle
        className="ring__arc"
        cx="100"
        cy="100"
        r={R}
        strokeDasharray={CIRCUMFERENCE}
        strokeDashoffset={CIRCUMFERENCE * (1 - progress)}
      />
      {active && (
        <g
          className="ring__tip"
          style={{ transform: `rotate(${progress * 360}deg)` }}
        >
          <circle cx="100" cy="8" r="5" />
        </g>
      )}
    </svg>
  );
}

/** Fixed-width glyph cells (no layout shift); only the characters that changed slide in. */
function Digits({ value }: { value: string }) {
  const previous = useRef<string | null>(null);
  const changed = digitDiff(previous.current, value);
  useEffect(() => {
    previous.current = value;
  });
  return (
    <span className="digits" aria-hidden="true">
      {[...value].map((ch, i) => (
        <span
          key={`${i}:${ch}`}
          className={`digits__cell${ch === ":" ? " digits__cell--colon" : ""}${changed[i] ? " digits__cell--tick" : ""}`}
        >
          {ch}
        </span>
      ))}
    </span>
  );
}

/** Width of the readout in em, to size the digits so they always fit inside the ring. */
const widthEm = (value: string) =>
  [...value].reduce((sum, ch) => sum + (ch === ":" ? 0.3 : 0.6), 0);

interface Props {
  block: UiBlock;
  conflict: Conflict | undefined;
  date: string;
  isToday: boolean;
  status: TimerStatus;
  nowMs: number;
  /** Set when the last Pomodoro run ended on its own. */
  finished: { cycles: number; id: number } | null;
  mode: TimerMode;
  onMode: (mode: TimerMode) => void;
  plan: PomodoroPlan;
  onPlan: (plan: PomodoroPlan) => void;
  tagColors: Record<string, string>;
  saveState: SaveState;
  noteVersion: number;
  /** An error or notice that would otherwise be hidden behind this screen. */
  banner: string | null;
  onDismiss: () => void;
  onNote: (key: string, note: string) => void;
  onFlush: () => void;
  onRetry: () => void;
  onResolve: (key: string, choice: "mine" | "file") => void;
  onStart: () => void;
  onStop: () => void;
  onSkipBreak: () => void;
  /** Called once the leave animation is over. */
  onExit: () => void;
}

/** Full-window screen for the current task: its title, a big animated timer, and its note. */
export function FocusScreen(p: Props) {
  const { block, status } = p;
  const [leaving, setLeaving] = useState(false);
  const [editingPlan, setEditingPlan] = useState(false);
  const leavingRef = useRef(false);
  const flushRef = useRef(p.onFlush);
  flushRef.current = p.onFlush;
  const exitRef = useRef(p.onExit);
  exitRef.current = p.onExit;

  const requestExit = useRef(() => {});
  requestExit.current = () => {
    if (leavingRef.current) return;
    leavingRef.current = true;
    flushRef.current();
    setLeaving(true);
    window.setTimeout(() => exitRef.current(), reducedMotion() ? 0 : LEAVE_MS);
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const mod = IS_MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (mod && e.shiftKey && !e.altKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        requestExit.current();
        return;
      }
      if (e.key !== "Escape") return;
      const target = e.target instanceof HTMLElement ? e.target : null;
      if (target?.closest("textarea")) return; // the editor blurs itself; the next Esc leaves
      e.preventDefault();
      if (target?.closest("input")) {
        target.blur();
        return;
      }
      requestExit.current();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

  const mine =
    status.state === "running" &&
    status.date === p.date &&
    sameBlock(status.block, block);
  const live = mine && status.state === "running" ? status : null;
  const readout = live ? timerReadout(live, p.nowMs) : null;
  const pomodoro = live ? live.mode === "pomodoro" : p.mode === "pomodoro";
  const inBreak = live?.phase === "break";
  const idleTime = pomodoro
    ? `${String(p.plan.focusMinutes).padStart(2, "0")}:00`
    : "0:00:00";
  const time = readout?.time ?? idleTime;
  const done = !live && p.finished ? finishedMessage(p.finished.cycles) : null;
  const phase = live && pomodoro ? (inBreak ? "Break" : "Focus") : null;
  const label =
    done ??
    (live
      ? phase
        ? live.cycle !== null && live.cycles !== null
          ? `${phase} ${live.cycle} of ${live.cycles}`
          : phase
        : "Tracking"
      : "Ready");
  const dots = live
    ? cycleDots(live)
    : pomodoro
      ? cycleDots({ state: "idle" }, p.plan.cycles)
      : [];
  const state = live ? (inBreak ? "break" : "run") : "idle";
  const em = widthEm(time);
  const canStart = p.isToday;
  const saveText = p.saveState === "error" ? null : SAVE_TEXT[p.saveState];

  return (
    <div
      className={`focus focus--${state}${leaving ? " focus--leaving" : ""}`}
      role="dialog"
      aria-modal="true"
      aria-label="Focus screen"
    >
      <button
        className="focus__close"
        aria-label="Leave the focus screen"
        title={`Leave (Esc, ${IS_MAC ? "⌘" : "Ctrl+"}⇧F)`}
        onClick={() => requestExit.current()}
      >
        <ArrowsPointingInIcon
          className="focus__close-icon"
          aria-hidden="true"
        />
      </button>
      {p.banner && (
        <div className="focus__toast" role="alert">
          <span>{p.banner}</span>
          <button className="focus__toast-btn" onClick={p.onDismiss}>
            Dismiss
          </button>
        </div>
      )}
      <div className="focus__layout">
        <div className="focus__main">
          <header className="focus__head focus__rise">
            <h1 className="focus__title">{block.title || "Untitled"}</h1>
            <p className="focus__meta">
              <span className="focus__time">
                {fmtMinutes(block.start)}–{fmtMinutes(block.end)}
              </span>
              {block.tags.map((tag) => (
                <span key={tag} className="focus__tag">
                  <span
                    className="focus__tag-dot"
                    style={{
                      background:
                        tagColorVar([tag], p.tagColors) ?? "var(--color-line)",
                    }}
                  />
                  #{tag}
                </span>
              ))}
            </p>
          </header>

          <section className="focus__stage focus__rise" aria-label="Timer">
            <div className="focus__ring">
              {done && (
                <span
                  key={p.finished?.id}
                  className="focus__burst"
                  aria-hidden="true"
                />
              )}
              <Ring
                progress={live ? ringProgress(live, p.nowMs) : 0}
                active={live !== null}
              />
              <div
                className="focus__inner"
                style={{ ["--em" as string]: String(Math.min(0.3, 0.76 / em)) }}
              >
                <span
                  key={label}
                  className={
                    done ? "focus__label focus__label--done" : "focus__label"
                  }
                >
                  {label}
                </span>
                <span
                  className="focus__clock"
                  role="timer"
                  aria-label={`Timer ${time}`}
                >
                  <Digits value={time} />
                </span>
                <span
                  className="focus__dots"
                  aria-label={
                    live && live.cycle !== null
                      ? `Pomodoro ${live.cycle} of ${live.cycles}`
                      : undefined
                  }
                >
                  {dots.map((d, i) => (
                    <span key={i} className={`dot dot--${d}`} />
                  ))}
                </span>
              </div>
            </div>

            <div className="focus__controls" key={state}>
              {!live &&
                (p.mode === "pomodoro" ? (
                  editingPlan ? (
                    <div
                      className="focus__plan-edit"
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === "Escape") {
                          e.preventDefault();
                          e.stopPropagation();
                          setEditingPlan(false);
                        }
                      }}
                      onBlur={(e) => {
                        if (
                          !e.currentTarget.contains(
                            e.relatedTarget as Node | null,
                          )
                        )
                          setEditingPlan(false);
                      }}
                    >
                      <PlanRow
                        plan={p.plan}
                        onChange={p.onPlan}
                        large
                        autoFocus
                      />
                    </div>
                  ) : (
                    <p className="focus__plan">
                      <span>
                        Pomodoro · {p.plan.cycles} × {p.plan.focusMinutes} +{" "}
                        {p.plan.breakMinutes}
                      </span>
                      <button
                        type="button"
                        className="focus__textbtn"
                        onClick={() => setEditingPlan(true)}
                      >
                        Edit plan
                      </button>
                      <button
                        type="button"
                        className="focus__textbtn"
                        onClick={() => p.onMode("free")}
                      >
                        Switch to free
                      </button>
                    </p>
                  )
                ) : (
                  <p className="focus__plan">
                    <span>Free-running</span>
                    <button
                      type="button"
                      className="focus__textbtn"
                      onClick={() => p.onMode("pomodoro")}
                    >
                      Switch to Pomodoro
                    </button>
                  </p>
                ))}
              <div className="focus__buttons">
                {live ? (
                  <>
                    {inBreak && (
                      <Button
                        size="lg"
                        icon={ForwardIcon}
                        onClick={p.onSkipBreak}
                      >
                        Skip break
                      </Button>
                    )}
                    <Button size="lg" icon={StopIcon} onClick={p.onStop}>
                      Stop
                    </Button>
                  </>
                ) : (
                  <Button
                    variant="primary"
                    size="lg"
                    icon={PlayIcon}
                    disabled={!canStart}
                    onClick={p.onStart}
                  >
                    Start
                  </Button>
                )}
              </div>
              {!live && !canStart && (
                <p className="focus__hint">Timers run on today's blocks.</p>
              )}
            </div>
          </section>
        </div>

        <section className="focus__card focus__rise" aria-label="Note">
          <div className="focus__card-top">
            <span className="focus__status" role="status" aria-live="polite">
              {p.saveState === "error" ? (
                <>
                  Not saved ·{" "}
                  <button className="focus__link" onClick={p.onRetry}>
                    Retry
                  </button>
                </>
              ) : (
                <span
                  key={p.saveState}
                  className={
                    p.saveState === "saved" ? "focus__saved" : undefined
                  }
                >
                  {saveText}
                </span>
              )}
            </span>
          </div>
          {p.conflict && (
            <div className="focus__conflict" role="alert">
              <span>This note changed on disk.</span>
              <span className="focus__conflict-actions">
                <button
                  className="focus__toast-btn"
                  onClick={() => p.onResolve(block.key, "mine")}
                >
                  Keep mine
                </button>
                <button
                  className="focus__toast-btn"
                  onClick={() => p.onResolve(block.key, "file")}
                >
                  Use file's
                </button>
              </span>
            </div>
          )}
          <NoteEditor
            key={`${block.key}:${p.noteVersion}`}
            initial={bodyToNote(p.conflict ? p.conflict.mine.body : block.body)}
            focusToken={0}
            onChange={(text) => p.onNote(block.key, text)}
            onBlur={p.onFlush}
            className="focus__editor"
            autoGrow
          />
        </section>
      </div>
    </div>
  );
}
