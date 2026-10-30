import { useEffect } from "react";
import type { GoalProgress, GoogleStatus, TimerStatus } from "../api";
import { useProjects } from "../hooks/useProjects";
import { useTags } from "../hooks/useTags";
import type { SettingsTab } from "../lib/settingsTabs";
import { GoalsSection } from "./GoalsSection";
import { Button } from "./ui/Button";
import "../projects.css";

interface Props {
  /** Shared with the timeline and the notes panel. */
  projects: ReturnType<typeof useProjects>;
  google: GoogleStatus | null;
  onOpenSettings: (tab: SettingsTab) => void;
  timerStatus: TimerStatus;
  nowMs: number;
  onPlay: (p: GoalProgress) => void;
}

/** The Goals page: only the goals. Projects, calendars and tags live in Settings. */
export function GoalsView({ projects, google, onOpenSettings, timerStatus, nowMs, onPlay }: Props) {
  const tags = useTags();
  const { refresh } = projects;
  const canRead = !!google?.connected && !google.needsReconnect;

  // Opening the page re-reads the calendar list for the goal form's pickers, at most every 10 minutes.
  useEffect(() => {
    if (canRead) void refresh(false);
  }, [canRead, refresh]);

  const empty = projects.list.length === 0 && tags.list.length === 0;

  return (
    <section className="goalspage" aria-label="Goals">
      <div className="goalspage__scroll">
        <div className="goalspage__inner">
          {empty && (
            <p className="goalspage__hint">
              Projects and tags live in Settings
              <Button variant="ghost" size="sm" onClick={() => onOpenSettings("projects")}>
                Open Settings
              </Button>
            </p>
          )}
          <GoalsSection projects={projects.list} tags={tags.list} timerStatus={timerStatus} nowMs={nowMs} onPlay={onPlay} />
        </div>
      </div>
    </section>
  );
}
