import { Menu } from "@tauri-apps/api/menu";
import type { MenuEntry } from "./contextMenu";

/**
 * The last menu shown. Each `Menu.new` holds a resource on the Rust side until closed; it is
 * released when the next menu opens (by then its action, if any, has run), so at most one is kept.
 */
let previous: Menu | null = null;

/** Pops up a native menu at the pointer; `onPick` gets the chosen id. Does nothing without the Tauri bridge (tests, plain browser). */
export async function popupMenu(entries: MenuEntry[], onPick: (id: string) => void): Promise<void> {
  try {
    const old = previous;
    previous = null;
    void old?.close().catch(() => {});
    const menu = await Menu.new({
      items: entries.map((e) => ("separator" in e ? { item: "Separator" as const } : { id: e.id, text: e.text, action: onPick })),
    });
    previous = menu;
    await menu.popup();
  } catch {
    // No bridge: the default menu was already suppressed for this target; nothing else to do.
  }
}
