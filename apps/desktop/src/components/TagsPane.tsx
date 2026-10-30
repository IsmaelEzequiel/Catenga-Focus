import { useState, type FormEvent } from "react";
import { PlusIcon, TrashIcon } from "@heroicons/react/20/solid";
import type { Tag, TagColor } from "../api";
import { isValidTagName, TAG_PALETTE, tagVar } from "../lib/tags";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import { ListRow } from "./ui/ListRow";
import "../card.css";
import "../field.css";
import "../projects.css";

export function TagsPane(props: {
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
        <p className="card__desc">Tags you use in block titles (#tag), with their colors.</p>
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
