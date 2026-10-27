import { useState, type FormEvent } from "react";
import { ArchiveBoxIcon, ArchiveBoxXMarkIcon, ArrowPathIcon, PlusIcon, TrashIcon } from "@heroicons/react/20/solid";
import type { Calendar, GoogleStatus, Project, Tag, TagColor } from "../api";
import { canReconnect, listProjects, projectStyle, reconnectHint } from "../lib/projects";
import { isValidTagName, TAG_PALETTE, tagVar } from "../lib/tags";
import { ProjectDot } from "./ProjectPicker";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import { ListRow } from "./ui/ListRow";
import "../card.css";
import "../field.css";
import "../projects.css";

interface Props {
  projects: Project[];
  calendars: Calendar[];
  projectsError: string | null;
  refreshing: boolean;
  google: GoogleStatus | null;
  onRefresh: () => void;
  onConnect: () => void;
  onSetShown: (id: number, shown: boolean) => void;
  onSetArchived: (id: number, archived: boolean) => void;
  tags: Tag[];
  tagsError: string | null;
  onSaveTag: (name: string, color: TagColor) => Promise<string | null>;
  onDeleteTag: (id: number) => Promise<string | null>;
}

/** Projects (repeating events) grouped by account, the calendars that feed the timeline, and the tag list with colors. */
export function ProjectsTags(p: Props) {
  const hint = reconnectHint(p.google);
  const projects = listProjects(p.projects);
  const calendars = p.calendars.filter((c) => !c.archived);
  // One header per account (the calendar the series lives in), so the email shows once.
  const groups = new Map<string, Project[]>();
  for (const x of projects) groups.set(x.calendarName, [...(groups.get(x.calendarName) ?? []), x]);
  return (
    <section className="projtags" aria-label="Projects, calendars and tags">
      <h3 className="projtags__h">Projects, calendars and tags</h3>

      <div className="card card--flush">
        <header className="card__head projtags__head">
          <div className="projtags__headtext">
            <h4 className="card__title">Projects</h4>
            <p className="card__desc">
              A repeating event is a project. Its occurrences, blocks and sessions belong to it.
            </p>
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
        {p.projectsError && (
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
              {p.projectsError}
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

      <div className="card card--flush">
        <header className="card__head">
          <h4 className="card__title">Calendars</h4>
          <p className="card__desc">Calendars only decide what shows on the timeline.</p>
        </header>
        {calendars.length === 0 ? (
          <p className="projtags__empty">No calendars yet.</p>
        ) : (
          calendars.map((c) => (
            <div key={c.id} className="projtags__item" style={projectStyle(c.color)}>
              <ListRow
                className="projtags__row--always"
                lead={<ProjectDot color={c.color} />}
                title={c.name}
                meta={c.isPrimary ? "primary" : null}
                actions={
                  <label className="projtags__toggle" title={c.isPrimary ? "The primary calendar is always shown" : undefined}>
                    <input
                      type="checkbox"
                      checked={c.showOnTimeline || c.isPrimary}
                      disabled={c.isPrimary}
                      onChange={(e) => p.onSetShown(c.id, e.target.checked)}
                    />
                    Show on timeline
                  </label>
                }
              />
            </div>
          ))
        )}
      </div>

      <TagsCard tags={p.tags} error={p.tagsError} onSave={p.onSaveTag} onDelete={p.onDeleteTag} />
    </section>
  );
}

function TagsCard(props: {
  tags: Tag[];
  error: string | null;
  onSave: (name: string, color: TagColor) => Promise<string | null>;
  onDelete: (id: number) => Promise<string | null>;
}) {
  const [name, setName] = useState("");
  const [err, setErr] = useState<string | null>(null);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    const clean = name.trim().replace(/^#+/, "");
    if (!isValidTagName(clean)) {
      setErr("A tag is letters, numbers, - or _ (no spaces)");
      return;
    }
    const failed = await props.onSave(clean, "sun");
    setErr(failed);
    if (!failed) setName("");
  };

  const message = err ?? props.error;
  return (
    <div className="card card--flush">
      <header className="card__head">
        <h4 className="card__title">Tags</h4>
      </header>
      <form className="projtags__new" onSubmit={add} aria-label="New tag">
        <input
          className="field__input projtags__input"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="New tag"
          aria-label="New tag name"
        />
        <Button type="submit" icon={PlusIcon} disabled={name.trim() === ""}>
          Add
        </Button>
      </form>
      {message && (
        <div className="projtags__notice">
          <Banner tone="error" variant="inline">
            {message}
          </Banner>
        </div>
      )}
      {props.tags.length === 0 ? (
        <p className="projtags__empty">Tags you use in block titles (#tag) show up here.</p>
      ) : (
        props.tags.map((t) => <TagRow key={t.name.toLowerCase()} tag={t} onSave={props.onSave} onDelete={props.onDelete} />)
      )}
    </div>
  );
}

function TagRow({
  tag,
  onSave,
  onDelete,
}: {
  tag: Tag;
  onSave: (name: string, color: TagColor) => Promise<string | null>;
  onDelete: (id: number) => Promise<string | null>;
}) {
  const [err, setErr] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);
  const used = tag.goalCount > 0;
  return (
    <div className="projtags__item">
      <ListRow
        className={picking ? "projtags__row--open" : undefined}
        lead={
          <button
            type="button"
            className="projtags__dot"
            style={{ ["--tag" as string]: tagVar(tag.color) }}
            aria-label={`Color of ${tag.name}`}
            aria-expanded={picking}
            onClick={() => setPicking((v) => !v)}
          />
        }
        title={`#${tag.name}`}
        meta={
          <>
            {!tag.saved && "from notes"}
            {picking && (
              <span className="projtags__swatches" role="group" aria-label={`Color of ${tag.name}`}>
                {TAG_PALETTE.map((c) => (
                  <button
                    key={c}
                    type="button"
                    className="projtags__swatch"
                    style={{ ["--swatch" as string]: tagVar(c) }}
                    aria-label={c}
                    aria-pressed={tag.color === c && tag.saved}
                    onClick={async () => {
                      setErr(await onSave(tag.name, c));
                      setPicking(false);
                    }}
                  />
                ))}
              </span>
            )}
          </>
        }
        actions={
          tag.saved && tag.id !== null ? (
            <Button
              variant="ghost"
              size="sm"
              className="button--icon"
              icon={TrashIcon}
              aria-label={`Delete tag ${tag.name}`}
              disabled={used}
              title={used ? `Used by ${tag.goalCount} ${tag.goalCount === 1 ? "goal" : "goals"}` : "Delete tag"}
              onClick={async () => setErr(await onDelete(tag.id as number))}
            />
          ) : undefined
        }
      />
      {err && (
        <Banner tone="error" variant="inline">
          {err}
        </Banner>
      )}
    </div>
  );
}
