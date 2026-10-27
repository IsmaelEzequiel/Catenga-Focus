import type { ReactNode } from "react";
import "../../row.css";

interface Props {
  lead?: ReactNode;
  title: ReactNode;
  meta?: ReactNode;
  /** Shown on hover or keyboard focus (always on touch screens). */
  actions?: ReactNode;
  onClick?: () => void;
  className?: string;
}

/** One list row for Projects, Calendars, Tags, Recap and Settings. Render inside a `<ul>`/`<div>` container. */
export function ListRow({ lead, title, meta, actions, onClick, className }: Props) {
  const classes = ["row", onClick ? "row--link" : "", className ?? ""].filter(Boolean).join(" ");
  return (
    <div
      className={classes}
      onClick={onClick}
      onKeyDown={
        onClick
          ? (e) => {
              if (e.target !== e.currentTarget || (e.key !== "Enter" && e.key !== " ")) return;
              e.preventDefault();
              onClick();
            }
          : undefined
      }
      role={onClick ? "button" : undefined}
      tabIndex={onClick ? 0 : undefined}
    >
      {lead !== undefined && <span className="row__lead">{lead}</span>}
      <span className="row__title">{title}</span>
      {meta !== undefined && meta !== null && <span className="row__meta">{meta}</span>}
      {actions !== undefined && <span className="row__actions">{actions}</span>}
    </div>
  );
}
