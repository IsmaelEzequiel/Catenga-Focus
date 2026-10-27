import { useState } from "react";
import { ArrowsPointingOutIcon, PlayIcon, StopIcon, TrashIcon } from "@heroicons/react/20/solid";
import type { PomodoroPlan, Session, TimerMode, TimerStatus } from "../api";
import { fmtDuration } from "../lib/dashboard";
import { cycleDots } from "../lib/focus";
import { blockTotalMinutes, fmtElapsed, fmtRemaining, listedSessions, sameBlock, sessionLine, sessionsForBlock } from "../lib/timer";
import { PlanRow } from "./PlanRow";
import { Button } from "./ui/Button";
import "../segmented.css";
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

/** The timer card: mode + focus screen on top, big digits + one primary button, then the Pomodoro plan line. */
export function TimerBar({ block, timer }: Props) {
  const { status, mode } = timer;
  const [editingPlan, setEditingPlan] = useState(false);
  const mine = status.state === "running" && status.date === timer.date && sameBlock(status.block, block);
  if (!timer.isToday && !mine) return null;
  const running = mine && status.state === "running" ? status : null;
  const readout = running ? timerReadout(running, timer.nowMs) : null;
  const pomodoro = running ? running.mode === "pomodoro" : mode === "pomodoro";
  const idleTime = mode === "free" ? "0:00:00" : `${String(timer.plan.focusMinutes).padStart(2, "0")}:00`;
  const dots = running ? cycleDots(running) : pomodoro ? cycleDots({ state: "idle" }, timer.plan.cycles) : [];
  const onBreak = running?.phase === "break";

  return (
    <section className="timer" aria-label="Timer">
      <div className="timer__top">
        <div className="seg seg--sm" role="group" aria-label="Timer mode">
          {(["free", "pomodoro"] as const).map((m) => (
            <button key={m} className="seg__item" aria-pressed={(running ? running.mode : mode) === m} disabled={mine} onClick={() => timer.onMode(m)}>
              {m === "free" ? "Free" : "Pomodoro"}
            </button>
          ))}
        </div>
        <Button
          variant="ghost"
          size="sm"
          icon={ArrowsPointingOutIcon}
          className="button--icon timer__focus"
          aria-label="Open the focus screen"
          title={`Focus screen (F, ${IS_MAC ? "⌘" : "Ctrl+"}⇧F)`}
          onClick={timer.onFocus}
        />
      </div>
      <div className="timer__main">
        <span className="timer__clock" role="timer" aria-label="Timer reading">
          {readout ? readout.time : idleTime}
        </span>
        {mine ? (
          <Button size="lg" icon={StopIcon} onClick={timer.onStop}>
            Stop
          </Button>
        ) : (
          <Button variant="primary" size="lg" icon={PlayIcon} onClick={timer.onStart}>
            Start
          </Button>
        )}
      </div>
      {pomodoro &&
        (editingPlan && !running ? (
          <div
            className="timer__plan-edit"
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === "Escape") {
                e.preventDefault();
                setEditingPlan(false);
              }
            }}
            onBlur={(e) => {
              if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setEditingPlan(false);
            }}
          >
            <PlanRow plan={timer.plan} onChange={timer.onPlan} autoFocus />
          </div>
        ) : (
          <div className="timer__plan">
            <span className="timer__dots" aria-hidden="true">
              {dots.map((d, i) => (
                <i key={i} className={`timer__dot timer__dot--${d}`} />
              ))}
            </span>
            <span className="timer__phase">
              {running && running.cycle !== null && running.cycles !== null
                ? `${onBreak ? "Break" : "Focus"} ${running.cycle} of ${running.cycles} · ${running.focusMinutes} + ${running.breakMinutes}`
                : `${timer.plan.cycles} × ${timer.plan.focusMinutes} min + ${timer.plan.breakMinutes} min break`}
            </span>
            {onBreak && (
              <button className="timer__link" onClick={timer.onSkipBreak}>
                Skip break
              </button>
            )}
            {!running && (
              <button className="timer__link timer__link--end" onClick={() => setEditingPlan(true)}>
                Edit plan
              </button>
            )}
          </div>
        ))}
    </section>
  );
}

/** Footer summary of the block's finished sessions ("2 sessions · 48m"); opens to list them, each deletable. */
export function SessionsSummary({ block, timer }: Props) {
  const { status } = timer;
  // The live (or stale, handled by its banner) session is not listed; other open ones are crash leftovers.
  const runningStart =
    (status.state === "running" && status.phase === "focus") || status.state === "stale" ? status.sessionStart : null;
  const listed = listedSessions(sessionsForBlock(timer.sessions, block), runningStart);
  if (listed.length === 0) return null;
  const total = blockTotalMinutes(listed);
  return (
    <details className="timer__sessions">
      <summary className="timer__summary">
        {listed.length === 1 ? "1 session" : `${listed.length} sessions`} · {fmtDuration(total)}
      </summary>
      <ul className="timer__list">
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
    </details>
  );
}
