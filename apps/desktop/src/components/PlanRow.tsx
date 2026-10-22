import { useState } from "react";
import { MinusIcon, PlusIcon } from "@heroicons/react/20/solid";
import type { PomodoroPlan } from "../api";
import { PLAN_LIMITS, parseCount } from "../lib/focus";

interface StepperProps {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
}

/** − [n] +, typable; the typed text is only normalised on blur so partial input isn't fought. */
function Stepper({ label, value, min, max, onChange }: StepperProps) {
  const [draft, setDraft] = useState<string | null>(null);
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  const commit = (text: string) => {
    const n = parseCount(text);
    if (n !== null) onChange(clamp(n));
  };
  return (
    <span className="stepper" role="group" aria-label={label}>
      <button type="button" className="stepper__btn" aria-label={`Less ${label}`} disabled={value <= min} onClick={() => onChange(clamp(value - 1))}>
        <MinusIcon className="stepper__icon" aria-hidden="true" />
      </button>
      <input
        className="stepper__input"
        inputMode="numeric"
        aria-label={label}
        value={draft ?? String(value)}
        onChange={(e) => {
          setDraft(e.target.value);
          commit(e.target.value);
        }}
        onFocus={(e) => e.currentTarget.select()}
        onBlur={() => setDraft(null)}
        onKeyDown={(e) => {
          if (e.key === "ArrowUp" || e.key === "ArrowDown") {
            e.preventDefault();
            setDraft(null);
            onChange(clamp(value + (e.key === "ArrowUp" ? 1 : -1)));
          }
        }}
      />
      <button type="button" className="stepper__btn" aria-label={`More ${label}`} disabled={value >= max} onClick={() => onChange(clamp(value + 1))}>
        <PlusIcon className="stepper__icon" aria-hidden="true" />
      </button>
    </span>
  );
}

interface Props {
  plan: PomodoroPlan;
  onChange: (plan: PomodoroPlan) => void;
  /** Larger type, for the focus screen. */
  large?: boolean;
}

/** `[4] × [25] min focus · [5] min break` */
export function PlanRow({ plan, onChange, large }: Props) {
  return (
    <div className={large ? "plan plan--large" : "plan"} aria-label="Pomodoro plan">
      <Stepper label="pomodoros" value={plan.cycles} {...PLAN_LIMITS.cycles} onChange={(cycles) => onChange({ ...plan, cycles })} />
      <span className="plan__text">×</span>
      <Stepper
        label="focus minutes"
        value={plan.focusMinutes}
        {...PLAN_LIMITS.focusMinutes}
        onChange={(focusMinutes) => onChange({ ...plan, focusMinutes })}
      />
      <span className="plan__text">min focus ·</span>
      <Stepper
        label="break minutes"
        value={plan.breakMinutes}
        {...PLAN_LIMITS.breakMinutes}
        onChange={(breakMinutes) => onChange({ ...plan, breakMinutes })}
      />
      <span className="plan__text">min break</span>
    </div>
  );
}
