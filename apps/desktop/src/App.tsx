import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChartBarIcon, ChevronLeftIcon, ChevronRightIcon, Cog6ToothIcon, FlagIcon } from "@heroicons/react/20/solid";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { loadConfig, onTagsChanged, timerRelink, type HistoryEntry, type HistoryKind, type SearchHit } from "./api";
import { CommandPalette } from "./components/CommandPalette";
import { DashboardView } from "./components/DashboardView";
import { DayView } from "./components/DayView";
import { FocusScreen } from "./components/FocusScreen";
import { GoalsView } from "./components/GoalsView";
import { HistoryView } from "./components/HistoryView";
import { NotesPanel } from "./components/NotesPanel";
import { TimerChip } from "./components/TimerChip";
import { Settings } from "./components/Settings";
import { useAccount } from "./hooks/useAccount";
import { useCalendar } from "./hooks/useCalendar";
import { LOST_MESSAGE, useGoogle } from "./hooks/useGoogle";
import { useProjects } from "./hooks/useProjects";
import { useDay, type UiBlock } from "./hooks/useDay";
import { useNow } from "./hooks/useNow";
import { useSessions, useTimer } from "./hooks/useTimer";
import { useTimerSetup } from "./hooks/useTimerSetup";
import { defaultPlan } from "./lib/focus";
import { matchBlocks } from "./lib/calendar";
import { isDashboardShortcut, isFocusShortcut, isGoalsShortcut, isSearchShortcut } from "./lib/keys";
import { blockCalendarId, blockProject } from "./lib/projects";
import { resolveBlockKey, type BlockTarget } from "./lib/search";
import { sameBlock, sessionRelinks, toBlockRef } from "./lib/timer";
import { addDays, fmtDateLabel, fmtMinutes, toDateString } from "./lib/time";

const IS_MAC = navigator.userAgent.includes("Mac");

/** A search result waiting for its day to load. */
type Pending = BlockTarget & { date: string };

type View = { kind: "day" } | { kind: "history"; by: HistoryKind; value: string } | { kind: "dashboard" } | { kind: "goals" };
const DAY_VIEW: View = { kind: "day" };

function App() {
  const now = useNow();
  const [date, setDate] = useState(() => toDateString(new Date()));
  const [tagColors, setTagColors] = useState<Record<string, string>>({});
  const [configError, setConfigError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [focusToken, setFocusToken] = useState(0);
  const [view, setView] = useState<View>(DAY_VIEW);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const account = useAccount();
  const google = useGoogle();
  const projects = useProjects();
  const [calendarMinutes, setCalendarMinutes] = useState(5);
  const [pending, setPending] = useState<Pending | null>(null);
  const [focusOpen, setFocusOpen] = useState(false);
  const [planDefaults, setPlanDefaults] = useState(() => defaultPlan(null));
  const [reveal, setReveal] = useState<{ minute: number; token: number } | null>(null);
  const onNotice = useCallback((message: string) => setNotice(message), []);
  const day = useDay(date, onNotice);
  const dayRef = useRef(day);
  dayRef.current = day;
  const calendar = useCalendar(date, google.status?.connected ?? false, calendarMinutes, onNotice, google.recheck);
  const calendarRef = useRef(calendar);
  calendarRef.current = calendar;

  const timer = useTimer(onNotice);
  const timerRef = useRef(timer);
  timerRef.current = timer;
  const selectedKeyRef = useRef(selectedKey);
  selectedKeyRef.current = selectedKey;
  const sessions = useSessions(date, onNotice);
  /** Edits waiting for `timer_relink`; day changes hold off meanwhile so the edit lands on its own day. */
  const relinkPending = useRef(0);
  const relinkChain = useRef<Promise<void>>(Promise.resolve());

  /**
   * Every block edit goes to the day file first; Google is told afterwards and can only add a notice.
   * Sessions follow a moved/renamed block: the sidecar is relinked before the save that rewrites the mirror lines.
   */
  const commitBlocks = useCallback((next: UiBlock[]) => {
    const prev = dayRef.current.blocks;
    const forDate = dateRef.current;
    const links = sessionRelinks(prev, next);
    const apply = () => {
      dayRef.current.commit(next);
      calendarRef.current.sync(prev, next);
    };
    if (links.length === 0 && relinkPending.current === 0) {
      apply();
      return;
    }
    relinkPending.current += 1;
    relinkChain.current = relinkChain.current.then(async () => {
      try {
        if (links.length > 0) await timerRelink(forDate, links);
      } catch (e) {
        // The edit itself still goes through; only the session link may be stale.
        setNotice(`Couldn't update the timer sessions: ${e instanceof Error ? e.message : String(e)}`);
      } finally {
        relinkPending.current -= 1;
      }
      apply();
    });
  }, []);

  useEffect(() => {
    if (google.error === LOST_MESSAGE) setNotice(LOST_MESSAGE);
  }, [google.error]);
  const dateRef = useRef(date);
  dateRef.current = date;
  const viewRef = useRef(view);
  viewRef.current = view;
  const paletteRef = useRef(paletteOpen);
  paletteRef.current = paletteOpen;
  const focusRef = useRef(focusOpen);
  focusRef.current = focusOpen;
  const dragActive = useRef(false);
  const closeAnyway = useRef(false);

  /** Day changes go through here so unsaved or conflicted notes are never abandoned. */
  const goTo = useCallback(
    (next: (current: string) => string) => {
      if (relinkPending.current > 0) return;
      const blocker = dayRef.current.leaveBlocker();
      if (blocker) {
        setNotice(blocker);
        return;
      }
      setSelectedKey(null);
      setPending(null);
      setView(DAY_VIEW);
      setDate(next);
    },
    [],
  );

  /** Opens a search result: its day, with the block selected and scrolled into view. */
  const openBlock = useCallback((target: Pending): boolean => {
    if (target.date !== dateRef.current) {
      if (relinkPending.current > 0) return false;
      const blocker = dayRef.current.leaveBlocker();
      if (blocker) {
        setNotice(blocker);
        return false;
      }
    }
    dayRef.current.flush();
    setPaletteOpen(false);
    setView(DAY_VIEW);
    if (target.date !== dateRef.current) {
      setSelectedKey(null);
      setDate(target.date);
    }
    setPending(target);
    return true;
  }, []);

  const openHit = useCallback((hit: SearchHit | HistoryEntry) => void openBlock(hit), [openBlock]);

  /** The notes panel unmounts while in history, so everything typed must be on disk first. */
  const openHistory = useCallback(async (by: HistoryKind, value: string) => {
    const refuse = () => {
      const blocker = dayRef.current.leaveBlocker();
      if (blocker) setNotice(blocker);
      return blocker !== null;
    };
    if (refuse()) return;
    await dayRef.current.flushAndDrain();
    if (refuse()) return;
    setPaletteOpen(false);
    setView({ kind: "history", by, value });
  }, []);

  /** Like history: the notes panel unmounts, so everything typed must be on disk first. */
  const openPage = useCallback(async (page: "dashboard" | "goals") => {
    const refuse = () => {
      const blocker = dayRef.current.leaveBlocker();
      if (blocker) setNotice(blocker);
      return blocker !== null;
    };
    if (refuse()) return;
    await dayRef.current.flushAndDrain();
    if (refuse()) return;
    setPaletteOpen(false);
    setView({ kind: page });
  }, []);
  const openDashboard = useCallback(() => openPage("dashboard"), [openPage]);
  const openGoals = useCallback(() => openPage("goals"), [openPage]);

  useEffect(() => {
    if (!pending || pending.date !== date) return;
    if (!day.loaded) {
      if (day.error) setPending(null);
      return;
    }
    const key = resolveBlockKey(day.blocks, pending);
    setPending(null);
    if (key === null) {
      setNotice("That block no longer exists in the file.");
      return;
    }
    setSelectedKey(key);
    setReveal((r) => ({ minute: pending.start, token: (r?.token ?? 0) + 1 }));
  }, [pending, date, day.loaded, day.blocks, day.error]);

  const onSelect = useCallback((key: string | null, opts?: { focusNote: boolean }) => {
    dayRef.current.flush();
    setSelectedKey(key);
    if (opts?.focusNote) setFocusToken((t) => t + 1);
  }, []);

  // Tag colors come from the app database; reload them whenever tags change.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onTagsChanged(() => {
      loadConfig().then(
        (c) => !cancelled && setTagColors(c.tagColors),
        () => {}, // keep the colors we have
      );
    }).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no event bridge (tests, plain browser)
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    loadConfig().then(
      (c) => {
        setTagColors(c.tagColors);
        if (c.calendarRefreshMinutes > 0) setCalendarMinutes(c.calendarRefreshMinutes);
        setPlanDefaults(defaultPlan(c));
      },
      (e) => setConfigError(String(e)),
    );
  }, []);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!isSearchShortcut(e, IS_MAC) || dragActive.current || focusRef.current) return;
      e.preventDefault();
      if (paletteRef.current) {
        setPaletteOpen(false);
      } else {
        dayRef.current.flush();
        setPaletteOpen(true);
      }
    };
    // Capture phase so it also works while typing in the note.
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (paletteRef.current || focusRef.current) return;
      if (e.key === "Escape" && viewRef.current.kind !== "day") {
        if (e.target instanceof HTMLElement && e.target.closest("textarea, input:not([type=checkbox])")) return;
        e.preventDefault();
        setView(DAY_VIEW);
        return;
      }
      if (viewRef.current.kind !== "day") return;
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || dragActive.current) return;
      if (e.target instanceof HTMLElement && e.target.closest("input, textarea, select, [contenteditable]")) return;
      e.preventDefault();
      goTo((d) => addDays(d, e.key === "ArrowLeft" ? -1 : 1));
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [goTo]);

  // Leaving the window or the app must not strand a pending note.
  useEffect(() => {
    const flush = () => dayRef.current.flush();
    const onVisibility = () => {
      if (document.visibilityState === "hidden") flush();
    };
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("pagehide", flush);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("pagehide", flush);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onCloseRequested(async (event) => {
        const current = dayRef.current;
        if (closeAnyway.current || !current.isBusy()) return;
        event.preventDefault();
        await current.flushAndDrain();
        if (!dayRef.current.isBusy()) {
          await getCurrentWindow().destroy();
          return;
        }
        closeAnyway.current = true;
        setNotice("Couldn't save your note. Close again to discard it.");
      })
      .then(
        (fn) => (cancelled ? fn() : (unlisten = fn)),
        (e) => setConfigError(String(e)),
      );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const running = timer.status.state === "running" ? timer.status : null;
  const timing = running && running.date === date ? running.block : null;
  const runningStart = running && running.date === date && running.phase === "focus" ? running.sessionStart : null;
  const openTimerBlock = () => {
    if (!running) return;
    openBlock({ date: running.date, start: running.block.start, end: running.block.end, title: running.block.title, ord: -1 });
  };
  const stale = timer.status.state === "stale" ? timer.status : null;
  const staleTime = (iso: string) => fmtMinutes(new Date(iso).getHours() * 60 + new Date(iso).getMinutes());

  const banner = day.error ?? configError ?? notice;
  const dismiss = () => {
    day.setError(null);
    setConfigError(null);
    setNotice(null);
  };
  const selectedBlock =
    day.blocks.find((b) => b.key === selectedKey) ?? day.conflicts.find((c) => c.key === selectedKey)?.mine ?? null;
  const selectedConflict = day.conflicts.find((c) => c.key === selectedKey);
  /** block key -> its Google event; the event's calendar is the block's project. */
  const matched = useMemo(() => matchBlocks(day.blocks, calendar.events), [day.blocks, calendar.events]);
  const calendarIdOf = (key: string) => blockCalendarId(blockProject(key, matched, projects.list));
  const selectedEvent = selectedBlock ? matched.get(selectedBlock.key) : undefined;
  const moveSelected = async (projectId: number) => {
    if (!selectedEvent) return;
    const err = await calendar.move(selectedEvent.id, projectId);
    if (err) setNotice(`Couldn't move the event: ${err}`);
  };
  const addSelected = async (projectId: number) => {
    if (!selectedBlock) return;
    const err = await calendar.addToProject(selectedBlock, projectId);
    if (err) setNotice(`Couldn't add the event to Google Calendar: ${err}`);
  };

  // The focus screen shows the block being timed, else the selected one.
  const timedBlock = running && running.date === date ? day.blocks.find((b) => sameBlock(b, running.block)) ?? null : null;
  const focusBlock = timedBlock ?? selectedBlock;
  const focusConflict = day.conflicts.find((c) => c.key === focusBlock?.key);
  const setup = useTimerSetup(selectedBlock?.title ?? "", planDefaults);

  const dayLoaded = day.loaded;
  /** Opens the focus screen on the running block (going to its day first) or on the selected block. */
  const enterFocus = useCallback(() => {
    const status = timerRef.current.status;
    const r = status.state === "running" ? status : null;
    if (r && r.date !== dateRef.current) {
      if (openBlock({ date: r.date, start: r.block.start, end: r.block.end, title: r.block.title, ord: -1 })) setFocusOpen(true);
      return;
    }
    const timed = r ? dayRef.current.blocks.find((b) => sameBlock(b, r.block)) : undefined;
    if (timed) onSelect(timed.key);
    else if (selectedKeyRef.current === null) return;
    dayRef.current.flush();
    setFocusOpen(true);
  }, [openBlock, onSelect]);

  // Nothing to show once the day is loaded and neither a timed nor a selected block is there: leave.
  useEffect(() => {
    if (focusOpen && dayLoaded && !focusBlock && !(running && running.date !== date)) setFocusOpen(false);
  }, [focusOpen, dayLoaded, focusBlock, running, date]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!isDashboardShortcut(e, IS_MAC) || focusRef.current || paletteRef.current || dragActive.current) return;
      e.preventDefault();
      if (viewRef.current.kind === "dashboard") setView(DAY_VIEW);
      else void openDashboard();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [openDashboard]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!isGoalsShortcut(e, IS_MAC) || focusRef.current || paletteRef.current || dragActive.current) return;
      e.preventDefault();
      if (viewRef.current.kind === "goals") setView(DAY_VIEW);
      else void openGoals();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [openGoals]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!isFocusShortcut(e, IS_MAC) || focusRef.current || paletteRef.current || dragActive.current) return;
      e.preventDefault();
      enterFocus();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [enterFocus]);

  return (
    <div className="app">
      <header className="app__header" inert={focusOpen}>
        <h1 className="app__brand">
          <img src="/mark.svg" width={24} height={24} alt="" />
          <span>
            <b>Catenga</b> Focus
          </span>
        </h1>
        <div className="app__nav">
          <button className="app__btn" aria-label="Previous day" onClick={() => goTo((d) => addDays(d, -1))}>
            <ChevronLeftIcon className="app__icon" aria-hidden="true" />
          </button>
          <button className="app__btn" onClick={() => goTo(() => toDateString(new Date()))}>
            Today
          </button>
          <button className="app__btn" aria-label="Next day" onClick={() => goTo((d) => addDays(d, 1))}>
            <ChevronRightIcon className="app__icon" aria-hidden="true" />
          </button>
        </div>
        <span className="app__date">{fmtDateLabel(date)}</span>
        <TimerChip status={timer.status} nowMs={timer.nowMs} onOpen={openTimerBlock} onStop={timer.stop} onFocus={enterFocus} />
        <button className="app__btn" aria-label="Goals" title="Goals" onClick={() => void openGoals()}>
          <FlagIcon className="app__icon" aria-hidden="true" />
          Goals
        </button>
        <button className="app__btn" aria-label="Dashboard" title="Dashboard" onClick={() => void openDashboard()}>
          <ChartBarIcon className="app__icon" aria-hidden="true" />
          Dashboard
        </button>
        <button className="app__btn app__gear" aria-label="Settings" onClick={() => setSettingsOpen(true)}>
          <Cog6ToothIcon className="app__icon" aria-hidden="true" />
        </button>
      </header>
      {banner && (
        <div className="app__banner" role="alert">
          <span>{banner}</span>
          <button className="app__btn" onClick={dismiss}>
            Dismiss
          </button>
        </div>
      )}
      {stale && (
        <div className="app__banner app__banner--timer" role="alert">
          <span>
            A timer was running when Catenga Focus closed ({stale.block.title || "Untitled"}, since {staleTime(stale.sessionStart)}).
          </span>
          <span className="banner__actions">
            <button className="app__btn" onClick={() => timer.resolveStale(false)}>
              Stop at {staleTime(stale.lastSeen)}
            </button>
            <button className="app__btn" onClick={() => timer.resolveStale(true)}>
              Keep running
            </button>
          </span>
        </div>
      )}
      <div className="app__main" inert={focusOpen}>
        {view.kind === "dashboard" ? (
          <DashboardView
            tagColors={tagColors}
            onBack={() => setView(DAY_VIEW)}
            onOpenDay={(d) => goTo(() => d)}
            onOpenHistory={openHistory}
          />
        ) : view.kind === "goals" ? (
          <GoalsView
            projects={projects}
            google={google.status}
            onConnect={google.connect}
            onBack={() => setView(DAY_VIEW)}
            timerStatus={timer.status}
            nowMs={timer.nowMs}
          />
        ) : view.kind === "history" ? (
          <HistoryView
            by={view.by}
            value={view.value}
            tagColors={tagColors}
            onBack={() => setView(DAY_VIEW)}
            onOpenBlock={openHit}
            onOpenHistory={openHistory}
          />
        ) : (
          <>
            <DayView
              key={date}
              date={date}
              blocks={day.blocks}
              events={calendar.events}
              loaded={day.loaded}
              tagColors={tagColors}
              now={now}
              sessions={sessions.sessions}
              timing={timing}
              runningStart={runningStart}
              clockMs={timer.nowMs}
              shortcutsOff={focusOpen}
              dragActive={dragActive}
              selected={selectedKey}
              reveal={reveal}
              onSelect={onSelect}
              onCommit={commitBlocks}
              onNotice={onNotice}
            />
            {!focusOpen && (
              <NotesPanel
                block={selectedBlock}
                conflict={selectedConflict}
                otherConflicts={day.conflicts}
                saveState={day.saveState}
                noteVersion={day.noteVersion}
                focusToken={focusToken}
                onNote={day.setNote}
                onBlur={day.flush}
                onRetry={day.flush}
                onSelect={onSelect}
                onResolve={day.resolveConflict}
                onOpenHistory={openHistory}
                timer={{
                  date,
                  isToday: date === toDateString(now),
                  status: timer.status,
                  nowMs: timer.nowMs,
                  sessions: sessions.sessions,
                  mode: setup.mode,
                  onMode: setup.setMode,
                  plan: setup.plan,
                  onPlan: setup.setPlan,
                  onStart: () => {
                    if (selectedBlock) void timer.start(date, toBlockRef(selectedBlock, calendarIdOf(selectedBlock.key)), setup.mode, setup.plan);
                  },
                  onStop: timer.stop,
                  onSkipBreak: timer.skipBreak,
                  onFocus: enterFocus,
                  onDeleteSession: sessions.remove,
                }}
                project={{
                  connected: google.status?.connected ?? false,
                  projects: projects.list,
                  current: selectedBlock ? blockProject(selectedBlock.key, matched, projects.list) : null,
                  hasEvent: selectedEvent !== undefined,
                  onMove: moveSelected,
                  onAdd: addSelected,
                }}
              />
            )}
          </>
        )}
      </div>
      {settingsOpen && (
        <Settings
          account={account.state}
          onSignIn={account.signIn}
          onCancel={account.cancel}
          onSignOut={account.signOut}
          google={google}
          onGoogleConnect={google.connect}
          onGoogleCancel={google.cancel}
          onGoogleDisconnect={google.disconnect}
          onClose={() => setSettingsOpen(false)}
        />
      )}
      {focusOpen && focusBlock && (
        <FocusScreen
          block={focusBlock}
          conflict={focusConflict}
          date={date}
          isToday={date === toDateString(now)}
          status={timer.status}
          nowMs={timer.nowMs}
          finished={timer.finished}
          mode={setup.mode}
          onMode={setup.setMode}
          plan={setup.plan}
          onPlan={setup.setPlan}
          tagColors={tagColors}
          saveState={day.saveState}
          noteVersion={day.noteVersion}
          banner={banner}
          onDismiss={dismiss}
          onNote={day.setNote}
          onFlush={day.flush}
          onRetry={day.flush}
          onResolve={day.resolveConflict}
          onStart={() => void timer.start(date, toBlockRef(focusBlock, calendarIdOf(focusBlock.key)), setup.mode, setup.plan)}
          onStop={timer.stop}
          onSkipBreak={timer.skipBreak}
          onExit={() => setFocusOpen(false)}
        />
      )}
      {paletteOpen && (
        <CommandPalette
          tagColors={tagColors}
          onClose={() => setPaletteOpen(false)}
          onOpenBlock={openHit}
          onOpenHistory={openHistory}
        />
      )}
    </div>
  );
}

export default App;
