import { ArchiveBoxIcon, ArchiveBoxXMarkIcon, ArrowPathIcon } from "@heroicons/react/20/solid";
import type { GoogleStatus, Project } from "../api";
import { canReconnect, listProjects, projectStyle, reconnectHint } from "../lib/projects";
import { ProjectDot } from "./ProjectPicker";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import { ListRow } from "./ui/ListRow";
import "../card.css";
import "../projects.css";

interface Props {
  projects: Project[];
  error: string | null;
  refreshing: boolean;
  google: GoogleStatus | null;
  onRefresh: () => void;
  onConnect: () => void;
  onSetArchived: (id: number, archived: boolean) => void;
}

/** Projects (repeating events) grouped by the calendar they live in, with Archive / Restore. */
export function ProjectsPane(p: Props) {
  const hint = reconnectHint(p.google);
  const projects = listProjects(p.projects);
  // One header per calendar (its name carries the account), so it shows once.
  const groups = new Map<string, Project[]>();
  for (const x of projects) groups.set(x.calendarName, [...(groups.get(x.calendarName) ?? []), x]);
  return (
    <div className="card card--flush">
      <header className="card__head projtags__head">
        <div className="projtags__headtext">
          <h4 className="card__title">Projects</h4>
          <p className="card__desc">A repeating event is a project. Its occurrences, blocks and sessions belong to it.</p>
        </div>
        {p.google?.connected && !canReconnect(p.google) && (
          <Button
            size="sm"
            icon={ArrowPathIcon}
            className={p.refreshing ? "projtags__spin" : undefined}
            onClick={p.onRefresh}
            disabled={p.refreshing}
          >
            Refresh
          </Button>
        )}
      </header>
      {hint && (
        <div className="projtags__notice">
          <Banner
            tone="info"
            variant="inline"
            actions={
              p.google?.configured ? (
                <Button size="sm" onClick={p.onConnect}>
                  {p.google.connected ? "Reconnect" : "Connect Google Calendar"}
                </Button>
              ) : undefined
            }
          >
            {hint}
          </Banner>
        </div>
      )}
      {p.error && (
        <div className="projtags__notice">
          <Banner
            tone="error"
            variant="inline"
            actions={
              <Button size="sm" onClick={p.onRefresh} disabled={p.refreshing}>
                Try again
              </Button>
            }
          >
            {p.error}
          </Banner>
        </div>
      )}
      {projects.length === 0 && !hint && <p className="projtags__empty">Repeating events from your calendar show up here.</p>}
      {[...groups.entries()].map(([account, list]) => (
        <div key={account}>
          <div className="projtags__group">{account}</div>
          {list.map((x) => (
            <div key={x.id} className="projtags__item" style={projectStyle(x.color)}>
              <ListRow
                lead={<ProjectDot color={x.color} />}
                title={<span className={x.archived ? "projtags__off" : undefined}>{x.name}</span>}
                meta={[x.kind === "event" ? "one-off" : null, x.archived ? "archived" : null].filter(Boolean).join(" · ") || null}
                actions={
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={x.archived ? ArchiveBoxXMarkIcon : ArchiveBoxIcon}
                    onClick={() => p.onSetArchived(x.id, !x.archived)}
                    aria-label={`${x.archived ? "Restore" : "Archive"} ${x.name}`}
                  >
                    {x.archived ? "Restore" : "Archive"}
                  </Button>
                }
              />
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
