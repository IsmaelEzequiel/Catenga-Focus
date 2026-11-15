import { CalendarIcon } from "@heroicons/react/20/solid";
import { useState } from "react";
import type { Calendar, GoogleStatus, Reminder } from "../api";
import { calendarGroups, newEventGroups } from "../lib/calendars";
import { MAX_REMINDER_MINUTES, reminderMinutes, reminderOfKind } from "../lib/reminders";
import { canReconnect, projectStyle } from "../lib/projects";
import { ProjectDot } from "./ProjectPicker";
import { ErrorBanner, Row } from "./settingsParts";
import { Banner } from "./ui/Banner";
import { Button } from "./ui/Button";
import { ListRow } from "./ui/ListRow";
import "../card.css";
import "../field.css";
import "../projects.css";

interface Props {
  status: GoogleStatus | null;
  connecting: boolean;
  error: string | null;
  calendars: Calendar[];
  onConnect: () => void;
  onCancel: () => void;
  onDisconnect: (email: string) => void;
  onSetNewEventCalendar: (googleCalendarId: string) => void;
  onSetNewEventReminder: (reminder: Reminder) => void;
  onSetShown: (id: number, shown: boolean) => void;
}

/** The notifications of events the app creates: Google's defaults, none, or one notification N minutes before. */
function ReminderField({ reminder, onChange }: { reminder: Reminder; onChange: (reminder: Reminder) => void }) {
  // What is being typed; saved as soon as it is a valid number of minutes.
  const [minutes, setMinutes] = useState(reminder.kind === "popup" ? String(reminder.minutes) : "");
  const pick = (kind: string) => {
    const next = reminderOfKind(kind, reminder);
    if (next.kind === "popup") setMinutes(String(next.minutes));
    onChange(next);
  };
  const type = (text: string) => {
    setMinutes(text);
    const n = reminderMinutes(text);
    if (n !== null) onChange({ kind: "popup", minutes: n });
  };
  return (
    <div className="field projtags__field">
      <label className="field__label" htmlFor="settings-new-event-reminder">
        Notifications for new events
      </label>
      <div className="projtags__reminder">
        <select
          id="settings-new-event-reminder"
          className="field__input"
          value={reminder.kind}
          onChange={(e) => pick(e.target.value)}
        >
          <option value="default">Google's default</option>
          <option value="none">None</option>
          <option value="popup">Notification before it starts</option>
        </select>
        {reminder.kind === "popup" && (
          <>
            <input
              className="field__input projtags__minutes"
              type="number"
              min={0}
              max={MAX_REMINDER_MINUTES}
              value={minutes}
              onChange={(e) => type(e.target.value)}
              aria-label="Minutes before the event"
            />
            <span className="field__hint">minutes before</span>
          </>
        )}
      </div>
      <span className="field__hint">Applies to events created here from now on.</span>
    </div>
  );
}

/** Google accounts (several), where new blocks go, their notifications, and the calendars that feed the timeline, grouped by account. */
export function CalendarsPane(p: Props) {
  const status = p.status;
  if (!status) return null;
  if (!status.configured) {
    return <Row label="Google Calendar" value="Not available in this build" />;
  }
  const retry = (
    <Button size="sm" onClick={p.onConnect}>
      Try again
    </Button>
  );
  const groups = calendarGroups(p.calendars, status.accounts);
  const options = newEventGroups(p.calendars, status.accounts);
  return (
    <div className="projtags">
      {status.accounts.map((a) => (
        <Row key={a.email} label={a.email} hint={a.needsReconnect ? "Needs to be reconnected" : "Google account"}>
          {a.needsReconnect && (
            <Button size="sm" icon={CalendarIcon} onClick={p.onConnect} disabled={p.connecting}>
              Reconnect
            </Button>
          )}
          <Button size="sm" onClick={() => p.onDisconnect(a.email)} aria-label={`Disconnect ${a.email}`}>
            Disconnect
          </Button>
        </Row>
      ))}
      {p.connecting ? (
        <Row label="Google Calendar" value="Waiting for Google in your browser…" status>
          <Button size="sm" onClick={p.onCancel}>
            Cancel
          </Button>
        </Row>
      ) : (
        <Row
          label={status.connected ? "Add another account" : "Google Calendar"}
          hint={status.connected ? undefined : "Not connected"}
        >
          <Button size="sm" icon={CalendarIcon} onClick={p.onConnect}>
            {status.connected ? "Add account" : "Connect Google Calendar"}
          </Button>
        </Row>
      )}
      {canReconnect(status) && (
        <Banner tone="info" variant="inline">
          Reconnect Google Calendar to load your calendars and repeating events.
        </Banner>
      )}
      {p.error && (
        <ErrorBanner
          text={status.connected ? "Google Calendar had a problem." : "Couldn't connect to Google Calendar."}
          detail={p.error}
          action={retry}
        />
      )}

      {options.length > 0 && (
        <div className="field projtags__field">
          <label className="field__label" htmlFor="settings-new-event-calendar">
            New blocks go to
          </label>
          <select
            id="settings-new-event-calendar"
            className="field__input"
            value={status.newEventCalendarId ?? ""}
            onChange={(e) => p.onSetNewEventCalendar(e.target.value)}
          >
            {status.newEventCalendarId === null && <option value="">Primary calendar</option>}
            {options.map((g) => (
              <optgroup key={g.account} label={g.account ?? ""}>
                {g.calendars.map((c) => (
                  <option key={c.id} value={c.googleCalendarId}>
                    {c.name}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
        </div>
      )}

      {status.connected && <ReminderField reminder={status.newEventReminder} onChange={p.onSetNewEventReminder} />}

      <div className="card card--flush">
        <header className="card__head">
          <h4 className="card__title">Calendars</h4>
          <p className="card__desc">Calendars only decide what shows on the timeline.</p>
        </header>
        {groups.length === 0 ? (
          <p className="projtags__empty">No calendars yet.</p>
        ) : (
          groups.map((g) => (
            <div key={g.account ?? "unknown"}>
              <div className="projtags__group">{g.account ?? "Other"}</div>
              {g.calendars.map((c) => (
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
              ))}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
