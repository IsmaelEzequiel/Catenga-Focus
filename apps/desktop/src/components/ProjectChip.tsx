import { useState } from "react";
import { PlusIcon } from "@heroicons/react/20/solid";
import type { Project } from "../api";
import { projectStyle } from "../lib/projects";
import { ProjectDot } from "./ProjectPicker";
import { Button } from "./ui/Button";
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
      <span className="projchip" style={projectStyle(cur.color)} title={`Project · calendar ${cur.calendarName}`}>
        <ProjectDot color={cur.color} />
        Project · {cur.name}
      </span>
    );
  }

  if (!panel.canMake) return null;
  return (
    <Button
      size="sm"
      variant="ghost"
      icon={PlusIcon}
      className="projchip__make"
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
      Make this a project
    </Button>
  );
}
