import { useState } from "react";
import { PlusIcon } from "@heroicons/react/20/solid";
import type { Project } from "../api";
import { projectStyle } from "../lib/projects";
import { ProjectDot, ProjectPicker } from "./ProjectPicker";
import "../projects.css";

/** What the notes panel needs to show and change the selected block's project. */
export interface ProjectPanel {
  /** Google Calendar is connected (otherwise there are no projects to show). */
  connected: boolean;
  projects: Project[];
  /** The block's project (its matched event's calendar). */
  current: Project | null;
  /** The block has a Google event. */
  hasEvent: boolean;
  /** Moves the matched event to another calendar. Resolves when done (errors are shown as notices). */
  onMove: (projectId: number) => Promise<void>;
  /** Creates the block's event in a calendar. */
  onAdd: (projectId: number) => Promise<void>;
}

/** "Mestrado ●" for a block that has a project (click to move it), else "Add to project…". */
export function ProjectChip({ panel, hasTitle }: { panel: ProjectPanel; hasTitle: boolean }) {
  const [busy, setBusy] = useState(false);
  if (!panel.connected) return null;
  const run = async (fn: (id: number) => Promise<void>, id: number | null) => {
    if (id === null) return;
    setBusy(true);
    try {
      await fn(id);
    } finally {
      setBusy(false);
    }
  };

  if (panel.hasEvent) {
    const cur = panel.current;
    if (!cur) return null;
    return (
      <div className="projchip" style={projectStyle(cur.color)}>
        <ProjectPicker
          projects={panel.projects}
          value={cur.id}
          onChange={(id) => void run(panel.onMove, id)}
          label={`Project: ${cur.name}. Move to another project`}
          caption="Move to project"
          variant="chip"
          disabled={busy}
          trigger={
            <>
              <span className="projchip__name">{cur.name}</span>
              <ProjectDot color={cur.color} />
            </>
          }
        />
      </div>
    );
  }

  return (
    <div className="projchip">
      <ProjectPicker
        projects={panel.projects}
        value={null}
        onChange={(id) => void run(panel.onAdd, id)}
        label="Add to project"
        caption="Create the event in"
        variant="chip"
        disabled={busy || !hasTitle}
        trigger={
          <>
            <PlusIcon className="projchip__plus" aria-hidden="true" />
            <span className="projchip__name">Add to project…</span>
          </>
        }
      />
      {!hasTitle && <span className="projchip__hint">Give the block a title first</span>}
    </div>
  );
}
