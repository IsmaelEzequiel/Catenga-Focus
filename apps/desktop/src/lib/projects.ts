/** Pure helpers for projects (Google calendars); see `docs/plans/M8b-projects.md` §1.3 and §1.5. */
import type { CSSProperties } from "react";
import type { CalEvent, GoogleStatus, Project } from "../api";

const HEX = /^#[0-9a-f]{3,8}$/i;

/** Inline style carrying a project's Google color as `--project`; undefined for anything that is not a hex color. */
export function projectStyle(color: string | null): CSSProperties | undefined {
  if (color === null || !HEX.test(color)) return undefined;
  return { ["--project" as string]: color };
}

/** A block's project: the calendar of the event it matches; unmatched blocks have none. */
export function blockProject(blockKey: string, matched: Map<string, CalEvent>, projects: Project[]): Project | null {
  const id = matched.get(blockKey)?.projectId;
  if (id === null || id === undefined) return null;
  return projects.find((p) => p.id === id) ?? null;
}

/** What timer sessions store as their project. */
export const blockCalendarId = (project: Project | null): string | null => project?.googleCalendarId ?? null;

/** Projects to offer in pickers: not archived; shown ones first, primary first, then by name. */
export function pickableProjects(projects: Project[]): Project[] {
  return projects
    .filter((p) => !p.archived)
    .sort(
      (a, b) =>
        Number(b.showOnTimeline) - Number(a.showOnTimeline) ||
        Number(b.isPrimary) - Number(a.isPrimary) ||
        a.name.localeCompare(b.name),
    );
}

/** Connected with a token granted before calendars became projects. */
export const canReconnect = (status: GoogleStatus | null): boolean => !!status && status.connected && status.needsReconnect;

/** Why there are no projects (or can't be refreshed), or null when nothing is wrong. */
export function reconnectHint(status: GoogleStatus | null): string | null {
  if (!status) return null;
  if (!status.configured) return "Google Calendar isn't configured in this build.";
  if (!status.connected) return "Connect Google Calendar to load your calendars as projects.";
  if (status.needsReconnect) return "Reconnect Google Calendar to load your calendars.";
  return null;
}
