export interface KeyLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
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
