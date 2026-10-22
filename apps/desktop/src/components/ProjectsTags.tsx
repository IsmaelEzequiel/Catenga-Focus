import { useState, type FormEvent } from "react";
import { ArrowPathIcon, PlusIcon, TrashIcon } from "@heroicons/react/20/solid";
import type { GoogleStatus, Project, Tag, TagColor } from "../api";
import { canReconnect, projectStyle, reconnectHint } from "../lib/projects";
import { isValidTagName, TAG_PALETTE, tagVar } from "../lib/tags";
import { ProjectDot } from "./ProjectPicker";
import "../projects.css";

interface Props {
  projects: Project[];
  projectsError: string | null;
  refreshing: boolean;
  google: GoogleStatus | null;
  onRefresh: () => void;
  onConnect: () => void;
  onSetShown: (id: number, shown: boolean) => void;
  tags: Tag[];
  tagsError: string | null;
  onSaveTag: (name: string, color: TagColor) => Promise<string | null>;
  onDeleteTag: (id: number) => Promise<string | null>;
}

/** Projects (Google calendars) with their timeline toggle, and the tag list with colors. */
export function ProjectsTags(p: Props) {
  const hint = reconnectHint(p.google);
  const shown = p.projects.filter((x) => !x.archived);
  return (
    <section className="projtags" aria-label="Projects and tags">
      <h3 className="dash__h projtags__h">Projects &amp; tags</h3>

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
        <p className="projtags__text">Each Google calendar is a project. Its events, and the blocks and timer sessions made from them, belong to it.</p>
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
        {shown.length === 0 && !hint && <p className="projtags__text">No calendars yet.</p>}
        {shown.length > 0 && (
          <ul className="projtags__list">
            {shown.map((x) => (
              <li key={x.id} className="projtags__row" style={projectStyle(x.color)}>
                <ProjectDot color={x.color} />
                <span className="projtags__name">{x.name}</span>
                {x.isPrimary && <span className="projtags__badge">primary</span>}
                <label className="projtags__toggle" title={x.isPrimary ? "The primary calendar is always shown" : undefined}>
                  <input
                    type="checkbox"
                    checked={x.showOnTimeline || x.isPrimary}
                    disabled={x.isPrimary}
                    onChange={(e) => p.onSetShown(x.id, e.target.checked)}
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
