import {
  CalendarIcon,
  ChartBarIcon,
  CheckCircleIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  Cog6ToothIcon,
  FlagIcon,
  MagnifyingGlassIcon,
  PlayIcon,
  PlusIcon,
  StopIcon,
} from "@heroicons/react/20/solid";
import type { TimerStatus } from "../api";
import { tagColorVar } from "../lib/tags";
import { fmtDateHeader, fmtMinutes } from "../lib/time";
import { timerReadout } from "./TimerBar";
import "../app.css";

const IS_MAC = navigator.userAgent.includes("Mac");
const MOD = IS_MAC ? "⌘" : "Ctrl+";

export type Page = "day" | "recap" | "goals" | "dashboard";

const PAGES = [
  { id: "day", label: "Day", Icon: CalendarIcon, hint: "" },
  { id: "recap", label: "Done", Icon: CheckCircleIcon, hint: "What got done on this day" },
  { id: "goals", label: "Goals", Icon: FlagIcon, hint: `Goals (${MOD}G)` },
  { id: "dashboard", label: "Dashboard", Icon: ChartBarIcon, hint: `Dashboard (${MOD}D)` },
] as const;

interface Props {
  /** The page shown; null for pages outside the nav (history). */
  page: Page | null;
  date: string;
  today: string;
  /** Today's current-or-next block for "Start"; null when unknown (another day) or nothing is left. */
  startTarget: { title: string; start: number; end: number } | null;
  /** True when today is shown, loaded and has nothing left to start. */
  nothingToStart: boolean;
  status: TimerStatus;
  nowMs: number;
  tagColors: Record<string, string>;
  onPrevDay: () => void;
  onNextDay: () => void;
  onToday: () => void;
  onStartNow: () => void;
  onOpenTimer: () => void;
  onStopTimer: () => void;
  onNav: (page: Page) => void;
  onQuickAdd: () => void;
  onSearch: () => void;
  onSettings: () => void;
  inert?: boolean;
}

/** Three zones: where (brand + day paging), now (one fixed-width slot: Start, then the running timer), go (pages + tools). */
export function Header(p: Props) {
  const running = p.status.state === "running" ? p.status : null;
  return (
    <header className="app__header" inert={p.inert}>
      <div className="app__brand">
        <img className="app__logo" src="/mark.svg" alt="" />
        <h1 className="app__wordmark">
          <b>Catenga</b> Focus
        </h1>
      </div>
      <div className="app__day" role="group" aria-label="Day">
        <button className="app__icon-btn app__icon-btn--muted" onClick={p.onPrevDay} aria-label="Previous day" title="Previous day (←)">
          <ChevronLeftIcon className="app__icon" aria-hidden="true" />
        </button>
        <time className="app__date" dateTime={p.date}>
          {fmtDateHeader(p.date, p.today)}
        </time>
        <button className="app__icon-btn app__icon-btn--muted" onClick={p.onNextDay} aria-label="Next day" title="Next day (→)">
          <ChevronRightIcon className="app__icon" aria-hidden="true" />
        </button>
        {p.date !== p.today && (
          <button className="app__today" onClick={p.onToday} title="Go to today (T)">
            Today
          </button>
        )}
      </div>
      {/* Fixed width: the row never reflows when the timer starts or stops. */}
      <div className="app__now">
        {running ? (
          <RunningTimer status={running} nowMs={p.nowMs} tagColors={p.tagColors} onOpen={p.onOpenTimer} onStop={p.onStopTimer} />
        ) : (
          <button
            className="app__start"
            onClick={p.onStartNow}
            disabled={p.nothingToStart}
            title={p.nothingToStart ? "Nothing left on today's calendar" : "Start the timer on the block happening now (or the next one)"}
          >
            <PlayIcon className="app__icon" aria-hidden="true" />
            <span className="app__start-label">{p.startTarget ? `Start ${p.startTarget.title}` : "Start now"}</span>
            {p.startTarget && (
              <span className="app__start-meta">
                {fmtMinutes(p.startTarget.start)}–{fmtMinutes(p.startTarget.end)}
              </span>
            )}
          </button>
        )}
      </div>
      <nav className="app__nav" aria-label="Pages">
        {PAGES.map(({ id, label, Icon, hint }) => (
          <button
            key={id}
            className="app__nav-item"
            aria-current={p.page === id ? "page" : undefined}
            aria-label={label}
            title={hint || label}
            onClick={() => p.onNav(id)}
          >
            <Icon className="app__icon" aria-hidden="true" />
            <span className="app__nav-label">{label}</span>
          </button>
        ))}
      </nav>
      <div className="app__tools">
        <button className="app__icon-btn" onClick={p.onQuickAdd} aria-label="Add a block" title={`Add block (N, ${MOD}N)`}>
          <PlusIcon className="app__icon" aria-hidden="true" />
        </button>
        <button className="app__icon-btn" onClick={p.onSearch} aria-label="Search notes" title={`Search notes (${MOD}K)`}>
          <MagnifyingGlassIcon className="app__icon" aria-hidden="true" />
        </button>
        <button className="app__icon-btn" onClick={p.onSettings} aria-label="Settings" title="Settings">
          <Cog6ToothIcon className="app__icon" aria-hidden="true" />
        </button>
      </div>
    </header>
  );
}

function RunningTimer(p: {
  status: Extract<TimerStatus, { state: "running" }>;
  nowMs: number;
  tagColors: Record<string, string>;
  onOpen: () => void;
  onStop: () => void;
}) {
  const { status } = p;
  const { time } = timerReadout(status, p.nowMs);
  const title = status.block.title || "Untitled";
  const phase =
    status.mode === "pomodoro" && status.cycle !== null && status.cycles !== null
      ? `${status.phase === "focus" ? "Focus" : "Break"} ${status.cycle}/${status.cycles}`
      : null;
  return (
    <div className="app__timer" role="group" aria-label="Running timer">
      <button className="app__timer-open" onClick={p.onOpen} title="Go to the running block">
        <span className="app__timer-dot" style={{ background: tagColorVar(status.block.tags, p.tagColors) ?? "var(--color-sun)" }} />
        <span className="app__timer-title">{title}</span>
        {phase && <span className="app__timer-phase">{phase}</span>}
        <span className="app__timer-time" role="timer" aria-live="off">
          {time}
        </span>
      </button>
      <button className="app__timer-stop" onClick={p.onStop} aria-label="Stop timer" title="Stop timer">
        <StopIcon className="app__icon" aria-hidden="true" />
      </button>
    </div>
  );
}
