import { useState, type FormEvent } from "react";
import { ArchiveBoxIcon, ArchiveBoxXMarkIcon, ArrowPathIcon, PlusIcon, TrashIcon } from "@heroicons/react/20/solid";
import type { Calendar, GoogleStatus, Project, Tag, TagColor } from "../api";
import { canReconnect, listProjects, projectHint, projectStyle, reconnectHint } from "../lib/projects";
import { isValidTagName, TAG_PALETTE, tagVar } from "../lib/tags";
import { ProjectDot } from "./ProjectPicker";
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

/** Projects (repeating events), the calendars that feed the timeline, and the tag list with colors. */
export function ProjectsTags(p: Props) {
  const hint = reconnectHint(p.google);
  const projects = listProjects(p.projects);
  const calendars = p.calendars.filter((c) => !c.archived);
  return (
    <section className="projtags" aria-label="Projects, calendars and tags">
      <h3 className="dash__h projtags__h">Projects, calendars &amp; tags</h3>

      <div className="projtags__card">
        <header className="projtags__head">
          <h4 className="projtags__sub">Projects</h4>
          {p.google?.connected && !canReconnect(p.google) && (
            <button className="app__btn" onClick={p.onRefresh} disabled={p.refreshing}>
              <ArrowPathIcon className={`app__icon${p.refreshing ? " projtags__spin" : ""}`} aria-hidden="true" />
              Refresh
            </button>
          )}
        </header>
        <p className="projtags__text">
          A repeating event is a project: all its occurrences, and the blocks and timer sessions made from them, belong to it.
        </p>
        {hint && (
          <p className="projtags__hint" role="status">
            {hint}
            {p.google?.configured && (
              <button className="app__btn projtags__hintbtn" onClick={p.onConnect}>
                {p.google.connected ? "Reconnect" : "Connect Google Calendar"}
              </button>
            )}
          </p>
        )}
        {p.projectsError && <p className="dash__error">{p.projectsError}</p>}
        {projects.length === 0 && !hint && <p className="projtags__text">Repeating events from your calendar show up here.</p>}
        {projects.length > 0 && (
          <ul className="projtags__list">
            {projects.map((x) => (
              <li key={x.id} className="projtags__row" style={projectStyle(x.color)}>
                <ProjectDot color={x.color} />
                <span className={`projtags__name${x.archived ? " projtags__name--off" : ""}`}>{x.name}</span>
                <span className="projtags__hintnote">{projectHint(x)}</span>
                {x.archived && <span className="projtags__badge">archived</span>}
                <button
                  type="button"
                  className="app__btn projtags__archive"
                  onClick={() => p.onSetArchived(x.id, !x.archived)}
                  aria-label={`${x.archived ? "Restore" : "Archive"} ${x.name}`}
                >
                  {x.archived ? (
                    <ArchiveBoxXMarkIcon className="app__icon" aria-hidden="true" />
                  ) : (
                    <ArchiveBoxIcon className="app__icon" aria-hidden="true" />
                  )}
                  {x.archived ? "Restore" : "Archive"}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="projtags__card">
        <header className="projtags__head">
          <h4 className="projtags__sub">Calendars</h4>
        </header>
        <p className="projtags__text">Calendars only decide what shows on the timeline.</p>
        {calendars.length === 0 ? (
          <p className="projtags__text">No calendars yet.</p>
        ) : (
          <ul className="projtags__list">
            {calendars.map((c) => (
              <li key={c.id} className="projtags__row" style={projectStyle(c.color)}>
                <ProjectDot color={c.color} />
                <span className="projtags__name">{c.name}</span>
                {c.isPrimary && <span className="projtags__badge">primary</span>}
                <label className="projtags__toggle" title={c.isPrimary ? "The primary calendar is always shown" : undefined}>
                  <input
                    type="checkbox"
                    checked={c.showOnTimeline || c.isPrimary}
                    disabled={c.isPrimary}
                    onChange={(e) => p.onSetShown(c.id, e.target.checked)}
                  />
                  Show on timeline
                </label>
              </li>
            ))}
          </ul>
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

  return (
    <div className="projtags__card">
      <header className="projtags__head">
        <h4 className="projtags__sub">Tags</h4>
      </header>
      <form className="projtags__new" onSubmit={add} aria-label="New tag">
        <input
          className="projtags__input"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="New tag"
          aria-label="New tag name"
        />
        <button type="submit" className="app__btn" disabled={name.trim() === ""}>
          <PlusIcon className="app__icon" aria-hidden="true" />
          Add
        </button>
      </form>
      {(err || props.error) && <p className="dash__error">{err ?? props.error}</p>}
      {props.tags.length === 0 ? (
        <p className="projtags__text">Tags you use in block titles (#tag) show up here.</p>
      ) : (
        <ul className="projtags__list">
          {props.tags.map((t) => (
            <TagRow key={t.name.toLowerCase()} tag={t} onSave={props.onSave} onDelete={props.onDelete} />
          ))}
        </ul>
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
  const used = tag.goalCount > 0;
  return (
    <li className="projtags__row" style={{ ["--chip" as string]: tagVar(tag.color) }}>
      <span className="projtags__tag">#{tag.name}</span>
      {!tag.saved && <span className="projtags__badge">from notes</span>}
      <span className="projtags__swatches" role="group" aria-label={`Color of ${tag.name}`}>
        {TAG_PALETTE.map((c) => (
          <button
            key={c}
            type="button"
            className="projtags__swatch"
            style={{ ["--swatch" as string]: tagVar(c) }}
            aria-label={c}
            aria-pressed={tag.color === c && tag.saved}
            onClick={async () => setErr(await onSave(tag.name, c))}
          />
        ))}
      </span>
      {tag.saved && tag.id !== null ? (
        <button
          type="button"
          className="projtags__delete"
          aria-label={`Delete tag ${tag.name}`}
          disabled={used}
          title={used ? `Used by ${tag.goalCount} ${tag.goalCount === 1 ? "goal" : "goals"}` : "Delete tag"}
          onClick={async () => setErr(await onDelete(tag.id as number))}
        >
          <TrashIcon aria-hidden="true" />
        </button>
      ) : (
        <span className="projtags__delete projtags__delete--none" aria-hidden="true" />
      )}
      {err && <p className="dash__error projtags__err">{err}</p>}
    </li>
  );
}
