import { describe, expect, it } from "vitest";
import type { CalEvent, Project } from "../api";
import { blockCalendarId, blockProject, canReconnect, pickableProjects, projectStyle, reconnectHint } from "./projects";

const project = (over: Partial<Project> = {}): Project => ({
  id: 1,
  googleCalendarId: "cal-1",
  name: "Mestrado",
  color: "#3d6fb6",
  showOnTimeline: true,
  isPrimary: false,
  archived: false,
  ...over,
});
const event = (over: Partial<CalEvent> = {}): CalEvent => ({
  id: "e1",
  start: 540,
  end: 600,
  title: "Study",
  projectId: 1,
  color: "#3d6fb6",
  ...over,
});

describe("projectStyle", () => {
  it("sets_the_css_variable", () => {
    expect(projectStyle("#3d6fb6")).toEqual({ "--project": "#3d6fb6" });
    expect(projectStyle("#abc")).toEqual({ "--project": "#abc" });
  });
  it("drops_anything_that_is_not_a_hex_color", () => {
    expect(projectStyle(null)).toBeUndefined();
    expect(projectStyle("")).toBeUndefined();
    expect(projectStyle("red; background: url(x)")).toBeUndefined();
    expect(projectStyle("var(--x)")).toBeUndefined();
  });
});

describe("blockProject", () => {
  const projects = [project(), project({ id: 2, googleCalendarId: "cal-2", name: "Headline" })];
  it("is_the_project_of_the_matched_event", () => {
    const matched = new Map([["b1", event({ projectId: 2 })]]);
    expect(blockProject("b1", matched, projects)?.name).toBe("Headline");
  });
  it("is_null_for_unmatched_blocks_or_unknown_projects", () => {
    expect(blockProject("b1", new Map(), projects)).toBeNull();
    expect(blockProject("b1", new Map([["b1", event({ projectId: null })]]), projects)).toBeNull();
    expect(blockProject("b1", new Map([["b1", event({ projectId: 99 })]]), projects)).toBeNull();
  });
});

describe("blockCalendarId", () => {
  it("is_the_google_calendar_id_or_null", () => {
    expect(blockCalendarId(project({ googleCalendarId: "cal-9" }))).toBe("cal-9");
    expect(blockCalendarId(null)).toBeNull();
  });
});

describe("pickableProjects", () => {
  it("hides_archived_and_lists_shown_first_then_primary_then_name", () => {
    const list = [
      project({ id: 1, name: "Zeta", showOnTimeline: false }),
      project({ id: 2, name: "Beta" }),
      project({ id: 3, name: "Gone", archived: true }),
      project({ id: 4, name: "Alpha" }),
      project({ id: 5, name: "Main", isPrimary: true }),
    ];
    expect(pickableProjects(list).map((p) => p.name)).toEqual(["Main", "Alpha", "Beta", "Zeta"]);
  });
});

describe("reconnect hints", () => {
  const status = (over: Partial<{ configured: boolean; connected: boolean; needsReconnect: boolean }>) => ({
    configured: true,
    connected: true,
    email: null,
    needsReconnect: false,
    ...over,
  });
  it("needs_reconnect_only_when_connected_and_flagged", () => {
    expect(canReconnect(status({ needsReconnect: true }))).toBe(true);
    expect(canReconnect(status({}))).toBe(false);
    expect(canReconnect(null)).toBe(false);
  });
  it("hint_depends_on_google_state", () => {
    expect(reconnectHint(null)).toBeNull();
    expect(reconnectHint(status({ configured: false, connected: false }))).toBe("Google Calendar isn't configured in this build.");
    expect(reconnectHint(status({ connected: false }))).toBe("Connect Google Calendar to load your calendars as projects.");
    expect(reconnectHint(status({ needsReconnect: true }))).toBe("Reconnect Google Calendar to load your calendars.");
    expect(reconnectHint(status({}))).toBeNull();
  });
});
