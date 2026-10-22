import { useState } from "react";
import { PlusIcon } from "@heroicons/react/20/solid";
import type { Project } from "../api";
import { projectStyle } from "../lib/projects";
import { ProjectDot } from "./ProjectPicker";
import "../projects.css";

/** What the notes panel needs to show the selected block's project. */
export interface ProjectPanel {
  /** Google Calendar is connected (otherwise there are no projects to show). */
  connected: boolean;
  /** The block's project (its matched event's series). */
  current: Project | null;
  /** The block matches a one-off event that is not a project yet. */
  canMake: boolean;
  /** Makes the matched one-off event a project. Resolves when done (errors are shown as notices). */
  onMake: () => Promise<void>;
}

/** "Learn Rust ●" (read-only) for a block with a project; "Make this a project" for a one-off event. */
export function ProjectChip({ panel }: { panel: ProjectPanel }) {
  const [busy, setBusy] = useState(false);
  if (!panel.connected) return null;

  if (panel.current) {
    const cur = panel.current;
    return (
      <div className="projchip" style={projectStyle(cur.color)}>
        <span className="projchip__label" title={`Project · calendar ${cur.calendarName}`}>
          <span className="projchip__name">{cur.name}</span>
          <ProjectDot color={cur.color} />
        </span>
      </div>
    );
  }

  if (!panel.canMake) return null;
  return (
    <div className="projchip">
      <button
        type="button"
        className="app__btn projchip__make"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            await panel.onMake();
          } finally {
            setBusy(false);
          }
        }}
      >
        <PlusIcon className="app__icon" aria-hidden="true" />
        Make this a project
      </button>
    </div>
  );
}
