import type { ReactNode } from "react";
import "../../stat.css";

/** One stat tile for every page (Goals, Dashboard, Recap): sentence-case label, display number, optional sub line. */
export function StatTile({ label, value, sub }: { label: string; value: ReactNode; sub?: ReactNode }) {
  return (
    <div className="stat">
      <span className="stat__label">{label}</span>
      <span className="stat__value">{value}</span>
      {sub !== undefined && sub !== null && <span className="stat__sub">{sub}</span>}
    </div>
  );
}
