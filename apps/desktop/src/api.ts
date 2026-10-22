import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Block {
  /** minutes since 00:00 */
  start: number;
  end: number;
  title: string;
  /** without the leading # */
  tags: string[];
  /** raw markdown under the heading; edit it only through `lib/note.ts` */
  body: string;
}

export interface Config {
  /** tag name -> palette name (sun | clay | leaf | sky) */
  tagColors: Record<string, string>;
  /** how often the visible day is re-fetched from Google Calendar */
  calendarRefreshMinutes: number;
  /** Pomodoro focus length, minutes */
  pomodoroFocusMinutes: number;
  /** Pomodoro break length, minutes */
  pomodoroBreakMinutes: number;
  /** Pomodoros per run */
  pomodoroCycles: number;
}

export interface LoadedDay {
  blocks: Block[];
  /** revision of the file content; "" when the file does not exist */
  rev: string;
}

export type SaveOutcome =
  | { status: "saved"; rev: string }
  /** The file changed outside the app; nothing was written. */
  | { status: "conflict"; blocks: Block[]; rev: string };

export interface DayChanged {
  date: string;
  rev: string;
}

export const loadConfig = () => invoke<Config>("load_config");
export const loadDay = (date: string) => invoke<LoadedDay>("load_day", { date });
export const saveDay = (date: string, blocks: Block[], baseRev: string) =>
  invoke<SaveOutcome>("save_day", { date, blocks, baseRev });
export const onDayChanged = (callback: (changed: DayChanged) => void): Promise<UnlistenFn> =>
  listen<DayChanged>("day-changed", (event) => callback(event.payload));

export interface Suggestion {
  label: string;
  count: number;
}

export interface SearchHit {
  date: string;
  start: number;
  end: number;
  ord: number;
  title: string;
  tags: string[];
  /** title with matches wrapped in \u0002 ... \u0003 */
  titleHl: string;
  /** body excerpt, same markers */
  snippet: string;
}

export interface SearchResponse {
  hits: SearchHit[];
  titles: Suggestion[];
  tags: Suggestion[];
  truncated: boolean;
  indexing: boolean;
}

export type HistoryKind = "title" | "tag";

export interface HistoryEntry {
  date: string;
  start: number;
  end: number;
  ord: number;
  title: string;
  tags: string[];
  /** raw block body; show it through `bodyToNote` */
  body: string;
}

export interface HistoryResult {
  kind: HistoryKind;
  label: string;
  /** newest first */
  entries: HistoryEntry[];
  truncated: boolean;
  indexing: boolean;
}

export interface IndexStatus {
  state: "building" | "ready";
  done: number;
  total: number;
}

export const search = (query: string) => invoke<SearchResponse>("search", { query });
export const history = (kind: HistoryKind, value: string) => invoke<HistoryResult>("history", { kind, value });
export const indexStatus = () => invoke<IndexStatus>("index_status");
export const onIndexStatus = (callback: (status: IndexStatus) => void): Promise<UnlistenFn> =>
  listen<IndexStatus>("index-status", (event) => callback(event.payload));

export const serverUrl = () => invoke<string>("server_url");
/** Opens the browser on the login page; resolves with the one-time code from the loopback callback. */
export const signIn = (codeChallenge: string) => invoke<string>("sign_in", { codeChallenge });
export const cancelSignIn = () => invoke<void>("cancel_sign_in");
export const getAccountToken = () => invoke<string | null>("get_account_token");
export const setAccountToken = (token: string) => invoke<void>("set_account_token", { token });
export const clearAccountToken = () => invoke<void>("clear_account_token");

export interface GoogleStatus {
  /** false when this build has no Google client credentials */
  configured: boolean;
  connected: boolean;
  email: string | null;
  /** Granted before calendars became projects: the token lacks the calendar-list scope. */
  needsReconnect: boolean;
}

export interface CalEvent {
  id: string;
  /** minutes since 00:00, clamped to the day */
  start: number;
  end: number;
  title: string;
  /** `Project.id` of the calendar the event belongs to */
  projectId: number | null;
  /** Google's color for that calendar (#rrggbb) */
  color: string;
}

export interface CalendarDay {
  events: CalEvent[];
  /** unix ms of the last successful fetch; null when never fetched */
  fetchedAt: number | null;
}

export const googleStatus = () => invoke<GoogleStatus>("google_status");
/** Opens the browser on Google's consent page; resolves once connected. */
export const googleConnect = (codeChallenge: string, codeVerifier: string, state: string) =>
  invoke<GoogleStatus>("google_connect", { codeChallenge, codeVerifier, state });
export const googleCancelConnect = () => invoke<void>("google_cancel_connect");
export const googleDisconnect = () => invoke<GoogleStatus>("google_disconnect");
/** Cached events only; empty when not connected. */
export const calendarDay = (date: string) => invoke<CalendarDay>("calendar_day", { date });
/** Fetches in the background; the result arrives as "calendar-changed" / "calendar-error". */
export const calendarRefresh = (date: string) => invoke<void>("calendar_refresh", { date });
/** `projectId: null` = the primary calendar. */
export const calendarCreate = (date: string, start: number, end: number, title: string, projectId: number | null) =>
  invoke<CalEvent>("calendar_create", { date, start, end, title, projectId });
/** Moves the event to another calendar (project). */
export const calendarMove = (date: string, eventId: string, projectId: number) =>
  invoke<CalEvent>("calendar_move", { date, eventId, projectId });
export const calendarUpdate = (date: string, eventId: string, start: number, end: number, title: string) =>
  invoke<CalEvent>("calendar_update", { date, eventId, start, end, title });

export interface CalendarChanged {
  date: string;
}
export interface CalendarError {
  date: string;
  message: string;
}
export const onCalendarChanged = (callback: (changed: CalendarChanged) => void): Promise<UnlistenFn> =>
  listen<CalendarChanged>("calendar-changed", (event) => callback(event.payload));
export const onCalendarError = (callback: (error: CalendarError) => void): Promise<UnlistenFn> =>
  listen<CalendarError>("calendar-error", (event) => callback(event.payload));

export type TimerMode = "free" | "pomodoro";

/** Snapshot of the block a session belongs to; minutes like `Block`. */
export interface BlockRef {
  start: number;
  end: number;
  title: string;
  tags: string[];
  /** Google calendar id of the block's project; null when unmatched or unknown */
  project: string | null;
}

export interface Session {
  /** RFC 3339; also the session's id */
  start: string;
  /** null while running */
  end: string | null;
  mode: TimerMode;
  block: BlockRef;
  /** whole minutes (so far, when running) */
  minutes: number;
}

export type TimerStatus =
  | { state: "idle" }
  | {
      state: "running";
      date: string;
      block: BlockRef;
      mode: TimerMode;
      phase: "focus" | "break";
      sessionStart: string;
      /** RFC 3339; Pomodoro only */
      phaseEndsAt: string | null;
      /** RFC 3339; when the current phase began (progress rings) */
      phaseStart: string;
      /** 1-based; null for a free timer */
      cycle: number | null;
      cycles: number | null;
      focusMinutes: number | null;
      breakMinutes: number | null;
    }
  | { state: "stale"; date: string; block: BlockRef; mode: TimerMode; sessionStart: string; lastSeen: string };

export interface SessionLink {
  from: BlockRef;
  to: BlockRef;
}

export const timerStatus = () => invoke<TimerStatus>("timer_status");
export interface PomodoroPlan {
  focusMinutes: number;
  breakMinutes: number;
  cycles: number;
}

/** `plan: null` means the config defaults; it is ignored for a free timer. */
export const timerStart = (date: string, block: BlockRef, mode: TimerMode, plan: PomodoroPlan | null) =>
  invoke<TimerStatus>("timer_start", { date, block, mode, plan });
/** During a break: starts the next focus now (or ends the run after the last break). */
export const timerSkipBreak = () => invoke<TimerStatus>("timer_skip_break");
export const timerStop = () => invoke<TimerStatus>("timer_stop");
export const timerResolveStale = (keepRunning: boolean) => invoke<TimerStatus>("timer_resolve_stale", { keepRunning });
export const timerSessions = (date: string) => invoke<Session[]>("timer_sessions", { date });
export const timerDeleteSession = (date: string, start: string) =>
  invoke<Session[]>("timer_delete_session", { date, start });
export const timerRelink = (date: string, links: SessionLink[]) => invoke<void>("timer_relink", { date, links });

export interface SessionsChanged {
  date: string;
}
export const onTimerChanged = (callback: (status: TimerStatus) => void): Promise<UnlistenFn> =>
  listen<TimerStatus>("timer-changed", (event) => callback(event.payload));
export const onSessionsChanged = (callback: (changed: SessionsChanged) => void): Promise<UnlistenFn> =>
  listen<SessionsChanged>("sessions-changed", (event) => callback(event.payload));

export type DashRange = "week" | "month" | "30d" | "year" | "all";
export interface TitleStat {
  label: string;
  /** Normalised title (grouping key). */
  key: string;
  minutes: number;
  sessions: number;
  doneDays: number;
  currentStreak: number;
  longestStreak: number;
}
/** A title's share of one tag; untitled sessions have `label` and `key` "". */
export interface TagTitle {
  label: string;
  key: string;
  minutes: number;
  sessions: number;
}
/** `key` "" is the Untagged group (always last). */
export interface TagStat {
  label: string;
  key: string;
  minutes: number;
  sessions: number;
  titles: TagTitle[];
}
/** `done` = a finished session or a note that day. */
export interface DayCell {
  date: string;
  minutes: number;
  done: boolean;
  note: boolean;
}
export interface Habit {
  label: string;
  key: string;
  currentStreak: number;
  longestStreak: number;
  /** Last 30 days, oldest first. */
  days: DayCell[];
}
export interface CalendarCell {
  date: string;
  minutes: number;
  done: boolean;
}
export interface Dashboard {
  range: DashRange;
  /** Null for "all". */
  from: string | null;
  to: string;
  totals: { minutes: number; sessions: number; activeDays: number; currentStreak: number; longestStreak: number };
  titles: TitleStat[];
  tags: TagStat[];
  /** Last 53 weeks ending today, every day present, oldest first. */
  calendar: CalendarCell[];
  habits: Habit[];
  indexing: boolean;
}

export const dashboard = (range: DashRange) => invoke<Dashboard>("dashboard", { range });

export interface Project {
  id: number;
  /** Google calendar id; what timer sessions store as their project */
  googleCalendarId: string;
  name: string;
  /** Google's backgroundColor (#rrggbb); data, shown through an inline CSS variable */
  color: string;
  showOnTimeline: boolean;
  isPrimary: boolean;
  /** The calendar is no longer in the user's Google list */
  archived: boolean;
}
export type TagColor = "sun" | "clay" | "leaf" | "sky";
export interface Tag {
  /** Null for a tag only seen in notes (not saved yet). */
  id: number | null;
  /** Without '#'. */
  name: string;
  color: TagColor;
  saved: boolean;
  goalCount: number;
}
export interface Goal {
  id: number;
  projectId: number | null;
  tagIds: number[];
  /** Optional title filter. */
  title: string | null;
  minutesPerDay: number;
  /** YYYY-MM-DD */
  start: string;
  /** Null = no end. */
  days: number | null;
  archived: boolean;
}
/** Tags by name: unknown names are created. */
export interface GoalDraft {
  projectId: number | null;
  tagNames: string[];
  title: string | null;
  minutesPerDay: number;
  start: string;
  days: number | null;
}
export interface GoalDay {
  date: string;
  minutes: number;
  met: boolean;
}
export interface GoalProgress {
  goal: Goal;
  project: Project | null;
  tags: Tag[];
  status: "upcoming" | "active" | "finished";
  todayMinutes: number;
  elapsedDays: number;
  metDays: number;
  totalMinutes: number;
  currentStreak: number;
  longestStreak: number;
  /** Last day of the goal; null if it has no end. */
  end: string | null;
  /** Oldest first. */
  last14: GoalDay[];
}

export const projects = () => invoke<Project[]>("projects");
/** Reads the user's Google calendars; needs a connected account. */
export const projectsRefresh = () => invoke<Project[]>("projects_refresh");
export const projectSetShown = (id: number, shown: boolean) => invoke<Project[]>("project_set_shown", { id, shown });
export const tags = () => invoke<Tag[]>("tags");
export const tagSave = (name: string, color: TagColor) => invoke<Tag[]>("tag_save", { name, color });
export const tagDelete = (id: number) => invoke<Tag[]>("tag_delete", { id });

export const goals = () => invoke<GoalProgress[]>("goals");
export const goalsArchived = () => invoke<GoalProgress[]>("goals_archived");
export const goalCreate = (draft: GoalDraft) => invoke<GoalProgress[]>("goal_create", { draft });
export const goalUpdate = (id: number, draft: GoalDraft) => invoke<GoalProgress[]>("goal_update", { id, draft });
export const goalSetArchived = (id: number, archived: boolean) =>
  invoke<GoalProgress[]>("goal_set_archived", { id, archived });
export const goalDelete = (id: number) => invoke<GoalProgress[]>("goal_delete", { id });
export const onGoalsChanged = (callback: () => void): Promise<UnlistenFn> =>
  listen<Record<string, never>>("goals-changed", () => callback());
export const onProjectsChanged = (callback: () => void): Promise<UnlistenFn> =>
  listen<Record<string, never>>("projects-changed", () => callback());
export const onTagsChanged = (callback: () => void): Promise<UnlistenFn> =>
  listen<Record<string, never>>("tags-changed", () => callback());
/** Both "tags-changed" and "goals-changed" (a goal can create tags and changes their use count). */
export const goalsChangedOrTags = async (callback: () => void): Promise<UnlistenFn> => {
  const [a, b] = await Promise.all([onTagsChanged(callback), onGoalsChanged(callback)]);
  return () => {
    a();
    b();
  };
};
