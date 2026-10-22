import { ArrowsPointingOutIcon, ClockIcon, StopIcon } from "@heroicons/react/20/solid";
import type { TimerStatus } from "../api";
import { timerReadout } from "./TimerBar";
import "../timer.css";

interface Props {
  status: TimerStatus;
  nowMs: number;
  onOpen: () => void;
  onStop: () => void;
  onFocus: () => void;
}

/** Header chip shown while a timer runs, on any day or view. */
export function TimerChip({ status, nowMs, onOpen, onStop, onFocus }: Props) {
  if (status.state !== "running") return null;
  const { time, phase } = timerReadout(status, nowMs);
  const title = status.block.title || "Untitled";
  return (
    <div className="chip">
      <button className="chip__open" onClick={onOpen} title="Go to the running block">
        <ClockIcon className="app__icon" aria-hidden="true" />
        <span className="chip__title">{title}</span>
        {phase && <span className="chip__phase">{phase}</span>}
        <span className="chip__time">{time}</span>
      </button>
      <button className="chip__stop" aria-label="Open the focus screen" title="Focus screen" onClick={onFocus}>
        <ArrowsPointingOutIcon className="app__icon" aria-hidden="true" />
      </button>
      <button className="chip__stop" aria-label="Stop timer" title="Stop timer" onClick={onStop}>
        <StopIcon className="app__icon" aria-hidden="true" />
      </button>
    </div>
  );
}
