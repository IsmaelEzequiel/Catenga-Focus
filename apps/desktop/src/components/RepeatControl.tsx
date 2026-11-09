import { useState } from "react";
import { ArrowPathIcon } from "@heroicons/react/20/solid";
import { WEEKDAYS, defaultRepeat, describeRepeat, repeatProblem, repeatRequest, toggleDay, type Repeat, type RepeatFreq } from "../lib/repeat";
import { Button } from "./ui/Button";
import "../repeat.css";
import "../segmented.css";
import "../field.css";

/** What the notes panel needs to offer "Repeat" on the selected block's Google event. */
export interface RepeatPanel {
  /** "none": no Google event; "series": it already repeats; "one-off": it can be made to repeat. */
  state: "none" | "series" | "one-off";
  /** The block's day (`YYYY-MM-DD`): the first occurrence. */
  date: string;
  /** Resolves to null when done, else the error to show. */
  onRepeat: (repeat: Repeat) => Promise<string | null>;
}

const FREQS: Array<{ value: RepeatFreq; label: string }> = [
  { value: "daily", label: "Daily" },
  { value: "weekdays", label: "Weekdays" },
  { value: "weekly", label: "Weekly" },
];

/** "Repeats" (read-only) for an occurrence of a series; "Repeat" opening a small form for a one-off event. */
export function RepeatControl({ panel }: { panel: RepeatPanel }) {
  const [draft, setDraft] = useState<Repeat | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (panel.state === "none") return null;
  if (panel.state === "series") {
    return (
      <span className="repeatchip" title="A repeating Google Calendar event. Change the series in Google Calendar.">
        <ArrowPathIcon className="repeatchip__icon" aria-hidden="true" />
        Repeats
      </span>
    );
  }

  if (!draft) {
    return (
      <Button size="sm" variant="ghost" icon={ArrowPathIcon} className="repeatchip__open" onClick={() => setDraft(defaultRepeat(panel.date))}>
        Repeat
      </Button>
    );
  }

  const problem = repeatProblem(draft, panel.date);
  const close = () => {
    setDraft(null);
    setError(null);
  };
  const save = async () => {
    if (problem || busy) return;
    setBusy(true);
    setError(null);
    const err = await panel.onRepeat(repeatRequest(draft));
    setBusy(false);
    if (err) setError(`Couldn't make it repeat: ${err}`);
    else close();
  };

  return (
    <div
      className="repeat"
      role="group"
      aria-label="Repeat this event"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          close();
        }
      }}
    >
      <div className="repeat__row">
        <span className="repeat__label">Repeat</span>
        <div className="seg seg--sm" role="group" aria-label="How often">
          {FREQS.map((f) => (
            <button key={f.value} type="button" className="seg__item" aria-pressed={draft.freq === f.value} onClick={() => setDraft({ ...draft, freq: f.value })}>
              {f.label}
            </button>
          ))}
        </div>
      </div>
      {draft.freq === "weekly" && (
        <div className="repeat__row">
          <span className="repeat__label">On</span>
          <div className="repeat__days" role="group" aria-label="Days of the week">
            {WEEKDAYS.map((d, i) => (
              <button
                key={d.name}
                type="button"
                className="repeat__day"
                aria-label={d.name}
                title={d.name}
                aria-pressed={draft.days.includes(i)}
                onClick={() => setDraft({ ...draft, days: toggleDay(draft.days, i) })}
              >
                {d.short}
              </button>
            ))}
          </div>
        </div>
      )}
      <div className="repeat__row">
        <span className="repeat__label">Ends</span>
        <div className="seg seg--sm" role="group" aria-label="Ends">
          <button type="button" className="seg__item" aria-pressed={draft.until === null} onClick={() => setDraft({ ...draft, until: null })}>
            Never
          </button>
          <button type="button" className="seg__item" aria-pressed={draft.until !== null} onClick={() => setDraft({ ...draft, until: draft.until ?? "" })}>
            On date
          </button>
        </div>
        {draft.until !== null && (
          <input
            type="date"
            className="field__input repeat__date"
            aria-label="Last day"
            min={panel.date}
            value={draft.until}
            onChange={(e) => setDraft({ ...draft, until: e.target.value })}
          />
        )}
      </div>
      <p className={error ? "repeat__note repeat__note--error" : "repeat__note"} role={error ? "alert" : undefined}>
        {error ?? problem ?? describeRepeat(draft)}
      </p>
      <div className="repeat__actions">
        <Button size="sm" variant="ghost" onClick={close}>
          Cancel
        </Button>
        <Button size="sm" disabled={problem !== null || busy} onClick={() => void save()}>
          {busy ? "Saving…" : "Make it repeat"}
        </Button>
      </div>
    </div>
  );
}
