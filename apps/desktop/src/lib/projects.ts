/** Pure helpers for projects (recurring event series); see `docs/plans/M8c-series.md` §1.3 and §1.5. */
import type { CSSProperties } from "react";
import type { CalEvent, GoogleStatus, Project } from "../api";

const HEX = /^#[0-9a-f]{3,8}$/i;

/** Inline style carrying a Google color as `--project`; undefined for anything that is not a hex color. */
export function projectStyle(color: string | null): CSSProperties | undefined {
  if (color === null || !HEX.test(color)) return undefined;
  return { ["--project" as string]: color };
}

/** What timer sessions store as their project: the key of the event the block matches, if it is a project. */
export function blockProjectKey(blockKey: string, matched: Map<string, CalEvent>): string | null {
  return matched.get(blockKey)?.projectKey ?? null;
}

/** A block's project: the series (or project event) of the event it matches; unmatched blocks have none. */
export function blockProject(blockKey: string, matched: Map<string, CalEvent>, projects: Project[]): Project | null {
  const key = blockProjectKey(blockKey, matched);
  if (key === null) return null;
  return projects.find((p) => p.key === key) ?? null;
}

/** "Make this a project" applies to a matched one-off event that is not a project yet. */
export function canMakeProject(blockKey: string, matched: Map<string, CalEvent>): boolean {
  const ev = matched.get(blockKey);
  return ev !== undefined && ev.projectKey === null;
}

const byKindThenName = (a: Project, b: Project) =>
  Number(b.kind === "series") - Number(a.kind === "series") || a.name.localeCompare(b.name, undefined, { sensitivity: "base" });

/** Projects to offer in pickers: not archived (except `keepId`), series first, then by name. */
export function pickableProjects(projects: Project[], keepId: number | null = null): Project[] {
  return projects.filter((p) => !p.archived || p.id === keepId).sort(byKindThenName);
}

/** Projects as listed on the Goals page: active first, then archived; series first, then by name. */
export function listProjects(projects: Project[]): Project[] {
  return [...projects].sort((a, b) => Number(a.archived) - Number(b.archived) || byKindThenName(a, b));
}

/** Muted hint next to a project's name: its calendar, plus "one-off" for a single event. */
export const projectHint = (p: Project): string => (p.kind === "event" ? `${p.calendarName} · one-off` : p.calendarName);

/** Connected with a token granted before calendars were listed. */
export const canReconnect = (status: GoogleStatus | null): boolean => !!status && status.connected && status.needsReconnect;

/** Why there are no projects (or can't be refreshed), or null when nothing is wrong. */
export function reconnectHint(status: GoogleStatus | null): string | null {
  if (!status) return null;
  if (!status.configured) return "Google Calendar isn't configured in this build.";
  if (!status.connected) return "Connect Google Calendar to load your repeating events.";
  if (status.needsReconnect) return "Reconnect Google Calendar to load your calendars and repeating events.";
  return null;
}
