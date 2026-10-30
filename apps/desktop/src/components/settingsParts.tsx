import type { ReactNode } from "react";
import { Banner } from "./ui/Banner";
import "../settings.css";

export function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: ReactNode;
}) {
  return (
    <section className="settings__section" aria-labelledby={id}>
      <h3 id={id} className="settings__heading">
        {title}
      </h3>
      <div className="settings__rows">{children}</div>
    </section>
  );
}

/** label on the left; value and small actions on the right. */
export function Row({
  label,
  hint,
  value,
  status,
  children,
}: {
  label: string;
  hint?: string;
  value?: string;
  status?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className="settings__row">
      <div className="settings__main">
        <span className="settings__label">{label}</span>
        {hint && (
          <span className="settings__hint" role={status ? "status" : undefined}>
            {hint}
          </span>
        )}
      </div>
      {(value || children) && (
        <div className="settings__side">
          {value && (
            <span
              className="settings__value"
              role={status ? "status" : undefined}
            >
              {value}
            </span>
          )}
          {children && <div className="settings__actions">{children}</div>}
        </div>
      )}
    </div>
  );
}

/** A friendly sentence with the raw error kept as a tooltip, plus a retry action. */
export function ErrorBanner({
  text,
  detail,
  action,
}: {
  text: string;
  detail: string;
  action: ReactNode;
}) {
  return (
    <Banner tone="error" variant="inline" actions={action}>
      <span title={detail}>{text}</span>
    </Banner>
  );
}
