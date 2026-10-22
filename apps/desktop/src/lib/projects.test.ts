import { describe, expect, it } from "vitest";
import type { CalEvent, Project } from "../api";
import {
  blockProject,
  blockProjectKey,
  canMakeProject,
  canReconnect,
  listProjects,
  pickableProjects,
  projectHint,
  projectStyle,
  reconnectHint,
} from "./projects";

const project = (over: Partial<Project> = {}): Project => ({
  id: 1,
  kind: "series",
  key: "series:abc",
  name: "Learn Rust",
  color: "#3d6fb6",
  calendarName: "Mestrado",
  archived: false,
  ...over,
});
const event = (over: Partial<CalEvent> = {}): CalEvent => ({
  id: "e1",
  start: 540,
  end: 600,
  title: "Study",
  projectKey: "series:abc",
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

describe("blockProjectKey", () => {
  it("is_the_project_key_of_the_matched_event", () => {
    expect(blockProjectKey("b1", new Map([["b1", event({ projectKey: "event:z" })]]))).toBe("event:z");
    expect(blockProjectKey("b1", new Map([["b1", event()]]))).toBe("series:abc");
  });
  it("is_null_for_unmatched_blocks_or_events_that_are_not_projects", () => {
    expect(blockProjectKey("b1", new Map())).toBeNull();
    expect(blockProjectKey("b1", new Map([["b1", event({ projectKey: null, projectId: null })]]))).toBeNull();
  });
});

describe("blockProject", () => {
  const projects = [project(), project({ id: 2, key: "series:def", name: "Deep work" })];
  it("finds_the_project_by_the_events_key", () => {
    expect(blockProject("b1", new Map([["b1", event({ projectKey: "series:def" })]]), projects)?.name).toBe("Deep work");
  });
  it("is_null_when_unmatched_not_a_project_or_unknown", () => {
    expect(blockProject("b1", new Map(), projects)).toBeNull();
    expect(blockProject("b1", new Map([["b1", event({ projectKey: null })]]), projects)).toBeNull();
    expect(blockProject("b1", new Map([["b1", event({ projectKey: "series:zzz" })]]), projects)).toBeNull();
  });
});

describe("canMakeProject", () => {
  it("only_for_a_matched_event_that_is_not_a_project", () => {
    expect(canMakeProject("b1", new Map([["b1", event({ projectKey: null, projectId: null })]]))).toBe(true);
    expect(canMakeProject("b1", new Map([["b1", event()]]))).toBe(false);
    expect(canMakeProject("b1", new Map())).toBe(false);
  });
});

describe("pickableProjects", () => {
  it("hides_archived_and_lists_series_before_events_then_by_name", () => {
    const list = [
      project({ id: 1, name: "Zeta" }),
      project({ id: 2, name: "One-off", kind: "event", key: "event:a" }),
      project({ id: 3, name: "Gone", archived: true }),
      project({ id: 4, name: "alpha" }),
      project({ id: 5, name: "Beta" }),
    ];
    expect(pickableProjects(list).map((p) => p.name)).toEqual(["alpha", "Beta", "Zeta", "One-off"]);
  });
  it("keeps_the_current_one_even_when_archived", () => {
    const list = [project({ id: 1, name: "Gone", archived: true }), project({ id: 2, name: "Live" })];
    expect(pickableProjects(list, 1).map((p) => p.name)).toEqual(["Gone", "Live"]);
  });
});

describe("listProjects", () => {
  it("puts_active_first_then_archived_each_series_first_by_name", () => {
    const list = [
      project({ id: 1, name: "Old", archived: true }),
      project({ id: 2, name: "B", kind: "event", key: "event:b" }),
      project({ id: 3, name: "A" }),
    ];
    expect(listProjects(list).map((p) => p.name)).toEqual(["A", "B", "Old"]);
  });
});

describe("projectHint", () => {
  it("is_the_calendar_name_and_marks_one_off_events", () => {
    expect(projectHint(project())).toBe("Mestrado");
    expect(projectHint(project({ kind: "event" }))).toBe("Mestrado · one-off");
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
    expect(reconnectHint(status({ connected: false }))).toBe("Connect Google Calendar to load your repeating events.");
    expect(reconnectHint(status({ needsReconnect: true }))).toBe("Reconnect Google Calendar to load your calendars and repeating events.");
    expect(reconnectHint(status({}))).toBeNull();
  });
});
