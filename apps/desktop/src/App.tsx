import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChartBarIcon, CheckCircleIcon, ChevronLeftIcon, ChevronRightIcon, Cog6ToothIcon, FlagIcon, PlayIcon, PlusIcon } from "@heroicons/react/20/solid";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { appCancelQuit, appExit, appHide, appRestart, appShow, setNotesDir, loadConfig, onOpenBlock, onQuitRequested, onStartNow, onTagsChanged, timerRelink, type GoalProgress, type HistoryEntry, type HistoryKind, type SearchHit } from "./api";
import { CommandPalette } from "./components/CommandPalette";
import { DashboardView } from "./components/DashboardView";
import { DayView } from "./components/DayView";
import { FocusScreen } from "./components/FocusScreen";
import { GoalsView } from "./components/GoalsView";
import { HistoryView } from "./components/HistoryView";
import { NotesPanel } from "./components/NotesPanel";
import { RecapView } from "./components/RecapView";
import { QuickAdd } from "./components/QuickAdd";
import { ShortcutsHelp } from "./components/ShortcutsHelp";
import { TimerChip } from "./components/TimerChip";
import { Settings } from "./components/Settings";
import { useAccount } from "./hooks/useAccount";
import { useSync } from "./hooks/useSync";
import { useCalendar } from "./hooks/useCalendar";
import { LOST_MESSAGE, useGoogle } from "./hooks/useGoogle";
import { useProjects } from "./hooks/useProjects";
import { useDay, type UiBlock } from "./hooks/useDay";
import { useRecovery } from "./hooks/useRecovery";
import { bannerText } from "./lib/recovery";
import { useNow } from "./hooks/useNow";
import { useSessions, useTimer } from "./hooks/useTimer";
import { useTimerSetup } from "./hooks/useTimerSetup";
import { defaultPlan, loadPlan, safeStorage } from "./lib/focus";
import { findGoalBlock } from "./lib/goalPlay";
import { goalSubjectText, matchesGoal } from "./lib/goals";
import { matchBlocks } from "./lib/calendar";
import { startNowTarget } from "./lib/startNow";
import { settleNotes } from "./lib/lifecycle";
import { applyNotesFolder } from "./lib/notesFolder";
import { menuTargetKind, shouldSuppressDefault } from "./lib/contextMenu";
import { isDashboardShortcut, isFocusShortcut, isGoalsShortcut, isSearchShortcut, shortcutFor, stepBlock, type ShortcutAction } from "./lib/keys";
import type { QuickAdd as QuickAddBlock } from "./lib/quickAdd";
import { blockProject, blockProjectKey, canMakeProject } from "./lib/projects";
import { resolveBlockKey, type BlockTarget } from "./lib/search";
import { sameBlock, sessionRelinks, toBlockRef } from "./lib/timer";
import { addDays, fmtDateLabel, fmtMinutes, toDateString } from "./lib/time";

const IS_MAC = navigator.userAgent.includes("Mac");

/** A search result waiting for its day to load. */
/** `ghost`: a calendar event with no block; there is nothing to select, only to reveal. */
type Pending = BlockTarget & { date: string; ghost?: boolean };

type View = { kind: "day" } | { kind: "history"; by: HistoryKind; value: string } | { kind: "dashboard" } | { kind: "goals" } | { kind: "recap" };
const DAY_VIEW: View = { kind: "day" };

function App() {
  const now = useNow();
  const [date, setDate] = useState(() => toDateString(new Date()));
  const [tagColors, setTagColors] = useState<Record<string, string>>({});
  const [configError, setConfigError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [focusToken, setFocusToken] = useState(0);
  const [renameToken, setRenameToken] = useState(0);
  const [view, setView] = useState<View>(DAY_VIEW);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [quickAddOpen, setQuickAddOpen] = useState(false);
  const account = useAccount();
  const sync = useSync(account.state);
  const google = useGoogle();
  const projects = useProjects();
  const [calendarMinutes, setCalendarMinutes] = useState(5);
  const [pending, setPending] = useState<Pending | null>(null);
  const [focusOpen, setFocusOpen] = useState(false);
  /** A goal whose Play is waiting for today's blocks and events to load. */
  const [playReq, setPlayReq] = useState<{ goal: GoalProgress; date: string } | null>(null);
  /** "Start now" waiting for today's blocks and events to load. */
  const [startReq, setStartReq] = useState<{ date: string } | null>(null);
  const [planDefaults, setPlanDefaults] = useState(() => defaultPlan(null));
  const [reveal, setReveal] = useState<{ minute: number; token: number } | null>(null);
  const onNotice = useCallback((message: string) => setNotice(message), []);
  const recovery = useRecovery(date);
  const day = useDay(date, onNotice, recovery.tracker);
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
    (next: (current: string) => string): boolean => {
      if (relinkPending.current > 0) return false;
      const blocker = dayRef.current.leaveBlocker();
      if (blocker) {
        setNotice(blocker);
        return false;
      }
      setSelectedKey(null);
      setPending(null);
      setPlayReq(null);
      setStartReq(null);
      setQuickAddOpen(false);
      setView(DAY_VIEW);
      setDate(next);
      return true;
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

  const openBlockRef = useRef(openBlock);
  openBlockRef.current = openBlock;

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
  const openPage = useCallback(async (page: "dashboard" | "goals" | "recap") => {
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
  const openRecap = useCallback(() => openPage("recap"), [openPage]);

  useEffect(() => {
    if (!pending || pending.date !== date) return;
    if (!day.loaded) {
      if (day.error) setPending(null);
      return;
    }
    setPending(null);
    if (pending.ghost) {
      setReveal((r) => ({ minute: pending.start, token: (r?.token ?? 0) + 1 }));
      return;
    }
    const key = resolveBlockKey(day.blocks, pending);
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

  // The webview's own right-click menu (Reload, Inspect Element) never shows in production; blocks and ghosts
  // handle theirs first (defaultPrevented), typing targets keep the default, and dev keeps it elsewhere for Inspect.
  useEffect(() => {
    const onContextMenu = (e: MouseEvent) => {
      if (e.defaultPrevented) return;
      const target = e.target instanceof Element ? e.target : null;
      if (shouldSuppressDefault(menuTargetKind(target), import.meta.env.PROD)) e.preventDefault();
    };
    window.addEventListener("contextmenu", onContextMenu);
    return () => window.removeEventListener("contextmenu", onContextMenu);
  }, []);

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

  // Closing hides the window (the app keeps running in the background); quitting asks here first.
  // Either way everything typed is flushed first; if that fails the window stays and says so.
  useEffect(() => {
    let cancelled = false;
    const unlisteners: (() => void)[] = [];
    const keep = (fn: () => void) => (cancelled ? fn() : unlisteners.push(fn));
    const fail = (e: unknown) => setConfigError(String(e));
    getCurrentWindow()
      .onCloseRequested(async (event) => {
        event.preventDefault();
        const outcome = await settleNotes(dayRef.current, closeAnyway.current);
        if (outcome === "proceed") {
          closeAnyway.current = false;
          await appHide();
          return;
        }
        closeAnyway.current = true;
        setNotice("Couldn't save your note. Close again to discard it.");
      })
      .then(keep, fail);
    onQuitRequested(async () => {
      const outcome = await settleNotes(dayRef.current, closeAnyway.current);
      if (outcome === "proceed") {
        await appExit();
        return;
      }
      closeAnyway.current = true;
      await appCancelQuit();
      await appShow();
      setNotice("Couldn't save your note. Quit again to discard it.");
    }).then(keep, fail);
    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  // A menu bar item was clicked: go to today's block (or reveal the calendar event) through the guarded path.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onOpenBlock((r) => openBlockRef.current({ date: r.date, start: r.start, end: r.end, title: r.title, ord: -1, ghost: r.ghost })).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no event bridge (tests, plain browser)
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const startNowRef = useRef<() => void>(() => {});
  // The menu bar "Start now" item: same action as the header button.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onStartNow(() => startNowRef.current()).then(
      (fn) => (cancelled ? fn() : (unlisten = fn)),
      () => {}, // no event bridge (tests, plain browser)
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
  const recoveryBanner = bannerText(recovery.entries);
  const recoveryTarget = recovery.entries.find((e) => e.date !== date && !e.unreadable)?.date;
  const retryRecovery = () => {
    dayRef.current.flush();
    recovery.retry();
  };
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
  const projectKeyOf = (key: string) => blockProjectKey(key, matched);
  const selectedEvent = selectedBlock ? matched.get(selectedBlock.key) : undefined;
  const makeSelectedProject = async () => {
    if (!selectedEvent) return;
    const err = await projects.fromEvent(date, selectedEvent.id);
    if (err) setNotice(`Couldn't make a project: ${err}`);
    else await calendar.reload();
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

  /** Goal Play: go to today (guarded), find or create the goal's block, start the timer and open the focus screen. */
  const playGoal = useCallback(
    (goal: GoalProgress) => {
      const status = timerRef.current.status;
      if (status.state === "running" && matchesGoal(goal, status.block)) {
        setView(DAY_VIEW);
        enterFocus();
        return;
      }
      const today = toDateString(new Date());
      if (today !== dateRef.current && !goTo(() => today)) return;
      setView(DAY_VIEW);
      setPlayReq({ goal, date: today });
    },
    [goTo, enterFocus],
  );

  /** Start now: go to today (guarded); the effect below picks the target once today has loaded. */
  const startNow = useCallback(() => {
    if (timerRef.current.status.state === "running") return;
    const today = toDateString(new Date());
    if (today !== dateRef.current && !goTo(() => today)) return;
    setView(DAY_VIEW);
    setStartReq({ date: today });
  }, [goTo]);
  startNowRef.current = startNow;

  // A pending Play or Start now is dropped when the user leaves the day view.
  useEffect(() => {
    if (playReq && view.kind !== "day") setPlayReq(null);
    if (startReq && view.kind !== "day") setStartReq(null);
  }, [playReq, startReq, view]);

  useEffect(() => {
    if (!startReq || startReq.date !== date || !day.loaded || !calendar.loaded || relinkPending.current > 0) return;
    setStartReq(null);
    if (timerRef.current.status.state === "running") return;
    const at = new Date();
    const target = startNowTarget(day.blocks, calendar.events, at.getHours() * 60 + at.getMinutes());
    if (!target) {
      setNotice("Nothing left on today's calendar to start.");
      return;
    }
    let block: { key: string; start: number; end: number; title: string; tags: string[] };
    let project: string | null;
    if (target.kind === "block") {
      const found = day.blocks.find((b) => b.key === target.key);
      if (!found) return;
      block = found;
      project = projectKeyOf(found.key);
    } else {
      // Same as clicking a ghost: the block takes the existing event, so no new Google event is made.
      const base = { start: target.event.start, end: target.event.end, title: target.event.title, tags: [], body: "" };
      block = { ...base, key: crypto.randomUUID() };
      project = target.event.projectKey;
      commitBlocks([...day.blocks, { ...base, key: block.key }]);
    }
    onSelect(block.key);
    const plan = loadPlan(safeStorage(), block.title, planDefaults);
    void timerRef.current.start(date, toBlockRef(block, project), setup.mode, plan);
  }, [startReq, date, day.loaded, day.blocks, calendar.loaded, calendar.events]);

  /** The header button's target: today's current block/event, else the next one. */
  const startTarget = useMemo(() => {
    if (running || date !== toDateString(now) || !day.loaded) return null;
    const target = startNowTarget(day.blocks, calendar.events, now.getHours() * 60 + now.getMinutes());
    if (!target) return null;
    const item = target.kind === "block" ? day.blocks.find((b) => b.key === target.key) : target.event;
    return item ? { title: item.title.trim() || "Untitled", start: item.start, end: item.end } : null;
  }, [running, date, now, day.loaded, day.blocks, calendar.events]);

  useEffect(() => {
    if (!playReq || playReq.date !== date || !day.loaded || !calendar.loaded || relinkPending.current > 0) return;
    const { goal } = playReq;
    setPlayReq(null);
    const at = new Date();
    const nowMinute = at.getHours() * 60 + at.getMinutes();
    const target = findGoalBlock(day.blocks, calendar.events, goal, nowMinute);
    if (target.kind === "none") {
      setNotice(
        goal.project
          ? `No “${goal.project.name}” in your calendar today.`
          : `No block for “${goalSubjectText(goal)}” today — drag one on the timeline first.`,
      );
      return;
    }
    let block: { key: string; start: number; end: number; title: string; tags: string[] };
    let project: string | null;
    if (target.kind === "block") {
      const found = day.blocks.find((b) => b.key === target.key);
      if (!found) return;
      block = found;
      project = projectKeyOf(found.key);
      // The M8d fallback block (no event of its own) still belongs to the goal's project.
      if (project === null) project = goal.project?.key ?? null;
    } else {
      // Same as clicking a ghost: the block takes the existing event, so no new Google event is made.
      const base = { start: target.event.start, end: target.event.end, title: target.event.title, tags: [], body: "" };
      block = { ...base, key: crypto.randomUUID() };
      project = target.event.projectKey;
      commitBlocks([...day.blocks, { ...base, key: block.key }]);
    }
    onSelect(block.key);
    const plan = loadPlan(safeStorage(), block.title, planDefaults);
    void timerRef.current.start(date, toBlockRef(block, project), setup.mode, plan);
    setFocusOpen(true);
  }, [playReq, date, day.loaded, day.blocks, calendar.loaded, calendar.events]);

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

  /** Quick-add (`N`, Cmd/Ctrl+N, header button): opens the bar on the day view; the block lands on the day shown. */
  const openQuickAdd = useCallback(() => {
    setView(DAY_VIEW);
    setQuickAddOpen(true);
    // Already open (e.g. Cmd/Ctrl+N from the note): bring the cursor back to the bar.
    document.querySelector<HTMLInputElement>(".quickadd__input")?.focus();
  }, []);

  const addQuickBlock = useCallback(
    (b: QuickAddBlock) => {
      if (!dayRef.current.loaded) return;
      const block: UiBlock = { key: crypto.randomUUID(), start: b.start, end: b.end, title: b.title, tags: b.tags, body: "" };
      // Same path as a block made on the grid: saved to the day file, then the Google event is created.
      commitBlocks([...dayRef.current.blocks, block]);
      onSelect(block.key);
      setReveal((r) => ({ minute: b.start, token: (r?.token ?? 0) + 1 }));
      setQuickAddOpen(false);
    },
    [commitBlocks, onSelect],
  );

  /** Space: stops the timer when it runs on the block shown, else starts it there (today only; same mode/plan as the timer bar). */
  const toggleTimer = () => {
    const block = focusRef.current ? focusBlock : selectedBlock;
    if (!block) return;
    const status = timerRef.current.status;
    if (status.state === "running" && status.date === date && sameBlock(status.block, block)) {
      void timerRef.current.stop();
      return;
    }
    if (date !== toDateString(new Date())) {
      setNotice("The timer only runs on today's blocks.");
      return;
    }
    void timerRef.current.start(date, toBlockRef(block, projectKeyOf(block.key)), setup.mode, setup.plan);
  };

  const actionsRef = useRef<Record<ShortcutAction, () => void>>(null as never);
  actionsRef.current = {
    quickAdd: openQuickAdd,
    today: () => {
      const today = toDateString(new Date());
      if (viewRef.current.kind !== "day" || today !== dateRef.current) goTo(() => today);
    },
    openNote: () => {
      if (selectedBlock && day.loaded) onSelect(selectedBlock.key, { focusNote: true });
    },
    toggleTimer,
    prevBlock: () => stepSelection(-1),
    nextBlock: () => stepSelection(1),
    focus: enterFocus,
    rename: () => setRenameToken((t) => t + 1),
    help: () => setHelpOpen(true),
  };
  const stepSelection = (dir: 1 | -1) => {
    if (!day.loaded) return;
    const key = stepBlock(day.blocks, selectedKey, dir);
    const block = day.blocks.find((b) => b.key === key);
    if (!block) return;
    if (key !== selectedKey) onSelect(key);
    setReveal((r) => ({ minute: block.start, token: (r?.token ?? 0) + 1 }));
  };

  const modalRef = useRef(false);
  modalRef.current = settingsOpen || helpOpen;

  // One listener for the plain-key shortcuts; the guards live in `shortcutFor`. Capture so Cmd/Ctrl+N also works in the note.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      const el = e.target instanceof HTMLElement ? e.target : null;
      const action = shortcutFor(e, {
        typing: el?.closest("input, textarea, select, [contenteditable]") != null,
        onControl: el?.closest("button, a[href], summary, [role=button], [role=menuitem]") != null,
        dragging: dragActive.current,
        focusOpen: focusRef.current,
        paletteOpen: paletteRef.current,
        modalOpen: modalRef.current,
        view: viewRef.current.kind,
        isMac: IS_MAC,
      });
      if (!action) return;
      e.preventDefault();
      actionsRef.current[action]();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

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
        <button className="app__btn" aria-label="Quick-add a block" title={`Quick-add (N, ${IS_MAC ? "⌘" : "Ctrl+"}N)`} onClick={openQuickAdd}>
          <PlusIcon className="app__icon" aria-hidden="true" />
          Add
        </button>
        {startTarget && (
          <button
            className="app__btn app__btn--start"
            title={`Start now: ${startTarget.title}, ${fmtMinutes(startTarget.start)}–${fmtMinutes(startTarget.end)}`}
            onClick={startNow}
          >
            <PlayIcon className="app__icon" aria-hidden="true" />
            Start now
          </button>
        )}
        <TimerChip status={timer.status} nowMs={timer.nowMs} onOpen={openTimerBlock} onStop={timer.stop} onFocus={enterFocus} />
        <button className="app__btn" aria-label="Done" title="What got done on this day" onClick={() => void openRecap()}>
          <CheckCircleIcon className="app__icon" aria-hidden="true" />
          Done
        </button>
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
      {recoveryBanner && (
        <div className="app__banner" role="alert">
          <span>{recoveryBanner}</span>
          <span className="banner__actions">
            {recoveryTarget && (
              <button className="app__btn" onClick={() => goTo(() => recoveryTarget)}>
                Open day
              </button>
            )}
            <button className="app__btn" onClick={retryRecovery}>
              Retry
            </button>
          </span>
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
            onPlay={playGoal}
          />
        ) : view.kind === "recap" ? (
          <RecapView
            date={date}
            isToday={date === toDateString(now)}
            loaded={day.loaded}
            blocks={day.blocks}
            sessions={sessions.sessions}
            status={timer.status}
            nowMs={timer.nowMs}
            tagColors={tagColors}
            onBack={() => setView(DAY_VIEW)}
            onOpenBlock={(b) => void openBlock({ date, start: b.start, end: b.end, title: b.title, ord: -1 })}
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
              renameToken={renameToken}
              clockMs={timer.nowMs}
              shortcutsOff={focusOpen || settingsOpen || helpOpen}
              top={
                quickAddOpen ? (
                  <QuickAdd date={date} isToday={date === toDateString(now)} loaded={day.loaded} onAdd={addQuickBlock} onClose={() => setQuickAddOpen(false)} />
                ) : undefined
              }
              dragActive={dragActive}
              selected={selectedKey}
              reveal={reveal}
              onSelect={onSelect}
              onCommit={commitBlocks}
              onNotice={onNotice}
              canDeleteEvents={google.status?.connected ?? false}
              onDeleteEvent={calendar.remove}
              onStartTimer={(key) => {
                const block = day.blocks.find((b) => b.key === key);
                if (!block) return;
                const plan = loadPlan(safeStorage(), block.title, planDefaults);
                void timer.start(date, toBlockRef(block, projectKeyOf(key)), setup.mode, plan);
              }}
              onStopTimer={timer.stop}
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
                    if (selectedBlock) void timer.start(date, toBlockRef(selectedBlock, projectKeyOf(selectedBlock.key)), setup.mode, setup.plan);
                  },
                  onStop: timer.stop,
                  onSkipBreak: timer.skipBreak,
                  onFocus: enterFocus,
                  onDeleteSession: sessions.remove,
                }}
                project={{
                  connected: google.status?.connected ?? false,
                  current: selectedBlock ? blockProject(selectedBlock.key, matched, projects.list) : null,
                  canMake: selectedBlock ? canMakeProject(selectedBlock.key, matched) : false,
                  onMake: makeSelectedProject,
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
          sync={sync.status}
          onSyncNow={sync.syncNow}
          google={google}
          onGoogleConnect={google.connect}
          onGoogleCancel={google.cancel}
          onGoogleDisconnect={google.disconnect}
          onApplyNotesFolder={(path) => applyNotesFolder(dayRef.current, path, setNotesDir, appRestart)}
          onClose={() => setSettingsOpen(false)}
          onShowShortcuts={() => {
            setSettingsOpen(false);
            setHelpOpen(true);
          }}
        />
      )}
      {helpOpen && <ShortcutsHelp onClose={() => setHelpOpen(false)} />}
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
          onStart={() => void timer.start(date, toBlockRef(focusBlock, projectKeyOf(focusBlock.key)), setup.mode, setup.plan)}
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
