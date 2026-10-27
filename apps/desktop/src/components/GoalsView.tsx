import { useEffect } from "react";
import type { GoalProgress, GoogleStatus, TimerStatus } from "../api";
import { useProjects } from "../hooks/useProjects";
import { useTags } from "../hooks/useTags";
import { GoalsSection } from "./GoalsSection";
import { ProjectsTags } from "./ProjectsTags";
import "../projects.css";

interface Props {
  /** Shared with the timeline and the notes panel. */
  projects: ReturnType<typeof useProjects>;
  google: GoogleStatus | null;
  onConnect: () => void;
  timerStatus: TimerStatus;
  nowMs: number;
  onPlay: (p: GoalProgress) => void;
}

/** The Goals page: goals, then the projects (repeating events), calendars and tags they can point at. */
export function GoalsView({ projects, google, onConnect, timerStatus, nowMs, onPlay }: Props) {
  const tags = useTags();
  const { refresh } = projects;
  const canRead = !!google?.connected && !google.needsReconnect;

  // Opening the page re-reads the calendar list, at most every 10 minutes.
  useEffect(() => {
    if (canRead) void refresh(false);
  }, [canRead, refresh]);

  return (
    <section className="goalspage" aria-label="Goals">
      <div className="goalspage__scroll">
        <div className="goalspage__inner">
          <GoalsSection projects={projects.list} tags={tags.list} timerStatus={timerStatus} nowMs={nowMs} onPlay={onPlay} />
          <ProjectsTags
            projects={projects.list}
            calendars={projects.calendars}
            projectsError={projects.error}
            refreshing={projects.refreshing}
            google={google}
            onRefresh={() => void projects.refresh(true)}
            onConnect={onConnect}
            onSetShown={(id, shown) => void projects.setShown(id, shown)}
            onSetArchived={(id, archived) => void projects.setArchived(id, archived)}
            tags={tags.list}
            tagsError={tags.error}
            onSaveTag={tags.save}
            onDeleteTag={tags.remove}
          />
        </div>
      </div>
    </section>
  );
}
