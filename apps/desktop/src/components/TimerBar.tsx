import { ArrowsPointingOutIcon, ForwardIcon, PlayIcon, StopIcon, TrashIcon } from "@heroicons/react/20/solid";
import type { PomodoroPlan, Session, TimerMode, TimerStatus } from "../api";
import { blockTotalMinutes, fmtElapsed, fmtRemaining, listedSessions, sameBlock, sessionLine, sessionsForBlock } from "../lib/timer";
import { PlanRow } from "./PlanRow";
import "../timer.css";

const IS_MAC = navigator.userAgent.includes("Mac");

export interface TimerPanel {
  /** The day being shown; Start only exists when it is today. */
  date: string;
  isToday: boolean;
  status: TimerStatus;
  nowMs: number;
  /** All sessions of the shown day. */
  sessions: Session[];
  /** What Start will use: the mode and, for Pomodoro, the plan (both remembered). */
  mode: TimerMode;
  onMode: (mode: TimerMode) => void;
  plan: PomodoroPlan;
  onPlan: (plan: PomodoroPlan) => void;
  onStart: () => void;
  onStop: () => void;
  onSkipBreak: () => void;
  /** Opens the focus screen. */
  onFocus: () => void;
  onDeleteSession: (start: string) => void;
}

interface Props {
  block: { start: number; end: number; title: string };
  timer: TimerPanel;
}

/** Display of a running timer: elapsed (free) or remaining + phase (Pomodoro). */
export function timerReadout(status: Extract<TimerStatus, { state: "running" }>, nowMs: number): { time: string; phase: string | null } {
  if (status.mode === "pomodoro" && status.phaseEndsAt) {
    return {
      time: fmtRemaining(new Date(status.phaseEndsAt).getTime() - nowMs),
      phase: status.phase === "focus" ? "Focus" : "Break",
    };
  }
  return { time: fmtElapsed(nowMs - new Date(status.sessionStart).getTime()), phase: null };
}

/** "Focus 2 of 4" / "Break 2 of 4"; null for a free timer. */
export function cycleLabel(status: Extract<TimerStatus, { state: "running" }>): string | null {
  if (status.mode !== "pomodoro" || status.cycle === null || status.cycles === null) return null;
  return `${status.phase === "focus" ? "Focus" : "Break"} ${status.cycle} of ${status.cycles}`;
}

export function TimerBar({ block, timer }: Props) {
  const { status, mode } = timer;
  const mine = status.state === "running" && status.date === timer.date && sameBlock(status.block, block);
  // The live (or stale, handled by its banner) session is not listed; other open ones are crash leftovers.
  const runningStart =
    (status.state === "running" && status.phase === "focus") || status.state === "stale" ? status.sessionStart : null;
  const listed = listedSessions(sessionsForBlock(timer.sessions, block), runningStart);
  const total = blockTotalMinutes(listed);
  const readout = mine && status.state === "running" ? timerReadout(status, timer.nowMs) : null;

  return (
    <section className="timer" aria-label="Timer">
      {(timer.isToday || mine) && (
        <div className="timer__row">
          <div className="timer__modes" role="group" aria-label="Timer mode">
            {(["free", "pomodoro"] as const).map((m) => (
              <button
                key={m}
                className={mode === m ? "timer__mode timer__mode--on" : "timer__mode"}
                aria-pressed={mode === m}
                disabled={mine}
                onClick={() => timer.onMode(m)}
              >
                {m === "free" ? "Free" : "Pomodoro"}
              </button>
            ))}
          </div>
          <span className="timer__clock" role="timer" aria-label="Timer reading">
            {readout ? readout.time : mode === "free" ? "0:00:00" : "--:--"}
          </span>
          {readout?.phase && <span className="timer__phase">{readout.phase}</span>}
          {mine && status.state === "running" && status.phase === "break" && (
            <button className="app__btn timer__btn" onClick={timer.onSkipBreak}>
              <ForwardIcon className="app__icon" aria-hidden="true" />
              Skip break
            </button>
          )}
          {mine ? (
            <button className="app__btn timer__btn" onClick={timer.onStop}>
              <StopIcon className="app__icon" aria-hidden="true" />
              Stop
            </button>
          ) : (
            <button className="app__btn timer__btn timer__btn--start" onClick={timer.onStart}>
              <PlayIcon className="app__icon" aria-hidden="true" />
              Start
            </button>
          )}
          <button
            className="app__btn timer__focus"
            aria-label="Open the focus screen"
            title={`Focus screen (${IS_MAC ? "⌘" : "Ctrl+"}⇧F)`}
            onClick={timer.onFocus}
          >
            <ArrowsPointingOutIcon className="app__icon" aria-hidden="true" />
          </button>
        </div>
      )}
      {mine && status.state === "running" && cycleLabel(status) ? (
        <p className="timer__cycle">
          {cycleLabel(status)} · {status.focusMinutes} min focus · {status.breakMinutes} min break
        </p>
      ) : (
        timer.isToday && mode === "pomodoro" && <PlanRow plan={timer.plan} onChange={timer.onPlan} />
      )}
      {listed.length > 0 && (
        <>
          <ul className="timer__sessions">
            {listed.map((s) => (
              <li key={s.start} className="timer__session">
                <span>{sessionLine(s)}</span>
                <button
                  className="timer__delete"
                  aria-label={`Delete session ${sessionLine(s)}`}
                  title="Delete this session"
                  onClick={() => timer.onDeleteSession(s.start)}
                >
                  <TrashIcon className="app__icon" aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
          <p className="timer__total">Total {total} min</p>
        </>
      )}
    </section>
  );
}
