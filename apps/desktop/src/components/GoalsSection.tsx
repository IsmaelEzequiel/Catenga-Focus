import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { CheckIcon, EllipsisHorizontalIcon, FireIcon, PlusIcon } from "@heroicons/react/20/solid";
import type { GoalDraft, GoalProgress, Project, Tag, TimerStatus } from "../api";
import { useGoals } from "../hooks/useGoals";
import { fmtDuration } from "../lib/dashboard";
import {
  applyLive,
  buildDraft,
  cellFill,
  fromMinutes,
  goalSummary,
  goalSubjectText,
  groupGoals,
  liveFocusMinutes,
  matchesGoal,
  ringFraction,
} from "../lib/goals";
import { projectStyle } from "../lib/projects";
import { tagVar } from "../lib/tags";
import { toDateString } from "../lib/time";
import { ProjectDot, ProjectPicker } from "./ProjectPicker";
import { TagInput } from "./TagInput";
import "../dashboard.css";
import "../goals.css";

interface Props {
  projects: Project[];
  /** Saved tags plus the ones seen in notes, for the tag input. */
  tags: Tag[];
  timerStatus: TimerStatus;
  nowMs: number;
}

const PRESETS = [30, 60, 120];
const RING_R = 24;
const RING_C = 2 * Math.PI * RING_R;

type FormState = { goal: GoalProgress | null } | null;

export function GoalsSection({ projects, tags, timerStatus, nowMs }: Props) {
  const g = useGoals();
  const [form, setForm] = useState<FormState>(null);
  const [showArchived, setShowArchived] = useState(false);
  const [open, setOpen] = useState<{ upcoming: boolean; past: boolean }>({ upcoming: false, past: false });

  const today = toDateString(new Date(nowMs));
  const live = liveFocusMinutes(timerStatus, nowMs);
  const runningBlock = timerStatus.state === "running" && timerStatus.phase === "focus" ? timerStatus.block : null;
  const view = (p: GoalProgress) =>
    runningBlock && timerStatus.state === "running" && timerStatus.date === today && matchesGoal(p, runningBlock)
      ? applyLive(p, live, today)
      : p;
  const groups = useMemo(() => groupGoals(g.list ?? []), [g.list]);
  const total = (g.list ?? []).length;

  const card = (p: GoalProgress) => (
    <GoalCard
      key={p.goal.id}
      p={view(p)}
      live={view(p) !== p}
      today={today}
      onEdit={() => setForm({ goal: p })}
      onArchive={() => void g.setArchived(p.goal.id, true)}
      onDelete={() => g.remove(p.goal.id)}
    />
  );

  return (
    <section className="goals" aria-label="Goals">
      <header className="goals__head">
        <h3 className="dash__h goals__title">Your goals</h3>
        {!form && total > 0 && (
          <button className="app__btn" onClick={() => setForm({ goal: null })}>
            <PlusIcon className="app__icon" aria-hidden="true" />
            New goal
          </button>
        )}
      </header>

      {g.error && <p className="dash__error">{g.error}</p>}

      {form && (
        <GoalForm
          key={form.goal?.goal.id ?? "new"}
          p={form.goal}
          projects={projects}
          tags={tags}
          today={today}
          onCancel={() => setForm(null)}
          onSave={async (draft) => {
            const err = await (form.goal ? g.update(form.goal.goal.id, draft) : g.create(draft));
            if (!err) setForm(null);
            return err;
          }}
        />
      )}

      {g.list !== null && total === 0 && !form && (
        <div className="goals__empty">
          <p>Set a goal — e.g. Mestrado, 1h a day for 30 days.</p>
          <button className="app__btn goals__primary" onClick={() => setForm({ goal: null })}>
            <PlusIcon className="app__icon" aria-hidden="true" />
            New goal
          </button>
        </div>
      )}

      {groups.active.length > 0 && <ul className="goals__list">{groups.active.map((p) => <li key={p.goal.id}>{card(p)}</li>)}</ul>}

      {(["upcoming", "past"] as const).map((k) =>
        groups[k].length === 0 ? null : (
          <div key={k} className="goals__group">
            <button
              className="goals__toggle"
              aria-expanded={open[k]}
              onClick={() => setOpen((o) => ({ ...o, [k]: !o[k] }))}
            >
              {k === "upcoming" ? "Upcoming" : "Past goals"} <span>{groups[k].length}</span>
            </button>
            {open[k] && <ul className="goals__list">{groups[k].map((p) => <li key={p.goal.id}>{card(p)}</li>)}</ul>}
          </div>
        ),
      )}

      {g.archived.length > 0 && (
        <div className="goals__group">
          <button className="goals__toggle" aria-expanded={showArchived} onClick={() => setShowArchived((v) => !v)}>
            Archived <span>{g.archived.length}</span>
          </button>
          {showArchived && (
            <ul className="goals__list">
              {g.archived.map((p) => (
                <li key={p.goal.id}>
                  <ArchivedRow
                    p={p}
                    onRestore={() => void g.setArchived(p.goal.id, false)}
                    onDelete={() => g.remove(p.goal.id)}
                  />
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </section>
  );
}

/** What a goal counts: project (color dot + name), tag chips and/or a title. */
function GoalSubject({ p }: { p: GoalProgress }) {
  const { goal, project, tags } = p;
  const none = !project && tags.length === 0 && !goal.title;
  return (
    <span className="goal__subjects" aria-label={goalSubjectText(p)}>
      {project && (
        <span className="goal__project" style={projectStyle(project.color)}>
          <ProjectDot color={project.color} />
          {project.name}
        </span>
      )}
      {tags.map((t) => (
        <span key={t.name} className="goal__chip" style={{ ["--chip" as string]: tagVar(t.color) }}>
          #{t.name}
        </span>
      ))}
      {goal.title && <span className="goal__subject">{goal.title}</span>}
      {none && <span className="goal__subject goal__subject--none">Goal</span>}
    </span>
  );
}

function Ring({ minutes, target }: { minutes: number; target: number }) {
  const met = minutes >= target;
  const frac = ringFraction(minutes, target);
  return (
    <span className="goalring" data-met={met} role="img" aria-label={`${fmtDuration(minutes)} of ${fmtDuration(target)} today`}>
      <svg viewBox="0 0 56 56" width="56" height="56" aria-hidden="true">
        <circle className="goalring__track" cx="28" cy="28" r={RING_R} />
        <circle
          className="goalring__arc"
          cx="28"
          cy="28"
          r={RING_R}
          strokeDasharray={RING_C}
          strokeDashoffset={RING_C * (1 - frac)}
          transform="rotate(-90 28 28)"
        />
      </svg>
      <span className="goalring__center">
        {met ? <CheckIcon className="goalring__check" aria-hidden="true" /> : <b>{Math.round(minutes)}</b>}
      </span>
    </span>
  );
}

function GoalCard(props: {
  p: GoalProgress;
  live: boolean;
  today: string;
  onEdit: () => void;
  onArchive: () => void;
  onDelete: () => Promise<string | null>;
}) {
  const { p, today } = props;
  const target = p.goal.minutesPerDay;
  const active = p.status === "active";
  const [confirm, setConfirm] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  return (
    <article className="goal" data-status={p.status}>
      {active ? <Ring minutes={p.todayMinutes} target={target} /> : <span className="goalring goalring--idle" aria-hidden="true" />}
      <div className="goal__body">
        <div className="goal__top">
          <GoalSubject p={p} />
          {props.live && <span className="goal__live" title="A timer is running for this goal">live</span>}
        </div>
        <p className="goal__meta">{goalSummary(p)}</p>
        {active && (
          <p className="goal__today">
            {Math.round(p.todayMinutes)} / {fmtDuration(target)} today
          </p>
        )}
      </div>
      {confirm ? (
        <div className="goal__confirm" role="group" aria-label="Confirm delete">
          <span>Delete this goal?</span>
          <button
            className="app__btn goal__danger"
            onClick={async () => {
              const e = await props.onDelete();
              if (e) setErr(e);
            }}
          >
            Delete
          </button>
          <button className="app__btn" onClick={() => setConfirm(false)}>
            Keep
          </button>
        </div>
      ) : (
        <GoalMenu onEdit={props.onEdit} onArchive={props.onArchive} onDelete={() => setConfirm(true)} />
      )}
      {err && <p className="dash__error goal__err">{err}</p>}
      <div className="goal__stats">
        <span>
          Met {p.metDays} of {p.elapsedDays} {p.elapsedDays === 1 ? "day" : "days"}
        </span>
        <span className="goal__streak" data-hot={p.currentStreak > 0}>
          <FireIcon className="goal__fire" aria-hidden="true" />
          <b>{p.currentStreak}</b> <span className="goal__best">best {p.longestStreak}</span>
        </span>
      </div>
      {p.last14.length > 0 && (
        <ol className="goalstrip" aria-label="Last 14 days">
          {p.last14.map((d) => {
            const c = cellFill(d, target);
            return (
              <li
                key={d.date}
                className={`goalstrip__cell${d.date === today ? " goalstrip__cell--today" : ""}`}
                data-state={c.state}
                title={`${d.date} · ${fmtDuration(d.minutes)}`}
                aria-label={`${d.date}, ${fmtDuration(d.minutes)}${d.met ? ", met" : ""}`}
              >
                <i style={{ height: `${c.fill * 100}%` }} />
              </li>
            );
          })}
        </ol>
      )}
    </article>
  );
}

function GoalMenu({ onEdit, onArchive, onDelete }: { onEdit: () => void; onArchive: () => void; onDelete: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  const item = (label: string, fn: () => void, danger = false) => (
    <button
      role="menuitem"
      className={`goal__item${danger ? " goal__item--danger" : ""}`}
      onClick={() => {
        setOpen(false);
        fn();
      }}
    >
      {label}
    </button>
  );
  return (
    <div className="goal__menu" ref={ref}>
      <button className="goal__kebab" aria-label="Goal actions" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <EllipsisHorizontalIcon aria-hidden="true" />
      </button>
      {open && (
        <div className="goal__pop" role="menu">
          {item("Edit", onEdit)}
          {item("Archive", onArchive)}
          {item("Delete", onDelete, true)}
        </div>
      )}
    </div>
  );
}

function ArchivedRow(props: {
  p: GoalProgress;
  onRestore: () => void;
  onDelete: () => Promise<string | null>;
}) {
  const [confirm, setConfirm] = useState(false);
  return (
    <div className="goal goal--row">
      <div className="goal__body">
        <GoalSubject p={props.p} />
        <p className="goal__meta">{goalSummary(props.p)}</p>
      </div>
      {confirm ? (
        <div className="goal__confirm" role="group" aria-label="Confirm delete">
          <span>Delete this goal?</span>
          <button className="app__btn goal__danger" onClick={() => void props.onDelete()}>
            Delete
          </button>
          <button className="app__btn" onClick={() => setConfirm(false)}>
            Keep
          </button>
        </div>
      ) : (
        <div className="goal__row-actions">
          <button className="app__btn" onClick={props.onRestore}>
            Restore
          </button>
          <button className="app__btn" onClick={() => setConfirm(true)}>
            Delete
          </button>
        </div>
      )}
    </div>
  );
}

function GoalForm(props: {
  p: GoalProgress | null;
  projects: Project[];
  tags: Tag[];
  today: string;
  onCancel: () => void;
  onSave: (draft: GoalDraft) => Promise<string | null>;
}) {
  const goal = props.p?.goal ?? null;
  const init = fromMinutes(goal?.minutesPerDay ?? 60);
  const [projectId, setProjectId] = useState<number | null>(goal?.projectId ?? null);
  const [tagNames, setTagNames] = useState<string[]>(props.p?.tags.map((t) => t.name) ?? []);
  const [title, setTitle] = useState(goal?.title ?? "");
  const [more, setMore] = useState(!!goal?.title);
  const [h, setH] = useState(init.h);
  const [min, setMin] = useState(init.min);
  const [endless, setEndless] = useState(goal ? goal.days === null : false);
  const [days, setDays] = useState(goal?.days ?? 30);
  const [start, setStart] = useState(goal?.start ?? props.today);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const total = h * 60 + min;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const r = buildDraft({ projectId, tags: tagNames, title, h, min, start, endless, days });
    if ("error" in r) {
      setError(r.error);
      return;
    }
    setBusy(true);
    const err = await props.onSave(r.draft);
    setBusy(false);
    if (err) setError(err);
  };

  const num = (v: string) => (v === "" ? 0 : Number(v));

  return (
    <form className="goalform" onSubmit={submit} aria-label={goal ? "Edit goal" : "New goal"}>
      <div className="goalform__row">
        <div className="goalform__field">
          <span>Project</span>
          <ProjectPicker
            projects={props.projects}
            value={projectId}
            onChange={setProjectId}
            noneLabel="No project"
            label="Project"
          />
        </div>
        <div className="goalform__field goalform__field--grow">
          <span>Tags (any of)</span>
          <TagInput value={tagNames} onChange={setTagNames} known={props.tags} label="Tags" placeholder="mestrado, rust…" />
        </div>
      </div>
      <div className="goalform__more">
        <button type="button" className="goalform__morebtn" aria-expanded={more} onClick={() => setMore((v) => !v)}>
          More
        </button>
        {more && (
          <label className="goalform__field">
            <span>Only blocks titled</span>
            <input
              className="goalform__input"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Rust"
              aria-label="Title filter"
            />
          </label>
        )}
      </div>
      <div className="goalform__row">
        <div className="goalform__field">
          <span>Time per day</span>
          <span className="goalform__inline">
            <input className="goalform__input goalform__num" type="number" min={0} max={12} value={h} onChange={(e) => setH(num(e.target.value))} aria-label="Hours per day" />
            h
            <input className="goalform__input goalform__num" type="number" min={0} max={59} value={min} onChange={(e) => setMin(num(e.target.value))} aria-label="Minutes per day" />
            min
            <span className="goalform__presets">
              {PRESETS.map((m) => (
                <button
                  key={m}
                  type="button"
                  className="goalform__preset"
                  aria-pressed={total === m}
                  onClick={() => {
                    const v = fromMinutes(m);
                    setH(v.h);
                    setMin(v.min);
                  }}
                >
                  {fmtDuration(m)}
                </button>
              ))}
            </span>
          </span>
        </div>
        <div className="goalform__field">
          <span>Duration</span>
          <span className="goalform__inline">
            <input
              className="goalform__input goalform__num"
              type="number"
              min={1}
              value={days}
              disabled={endless}
              onChange={(e) => setDays(num(e.target.value))}
              aria-label="Number of days"
            />
            days
            <label className="goalform__check">
              <input type="checkbox" checked={endless} onChange={(e) => setEndless(e.target.checked)} /> No end
            </label>
          </span>
        </div>
        <label className="goalform__field">
          <span>Start</span>
          <input className="goalform__input" type="date" value={start} onChange={(e) => setStart(e.target.value)} />
        </label>
      </div>
      {error && (
        <p className="dash__error" role="alert">
          {error}
        </p>
      )}
      <div className="goalform__actions">
        <button type="submit" className="app__btn goals__primary" disabled={busy}>
          {goal ? "Save" : "Add goal"}
        </button>
        <button type="button" className="app__btn" onClick={props.onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
