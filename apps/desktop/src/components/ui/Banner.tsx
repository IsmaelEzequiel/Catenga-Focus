import type { ReactNode } from "react";
import { ClockIcon, ExclamationTriangleIcon, InformationCircleIcon, XMarkIcon } from "@heroicons/react/20/solid";
import "../../banner.css";

const ICONS = { error: ExclamationTriangleIcon, info: InformationCircleIcon, timer: ClockIcon } as const;

interface Props {
  tone?: keyof typeof ICONS;
  children: ReactNode;
  /** One or two buttons (use `Button size="sm"`). */
  actions?: ReactNode;
  onDismiss?: () => void;
  /** "inline": inside a page or panel (rounded); default: the full-width slot under the header. */
  variant?: "slot" | "inline";
}

/** Every banner looks the same: an icon, one sentence, an action, dismiss. It pushes content down, never overlaps it. */
export function Banner({ tone = "error", children, actions, onDismiss, variant = "slot" }: Props) {
  const Icon = ICONS[tone];
  return (
    <div className={`banner banner--${tone} banner--${variant}`} role={tone === "error" ? "alert" : "status"}>
      <Icon className="banner__icon" aria-hidden="true" />
      <span className="banner__text">{children}</span>
      {actions && <span className="banner__actions">{actions}</span>}
      {onDismiss && (
        <button type="button" className="banner__dismiss" onClick={onDismiss} aria-label="Dismiss" title="Dismiss">
          <XMarkIcon className="banner__dismiss-icon" aria-hidden="true" />
        </button>
      )}
    </div>
  );
}
