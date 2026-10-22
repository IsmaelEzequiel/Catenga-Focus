import { describe, expect, it } from "vitest";
import type { CalEvent, GoalProgress, Project, Tag } from "../api";
import { findGoalBlock } from "./goalPlay";

const project: Project = { id: 1, kind: "series", key: "series:tc", name: "Teoria", color: "#3d6fb6", calendarName: "Mestrado", archived: false };
const tag = (id: number, name: string): Tag => ({ id, name, color: "sun", saved: true, goalCount: 1 });
const goal = (over: Partial<GoalProgress> = {}, g: Partial<GoalProgress["goal"]> = {}): GoalProgress => ({
  goal: { id: 1, projectId: null, tagIds: [], title: null, minutesPerDay: 60, start: "2026-10-10", days: null, archived: false, ...g },
  project: null,
  tags: [],
  status: "active",
  todayMinutes: 0,
  elapsedDays: 1,
  metDays: 0,
  totalMinutes: 0,
  currentStreak: 0,
  longestStreak: 0,
  end: null,
  last14: [],
  ...over,
});
const block = (key: string, start: number, end: number, title: string, tags: string[] = []) => ({ key, start, end, title, tags, body: "" });
const event = (id: string, start: number, end: number, title: string, projectKey: string | null = "series:tc"): CalEvent => ({
  id,
  start,
  end,
  title,
  projectKey,
  projectId: projectKey ? 1 : null,
  color: "#3d6fb6",
});

describe("findGoalBlock", () => {
  const byTitle = goal({}, { title: "Rust" });

  it("finds a block by title, case-insensitively", () => {
    const blocks = [block("a", 540, 600, "Other"), block("b", 600, 660, " rust ")];
    expect(findGoalBlock(blocks, [], byTitle, 700)).toEqual({ kind: "block", key: "b" });
  });
  it("finds a block by tag", () => {
    const g = goal({ tags: [tag(1, "mestrado")] }, { tagIds: [1] });
    expect(findGoalBlock([block("a", 540, 600, "x", ["Mestrado"])], [], g, 700)).toEqual({ kind: "block", key: "a" });
  });
  it("prefers the block that contains now, else the latest started", () => {
    const blocks = [block("a", 540, 600, "Rust"), block("b", 620, 680, "Rust"), block("c", 700, 760, "Rust")];
    expect(findGoalBlock(blocks, [], byTitle, 650)).toEqual({ kind: "block", key: "b" });
    expect(findGoalBlock(blocks, [], byTitle, 690)).toEqual({ kind: "block", key: "b" });
  });
  it("takes the earliest upcoming one when none has started", () => {
    const blocks = [block("c", 800, 860, "Rust"), block("b", 700, 760, "Rust")];
    expect(findGoalBlock(blocks, [], byTitle, 600)).toEqual({ kind: "block", key: "b" });
  });
  it("matches a project goal through the block's event", () => {
    const p = goal({ project }, { projectId: 1 });
    const blocks = [block("a", 540, 600, "Aula")];
    const events = [event("e1", 540, 600, "Aula")];
    expect(findGoalBlock(blocks, events, p, 560)).toEqual({ kind: "block", key: "a" });
    expect(findGoalBlock(blocks, [event("e1", 540, 600, "Aula", null)], p, 560)).toEqual({ kind: "none" });
  });
  it("finds the block Play made for a project goal on a day without its event", () => {
    // No event → the block has no project; it is recognised by the project's name, so a
    // second Play reuses it instead of creating another one.
    const p = goal({ project }, { projectId: 1 });
    const blocks = [block("m", 600, 660, "Teoria")];
    expect(findGoalBlock(blocks, [], p, 620)).toEqual({ kind: "block", key: "m" });
    // A block of another project with the same title is not taken.
    const other = [block("o", 540, 600, "Teoria")];
    const otherEvent = [event("e9", 540, 600, "Teoria", "series:other")];
    expect(findGoalBlock(other, otherEvent, p, 560)).toEqual({ kind: "none" });
  });
  it("offers a ghost event of the project when no block matches", () => {
    const p = goal({ project }, { projectId: 1 });
    const ev = event("e1", 540, 600, "Aula");
    expect(findGoalBlock([], [ev, event("e2", 540, 600, "Other", "series:x")], p, 560)).toEqual({ kind: "event", event: ev });
  });
  it("a project goal takes the project's block whatever its title and tags", () => {
    const p = goal({ project, tags: [tag(1, "mestrado")] }, { projectId: 1, tagIds: [1], title: "Teoria" });
    const blocks = [block("a", 1260, 1320, "Aula 7", ["other"])];
    expect(findGoalBlock(blocks, [event("e1", 1260, 1320, "Aula 7")], p, 1300)).toEqual({ kind: "block", key: "a" });
  });
  it("ignores events for goals without a project", () => {
    expect(findGoalBlock([], [event("e1", 540, 600, "Rust")], byTitle, 560)).toEqual({ kind: "none" });
  });
  it("picks the ghost that contains now, else the latest started", () => {
    const p = goal({ project }, { projectId: 1 });
    const a = event("a", 540, 600, "Aula");
    const b = event("b", 700, 760, "Aula");
    expect(findGoalBlock([], [a, b], p, 720)).toEqual({ kind: "event", event: b });
    expect(findGoalBlock([], [a, b], p, 650)).toEqual({ kind: "event", event: a });
  });
  it("finds nothing when nothing matches", () => {
    expect(findGoalBlock([block("a", 540, 600, "Other")], [], byTitle, 560)).toEqual({ kind: "none" });
  });
});
