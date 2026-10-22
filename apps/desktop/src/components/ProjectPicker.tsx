import { useEffect, useRef, useState, type ReactNode } from "react";
import { CheckIcon, ChevronUpDownIcon } from "@heroicons/react/20/solid";
import type { Project } from "../api";
import { pickableProjects, projectStyle } from "../lib/projects";
import "../projects.css";

interface Props {
  projects: Project[];
  /** Selected project id; null = none. */
  value: number | null;
  onChange: (id: number | null) => void;
  /** When set, adds a first entry for "no project" with this label. */
  noneLabel?: string;
  /** Accessible name of the trigger button. */
  label: string;
  /** Replaces the trigger's content (the notes panel shows a chip). */
  trigger?: ReactNode;
  variant?: "field" | "chip";
  disabled?: boolean;
  /** Title of the menu (small caption above the options). */
  caption?: string;
}

export function ProjectDot({ color }: { color: string }) {
  return <i className="projpick__dot" style={projectStyle(color)} aria-hidden="true" />;
}

/** A button that opens a small list of projects (color dot + name). */
export function ProjectPicker({ projects, value, onChange, noneLabel, label, trigger, variant = "field", disabled, caption }: Props) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const options = pickableProjects(projects);
  const current = projects.find((p) => p.id === value) ?? null;
  // A goal can point at a calendar that has since left the user's list: keep it selectable.
  if (current && !options.some((p) => p.id === current.id)) options.push(current);

  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Close the list only; the page behind must stay.
      e.stopPropagation();
      setOpen(false);
    };
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  const pick = (id: number | null) => {
    setOpen(false);
    if (id !== value) onChange(id);
  };

  return (
    <div className={`projpick projpick--${variant}`} ref={ref}>
      <button
        type="button"
        className="projpick__trigger"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        {trigger ?? (
          <>
            {current ? <ProjectDot color={current.color} /> : null}
            <span className="projpick__name">{current ? current.name : (noneLabel ?? "Choose")}</span>
            <ChevronUpDownIcon className="projpick__chev" aria-hidden="true" />
          </>
        )}
      </button>
      {open && (
        <div className="projpick__pop" role="listbox" aria-label={label}>
          {caption && <p className="projpick__caption">{caption}</p>}
          {noneLabel !== undefined && (
            <button type="button" role="option" aria-selected={value === null} className="projpick__option" onClick={() => pick(null)}>
              <span className="projpick__name">{noneLabel}</span>
              {value === null && <CheckIcon className="projpick__check" aria-hidden="true" />}
            </button>
          )}
          {options.map((p) => (
            <button
              key={p.id}
              type="button"
              role="option"
              aria-selected={p.id === value}
              className="projpick__option"
              onClick={() => pick(p.id)}
            >
              <ProjectDot color={p.color} />
              <span className="projpick__name">{p.name}</span>
              {p.archived && <span className="projpick__tag">archived</span>}
              {!p.showOnTimeline && !p.archived && <span className="projpick__tag">hidden</span>}
              {p.id === value && <CheckIcon className="projpick__check" aria-hidden="true" />}
            </button>
          ))}
          {options.length === 0 && noneLabel === undefined && <p className="projpick__caption">No calendars yet.</p>}
        </div>
      )}
    </div>
  );
}
