/** The Settings sidebar: which panes exist and how the arrow keys move between them. */
export type SettingsTab = "general" | "calendars" | "projects" | "tags";

export const SETTINGS_TABS: ReadonlyArray<{ id: SettingsTab; label: string }> = [
  { id: "general", label: "General" },
  { id: "calendars", label: "Calendars" },
  { id: "projects", label: "Projects" },
  { id: "tags", label: "Tags" },
];

/** The tab a navigation key lands on (↑/↓ wrap, Home/End), or null when the key does nothing. */
export function moveTab(current: SettingsTab, key: string): SettingsTab | null {
  const i = SETTINGS_TABS.findIndex((t) => t.id === current);
  const n = SETTINGS_TABS.length;
  if (key === "ArrowDown") return SETTINGS_TABS[(i + 1) % n].id;
  if (key === "ArrowUp") return SETTINGS_TABS[(i + n - 1) % n].id;
  if (key === "Home") return SETTINGS_TABS[0].id;
  if (key === "End") return SETTINGS_TABS[n - 1].id;
  return null;
}
