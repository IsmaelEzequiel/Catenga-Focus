export interface KeyLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  /** Auto-repeat of a held key. */
  repeat?: boolean;
}

/** Cmd+K on macOS, Ctrl+K elsewhere. */
export function isSearchShortcut(e: KeyLike, isMac: boolean): boolean {
  if (e.shiftKey || e.altKey || e.key.toLowerCase() !== "k") return false;
  return isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

/** Cmd+Shift+F on macOS, Ctrl+Shift+F elsewhere: opens the focus screen. */
export function isFocusShortcut(e: KeyLike, isMac: boolean): boolean {
  if (!e.shiftKey || e.altKey || e.key.toLowerCase() !== "f") return false;
  return isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

/** Cmd+D on macOS, Ctrl+D elsewhere: opens the dashboard. */
export function isDashboardShortcut(e: KeyLike, isMac: boolean): boolean {
  if (e.shiftKey || e.altKey || e.key.toLowerCase() !== "d") return false;
  return isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

/** Cmd+G on macOS, Ctrl+G elsewhere: opens the goals page. */
export function isGoalsShortcut(e: KeyLike, isMac: boolean): boolean {
  if (e.shiftKey || e.altKey || e.key.toLowerCase() !== "g") return false;
  return isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

export type ShortcutView = "day" | "history" | "dashboard" | "goals" | "recap";

export interface KeyContext {
  /** Focus is in an input, textarea, select or contenteditable. */
  typing: boolean;
  /** Focus is on a button/link: Enter and Space belong to it. */
  onControl: boolean;
  dragging: boolean;
  focusOpen: boolean;
  paletteOpen: boolean;
  /** Settings, the help overlay or another dialog is open. */
  modalOpen: boolean;
  view: ShortcutView;
  isMac: boolean;
}

export type ShortcutAction =
  | "quickAdd"
  | "today"
  | "openNote"
  | "toggleTimer"
  | "prevBlock"
  | "nextBlock"
  | "focus"
  | "rename"
  | "help";

const PLAIN: Record<string, ShortcutAction> = {
  n: "quickAdd",
  t: "today",
  Enter: "openNote",
  " ": "toggleTimer",
  ArrowUp: "prevBlock",
  ArrowDown: "nextBlock",
  f: "focus",
  r: "rename",
  "?": "help",
};
const DAY_ONLY: ReadonlySet<ShortcutAction> = new Set([
  "openNote",
  "toggleTimer",
  "prevBlock",
  "nextBlock",
  "focus",
  "rename",
]);

/**
 * The M10 shortcuts (`docs/plans/M10-speed.md` §2). Cmd/Ctrl+K, D, G and Cmd/Ctrl+Shift+F keep their own predicates
 * above; ←/→, Delete and Esc stay with the views that own them. Returns null when the key is not ours right now.
 */
export function shortcutFor(e: KeyLike, c: KeyContext): ShortcutAction | null {
  if (c.dragging || c.paletteOpen || c.modalOpen) return null;
  const mod = c.isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "n")
    return c.focusOpen || e.repeat ? null : "quickAdd";
  if (e.metaKey || e.ctrlKey || e.altKey || c.typing || e.repeat) return null;
  if (e.shiftKey && e.key !== "?") return null;
  const action = PLAIN[e.key.length === 1 ? e.key.toLowerCase() : e.key];
  if (!action) return null;
  if (c.focusOpen)
    return action === "toggleTimer" && !c.onControl ? action : null;
  if ((action === "openNote" || action === "toggleTimer") && c.onControl)
    return null;
  if (DAY_ONLY.has(action) && c.view !== "day") return null;
  return action;
}

/** The block to select when stepping up/down by start time; stays at the ends, starts from an edge with no selection. */
export function stepBlock(
  blocks: { key: string; start: number; end: number }[],
  selected: string | null,
  dir: 1 | -1,
): string | null {
  if (blocks.length === 0) return null;
  const order = blocks
    .map((b, i) => ({ ...b, i }))
    .sort((a, b) => a.start - b.start || a.end - b.end || a.i - b.i);
  const at = order.findIndex((b) => b.key === selected);
  if (at === -1) return (dir === 1 ? order[0] : order[order.length - 1]).key;
  return order[Math.min(Math.max(at + dir, 0), order.length - 1)].key;
}

export interface ShortcutRow {
  keys: string[];
  label: string;
}

export interface ShortcutGroup {
  name: string;
  rows: ShortcutRow[];
}

/** What the help overlay lists, by context; `Cmd` is shown as ⌘ on macOS and Ctrl elsewhere. */
export function shortcutGroups(isMac: boolean): ShortcutGroup[] {
  const mod = isMac ? "⌘" : "Ctrl";
  return [
    {
      name: "Day",
      rows: [
        { keys: ["N"], label: "Quick-add a block" },
        { keys: [mod, "N"], label: "Quick-add (also while typing)" },
        { keys: ["T"], label: "Go to today" },
        { keys: ["←", "→"], label: "Previous / next day" },
      ],
    },
    {
      name: "Block",
      rows: [
        { keys: ["↑", "↓"], label: "Select previous / next block" },
        { keys: ["Enter"], label: "Open the selected block's note" },
        { keys: ["R"], label: "Rename the selected block" },
        { keys: ["Delete"], label: "Delete the selected block" },
      ],
    },
    {
      name: "Timer",
      rows: [
        { keys: ["Space"], label: "Start / stop the timer (today)" },
        { keys: ["F"], label: "Focus screen for the selected block" },
        { keys: [mod, "⇧", "F"], label: "Focus screen, anywhere" },
      ],
    },
    {
      name: "Go to",
      rows: [
        { keys: [mod, "K"], label: "Search" },
        { keys: [mod, "D"], label: "Dashboard" },
        { keys: [mod, "G"], label: "Goals" },
        { keys: ["Esc"], label: "Close / go back" },
        { keys: ["?"], label: "Show the keyboard shortcuts" },
      ],
    },
  ];
}

/** Every row of every group, flat. */
export const shortcutRows = (isMac: boolean): ShortcutRow[] =>
  shortcutGroups(isMac).flatMap((g) => g.rows);
