/** Pure decisions for the native right-click menus (timeline blocks and Google event ghosts). */

export type MenuEntry = { id: string; text: string } | { separator: true };
export type MenuTargetKind = "typing" | "block" | "ghost" | "other";

const SEP: MenuEntry = { separator: true };
const DELETE_EVENT: MenuEntry = { id: "delete-event", text: "Delete event from Google Calendar" };

export function blockMenu(p: { isToday: boolean; timingHere: boolean; hasEvent: boolean; canDeleteEvents: boolean }): MenuEntry[] {
  const items: MenuEntry[] = [{ id: "open", text: "Open note" }];
  if (p.timingHere) items.push({ id: "stop", text: "Stop timer" });
  else if (p.isToday) items.push({ id: "start", text: "Start timer" });
  items.push(SEP, { id: "delete-block", text: "Delete block" });
  if (p.hasEvent && p.canDeleteEvents) items.push(DELETE_EVENT);
  return items;
}

export function ghostMenu(p: { canDeleteEvents: boolean }): MenuEntry[] {
  const items: MenuEntry[] = [{ id: "open", text: "Open as block" }];
  if (p.canDeleteEvents) items.push(SEP, DELETE_EVENT);
  return items;
}

/** Our menus replace the webview's on blocks and ghosts; elsewhere the default only survives in typing targets and in dev. */
export function shouldSuppressDefault(kind: MenuTargetKind, prod: boolean): boolean {
  if (kind === "typing") return false;
  if (kind === "block" || kind === "ghost") return true;
  return prod;
}

interface Closest {
  closest(selector: string): unknown;
}

export function menuTargetKind(target: Closest | null): MenuTargetKind {
  if (!target) return "other";
  if (target.closest("input, textarea, select, [contenteditable]")) return "typing";
  if (target.closest("[data-ghost]")) return "ghost";
  if (target.closest("[data-block]")) return "block";
  return "other";
}
